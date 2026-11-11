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
//! Uses libadwaita 1.5 (Ubuntu 24.04), laid out like GNOME Settings: an
//! AdwNavigationSplitView with the pages in a sidebar, collapsed to one
//! pane below 550sp; AdwToolbarView, AdwBanner, AdwToastOverlay,
//! AdwPreferencesPage, AdwSwitchRow, AdwComboRow, AdwSpinRow, AdwEntryRow,
//! AdwStatusPage and AdwAlertDialog.
//!
//! Environment: HWTOOL_GTK_PAGE (board, overlays, interfaces, kernel, boot,
//! cpu, memory, devices or npu; "boot" is the Boot page: boot options,
//! devices, bootloader version and boot order, migration),
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

/// Sidebar pages: name, title, icon.
const PAGES: &[(&str, &str, &str)] = &[
    ("board", "Board", "computer-symbolic"),
    ("overlays", "Overlays", "view-list-symbolic"),
    ("interfaces", "Pin Header", "input-dialpad-symbolic"),
    ("kernel", "Kernel", "emblem-system-symbolic"),
    ("boot", "Boot", "drive-harddisk-symbolic"),
    ("cpu", "CPU", "power-profile-performance-symbolic"),
    ("memory", "Memory", "drive-harddisk-system-symbolic"),
    ("devices", "Devices", "preferences-other-symbolic"),
    ("npu", "NPU", "applications-science-symbolic"),
];

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
    gtk::Window::set_default_icon_name(APP_ID);
    let menu = gio::Menu::new();
    menu.append(Some("About Hardware Tool"), Some("app.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .primary(true)
        .build();
    // GNOME Settings layout: pages in a sidebar, the page beside it; below
    // 550sp the sidebar and the page are shown one at a time.
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Hardware Tool", "")));
    sidebar_header.pack_end(&menu_button);
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build(),
    ));
    let sidebar = adw::NavigationPage::builder()
        .title("Hardware Tool")
        .tag("sidebar")
        .child(&sidebar_view)
        .build();

    let banner = adw::Banner::new(
        "A change is running. Do not switch the board off and do not close this window.",
    );
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.add_top_bar(&banner);
    view.set_content(Some(&toasts));
    let content = adw::NavigationPage::builder()
        .title("Board")
        .tag("content")
        .child(&view)
        .build();
    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar)
        .content(&content)
        .min_sidebar_width(200.0)
        .max_sidebar_width(260.0)
        .build();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .icon_name(APP_ID)
        .title("Hardware Tool")
        .default_width(w)
        .default_height(h)
        .width_request(360)
        .height_request(294)
        .content(&split)
        .build();
    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 550sp").expect("valid breakpoint"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    let about = gio::SimpleAction::new("about", None);
    let win_about = window.clone();
    about.connect_activate(move |_, _| {
        adw::AboutDialog::builder()
            .application_name("Hardware Tool")
            .application_icon(APP_ID)
            .developer_name("Roxora Project")
            .version(env!("CARGO_PKG_VERSION"))
            .website("https://github.com/roxora-os/hwtool")
            .issue_url("https://github.com/roxora-os/hwtool/issues")
            .license_type(gtk::License::Gpl20Only)
            .build()
            .present(Some(&win_about));
    });
    app.add_action(&about);
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
    for (name, title, icon_name) in PAGES {
        let holder = adw::Bin::new();
        stack.add_titled(&holder, Some(name), title);
        ctx.pages.borrow_mut().push((name.to_string(), holder));
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.append(&icon(icon_name));
        b.append(&gtk::Label::builder().label(*title).xalign(0.0).build());
        let r = gtk::ListBoxRow::builder().child(&b).build();
        r.set_widget_name(name);
        list.append(&r);
    }
    let (stack2, content2, split2) = (stack.clone(), content.clone(), split.clone());
    list.connect_row_activated(move |_, r| {
        let name = r.widget_name();
        stack2.set_visible_child_name(&name);
        if let Some((_, t, _)) = PAGES.iter().find(|(n, _, _)| *n == name.as_str()) {
            content2.set_title(t);
        }
        split2.set_show_content(true);
    });
    let select = {
        let list = list.clone();
        move |name: &str| {
            let mut i = 0;
            while let Some(r) = list.row_at_index(i) {
                if r.widget_name() == name {
                    list.select_row(Some(&r));
                    r.emit_activate();
                    return;
                }
                i += 1;
            }
        }
    };
    select("board");
    split.set_show_content(false);

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
        select(&p);
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
    propose_or(ctx, heading, commands, destructive, || {});
}

