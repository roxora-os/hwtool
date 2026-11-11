//! GTK 4 front end for hwtool.
//!
//! The app never runs as root and has no logic of its own. It reads state
//! with `hwtool --json ...`, shows the output of `hwtool ... --dry-run` in a
//! confirmation dialog, and runs the change with
//! `pkexec /usr/bin/hwtool ... --yes` (polkit action
//! io.github.roxora_os.hwtool.run).
//!
//! The confirmation shows a dry run made as the user; the root run gets its
//! fingerprint (--expect-plan) and refuses if its own plan differs. While a
//! change runs the window cannot be closed and offers no other action.
//!
//! Environment: HWTOOL_GTK_PAGE (overlays, kernel or boot),
//! HWTOOL_GTK_EXIT_AFTER_MS (quit after that many milliseconds) and
//! HWTOOL_GTK_SCREENSHOT (save the window as PNG to that path, then quit).
//! Built with the `test-hooks` feature only, for CI: HWTOOL_CLI (path of
//! the hwtool binary), HWTOOL_ROOT (passed as --root),
//! HWTOOL_GTK_NO_PKEXEC=1 (run hwtool directly) and HWTOOL_GTK_DEMO_BUSY=1
//! (show the window as it looks while a change runs). Release packages are built
//! without it, so the app always runs /usr/bin/hwtool through pkexec.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

const APP_ID: &str = "io.github.roxora_os.HwTool";

struct Ctx {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    banner: gtk::Label,
    busy: Cell<bool>,
    cli: String,
    root: Option<String>,
    pkexec: bool,
    overlays: gtk::Box,
    kernel: gtk::Box,
    boot: gtk::Box,
}

type Shared = Rc<Ctx>;

/// One hwtool command: its words, then names passed after `--`.
#[derive(Clone)]
struct Cmd {
    args: Vec<String>,
    names: Vec<String>,
}

impl Cmd {
    fn new(args: &[&str]) -> Self {
        Cmd {
            args: args.iter().map(|s| s.to_string()).collect(),
            names: Vec::new(),
        }
    }

    /// The command with `flags` before the names, which follow `--` so a
    /// name can never be read as an option.
    fn argv(&self, flags: &[String]) -> Vec<String> {
        let mut v = self.args.clone();
        v.extend(flags.iter().cloned());
        if !self.names.is_empty() {
            v.push("--".into());
            v.extend(self.names.iter().cloned());
        }
        v
    }
}

fn euid_is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

#[cfg(feature = "test-hooks")]
fn hooks() -> (String, Option<String>, bool) {
    (
        std::env::var("HWTOOL_CLI").unwrap_or_else(|_| "/usr/bin/hwtool".into()),
        std::env::var("HWTOOL_ROOT").ok().filter(|s| !s.is_empty()),
        std::env::var("HWTOOL_GTK_NO_PKEXEC").as_deref() != Ok("1"),
    )
}

#[cfg(not(feature = "test-hooks"))]
fn hooks() -> (String, Option<String>, bool) {
    ("/usr/bin/hwtool".into(), None, true)
}

fn main() -> glib::ExitCode {
    if euid_is_root() {
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            b"hwtool-gtk: do not run this app as root; it asks for the administrator password when it changes something\n",
        );
        return glib::ExitCode::FAILURE;
    }
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run_with_args::<&str>(&[])
}

fn page_box() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.set_margin_top(18);
    b.set_margin_bottom(18);
    b.set_margin_start(18);
    b.set_margin_end(18);
    b
}

fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Hardware Tool")
        .default_width(820)
        .default_height(600)
        .build();
    let stack = gtk::Stack::new();
    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    let header = gtk::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    window.set_titlebar(Some(&header));

    let (cli, root, pkexec) = hooks();
    let banner = gtk::Label::new(None);
    banner.set_visible(false);
    banner.set_wrap(true);
    banner.set_margin_top(12);
    banner.set_margin_start(18);
    banner.set_margin_end(18);
    let ctx: Shared = Rc::new(Ctx {
        window: window.clone(),
        stack: stack.clone(),
        banner: banner.clone(),
        busy: Cell::new(false),
        cli,
        root,
        pkexec,
        overlays: page_box(),
        kernel: page_box(),
        boot: page_box(),
    });
    // While a change runs the window stays open: closing it would leave the
    // change without anyone to show its result to.
    let ctx_close = ctx.clone();
    window.connect_close_request(move |_| {
        if ctx_close.busy.get() {
            message(
                &ctx_close,
                "A change is running",
                "Wait until it has finished. Closing the window now would not stop it, \
                 and you would not see whether it worked.",
            );
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    for (name, title, page) in [
        ("overlays", "Overlays", &ctx.overlays),
        ("kernel", "Kernel", &ctx.kernel),
        ("boot", "Boot storage", &ctx.boot),
    ] {
        let scroll = gtk::ScrolledWindow::builder()
            .child(page)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        stack.add_titled(&scroll, Some(name), title);
    }
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&banner);
    outer.append(&stack);
    window.set_child(Some(&outer));
    refresh(&ctx);
    #[cfg(feature = "test-hooks")]
    if std::env::var("HWTOOL_GTK_DEMO_BUSY").as_deref() == Ok("1") {
        set_busy(&ctx, true);
    }
    if let Ok(p) = std::env::var("HWTOOL_GTK_PAGE") {
        stack.set_visible_child_name(&p);
    }
    window.present();
    if let Ok(path) = std::env::var("HWTOOL_GTK_SCREENSHOT") {
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Err(e) = screenshot(&window, &path) {
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    format!("hwtool-gtk: screenshot failed: {e}\n").as_bytes(),
                );
            }
            app.quit();
        });
    }
    if let Some(ms) = std::env::var("HWTOOL_GTK_EXIT_AFTER_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(ms), move || app.quit());
    }
}

/// Render the window into a PNG file.
fn screenshot(window: &gtk::ApplicationWindow, path: &str) -> Result<(), String> {
    let child = window.child().ok_or("the window is empty")?;
    let (w, h) = (child.width(), child.height());
    let paintable = gtk::WidgetPaintable::new(Some(&child));
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, f64::from(w), f64::from(h));
    let node = snap.to_node().ok_or("the window drew nothing")?;
    let renderer = window.renderer().ok_or("the window has no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

// ---- talking to hwtool ----------------------------------------------------

fn base_args(ctx: &Ctx) -> Vec<String> {
    let mut v = vec![ctx.cli.clone()];
    if let Some(r) = &ctx.root {
        v.push("--root".into());
        v.push(r.clone());
    }
    v
}

/// Run hwtool as the current user and return stdout, or the error output.
fn run_cli(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let argv = [base_args(ctx), args.to_vec()].concat();
    match Command::new(&argv[0]).args(&argv[1..]).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("cannot run {}: {e}", ctx.cli)),
    }
}

fn json(ctx: &Ctx, args: &[&str]) -> Result<Value, String> {
    let mut a = vec!["--json".to_string()];
    a.extend(args.iter().map(|s| s.to_string()));
    let out = run_cli(ctx, &a)?;
    serde_json::from_str(&out).map_err(|e| format!("unexpected output from hwtool: {e}"))
}

/// Show what `commands` would do, and run them after the user agrees.
fn propose(ctx: &Shared, title: &str, commands: Vec<Cmd>) {
    if ctx.busy.get() {
        message(ctx, "A change is running", "Wait until it has finished.");
        return;
    }
    let mut text = String::new();
    let mut confirmed = Vec::new();
    for c in &commands {
        let mut a = vec!["--json".to_string()];
        a.extend(c.argv(&["--dry-run".to_string()]));
        match run_cli(ctx, &a)
            .and_then(|o| serde_json::from_str::<Value>(&o).map_err(|e| e.to_string()))
        {
            Ok(v) => {
                text.push_str(v["text"].as_str().unwrap_or(""));
                let fp = v["fingerprint"].as_str().unwrap_or("").to_string();
                confirmed.push((c.clone(), fp));
            }
            Err(e) => {
                message(ctx, "Not possible", &e);
                return;
            }
        }
        text.push('\n');
    }
    let ctx2 = ctx.clone();
    confirm(ctx, title, &text, move || execute(&ctx2, confirmed.clone()));
}

fn set_busy(ctx: &Ctx, busy: bool) {
    ctx.busy.set(busy);
    ctx.stack.set_sensitive(!busy);
    ctx.banner.set_visible(busy);
    if busy {
        ctx.banner.set_markup(
            "<b>A change is running. Do not switch the board off and do not close this window.</b>",
        );
    }
}

