//! Hardware Tool: the GTK 4 and libadwaita front end for hwtool.
//!
//! The app never runs as root and has no logic of its own. It reads state
//! with `hwtool --json ...`, shows the plan of `hwtool ... --dry-run` in a
//! confirmation dialog, and runs the change with
//! `pkexec /usr/bin/hwtool ... --yes --expect-plan <fingerprint>` (polkit
//! action io.github.roxora_os.hwtool.run), so what runs as root is the plan
//! the user confirmed. While a change runs the pages are insensitive, a
//! banner says so, and the window cannot be closed.
//!
//! Uses libadwaita 1.5 (Ubuntu 24.04): AdwToolbarView, AdwViewSwitcher with
//! a bottom AdwViewSwitcherBar below 550sp, AdwBanner, AdwToastOverlay,
//! AdwPreferencesPage, AdwSwitchRow, AdwComboRow, AdwExpanderRow,
//! AdwStatusPage and AdwAlertDialog.
//!
//! Environment: HWTOOL_GTK_PAGE (board, overlays, kernel, boot or npu),
//! HWTOOL_GTK_SIZE (WIDTHxHEIGHT), HWTOOL_GTK_DARK=1, HWTOOL_GTK_EXIT_AFTER_MS
//! and HWTOOL_GTK_SCREENSHOT (save the window as PNG to that path, then
//! quit). Built with the `test-hooks` feature only, for CI: HWTOOL_CLI,
//! HWTOOL_ROOT (passed as --root), HWTOOL_GTK_NO_PKEXEC=1,
//! HWTOOL_GTK_DEMO_BUSY=1 and HWTOOL_GTK_DEMO_CONFIRM=1 (open the
//! confirmation of a bootloader write). Release packages are built without
//! it, so the app always runs /usr/bin/hwtool through pkexec.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

const APP_ID: &str = "io.github.roxora_os.HwTool";

const CSS: &str = "
.tag {
  padding: 2px 8px;
  border-radius: 999px;
  background-color: alpha(@accent_bg_color, 0.15);
  color: @accent_color;
  font-size: smaller;
  font-weight: bold;
}
.tag.warning {
  background-color: alpha(@warning_bg_color, 0.2);
  color: @warning_color;
}
.tag.success {
  background-color: alpha(@success_bg_color, 0.2);
  color: @success_color;
}
.plan {
  font-family: monospace;
  font-size: smaller;
}
";

struct Ctx {
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    busy: Cell<bool>,
    cli: String,
    root: Option<String>,
    pkexec: bool,
    pages: RefCell<Vec<(String, adw::Bin)>>,
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

fn errln(msg: &str) {
    let _ = std::io::Write::write_all(&mut std::io::stderr(), format!("{msg}\n").as_bytes());
}

fn main() -> glib::ExitCode {
    if euid_is_root() {
        errln("hwtool-gtk: do not run this app as root; it asks for the administrator password when it changes something");
        return glib::ExitCode::FAILURE;
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        if let Some(d) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &d,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        match std::env::var("HWTOOL_GTK_DARK").as_deref() {
            Ok("1") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
            Ok("0") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
            _ => {}
        }
    });
    app.connect_activate(build_ui);
    app.run_with_args::<&str>(&[])
}