/// [`propose`], calling `cancelled` when nothing runs (a switch or a choice
/// that started it goes back).
fn propose_or(
    ctx: &Shared,
    heading: &str,
    commands: Vec<Cmd>,
    destructive: bool,
    cancelled: impl Fn() + 'static,
) {
    if ctx.busy.get() {
        alert(ctx, "A Change Is Running", "Wait until it has finished.");
        cancelled();
        return;
    }
    let mut text = String::new();
    let mut confirmed = Vec::new();
    let mut reboot = false;
    for c in &commands {
        let mut a = vec!["--json".to_string()];
        a.extend(c.argv(&["--dry-run".to_string()]));
        match run_cli(ctx, &a)
            .and_then(|o| serde_json::from_str::<Value>(&o).map_err(|e| e.to_string()))
        {
            Ok(v) => {
                text.push_str(v["text"].as_str().unwrap_or(""));
                let fp = v["fingerprint"].as_str().unwrap_or("").to_string();
                reboot |= v["plan"]["reboot_required"].as_bool() == Some(true);
                confirmed.push((c.clone(), fp));
            }
            Err(e) => {
                alert(ctx, "Not Possible", &strip_prefix(&e));
                cancelled();
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
            execute(&ctx2, confirmed.clone(), reboot);
        } else {
            cancelled();
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

fn execute(ctx: &Shared, commands: Vec<(Cmd, String)>, reboot: bool) {
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
                .title(if reboot {
                    "Done. Reboot for the change to take effect."
                } else {
                    "Done."
                })
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
    let r = adw::ActionRow::builder().use_markup(false).build();
    r.set_title(title);
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    r
}

/// A row that shows a value: dim title above, value below (GNOME Settings
/// "About" style).
fn info(title: &str, value: &str) -> adw::ActionRow {
    // use_markup first: values such as a kernel command line contain "<".
    let r = adw::ActionRow::builder().use_markup(false).build();
    r.set_title(title);
    r.set_subtitle(value);
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
    let r = adw::ActionRow::builder().use_markup(false).build();
    r.set_title(title);
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

/// "crw-rw----" -> "read and write for the group".
fn access_words(mode: &str) -> String {
    let m: Vec<char> = mode.chars().collect();
    if m.len() < 10 {
        return mode.to_string();
    }
    let perm = |r: char, w: char| match (r == 'r', w == 'w') {
        (true, true) => Some("read and write"),
        (true, false) => Some("read only"),
        (false, true) => Some("write only"),
        _ => None,
    };
    let mut parts = Vec::new();
    if let Some(p) = perm(m[4], m[5]) {
        parts.push(format!("{p} for the group"));
    }
    if let Some(p) = perm(m[7], m[8]) {
        parts.push(format!("{p} for everyone"));
    }
    if parts.is_empty() {
        "owner only".into()
    } else {
        parts.join(", ")
    }
}

fn kind_name(k: &str) -> &'static str {
    match k {
        "sd" => "SD card",
        "emmc" => "eMMC",
        "nvme" => "NVMe drive",
        "usb" => "USB drive",
        "sata" => "SATA drive",
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
    interfaces_page(ctx);
    kernel_page(ctx);
    boot_page(ctx);
    cpu_page(ctx);
    memory_page(ctx);
    devices_page(ctx);
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

/// Value of a boot option, or boot.scr's default.
fn opt(be: &Value, key: &str) -> Option<String> {
    be["options"]
        .as_array()?
        .iter()
        .find(|o| s(o, "key") == key)
        .map(|o| {
            o["value"]
                .as_str()
                .or_else(|| o["default"].as_str())
                .unwrap_or("")
                .to_string()
        })
}

/// A boot option row: key, value when the page was built, current value.
type Field = (String, String, Box<dyn Fn() -> String>);

fn boot_options(ctx: &Shared, page: &adw::PreferencesPage, be: &Value) {
    let g = group(
        "Boot Options",
        Some("Settings in /boot/armbianEnv.txt that boot.scr reads. They take effect at the next boot."),
    );
    let mut fields: Vec<Field> = Vec::new();
    if let Some(v) = opt(be, "verbosity") {
        let levels = ["0", "1", "2", "3", "4", "5", "6", "7"];
        let c = adw::ComboRow::builder()
            .title("Kernel log level")
            .subtitle("0 is quiet, 7 shows everything")
            .model(&gtk::StringList::new(&levels))
            .selected(levels.iter().position(|l| *l == v).unwrap_or(1) as u32)
            .build();
        g.add(&c);
        fields.push((
            "verbosity".into(),
            v,
            Box::new(move || levels[c.selected() as usize].to_string()),
        ));
    }
    if let Some(v) = opt(be, "console") {
        let values = ["both", "serial", "display"];
        let labels = ["Serial and display", "Serial port", "Display"];
        let c = adw::ComboRow::builder()
            .title("Console")
            .model(&gtk::StringList::new(&labels))
            .selected(values.iter().position(|l| *l == v).unwrap_or(0) as u32)
            .build();
        g.add(&c);
        fields.push((
            "console".into(),
            v,
            Box::new(move || values[c.selected() as usize].to_string()),
        ));
    }
    for (key, title, subtitle, on, off) in [
        (
            "bootlogo",
            "Boot splash",
            "Show a logo instead of kernel messages",
            "true",
            "false",
        ),
        (
            "earlycon",
            "Early console",
            "Kernel messages from the first moment of boot",
            "on",
            "off",
        ),
        (
            "docker_optimizations",
            "Container options",
            "cgroup memory and cpuset options for Docker",
            "on",
            "off",
        ),
    ] {
        if let Some(v) = opt(be, key) {
            let sw = adw::SwitchRow::builder()
                .title(title)
                .subtitle(subtitle)
                .active(v == on)
                .build();
            g.add(&sw);
            fields.push((
                key.into(),
                v,
                Box::new(move || if sw.is_active() { on } else { off }.to_string()),
            ));
        }
    }
    if let Some(v) = opt(be, "extraargs") {
        let e = adw::EntryRow::builder()
            .title("Extra kernel arguments")
            .text(v.as_str())
            .build();
        g.add(&e);
        fields.push((
            "extraargs".into(),
            v,
            Box::new(move || e.text().to_string()),
        ));
    }
    if let Some(c) = be["cmdline"].as_str() {
        let r = info("Kernel command line", c);
        r.set_subtitle_lines(0);
        g.add(&r);
    }
    let (r, b) = action_row(
        "Apply boot options",
        "Backs up armbianEnv.txt first",
        "Apply",
        "",
    );
    g.add(&r);
    page.add(&g);
    let ctx2 = ctx.clone();
    b.connect_clicked(move |_| {
        let cmds: Vec<Cmd> = fields
            .iter()
            .filter(|(_, was, now)| *was != now())
            .map(|(k, _, now)| {
                let mut c = Cmd::new(&["bootenv", "set"]);
                c.names.push(format!("{k}={}", now()));
                c
            })
            .collect();
        if cmds.is_empty() {
            ctx2.toasts
                .add_toast(adw::Toast::new("No boot option was changed."));
        } else {
            propose(&ctx2, "Change the Boot Options?", cmds, false);
        }
    });
}

fn cpu_page(ctx: &Shared) {
    let holder = page_holder(ctx, "cpu");
    let st = match json(ctx, &["cpu", "show"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    for pol in st["policies"].as_array().cloned().unwrap_or_default() {
        let name = s(&pol, "name").to_string();
        let cpus: Vec<String> = pol["cpus"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_u64)
                    .map(|c| c.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let core = s(&pol, "core");
        let title = if core.is_empty() {
            format!("CPUs {}", cpus.join(", "))
        } else {
            format!("{core} Cores")
        };
        let g = group(
            &title,
            Some(&format!(
                "CPUs {} ({name}). Now {} MHz to {} MHz, governor {}.",
                cpus.join(", "),
                pol["min_khz"].as_u64().unwrap_or(0) / 1000,
                pol["max_khz"].as_u64().unwrap_or(0) / 1000,
                s(&pol, "governor")
            )),
        );
        let governors: Vec<String> = pol["governors"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let freqs: Vec<u64> = pol["frequencies_khz"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let gov_refs: Vec<&str> = governors.iter().map(String::as_str).collect();
        let gov = adw::ComboRow::builder()
            .title("Governor")
            .model(&gtk::StringList::new(&gov_refs))
            .selected(
                governors
                    .iter()
                    .position(|g| g == s(&pol, "governor"))
                    .unwrap_or(0) as u32,
            )
            .build();
        let labels: Vec<String> = freqs.iter().map(|f| format!("{} MHz", f / 1000)).collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let pos = |k: &str| {
            freqs
                .iter()
                .position(|f| Some(*f) == pol[k].as_u64())
                .unwrap_or(0) as u32
        };
        let min = adw::ComboRow::builder()
            .title("Lowest frequency")
            .model(&gtk::StringList::new(&label_refs))
            .selected(pos("min_khz"))
            .build();
        let max = adw::ComboRow::builder()
            .title("Highest frequency")
            .model(&gtk::StringList::new(&label_refs))
            .selected(pos("max_khz"))
            .build();
        g.add(&gov);
        if !freqs.is_empty() {
            g.add(&min);
            g.add(&max);
        }
        let (r, b) = action_row("Apply", "Now and at every boot", "Apply", "");
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            let mut c = Cmd::new(&["cpu", "set", "--policy", &name]);
            if let Some(g) = governors.get(gov.selected() as usize) {
                c.args.push("--governor".into());
                c.args.push(g.clone());
            }
            if let (Some(lo), Some(hi)) = (
                freqs.get(min.selected() as usize),
                freqs.get(max.selected() as usize),
            ) {
                c.args.extend([
                    "--min".into(),
                    lo.to_string(),
                    "--max".into(),
                    hi.to_string(),
                ]);
            }
            propose(&ctx2, "Change the CPU Frequency?", vec![c], false);
        });
    }
    if st["persistent"].as_bool() == Some(true) {
        let g = group("", None);
        let (r, b) = action_row(
            "Settings applied at boot",
            "/etc/hwtool/cpufreq.conf",
            "Reset",
            "destructive-action",
        );
        g.add(&r);
        page.add(&g);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            propose(
                &ctx2,
                "Reset the CPU Frequency?",
                vec![Cmd::new(&["cpu", "reset"])],
                true,
            );
        });
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
        let sp = status_page(
            "view-list-symbolic",
            "No Overlays for This Board",
            &format!(
                "The installed device tree package has no overlays with the prefix {}. \
                 You can add your own from a .dts or .dtso file.",
                prefixes.join(" or ")
            ),
        );
        let add = gtk::Button::with_label("Add Overlay…");
        add.add_css_class("pill");
        add.add_css_class("suggested-action");
        add.set_halign(gtk::Align::Center);
        let ctx2 = ctx.clone();
        add.connect_clicked(move |_| choose_overlay(&ctx2));
        sp.set_child(Some(&add));
        show(&holder, &sp);
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
            let rm = gtk::Button::from_icon_name("user-trash-symbolic");
            rm.set_valign(gtk::Align::Center);
            rm.add_css_class("flat");
            rm.set_tooltip_text(Some("Remove this user overlay"));
            let ctx2 = ctx.clone();
            let n = name.clone();
            rm.connect_clicked(move |_| {
                propose(
                    &ctx2,
                    "Remove the User Overlay?",
                    vec![Cmd {
                        names: vec![n.clone()],
                        ..Cmd::new(&["overlays", "remove"])
                    }],
                    true,
                );
            });
            r.add_suffix(&tag("User", ""));
            r.add_suffix(&rm);
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
    let (r, add) = action_row(
        "Add an overlay",
        "Compile a .dts or .dtso file and enable it",
        "Add…",
        "",
    );
    g.add(&r);
    let ctx2 = ctx.clone();
    add.connect_clicked(move |_| choose_overlay(&ctx2));
    let (r, b) = action_row(
        "Apply changes",
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

fn choose_overlay(ctx: &Shared) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Device tree overlay sources"));
    filter.add_pattern("*.dts");
    filter.add_pattern("*.dtso");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Add an Overlay")
        .filters(&filters)
        .modal(true)
        .build();
    let ctx2 = ctx.clone();
    dialog.open(Some(&ctx.window), gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                let mut c = Cmd::new(&["overlays", "add"]);
                c.names.push(path.display().to_string());
                propose(&ctx2, "Add the Overlay?", vec![c], false);
            }
        }
    });
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
        let (r, b) = action_row("Switch", "Needs a reboot", "Switch", "");
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
            "Revert",
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
    // Headers and hold.
    let current_branch = current.clone();
    let headers_installed = st["branches"]
        .as_array()
        .and_then(|a| a.iter().find(|b| s(b, "branch") == current_branch))
        .and_then(|b| b["installed"].as_array())
        .and_then(|a| {
            a.iter()
                .find(|p| s(p, "name").starts_with("linux-headers-"))
        })
        .map(|p| format!("{} {}", s(p, "name"), s(p, "version")));
    let dkms: Vec<&str> = st["dkms"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let g = group(
        "Kernel Headers",
        Some("DKMS needs the headers of the running kernel to build its modules for each new kernel."),
    );
    if !dkms.is_empty() {
        g.add(&info("DKMS modules", &dkms.join(", ")));
    }
    let (r, b) = match &headers_installed {
        Some(v) => action_row("Kernel headers", v, "Remove", "destructive-action"),
        None => action_row("Kernel headers", "Not installed", "Install", ""),
    };
    g.add(&r);
    let install = headers_installed.is_none();
    let ctx2 = ctx.clone();
    b.connect_clicked(move |_| {
        let c = Cmd::new(&[
            "kernel",
            "headers",
            if install { "install" } else { "remove" },
        ]);
        propose(
            &ctx2,
            if install {
                "Install the Kernel Headers?"
            } else {
                "Remove the Kernel Headers?"
            },
            vec![c],
            !install,
        );
    });
    page.add(&g);

    let held: Vec<&str> = st["held"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let g = group(
        "Updates",
        Some("Holding keeps apt from upgrading the kernel, device tree, U-Boot and firmware packages. Kernel security fixes do not arrive while they are held."),
    );
    let (r, b) = if held.is_empty() {
        action_row("Kernel packages", "Upgraded by apt", "Hold", "")
    } else {
        let r = action_row(
            "Kernel packages",
            &format!("Held: {}", held.join(", ")),
            "Release",
            "",
        );
        r.0.add_prefix(&icon("changes-prevent-symbolic"));
        r
    };
    g.add(&r);
    let hold = held.is_empty();
    let ctx2 = ctx.clone();
    b.connect_clicked(move |_| {
        let c = Cmd::new(&["kernel", if hold { "hold" } else { "unhold" }]);
        propose(
            &ctx2,
            if hold {
                "Hold the Kernel Packages?"
            } else {
                "Release the Kernel Packages?"
            },
            vec![c],
            false,
        );
    });
    page.add(&g);
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
    if let Ok(be) = json(ctx, &["bootenv", "show"]) {
        boot_options(ctx, &page, &be);
    }
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

    bootloader_groups(ctx, &page);

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
        let (r, b) = action_row("Write bootloader", "", "Write", "destructive-action");
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
        .filter(|d| {
            matches!(
                d["kind"].as_str(),
                Some("emmc") | Some("nvme") | Some("usb") | Some("sata")
            )
        })
        .filter_map(|d| d["path"].as_str().map(str::to_string))
        .collect();
    if !dests.is_empty() {
        let g = group(
            "Move the System",
            Some("Erases the chosen drive, copies the running system to it and makes it bootable. The SD card is not changed. USB drives need U-Boot in SPI flash."),
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
            "Erase and copy",
            "Everything on the chosen drive is lost",
            "Erase",
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
        g.add(&row("NPU core", "None in the device tree"));
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
        g.add(&row("Accelerator device", "None in /dev/accel"));
    }
    for a in &accel {
        g.add(&row(
            "Accelerator device",
            &format!(
                "{}, {}:{}, {}",
                s(a, "path"),
                s(a, "owner"),
                s(a, "group"),
                access_words(s(a, "mode"))
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
                "Remove",
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
                    s(&st, "verdict")
                } else {
                    "apt has no charsiu package: is the Armbian repository enabled?"
                },
                "Install",
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

/// Read state that needs root (the boot media) through pkexec, then call
/// `done` with hwtool's JSON.
fn privileged_json(
    ctx: &Shared,
    args: &[&str],
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let mut argv: Vec<String> = Vec::new();
    if ctx.pkexec {
        argv.push("pkexec".into());
    }
    argv.extend(base_args(ctx));
    argv.push("--json".into());
    argv.extend(args.iter().map(|s| s.to_string()));
    glib::spawn_future_local(async move {
        let os: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
        let r = match gio::Subprocess::newv(
            &os,
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
        ) {
            Err(e) => Err(e.to_string()),
            Ok(p) => match p.communicate_utf8_future(None).await {
                Err(e) => Err(e.to_string()),
                Ok((out, _)) if p.exit_status() == 0 => {
                    serde_json::from_str(out.as_deref().unwrap_or("")).map_err(|e| e.to_string())
                }
                Ok((_, err)) => Err(strip_prefix(err.as_deref().unwrap_or("").trim())),
            },
        };
        done(r);
    });
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn note_row(text: &str) -> adw::ActionRow {
    let r = row(text, "");
    r.set_title_lines(0);
    r.add_prefix(&icon("dialog-information-symbolic"));
    r
}

// ---- pin header -----------------------------------------------------------

fn interfaces_page(ctx: &Shared) {
    let holder = page_holder(ctx, "interfaces");
    let st = match json(ctx, &["interfaces", "list"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let list = st["interfaces"].as_array().cloned().unwrap_or_default();
    let intro = group(
        "",
        Some("Functions of the 40-pin header. Each one is a device tree overlay that boot.scr applies at the next boot. Functions that share pins cannot be on together."),
    );
    page.add(&intro);
    for (kind, title) in [
        ("i2c", "I2C"),
        ("spi", "SPI"),
        ("uart", "UART"),
        ("pwm", "PWM"),
        ("w1", "1-Wire"),
    ] {
        let items: Vec<&Value> = list.iter().filter(|i| s(i, "kind") == kind).collect();
        if items.is_empty() {
            continue;
        }
        let g = group(title, None);
        for i in items {
            let name = s(i, "name").to_string();
            let pins: Vec<String> = i["pins"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|p| format!("{} {}", p["pin"].as_u64().unwrap_or(0), s(p, "signal")))
                .collect();
            let blocked: Vec<String> = strings(&i["blocked_by"])
                .iter()
                .map(|b| {
                    list.iter()
                        .find(|o| s(o, "name") == b)
                        .map(|o| s(o, "title").to_string())
                        .unwrap_or_else(|| b.clone())
                })
                .collect();
            let on = i["enabled"].as_bool() == Some(true);
            let mut sub = format!("Pins {}", pins.join(", "));
            if !blocked.is_empty() && !on {
                sub.push_str(&format!("\nIn use by {}", blocked.join(", ")));
            }
            let r = adw::SwitchRow::builder()
                .title(s(i, "title"))
                .subtitle(&sub)
                .subtitle_lines(0)
                .use_markup(false)
                .active(on)
                .sensitive(on || blocked.is_empty())
                .build();
            let ctx2 = ctx.clone();
            r.connect_active_notify(move |r| {
                let want = r.is_active();
                if want == on {
                    return;
                }
                let mut c = Cmd::new(&["interfaces", if want { "enable" } else { "disable" }]);
                c.names.push(name.clone());
                let r2 = r.clone();
                propose_or(
                    &ctx2,
                    if want {
                        "Turn On This Function?"
                    } else {
                        "Turn Off This Function?"
                    },
                    vec![c],
                    false,
                    move || r2.set_active(on),
                );
            });
            g.add(&r);
        }
        page.add(&g);
    }
    let busy = st["busy"].as_array().cloned().unwrap_or_default();
    if !busy.is_empty() {
        let g = group(
            "Used by the Board",
            Some("These pins are taken by the board itself and are not offered."),
        );
        for b in busy {
            let pin = b[0].as_u64().unwrap_or(0);
            g.add(&info(&format!("Pin {pin}"), b[1].as_str().unwrap_or("")));
        }
        page.add(&g);
    }
    show(&holder, &page);
}

// ---- memory ---------------------------------------------------------------

fn memory_page(ctx: &Shared) {
    let holder = page_holder(ctx, "memory");
    let st = match json(ctx, &["swap", "show"]) {
        Ok(v) => v,
        Err(e) => {
            show(&holder, &error_page(&e));
            return;
        }
    };
    let page = adw::PreferencesPage::new();
    let g = group("Memory", None);
    g.add(&info("Installed", &size(&st["mem_total_bytes"])));
    g.add(&info("Available now", &size(&st["mem_available_bytes"])));
    page.add(&g);

    let g = group(
        "Swap",
        Some("Swap in use now. zram keeps compressed memory in memory and is used before the swap file."),
    );
    let active = st["active"].as_array().cloned().unwrap_or_default();
    if active.is_empty() {
        g.add(&row("No swap in use", ""));
    }
    for a in &active {
        let name = s(a, "name");
        let r = row(
            if name.starts_with("/dev/zram") {
                "Compressed memory (zram)"
            } else {
                name
            },
            &format!(
                "{}, {} used, priority {}",
                size(&a["size_bytes"]),
                size(&a["used_bytes"]),
                a["priority"].as_i64().unwrap_or(0)
            ),
        );
        g.add(&r);
    }
    page.add(&g);

    let file = s(&st, "file").to_string();
    let exists = st["file_bytes"].is_u64();
    let g = group(
        "Swap File",
        Some(&format!(
            "{file}, made by cloud-init at the first boot. The size can be up to twice the memory and must leave a quarter of the system drive free."
        )),
    );
    g.add(&info(
        "Size",
        &if exists {
            size(&st["file_bytes"])
        } else {
            "No swap file".into()
        },
    ));
    let mem = st["mem_total_bytes"].as_u64().unwrap_or(0);
    let mut choices: Vec<u64> = [256u64, 512, 1024, 2048, 4096, 8192]
        .iter()
        .map(|m| m << 20)
        .filter(|b| mem == 0 || *b <= 2 * mem)
        .collect();
    if let Some(cur) = st["file_bytes"].as_u64() {
        if !choices.contains(&cur) {
            choices.push(cur);
            choices.sort();
        }
    }
    let labels: Vec<String> = choices.iter().map(|b| size(&Value::from(*b))).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let combo = adw::ComboRow::builder()
        .title(if exists { "New size" } else { "Size" })
        .model(&gtk::StringList::new(&refs))
        .selected(
            st["file_bytes"]
                .as_u64()
                .and_then(|c| choices.iter().position(|b| *b == c))
                .or_else(|| choices.iter().position(|b| *b == 1 << 30))
                .unwrap_or(0) as u32,
        )
        .build();
    g.add(&combo);
    let (r, b) = action_row(
        if exists {
            "Resize the swap file"
        } else {
            "Make a swap file"
        },
        "Now and at every boot",
        if exists { "Resize" } else { "Make" },
        "",
    );
    g.add(&r);
    let ctx2 = ctx.clone();
    b.connect_clicked(move |_| {
        if let Some(bytes) = choices.get(combo.selected() as usize) {
            let c = Cmd::new(&["swap", "set", &format!("{}M", bytes >> 20)]);
            propose(&ctx2, "Change the Swap File?", vec![c], false);
        }
    });
    if exists || !st["fstab_line"].is_null() {
        let (r, b) = action_row(
            "Remove the swap file",
            "Stops swapping to it and deletes it",
            "Remove",
            "destructive-action",
        );
        g.add(&r);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            propose(
                &ctx2,
                "Remove the Swap File?",
                vec![Cmd::new(&["swap", "off"])],
                true,
            );
        });
    }
    page.add(&g);

    if let Some(cur) = st["swappiness"].as_u64() {
        let g = group(
            "Swappiness",
            Some("How readily the kernel moves memory to swap, 0 to 200. The default is 60."),
        );
        let spin = adw::SpinRow::with_range(0.0, 200.0, 10.0);
        spin.set_title("vm.swappiness");
        spin.set_value(cur as f64);
        g.add(&spin);
        let (r, b) = action_row("Apply", "Now and at every boot", "Apply", "");
        g.add(&r);
        let ctx2 = ctx.clone();
        b.connect_clicked(move |_| {
            let v = (spin.value() as u32).to_string();
            propose(
                &ctx2,
                "Change the Swappiness?",
                vec![Cmd::new(&["swap", "swappiness", &v])],
                false,
            );
        });
        page.add(&g);
    }
    show(&holder, &page);
}

// ---- devices: LEDs, Wi-Fi, temperatures -----------------------------------------

fn devices_page(ctx: &Shared) {
    let holder = page_holder(ctx, "devices");
    let page = adw::PreferencesPage::new();
    match json(ctx, &["leds", "list"]) {
        Ok(st) => {
            let g = group(
                "LEDs",
                Some(if st["armbian_led_state"].as_bool() == Some(true) {
                    "What makes each LED light up. Kept across reboots by armbian-led-state."
                } else {
                    "What makes each LED light up. Kept across reboots by hwtool."
                }),
            );
            for l in st["leds"].as_array().cloned().unwrap_or_default() {
                let name = s(&l, "name").to_string();
                let triggers = strings(&l["triggers"]);
                let refs: Vec<&str> = triggers.iter().map(String::as_str).collect();
                let cur = triggers
                    .iter()
                    .position(|t| t == s(&l, "trigger"))
                    .unwrap_or(0) as u32;
                let combo = adw::ComboRow::builder()
                    .title(&name)
                    .use_markup(false)
                    .model(&gtk::StringList::new(&refs))
                    .enable_search(triggers.len() > 8)
                    .selected(cur)
                    .build();
                let ctx2 = ctx.clone();
                combo.connect_selected_notify(move |c| {
                    if c.selected() == cur {
                        return;
                    }
                    let Some(t) = triggers.get(c.selected() as usize) else {
                        return;
                    };
                    let mut cmd = Cmd::new(&["leds", "set"]);
                    cmd.names = vec![name.clone(), t.clone()];
                    let c2 = c.clone();
                    propose_or(&ctx2, "Change the LED?", vec![cmd], false, move || {
                        c2.set_selected(cur)
                    });
                });
                g.add(&combo);
            }
            page.add(&g);
        }
        Err(e) => {
            let g = group("LEDs", None);
            g.add(&note_row(&strip_prefix(&e)));
            page.add(&g);
        }
    }

    if let Ok(st) = json(ctx, &["wifi", "country"]) {
        let g = group(
            "Wi-Fi Country",
            Some("The country whose radio rules Wi-Fi follows: a two-letter ISO 3166 code such as DE or US."),
        );
        g.add(&info(
            "In use",
            st["country"]
                .as_str()
                .unwrap_or("Unknown (iw is not installed)"),
        ));
        let entry = adw::EntryRow::builder()
            .title("Country code")
            .text(st["configured"].as_str().unwrap_or(""))
            .show_apply_button(true)
            .build();
        g.add(&entry);
        let aic = st["aic8800"].as_bool() == Some(true);
        let follow = adw::SwitchRow::builder()
            .title("Let the Wi-Fi driver follow it")
            .subtitle("The aic8800 driver otherwise uses its own permissive rules. Not yet tried on a board.")
            .active(st["aic8800_follows_country"].as_bool() == Some(true))
            .build();
        if aic {
            g.add(&follow);
        }
        let ctx2 = ctx.clone();
        entry.connect_apply(move |e| {
            let code = e.text().to_uppercase();
            let mut c = Cmd::new(&["wifi", "country"]);
            if aic && follow.is_active() {
                c.args.push("--aic8800-follow-country".into());
            }
            c.names.push(code);
            propose(&ctx2, "Set the Wi-Fi Country?", vec![c], false);
        });
        for n in strings(&st["notes"]) {
            if !(aic && n.contains("aic8800")) {
                g.add(&note_row(&n));
            }
        }
        page.add(&g);
    }

    if let Ok(st) = json(ctx, &["fan", "show"]) {
        let g = group("Temperatures", None);
        for z in st["zones"].as_array().cloned().unwrap_or_default() {
            let trips: Vec<String> = z["trips"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|t| {
                    format!(
                        "{} at {:.0} °C",
                        s(t, "kind"),
                        t["temp_mc"].as_i64().unwrap_or(0) as f64 / 1000.0
                    )
                })
                .collect();
            let r = row(
                &zone_name(s(&z, "kind")),
                &if trips.is_empty() {
                    String::new()
                } else {
                    format!("Trips: {}", trips.join(", "))
                },
            );
            r.set_subtitle_lines(0);
            r.add_suffix(&gtk::Label::new(Some(
                &z["temp_mc"]
                    .as_i64()
                    .map(|t| format!("{:.1} °C", t as f64 / 1000.0))
                    .unwrap_or_else(|| "unknown".into()),
            )));
            g.add(&r);
        }
        page.add(&g);
        let g = group("Cooling", None);
        g.add(&info(
            "Fan",
            if st["fan"].as_bool() == Some(true) {
                "Yes"
            } else {
                "None"
            },
        ));
        for n in strings(&st["notes"]) {
            g.add(&note_row(&n));
        }
        page.add(&g);
    }
    show(&holder, &page);
}

// ---- boot page: U-Boot versions and boot order ---------------------------------

fn media_name(m: &Value) -> String {
    let dev = s(m, "device");
    match s(m, "kind") {
        "spi" => format!("SPI flash ({dev})"),
        k => format!("{} ({dev})", kind_name(k)),
    }
}

/// Fill `g` with the versions; returns the rows added.
fn version_rows(ctx: &Shared, g: &adw::PreferencesGroup, v: &Value) -> Vec<gtk::Widget> {
    let mut rows: Vec<gtk::Widget> = Vec::new();
    let r = info(
        "In the U-Boot package",
        v["package"].as_str().unwrap_or("no version found"),
    );
    g.add(&r);
    rows.push(r.upcast());
    let starts = v["starts_from"].as_str().unwrap_or("");
    for m in v["media"].as_array().cloned().unwrap_or_default() {
        let state = s(&m, "state");
        let r = row(
            &media_name(&m),
            m["version"]
                .as_str()
                .unwrap_or(if state == "none" { "No U-Boot" } else { "" }),
        );
        r.set_subtitle_lines(0);
        if s(&m, "device") == starts {
            r.add_suffix(&tag("Starts here", ""));
        }
        let (label, kind) = match state {
            "up to date" => ("Up to date", "success"),
            "older" => ("Older", "warning"),
            "newer" => ("Newer", ""),
            "different" => ("Different", "warning"),
            "none" => ("", ""),
            _ => ("Needs administrator", ""),
        };
        if !label.is_empty() {
            r.add_suffix(&tag(label, kind));
        }
        g.add(&r);
        rows.push(r.upcast());
    }
    let start = v["media"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|m| s(m, "device") == starts);
    if let Some(m) = start {
        if matches!(s(&m, "state"), "older" | "newer" | "different") {
            let (r, b) = action_row(
                "Update the bootloader",
                &format!("Writes the package's U-Boot to {}", media_name(&m)),
                "Update",
                "destructive-action",
            );
            g.add(&r);
            rows.push(r.upcast());
            let target = if s(&m, "kind") == "spi" {
                "spi".to_string()
            } else {
                s(&m, "device").to_string()
            };
            let ctx2 = ctx.clone();
            b.connect_clicked(move |_| {
                let c = Cmd::new(&[
                    "boot",
                    "write-bootloader",
                    "--target",
                    &target,
                    "--allow-booted-device",
                ]);
                propose(&ctx2, "Update the Bootloader?", vec![c], true);
            });
        }
    }
    rows
}

fn bootloader_groups(ctx: &Shared, page: &adw::PreferencesPage) {
    if let Ok(v) = json(ctx, &["boot", "version"]) {
        let g = group(
            "Bootloader Version",
            Some("The U-Boot on each boot device, compared with the installed U-Boot package."),
        );
        let rows = Rc::new(RefCell::new(version_rows(ctx, &g, &v)));
        let unreadable = v["media"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| s(m, "state").starts_with("unreadable")));
        if unreadable {
            let (r, b) = action_row(
                "Read the boot devices",
                "Reading them needs the administrator password",
                "Check",
                "",
            );
            g.add(&r);
            let (ctx2, g2) = (ctx.clone(), g.clone());
            b.connect_clicked(move |_| {
                let (ctx3, g3, rows3, r3) = (ctx2.clone(), g2.clone(), rows.clone(), r.clone());
                privileged_json(&ctx2, &["boot", "version"], move |res| match res {
                    Ok(v) => {
                        for w in rows3.borrow_mut().drain(..) {
                            g3.remove(&w);
                        }
                        g3.remove(&r3);
                        *rows3.borrow_mut() = version_rows(&ctx3, &g3, &v);
                    }
                    Err(e) => alert(&ctx3, "Cannot Read the Boot Devices", &e),
                });
            });
        }
        page.add(&g);
    }

    let Ok(o) = json(ctx, &["boot", "order", "show"]) else {
        return;
    };
    let g = group(
        "Boot Order",
        Some("The order in which U-Boot looks for a system to start. In U-Boot's device tree mmc0 is the SD card; mmc1 is the eMMC."),
    );
    let default = strings(&o["default"]);
    g.add(&info("Built in", &default.join(", ")));
    match o["writable"].as_str() {
        None => {
            g.add(&note_row(
                o["reason"]
                    .as_str()
                    .unwrap_or("Not possible on this board."),
            ));
        }
        Some(_) => {
            if let Some(saved) = o["saved"].as_array() {
                let saved: Vec<&str> = saved.iter().filter_map(Value::as_str).collect();
                g.add(&info("Saved", &saved.join(", ")));
            }
            let refs: Vec<&str> = default.iter().map(String::as_str).collect();
            let combo = adw::ComboRow::builder()
                .title("Try first")
                .model(&gtk::StringList::new(&refs))
                .build();
            g.add(&combo);
            let (r, b) = action_row(
                "Save the boot order",
                "Saved in U-Boot's environment in SPI flash",
                "Save",
                "",
            );
            g.add(&r);
            let ctx2 = ctx.clone();
            let d2 = default.clone();
            b.connect_clicked(move |_| {
                let Some(first) = d2.get(combo.selected() as usize) else {
                    return;
                };
                let mut c = Cmd::new(&["boot", "order", "set"]);
                c.names.push(first.clone());
                c.names.extend(d2.iter().filter(|t| *t != first).cloned());
                propose(&ctx2, "Change the Boot Order?", vec![c], false);
            });
            let (r, b) = action_row(
                "Use the built-in order",
                "Erases U-Boot's saved environment",
                "Reset",
                "destructive-action",
            );
            g.add(&r);
            let ctx2 = ctx.clone();
            b.connect_clicked(move |_| {
                propose(
                    &ctx2,
                    "Use the Built-In Boot Order?",
                    vec![Cmd::new(&["boot", "order", "reset"])],
                    true,
                );
            });
        }
    }
    page.add(&g);
}

/// "bigcore-thermal" -> "Big CPU cores" (zone names of rk3576.dtsi).
fn zone_name(kind: &str) -> String {
    match kind.trim_end_matches("-thermal") {
        "package" => "SoC".into(),
        "bigcore" => "Big CPU cores".into(),
        "littlecore" => "Little CPU cores".into(),
        "gpu" => "GPU".into(),
        "npu" => "NPU".into(),
        "ddr" => "Memory".into(),
        other => other.to_string(),
    }
}