fn execute(ctx: &Shared, commands: Vec<(Cmd, String)>) {
    let ctx = ctx.clone();
    set_busy(&ctx, true);
    glib::spawn_future_local(async move {
        let mut log = String::new();
        let mut ok = true;
        for (c, fp) in commands {
            let mut argv: Vec<String> = Vec::new();
            if ctx.pkexec {
                argv.push("pkexec".into());
            }
            argv.extend(base_args(&ctx));
            argv.extend(c.argv(&["--yes".to_string(), "--expect-plan".to_string(), fp]));
            let os: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
            let proc = match gio::Subprocess::newv(
                &os,
                gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
            ) {
                Ok(p) => p,
                Err(e) => {
                    log.push_str(&format!("cannot start {}: {e}\n", argv[0]));
                    ok = false;
                    break;
                }
            };
            match proc.communicate_utf8_future(None).await {
                Ok((out, _)) => {
                    log.push_str(out.as_deref().unwrap_or(""));
                    if proc.exit_status() != 0 {
                        ok = false;
                        break;
                    }
                }
                Err(e) => {
                    log.push_str(&format!("{e}\n"));
                    ok = false;
                    break;
                }
            }
        }
        set_busy(&ctx, false);
        refresh(&ctx);
        message(&ctx, if ok { "Done" } else { "Failed" }, &log);
    });
}

// ---- dialogs --------------------------------------------------------------

fn text_view(text: &str) -> gtk::ScrolledWindow {
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.buffer().set_text(text);
    gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .min_content_height(320)
        .build()
}

fn dialog(ctx: &Ctx, title: &str, text: &str) -> (gtk::Window, gtk::Box) {
    let win = gtk::Window::builder()
        .transient_for(&ctx.window)
        .modal(true)
        .title(title)
        .default_width(720)
        .default_height(480)
        .build();
    let b = page_box();
    let heading = gtk::Label::new(None);
    heading.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(title)));
    heading.set_xalign(0.0);
    b.append(&heading);
    b.append(&text_view(text));
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    b.append(&buttons);
    win.set_child(Some(&b));
    (win, buttons)
}

fn confirm(ctx: &Shared, title: &str, text: &str, on_yes: impl Fn() + 'static) {
    let (win, buttons) = dialog(ctx, title, text);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("destructive-action");
    buttons.append(&cancel);
    buttons.append(&apply);
    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let w = win.clone();
    apply.connect_clicked(move |_| {
        w.close();
        on_yes();
    });
    win.present();
}

fn message(ctx: &Ctx, title: &str, text: &str) {
    let (win, buttons) = dialog(ctx, title, text);
    let close = gtk::Button::with_label("Close");
    buttons.append(&close);
    let w = win.clone();
    close.connect_clicked(move |_| w.close());
    win.present();
}

// ---- pages ----------------------------------------------------------------

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn heading(b: &gtk::Box, text: &str) {
    let l = gtk::Label::new(None);
    l.set_markup(&format!(
        "<big><b>{}</b></big>",
        glib::markup_escape_text(text)
    ));
    l.set_xalign(0.0);
    b.append(&l);
}

fn para(b: &gtk::Box, text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    b.append(&l);
    l
}

fn refresh(ctx: &Shared) {
    overlays_page(ctx);
    kernel_page(ctx);
    boot_page(ctx);
}