fn build_ui(app: &adw::Application) {
    let (w, h) = std::env::var("HWTOOL_GTK_SIZE")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((900, 700));
    let stack = adw::ViewStack::new();
    let header = adw::HeaderBar::new();
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));
    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
    let banner = adw::Banner::new(
        "A change is running. Do not switch the board off and do not close this window.",
    );
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);
    view.set_content(Some(&toasts));
    view.add_bottom_bar(&switcher_bar);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Hardware Tool")
        .default_width(w)
        .default_height(h)
        .width_request(360)
        .height_request(294)
        .content(&view)
        .build();
    // Narrow windows: title in the header bar, page switcher at the bottom.
    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 550sp").expect("valid breakpoint"),
    );
    let title = adw::WindowTitle::new("Hardware Tool", "");
    narrow.add_setter(&header, "title-widget", Some(&title.to_value()));
    narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    let (cli, root, pkexec) = hooks();
    let ctx: Shared = Rc::new(Ctx {
        window: window.clone(),
        stack: stack.clone(),
        banner,
        toasts,
        busy: Cell::new(false),
        cli,
        root,
        pkexec,
        pages: RefCell::new(Vec::new()),
    });
    for (name, title, icon) in [
        ("board", "Board", "computer-symbolic"),
        ("overlays", "Overlays", "view-list-symbolic"),
        ("kernel", "Kernel", "emblem-system-symbolic"),
        ("boot", "Storage", "drive-harddisk-symbolic"),
        ("npu", "NPU", "applications-science-symbolic"),
    ] {
        let holder = adw::Bin::new();
        stack.add_titled_with_icon(&holder, Some(name), title, icon);
        ctx.pages.borrow_mut().push((name.to_string(), holder));
    }

    // While a change runs the window stays open: closing it would leave the
    // change without anyone to show its result to.
    let ctx_close = ctx.clone();
    window.connect_close_request(move |_| {
        if ctx_close.busy.get() {
            alert(
                &ctx_close,
                "A Change Is Running",
                "Wait until it has finished. Closing the window would not stop it, and you would not see whether it worked.",
            );
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });

    refresh(&ctx);
    if let Ok(p) = std::env::var("HWTOOL_GTK_PAGE") {
        stack.set_visible_child_name(&p);
    }
    window.present();
    #[cfg(feature = "test-hooks")]
    {
        if std::env::var("HWTOOL_GTK_DEMO_BUSY").as_deref() == Ok("1") {
            set_busy(&ctx, true);
        }
        if std::env::var("HWTOOL_GTK_DEMO_CONFIRM").as_deref() == Ok("1") {
            let c = Cmd::new(&["boot", "write-bootloader", "--target", "spi"]);
            propose(&ctx, "Write the Bootloader?", vec![c], true);
        }
    }
    if let Ok(path) = std::env::var("HWTOOL_GTK_SCREENSHOT") {
        let app = app.clone();
        let win = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(2000), move || {
            if let Err(e) = screenshot(&win, &path) {
                errln(&format!("hwtool-gtk: screenshot failed: {e}"));
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

/// Render the whole window (header bar, page and any open dialog) to PNG.
fn screenshot(window: &adw::ApplicationWindow, path: &str) -> Result<(), String> {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let image = paintable.current_image();
    let (w, h) = (window.width(), window.height());
    let snap = gtk::Snapshot::new();
    image.snapshot(&snap, f64::from(w), f64::from(h));
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

fn strip_prefix(e: &str) -> String {
    e.strip_prefix("hwtool: ").unwrap_or(e).to_string()
}

/// Show what `commands` would do, and run them after the user agrees.
fn propose(ctx: &Shared, heading: &str, commands: Vec<Cmd>, destructive: bool) {
    if ctx.busy.get() {
        alert(ctx, "A Change Is Running", "Wait until it has finished.");
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
                alert(ctx, "Not Possible", &strip_prefix(&e));
                return;
            }
        }
        text.push('\n');
    }
    let dialog = adw::AlertDialog::new(
        Some(heading),
        Some("hwtool will do exactly this, as administrator. Read it before you go on."),
    );
    dialog.set_extra_child(Some(&plan_view(text.trim_end())));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("apply", if destructive { "Go Ahead" } else { "Apply" });
    dialog.set_response_appearance(
        "apply",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let ctx2 = ctx.clone();
    dialog.connect_response(None, move |_, r| {
        if r == "apply" {
            execute(&ctx2, confirmed.clone());
        }
    });
    dialog.present(Some(&ctx.window));
}

fn plan_view(text: &str) -> gtk::ScrolledWindow {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    view.add_css_class("plan");
    view.buffer().set_text(text);
    let sw = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(260)
        .max_content_height(420)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    sw.add_css_class("card");
    sw
}

fn set_busy(ctx: &Ctx, busy: bool) {
    ctx.busy.set(busy);
    ctx.stack.set_sensitive(!busy);
    ctx.banner.set_revealed(busy);
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
        if ok {
            let toast = adw::Toast::builder()
                .title("Done. Reboot for the change to take effect.")
                .button_label("Details")
                .timeout(8)
                .build();
            let ctx2 = ctx.clone();
            toast.connect_button_clicked(move |_| output(&ctx2, "Done", &log));
            ctx.toasts.add_toast(toast);
        } else {
            output(&ctx, "The Change Failed", &log);
        }
    });
}

fn alert(ctx: &Ctx, heading: &str, body: &str) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("close", "Close");
    d.present(Some(&ctx.window));
}

fn output(ctx: &Ctx, heading: &str, text: &str) {
    let d = adw::AlertDialog::new(
        Some(heading),
        Some("What hwtool printed. The full log is in /var/log/hwtool."),
    );
    d.set_extra_child(Some(&plan_view(text.trim_end())));
    d.add_response("close", "Close");
    d.present(Some(&ctx.window));
}

// ---- building blocks ------------------------------------------------------

fn page_holder(ctx: &Ctx, name: &str) -> adw::Bin {
    ctx.pages
        .borrow()
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.clone())
        .expect("page exists")
}