fn overlays_page(ctx: &Shared) {
    let b = &ctx.overlays;
    clear(b);
    heading(b, "Device tree overlays");
    para(b, "Overlays change the device tree the kernel starts with, for example to turn on a UART or I2C bus on the pin header. U-Boot applies them at the next boot.");
    let st = match json(ctx, &["overlays", "list"]) {
        Ok(v) => v,
        Err(e) => {
            para(b, &e);
            return;
        }
    };
    let prefixes: Vec<&str> = st["overlay_prefix"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    para(b, &format!("Overlay prefixes: {}", prefixes.join(" ")));
    let list = st["overlays"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        para(
            b,
            "The installed device tree package has no overlays for this board.",
        );
    }
    let checks: Rc<RefCell<Vec<(String, bool, gtk::CheckButton)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let listbox = gtk::ListBox::new();
    listbox.set_selection_mode(gtk::SelectionMode::None);
    for o in &list {
        let name = o["name"].as_str().unwrap_or("").to_string();
        let enabled = o["enabled"].as_bool().unwrap_or(false);
        let kind = if o["kind"] == "user" { " (user)" } else { "" };
        let c = gtk::CheckButton::with_label(&format!("{name}{kind}"));
        c.set_active(enabled);
        c.set_tooltip_text(o["file"].as_str());
        listbox.append(&c);
        checks.borrow_mut().push((name, enabled, c));
    }
    if !list.is_empty() {
        b.append(&listbox);
    }
    for m in st["missing"].as_array().cloned().unwrap_or_default() {
        para(
            b,
            &format!(
                "{} is enabled, but no file exists for it; it is skipped at boot.",
                m.as_str().unwrap_or("")
            ),
        );
    }
    let apply = gtk::Button::with_label("Apply changes");
    apply.set_halign(gtk::Align::Start);
    apply.set_sensitive(!list.is_empty());
    let ctx2 = ctx.clone();
    apply.connect_clicked(move |_| {
        let mut on = Vec::new();
        let mut off = Vec::new();
        for (name, was, c) in checks.borrow().iter() {
            match (was, c.is_active()) {
                (false, true) => on.push(name.clone()),
                (true, false) => off.push(name.clone()),
                _ => {}
            }
        }
        let mut cmds = Vec::new();
        if !on.is_empty() {
            cmds.push(Cmd {
                names: on,
                ..Cmd::new(&["overlays", "enable"])
            });
        }
        if !off.is_empty() {
            cmds.push(Cmd {
                names: off,
                ..Cmd::new(&["overlays", "disable"])
            });
        }
        if cmds.is_empty() {
            message(
                &ctx2,
                "Nothing to change",
                "No overlay was turned on or off.",
            );
        } else {
            propose(&ctx2, "Change overlays", cmds);
        }
    });
    b.append(&apply);
}

fn kernel_page(ctx: &Shared) {
    let b = &ctx.kernel;
    clear(b);
    heading(b, "Kernel branch");
    let st = match json(ctx, &["kernel", "status"]) {
        Ok(v) => v,
        Err(e) => {
            para(b, &e);
            return;
        }
    };
    let board = &st["board"];
    para(
        b,
        &format!(
            "{} ({}, {})",
            board["board_name"].as_str().unwrap_or(""),
            board["board"].as_str().unwrap_or(""),
            board["soc"].as_str().unwrap_or("")
        ),
    );
    let current = st["current"].as_str().unwrap_or("").to_string();
    para(
        b,
        &format!(
            "Kernel for the next boot: {} (branch {})",
            st["kernel"].as_str().unwrap_or("unknown"),
            if current.is_empty() {
                "unknown"
            } else {
                &current
            }
        ),
    );
    let mut others = Vec::new();
    for br in st["branches"].as_array().cloned().unwrap_or_default() {
        let name = br["branch"].as_str().unwrap_or("").to_string();
        let inst: Vec<String> = br["installed"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        format!(
                            "{} {}",
                            p["name"].as_str().unwrap_or(""),
                            p["version"].as_str().unwrap_or("")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        para(
            b,
            &format!(
                "Branch {name}{}: {}",
                if name == current { " (in use)" } else { "" },
                if inst.is_empty() {
                    "not installed".to_string()
                } else {
                    inst.join(", ")
                }
            ),
        );
        if name != current {
            others.push(name);
        }
    }
    para(b, "Switching installs the kernel, device tree and U-Boot packages of the other branch from the Armbian repository, removes the old kernel packages and needs a reboot. The bootloader on the boot device is not rewritten.");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let refs: Vec<&str> = others.iter().map(String::as_str).collect();
    let dd = gtk::DropDown::from_strings(&refs);
    let keep = gtk::CheckButton::with_label("Keep the old kernel installed");
    let switch = gtk::Button::with_label("Switch branch");
    switch.set_sensitive(!others.is_empty());
    row.append(&dd);
    row.append(&switch);
    b.append(&row);
    b.append(&keep);
    let ctx2 = ctx.clone();
    switch.connect_clicked(move |_| {
        if let Some(br) = others.get(dd.selected() as usize) {
            let mut c = Cmd::new(&["kernel", "switch"]);
            if keep.is_active() {
                c.args.push("--keep-old".into());
            }
            c.names.push(br.clone());
            propose(&ctx2, &format!("Switch to the {br} kernel"), vec![c]);
        }
    });
    let history = st["history"].as_array().cloned().unwrap_or_default();
    if let Some(h) = history.last() {
        para(
            b,
            &format!(
                "Last switch: {} from {} to {}.",
                h["time"].as_str().unwrap_or(""),
                h["from_branch"].as_str().unwrap_or("unknown"),
                h["to_branch"].as_str().unwrap_or("")
            ),
        );
        let revert = gtk::Button::with_label("Go back to the previous branch");
        revert.set_halign(gtk::Align::Start);
        let ctx2 = ctx.clone();
        revert.connect_clicked(move |_| {
            propose(
                &ctx2,
                "Revert the kernel branch",
                vec![Cmd::new(&["kernel", "revert"])],
            );
        });
        b.append(&revert);
    }
}

fn size(v: &Value) -> String {
    let bytes = v.as_u64().unwrap_or(0) as f64;
    if bytes >= (1u64 << 30) as f64 {
        format!("{:.1} GiB", bytes / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MiB", bytes / (1u64 << 20) as f64)
    }
}

fn boot_page(ctx: &Shared) {
    let b = &ctx.boot;
    clear(b);
    heading(b, "Boot storage");
    let st = match json(ctx, &["boot", "status"]) {
        Ok(v) => v,
        Err(e) => {
            para(b, &e);
            return;
        }
    };
    match st["uboot"].as_object() {
        Some(u) => para(
            b,
            &format!(
                "U-Boot package: {} {}",
                u.get("package")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                u.get("version").and_then(Value::as_str).unwrap_or("")
            ),
        ),
        None => para(b, "No U-Boot package is installed."),
    };
    let disks = st["disks"].as_array().cloned().unwrap_or_default();
    let spi = st["spi"].as_array().cloned().unwrap_or_default();
    for d in &disks {
        para(
            b,
            &format!(
                "{}  {}  {}{}",
                d["path"].as_str().unwrap_or(""),
                d["kind"].as_str().unwrap_or(""),
                size(&d["size_bytes"]),
                if d["holds_root"].as_bool() == Some(true) {
                    "  (running system)"
                } else {
                    ""
                }
            ),
        );
    }
    for m in &spi {
        para(
            b,
            &format!(
                "{}  SPI flash  {}",
                m["path"].as_str().unwrap_or(""),
                size(&m["size_bytes"])
            ),
        );
    }
    for m in st["missing"].as_array().cloned().unwrap_or_default() {
        para(b, &format!("Not available: {}", m.as_str().unwrap_or("")));
    }

    // Write the bootloader.
    heading(b, "Write the bootloader");
    para(
        b,
        "Writes U-Boot from the installed U-Boot package with the package's own writer.",
    );
    let mut targets: Vec<(String, String)> = Vec::new();
    if !spi.is_empty() {
        targets.push(("spi".into(), "SPI flash".into()));
    }
    for d in &disks {
        let p = d["path"].as_str().unwrap_or("");
        if p.starts_with("/dev/mmcblk") {
            targets.push((
                p.to_string(),
                format!("{p} ({})", d["kind"].as_str().unwrap_or("")),
            ));
        }
    }
    let labels: Vec<&str> = targets.iter().map(|t| t.1.as_str()).collect();
    let dd = gtk::DropDown::from_strings(&labels);
    let booted = gtk::CheckButton::with_label("Allow writing to the device the system runs from");
    let write = gtk::Button::with_label("Write bootloader");
    write.set_sensitive(!targets.is_empty());
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&dd);
    row.append(&write);
    b.append(&row);
    b.append(&booted);
    let ctx2 = ctx.clone();
    write.connect_clicked(move |_| {
        if let Some((t, _)) = targets.get(dd.selected() as usize) {
            let mut c = vec![
                "boot".to_string(),
                "write-bootloader".into(),
                "--target".into(),
                t.clone(),
            ];
            if booted.is_active() {
                c.push("--allow-booted-device".into());
            }
            propose(
                &ctx2,
                "Write the bootloader",
                vec![Cmd {
                    args: c,
                    names: vec![],
                }],
            );
        }
    });

    // Move the system.
    heading(b, "Move the system to eMMC or NVMe");
    para(b, "Erases the chosen device, copies the running system to it and makes it bootable. The SD card is not changed.");
    let dests: Vec<String> = disks
        .iter()
        .filter(|d| d["holds_root"].as_bool() != Some(true))
        .filter(|d| matches!(d["kind"].as_str(), Some("emmc") | Some("nvme")))
        .filter_map(|d| d["path"].as_str().map(str::to_string))
        .collect();
    let refs: Vec<&str> = dests.iter().map(String::as_str).collect();
    let dd2 = gtk::DropDown::from_strings(&refs);
    let spi_too = gtk::CheckButton::with_label("Also write U-Boot to SPI flash (for NVMe)");
    spi_too.set_sensitive(!spi.is_empty());
    let mv = gtk::Button::with_label("Copy the system");
    mv.add_css_class("destructive-action");
    mv.set_sensitive(!dests.is_empty());
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&dd2);
    row.append(&mv);
    b.append(&row);
    b.append(&spi_too);
    let ctx2 = ctx.clone();
    mv.connect_clicked(move |_| {
        if let Some(t) = dests.get(dd2.selected() as usize) {
            let mut c = vec![
                "boot".to_string(),
                "migrate".into(),
                "--target".into(),
                t.clone(),
            ];
            if spi_too.is_active() {
                c.push("--write-spi".into());
            }
            propose(
                &ctx2,
                &format!("Erase {t} and copy the system to it"),
                vec![Cmd {
                    args: c,
                    names: vec![],
                }],
            );
        }
    });
}