fn show(holder: &adw::Bin, child: &impl IsA<gtk::Widget>) {
    child.set_hexpand(true);
    child.set_vexpand(true);
    holder.set_child(Some(child));
}

fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    if let Some(d) = description {
        g.set_description(Some(d));
    }
    g
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    r
}

/// A row that shows a value: dim title above, value below (GNOME Settings
/// "About" style).
fn info(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .use_markup(false)
        .build();
    r.add_css_class("property");
    r
}

fn tag(text: &str, kind: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("tag");
    if !kind.is_empty() {
        l.add_css_class(kind);
    }
    l.set_valign(gtk::Align::Center);
    l
}

fn icon(name: &str) -> gtk::Image {
    gtk::Image::from_icon_name(name)
}

fn action_row(
    title: &str,
    subtitle: &str,
    label: &str,
    style: &str,
) -> (adw::ActionRow, gtk::Button) {
    let r = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    let b = gtk::Button::with_label(label);
    b.set_valign(gtk::Align::Center);
    if !style.is_empty() {
        b.add_css_class(style);
    }
    r.add_suffix(&b);
    r.set_activatable_widget(Some(&b));
    (r, b)
}

fn status_page(icon_name: &str, title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(icon_name)
        .title(title)
        .description(glib::markup_escape_text(description).as_str())
        .build()
}

/// The page for an error: unknown board, missing files.
fn error_page(e: &str) -> adw::StatusPage {
    let e = strip_prefix(e);
    if e.contains("cannot tell which board") {
        status_page(
            "dialog-question-symbolic",
            "Board Not Recognised",
            &format!(
                "Hardware Tool works on Roxora images for the boards Roxora supports \
                 (Radxa ROCK 4D, ArmSoM CM5-IO). It reads /etc/armbian-release, which \
                 the board support package installs.\n\n{e}"
            ),
        )
    } else {
        status_page("dialog-warning-symbolic", "Not Available", &e)
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn size(v: &Value) -> String {
    let bytes = v.as_u64().unwrap_or(0) as f64;
    if bytes >= (1u64 << 30) as f64 {
        format!("{:.1} GiB", bytes / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MiB", bytes / (1u64 << 20) as f64)
    }
}

fn kind_name(k: &str) -> &'static str {
    match k {
        "sd" => "SD card",
        "emmc" => "eMMC",
        "nvme" => "NVMe drive",
        "usb" => "USB drive",
        _ => "Disk",
    }
}

fn kind_icon(k: &str) -> &'static str {
    match k {
        "sd" => "drive-removable-media-symbolic",
        "emmc" | "nvme" => "drive-harddisk-solidstate-symbolic",
        "usb" => "drive-harddisk-usb-symbolic",
        _ => "drive-harddisk-symbolic",
    }
}

fn device_rows(g: &adw::PreferencesGroup, boot: &Value) {
    for d in boot["disks"].as_array().cloned().unwrap_or_default() {
        let r = row(
            kind_name(s(&d, "kind")),
            &format!("{}, {}", s(&d, "path"), size(&d["size_bytes"])),
        );
        r.add_prefix(&icon(kind_icon(s(&d, "kind"))));
        if d["holds_root"].as_bool() == Some(true) {
            r.add_suffix(&tag("Running system", ""));
        }
        g.add(&r);
    }
    for m in boot["spi"].as_array().cloned().unwrap_or_default() {
        let r = row(
            "SPI flash",
            &format!(
                "{}, {}, {}",
                s(&m, "path"),
                size(&m["size_bytes"]),
                s(&m, "label")
            ),
        );
        r.add_prefix(&icon("media-flash-symbolic"));
        g.add(&r);
    }
}

fn refresh(ctx: &Shared) {
    board_page(ctx);
    overlays_page(ctx);
    kernel_page(ctx);
    boot_page(ctx);
    npu_page(ctx);
}

// ---- pages ----------------------------------------------------------------

fn board_page(ctx: &Shared) {
    let holder = page_holder(ctx, "board");
    let k = match json(ctx, &["kernel", "status"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let b = &k["board"];
    let g = group("Board", None);
    g.add(&info("Model", s(b, "board_name")));
    g.add(&info("Board name", s(b, "board")));
    g.add(&info("SoC", &s(b, "soc").to_uppercase()));
    page.add(&g);
    let g = group("Kernel", None);
    g.add(&info(
        "Branch",
        if s(&k, "current").is_empty() {
            "unknown"
        } else {
            s(&k, "current")
        },
    ));
    g.add(&info("Kernel for the next boot", s(&k, "kernel")));
    page.add(&g);
    if let Ok(boot) = json(ctx, &["boot", "status"]) {
        let g = group("Bootloader", None);
        match boot["uboot"].as_object() {
            Some(u) => g.add(&info(
                "U-Boot package",
                &format!(
                    "{} {}",
                    u.get("package")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    u.get("version").and_then(Value::as_str).unwrap_or("")
                ),
            )),
            None => g.add(&info("U-Boot package", "none installed")),
        }
        page.add(&g);
        let g = group("Storage", None);
        device_rows(&g, &boot);
        page.add(&g);
    }
    show(&holder, &page);
}

fn overlays_page(ctx: &Shared) {
    let holder = page_holder(ctx, "overlays");
    let st = match json(ctx, &["overlays", "list"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let prefixes: Vec<&str> = st["overlay_prefix"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let list = st["overlays"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        show(
            &holder,
            &status_page(
                "view-list-symbolic",
                "No Overlays for This Board",
                &format!(
                    "The installed device tree package has no overlays with the prefix {}. \
                     Overlays you compile with armbian-add-overlay appear here.",
                    prefixes.join(" or ")
                ),
            ),
        );
        return;
    }
    let page = adw::PreferencesPage::new();
    let g = group(
        "Overlays",
        Some("Overlays change the device tree the kernel starts with, for example to turn on a UART or I2C bus. U-Boot applies them at the next boot."),
    );
    let switches: Rc<RefCell<Vec<(String, bool, adw::SwitchRow)>>> =
        Rc::new(RefCell::new(Vec::new()));
    for o in &list {
        let name = s(o, "name").to_string();
        let enabled = o["enabled"].as_bool().unwrap_or(false);
        let r = adw::SwitchRow::builder()
            .title(name.as_str())
            .subtitle(s(o, "file"))
            .use_markup(false)
            .active(enabled)
            .build();
        if o["kind"] == "user" {
            r.add_suffix(&tag("User", ""));
        }
        g.add(&r);
        switches.borrow_mut().push((name, enabled, r));
    }
    page.add(&g);
    let missing = st["missing"].as_array().cloned().unwrap_or_default();
    if !missing.is_empty() {
        let g = group(
            "Enabled but Missing",
            Some("boot.scr skips these: no file exists for them."),
        );
        for m in missing {
            let r = row(m.as_str().unwrap_or(""), "");
            r.add_prefix(&icon("dialog-warning-symbolic"));
            g.add(&r);
        }
        page.add(&g);
    }
    let g = group("", None);
    let (r, b) = action_row(
        "Apply Changes",
        &format!(
            "Prefixes: {}. Takes effect after a reboot.",
            prefixes.join(" ")
        ),
        "Apply",
        "suggested-action",
    );
    g.add(&r);
    page.add(&g);
    let ctx2 = ctx.clone();
    b.connect_clicked(move |_| {
        let mut on = Vec::new();
        let mut off = Vec::new();
        for (name, was, sw) in switches.borrow().iter() {
            match (was, sw.is_active()) {
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
            ctx2.toasts
                .add_toast(adw::Toast::new("No overlay was turned on or off."));
        } else {
            propose(&ctx2, "Change Overlays?", cmds, false);
        }
    });
    show(&holder, &page);
}

fn kernel_page(ctx: &Shared) {
    let holder = page_holder(ctx, "kernel");
    let st = match json(ctx, &["kernel", "status"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let current = s(&st, "current").to_string();
    let g = group(
        "Branches",
        Some("A branch is a kernel, its device tree and its U-Boot, from the Armbian repository."),
    );
    let mut others = Vec::new();
    for br in st["branches"].as_array().cloned().unwrap_or_default() {
        let name = s(&br, "branch").to_string();
        let installed = br["installed"].as_array().cloned().unwrap_or_default();
        let e = adw::ExpanderRow::builder()
            .title(name.as_str())
            .subtitle(if name == current {
                format!("In use, kernel {}", s(&st, "kernel"))
            } else if installed.is_empty() {
                "Not installed".to_string()
            } else {
                "Installed, not in use".to_string()
            })
            .build();
        if name == current {
            e.add_suffix(&tag("In use", ""));
        }
        let names: Vec<String> = br["packages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for p in &installed {
            e.add_row(&row(s(p, "name"), s(p, "version")));
        }
        for n in names
            .iter()
            .filter(|n| !installed.iter().any(|p| s(p, "name") == n.as_str()))
        {
            e.add_row(&row(n, "not installed"));
        }
        g.add(&e);
        if name != current {
            others.push(name);
        }
    }
    page.add(&g);

    if !others.is_empty() {
        let g = group(
            "Switch Branch",
            Some("Installs the other branch's packages, removes the old kernel packages and keeps them as files so you can go back. The bootloader on the boot device is not rewritten."),
        );
        let refs: Vec<&str> = others.iter().map(String::as_str).collect();
        let combo = adw::ComboRow::builder()
            .title("Branch")
            .model(&gtk::StringList::new(&refs))
            .build();
        let keep = adw::SwitchRow::builder()
            .title("Keep the old kernel installed")
            .subtitle("Lets you boot the old kernel again by hand if the new one does not start")
            .build();
        let (r, b) = action_row("Switch", "Needs a reboot", "Switch…", "");
        g.add(&combo);
        g.add(&keep);
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            if let Some(br) = others.get(combo.selected() as usize) {
                let mut c = Cmd::new(&["kernel", "switch"]);
                if keep.is_active() {
                    c.args.push("--keep-old".into());
                }
                c.names.push(br.clone());
                propose(
                    &ctx2,
                    &format!("Switch to the {br} Kernel?"),
                    vec![c],
                    false,
                );
            }
        });
    }

    if let Some(h) = st["history"].as_array().and_then(|a| a.last()) {
        let pending = s(h, "state") == "pending";
        let g = group("Previous Branch", None);
        let (r, b) = action_row(
            &format!(
                "Go back to {}",
                h["from_branch"].as_str().unwrap_or("the previous branch")
            ),
            &if pending {
                format!("The last switch, to {}, did not finish", s(h, "to_branch"))
            } else {
                format!(
                    "Last switch: {} to {}",
                    s(h, "from_branch"),
                    s(h, "to_branch")
                )
            },
            "Revert…",
            "",
        );
        if pending {
            r.add_prefix(&icon("dialog-warning-symbolic"));
        }
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            propose(
                &ctx2,
                "Revert the Kernel Branch?",
                vec![Cmd::new(&["kernel", "revert"])],
                false,
            );
        });
    }
    show(&holder, &page);
}

fn boot_page(ctx: &Shared) {
    let holder = page_holder(ctx, "boot");
    let st = match json(ctx, &["boot", "status"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let g = group("Devices", None);
    device_rows(&g, &st);
    page.add(&g);
    let missing = st["missing"].as_array().cloned().unwrap_or_default();
    if !missing.is_empty() {
        let g = group("Not Available", None);
        for m in missing {
            let r = row(m.as_str().unwrap_or(""), "");
            r.set_title_lines(0);
            r.add_prefix(&icon("dialog-information-symbolic"));
            g.add(&r);
        }
        page.add(&g);
    }

    let disks = st["disks"].as_array().cloned().unwrap_or_default();
    let spi = st["spi"].as_array().cloned().unwrap_or_default();

    let mut targets: Vec<(String, String)> = Vec::new();
    if !spi.is_empty() {
        targets.push(("spi".into(), "SPI flash".into()));
    }
    for d in &disks {
        let p = s(d, "path");
        if p.starts_with("/dev/mmcblk") {
            targets.push((p.to_string(), format!("{p} ({})", kind_name(s(d, "kind")))));
        }
    }
    if !targets.is_empty() && st["uboot"].is_object() {
        let g = group(
            "Write the Bootloader",
            Some("Writes U-Boot from the installed U-Boot package with the package's own writer. If the write is interrupted the board may need recovery in Maskrom mode."),
        );
        let labels: Vec<&str> = targets.iter().map(|t| t.1.as_str()).collect();
        let combo = adw::ComboRow::builder()
            .title("Write to")
            .model(&gtk::StringList::new(&labels))
            .build();
        let booted = adw::SwitchRow::builder()
            .title("Allow the device the system runs from")
            .subtitle("Replaces the bootloader this system starts with")
            .build();
        let (r, b) = action_row("Write Bootloader", "", "Write…", "destructive-action");
        g.add(&combo);
        g.add(&booted);
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            if let Some((t, _)) = targets.get(combo.selected() as usize) {
                let mut c = Cmd::new(&["boot", "write-bootloader", "--target", t]);
                if booted.is_active() {
                    c.args.push("--allow-booted-device".into());
                }
                propose(&ctx2, "Write the Bootloader?", vec![c], true);
            }
        });
    }

    let dests: Vec<String> = disks
        .iter()
        .filter(|d| d["holds_root"].as_bool() != Some(true))
        .filter(|d| matches!(d["kind"].as_str(), Some("emmc") | Some("nvme")))
        .filter_map(|d| d["path"].as_str().map(str::to_string))
        .collect();
    if !dests.is_empty() {
        let g = group(
            "Move the System",
            Some("Erases the chosen eMMC or NVMe drive, copies the running system to it and makes it bootable. The SD card is not changed."),
        );
        let refs: Vec<&str> = dests.iter().map(String::as_str).collect();
        let combo = adw::ComboRow::builder()
            .title("Copy to")
            .model(&gtk::StringList::new(&refs))
            .build();
        let spi_too = adw::SwitchRow::builder()
            .title("Also write U-Boot to SPI flash")
            .subtitle("For NVMe: U-Boot in SPI flash then starts the drive")
            .sensitive(!spi.is_empty())
            .build();
        let (r, b) = action_row(
            "Erase and Copy",
            "Everything on the chosen drive is lost",
            "Erase…",
            "destructive-action",
        );
        g.add(&combo);
        g.add(&spi_too);
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            if let Some(t) = dests.get(combo.selected() as usize) {
                let mut c = Cmd::new(&["boot", "migrate", "--target", t]);
                if spi_too.is_active() {
                    c.args.push("--write-spi".into());
                }
                propose(
                    &ctx2,
                    &format!("Erase {t} and Copy the System?"),
                    vec![c],
                    true,
                );
            }
        });
    }
    show(&holder, &page);
}

fn npu_page(ctx: &Shared) {
    let holder = page_holder(ctx, "npu");
    let st = match json(ctx, &["npu", "status"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let ready = st["ready"].as_bool() == Some(true);
    let g = group("NPU", None);
    let verdict = row(s(&st, "verdict"), "");
    verdict.set_title_lines(0);
    verdict.add_prefix(&icon(if ready {
        "emblem-ok-symbolic"
    } else {
        "dialog-information-symbolic"
    }));
    g.add(&verdict);
    g.add(&info(
        "Kernel",
        &format!(
            "{} (branch {})",
            if s(&st, "kernel").is_empty() {
                "unknown"
            } else {
                s(&st, "kernel")
            },
            if s(&st, "branch").is_empty() {
                "unknown"
            } else {
                s(&st, "branch")
            }
        ),
    ));
    if !s(&st, "running_kernel").is_empty() {
        g.add(&info("Running now", s(&st, "running_kernel")));
    }
    page.add(&g);

    let g = group("Driver and Device", None);
    for (key, title) in [("rocket", "rocket (mainline)"), ("rknpu", "rknpu (vendor)")] {
        let d = &st[key];
        let available = !d["module_file"].is_null() || d["builtin"].as_bool() == Some(true);
        if key == "rknpu" && !available && d["loaded"].as_bool() != Some(true) {
            continue;
        }
        let r = row(
            title,
            if !d["module_file"].is_null() {
                s(d, "module_file")
            } else if d["builtin"].as_bool() == Some(true) {
                "built into the kernel"
            } else {
                "not in this kernel"
            },
        );
        r.add_suffix(&if d["loaded"].as_bool() == Some(true) {
            tag("Loaded", "success")
        } else {
            tag("Not loaded", "warning")
        });
        g.add(&r);
    }
    let nodes = st["nodes"].as_array().cloned().unwrap_or_default();
    if nodes.is_empty() {
        g.add(&row("NPU core", "none in the device tree"));
    }
    for n in &nodes {
        let compat: Vec<&str> = n["compatible"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let r = row(
            "NPU core",
            &format!("{}, {}", s(n, "path"), compat.join(" ")),
        );
        let on = matches!(s(n, "status"), "okay" | "ok");
        r.add_suffix(&tag(s(n, "status"), if on { "success" } else { "warning" }));
        g.add(&r);
    }
    let accel = st["accel"].as_array().cloned().unwrap_or_default();
    if accel.is_empty() {
        g.add(&row("Accelerator device", "none in /dev/accel"));
    }
    for a in &accel {
        g.add(&row(
            "Accelerator device",
            &format!(
                "{} {} {}:{}",
                s(a, "path"),
                s(a, "mode"),
                s(a, "owner"),
                s(a, "group")
            ),
        ));
    }
    page.add(&g);

    let c = &st["charsiu"];
    let installed = c["installed"].as_str().map(str::to_string);
    let g = group(
        "charsiu",
        Some("charsiu runs language models on the NPU through the rocket driver. Models are not part of the package: fetch one with charsiu pull."),
    );
    g.add(&info(
        "Available",
        &match (c["candidate"].as_str(), c["origin"].as_str()) {
            (Some(v), Some(o)) => format!("{v} from {o}"),
            (Some(v), None) => v.to_string(),
            _ => "no package in the configured repositories".into(),
        },
    ));
    match installed {
        Some(v) => {
            let (r, b) = action_row(
                &format!("charsiu {v} is installed"),
                "",
                "Remove…",
                "destructive-action",
            );
            g.add(&r);
            let ctx2 = ctx.clone();
            b.connect_clicked(move |_| {
                propose(
                    &ctx2,
                    "Remove charsiu?",
                    vec![Cmd::new(&["npu", "remove-charsiu"])],
                    true,
                );
            });
        }
        None => {
            let npu_ok = st["can_install"].as_bool() == Some(true);
            let can = npu_ok && !c["candidate"].is_null();
            let (r, b) = action_row(
                "Install charsiu",
                if can {
                    "From the Armbian repository, with apt"
                } else if !npu_ok {
                    "Needs the edge kernel with the rocket driver and an enabled NPU"
                } else {
                    "apt has no charsiu package: is the Armbian repository enabled?"
                },
                "Install…",
                "suggested-action",
            );
            b.set_sensitive(can);
            g.add(&r);
            let ctx2 = ctx.clone();
            b.connect_clicked(move |_| {
                propose(
                    &ctx2,
                    "Install charsiu?",
                    vec![Cmd::new(&["npu", "install-charsiu"])],
                    false,
                );
            });
        }
    }
    page.add(&g);
    show(&holder, &page);
}
