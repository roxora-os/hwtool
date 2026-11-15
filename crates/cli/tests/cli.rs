//! Runs the hwtool binary against a small fake root.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output};

use hwtool_core::progress::{Event, State};
use tempfile::TempDir;

const ENV: &str = "verbosity=1\noverlay_prefix=rockchip-rk3576\nrootdev=UUID=x\nrootfstype=ext4\n";

fn write(base: &Path, abs: &str, content: &str) {
    let p = base.join(abs.trim_start_matches('/'));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn fake() -> TempDir {
    let d = TempDir::new().unwrap();
    let b = d.path();
    write(
        b,
        "/etc/armbian-release",
        "BOARD=radxa-rock-4d\nBOARDFAMILY=rk35xx\nBOOT_SOC=rk3576\nKERNEL_TARGET=vendor,edge\n",
    );
    write(b, "/boot/armbianEnv.txt", ENV);
    write(
        b,
        "/boot/dtb-7.3.4-edge-rockchip64/rockchip/overlay/rockchip-rk3576-uart1.dtbo",
        "",
    );
    symlink("dtb-7.3.4-edge-rockchip64", b.join("boot/dtb")).unwrap();
    write(b, "/boot/vmlinuz-7.3.4-edge-rockchip64", "");
    symlink("vmlinuz-7.3.4-edge-rockchip64", b.join("boot/Image")).unwrap();
    write(
        b,
        "/var/lib/dpkg/status",
        "Package: linux-image-edge-rockchip64\nStatus: install ok installed\nVersion: 26.11.0-trunk\n\n",
    );
    d
}

fn hwtool(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hwtool"))
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn overlays_list_and_json() {
    let d = fake();
    let o = hwtool(d.path(), &["overlays", "list"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("[ ] uart1"), "{}", text(&o));
    let o = hwtool(d.path(), &["--json", "overlays", "list"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["overlays"][0]["name"], "uart1");
}

#[test]
fn dry_run_changes_nothing() {
    let d = fake();
    let o = hwtool(d.path(), &["overlays", "enable", "uart1", "--dry-run"]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(
        t.contains("| overlays=uart1") && t.contains("Dry run: nothing was changed."),
        "{t}"
    );
    assert_eq!(
        fs::read_to_string(d.path().join("boot/armbianEnv.txt")).unwrap(),
        ENV
    );
    let o = hwtool(
        d.path(),
        &["--json", "overlays", "enable", "uart1", "--dry-run"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["plan"]["steps"][1]["kind"], "write_file");
}

#[test]
fn no_terminal_needs_yes() {
    let d = fake();
    let o = hwtool(d.path(), &["overlays", "enable", "uart1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("add --yes"), "{}", text(&o));
    let o = hwtool(d.path(), &["overlays", "enable", "uart1", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(fs::read_to_string(d.path().join("boot/armbianEnv.txt"))
        .unwrap()
        .ends_with("overlays=uart1\n"));
    let backups = fs::read_dir(d.path().join("var/backups/hwtool"))
        .unwrap()
        .count();
    assert_eq!(backups, 1);
}

#[test]
fn commands_are_not_run_on_a_fake_root() {
    let d = fake();
    write(d.path(), "/usr/lib/u-boot/platform_install.sh", "DIR=/usr/lib/x\nwrite_uboot_platform ()\n{\n dd if=$1/u-boot-rockchip.bin of=$2 bs=32k seek=1 conv=notrunc\n}\n");
    write(d.path(), "/usr/lib/x/u-boot-rockchip.bin", "x");
    write(d.path(), "/sys/block/mmcblk0/size", "1000000\n");
    write(d.path(), "/sys/block/mmcblk0/device/type", "MMC\n");
    // The running system is on the SD card (mmcblk1p1, 179:97).
    write(d.path(), "/sys/block/mmcblk1/size", "1000000\n");
    write(d.path(), "/sys/block/mmcblk1/mmcblk1p1/partition", "1\n");
    std::fs::create_dir_all(d.path().join("sys/dev/block")).unwrap();
    symlink(
        "../../block/mmcblk1/mmcblk1p1",
        d.path().join("sys/dev/block/179:97"),
    )
    .unwrap();
    write(
        d.path(),
        "/proc/self/mountinfo",
        "22 1 179:97 / / rw,relatime - ext4 /dev/mmcblk1p1 rw\n",
    );
    let o = hwtool(
        d.path(),
        &[
            "boot",
            "write-bootloader",
            "--target",
            "/dev/mmcblk0",
            "--dry-run",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains(
            "$ dd if=/usr/lib/x/u-boot-rockchip.bin of=/dev/mmcblk0 bs=32k seek=1 conv=notrunc"
        ),
        "{}",
        text(&o)
    );
    let o = hwtool(
        d.path(),
        &[
            "boot",
            "write-bootloader",
            "--target",
            "/dev/mmcblk0",
            "--yes",
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("does not do with --root"), "{}", text(&o));
}

#[test]
fn kernel_status() {
    let d = fake();
    let o = hwtool(d.path(), &["kernel", "status"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("Kernel for the next boot: 7.3.4-edge-rockchip64 (branch edge)"),
        "{t}"
    );
}

fn hwtool_env(root: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hwtool"))
        .arg("--root")
        .arg(root)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

#[test]
fn root_option_is_refused_with_root_rights() {
    let d = fake();
    for var in ["SUDO_UID", "PKEXEC_UID"] {
        let o = hwtool_env(d.path(), &[(var, "1000")], &["overlays", "list"]);
        assert!(!o.status.success());
        assert!(text(&o).contains("--root is for tests"), "{}", text(&o));
    }
}

#[test]
fn the_confirmed_plan_is_the_one_that_runs() {
    let d = fake();
    let o = hwtool(
        d.path(),
        &["--json", "overlays", "enable", "--dry-run", "--", "uart1"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let fp = v["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(fp.len(), 16);
    // The configuration changes after the dry run: refused.
    write(
        d.path(),
        "/boot/armbianEnv.txt",
        &format!("{ENV}verbosity=7\n"),
    );
    let o = hwtool(
        d.path(),
        &[
            "overlays",
            "enable",
            "--yes",
            "--expect-plan",
            &fp,
            "--",
            "uart1",
        ],
    );
    assert!(!o.status.success());
    assert!(
        text(&o).contains("the plan changed since it was confirmed"),
        "{}",
        text(&o)
    );
    assert!(!fs::read_to_string(d.path().join("boot/armbianEnv.txt"))
        .unwrap()
        .contains("overlays="));
    // A fresh dry run and its fingerprint: it runs.
    let o = hwtool(
        d.path(),
        &["--json", "overlays", "enable", "--dry-run", "--", "uart1"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let fp = v["fingerprint"].as_str().unwrap().to_string();
    let o = hwtool(
        d.path(),
        &[
            "overlays",
            "enable",
            "--yes",
            "--expect-plan",
            &fp,
            "--",
            "uart1",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn option_like_overlay_names_are_refused() {
    let d = fake();
    let o = hwtool(d.path(), &["overlays", "enable", "--dry-run", "--", "-x"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("invalid overlay name"), "{}", text(&o));
}

/// The events of a `--progress-json` run, one per line of standard output.
fn events(o: &Output) -> Vec<Event> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| Event::decode(l).unwrap_or_else(|| panic!("not a progress event: {l}")))
        .collect()
}

fn not_log(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .filter(|e| !matches!(e, Event::Log { .. }))
        .cloned()
        .collect()
}

fn step(step: usize, title: &str, state: State) -> Event {
    Event::Step {
        step,
        of: 2,
        title: title.into(),
        state,
        error: None,
    }
}

#[test]
fn progress_json_reports_a_run_in_order() {
    let d = fake();
    let o = hwtool(
        d.path(),
        &["overlays", "enable", "uart1", "--yes", "--progress-json"],
    );
    assert!(o.status.success(), "{}", text(&o));
    // Nothing but events on standard output, and no text on standard error.
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let all = events(&o);
    let Some(Event::Done { log: Some(log), .. }) = all.last().cloned() else {
        panic!("the last event is not done with a log: {all:?}");
    };
    assert!(
        log.starts_with("/var/log/hwtool/") && log.ends_with(".log"),
        "{log}"
    );
    let (backup, write) = (
        "Back up /boot/armbianEnv.txt",
        "Write the new /boot/armbianEnv.txt",
    );
    let Event::Plan {
        title,
        steps,
        reboot,
    } = &all[0]
    else {
        panic!("the first event is not the plan: {all:?}");
    };
    assert!(title.contains("uart1"), "{title}");
    assert_eq!(steps, &[backup.to_string(), write.to_string()]);
    assert!(*reboot);
    assert_eq!(
        not_log(&all)[1..],
        [
            step(1, backup, State::Running),
            step(1, backup, State::Done),
            step(2, write, State::Running),
            step(2, write, State::Done),
            Event::Done {
                ok: true,
                log: Some(log.clone()),
                reboot: true,
                failed_step: None,
                error: None,
            },
        ]
    );
    // The text of a run arrives as log events, each between the events of
    // its step, and is the content of the log file.
    let lines: Vec<String> = all
        .iter()
        .filter_map(|e| match e {
            Event::Log { line } => Some(line.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(lines[0], format!("Log: {log}"));
    assert_eq!(lines[1], format!("[1/2] {backup}"));
    assert!(
        lines[2]
            .trim_start()
            .starts_with("backup: /var/backups/hwtool/"),
        "{lines:?}"
    );
    assert_eq!(lines[3], format!("[2/2] {write}"));
    let at = |e: &Event| all.iter().position(|x| x == e).unwrap();
    let first_line = at(&Event::Log {
        line: lines[1].clone(),
    });
    assert!(at(&step(1, backup, State::Running)) < first_line);
    assert!(first_line < at(&step(1, backup, State::Done)));
    let file = fs::read_to_string(d.path().join(log.trim_start_matches('/'))).unwrap();
    assert_eq!(file, lines.join("\n") + "\n");
    assert!(fs::read_to_string(d.path().join("boot/armbianEnv.txt"))
        .unwrap()
        .ends_with("overlays=uart1\n"));
}

#[test]
fn progress_json_reports_the_step_that_failed() {
    use std::os::unix::fs::PermissionsExt;
    let d = fake();
    // The backup goes to /var; /boot cannot be written, so step 2 fails.
    let boot = d.path().join("boot");
    fs::set_permissions(&boot, fs::Permissions::from_mode(0o555)).unwrap();
    let o = hwtool(
        d.path(),
        &["overlays", "enable", "uart1", "--yes", "--progress-json"],
    );
    fs::set_permissions(&boot, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let all = not_log(&events(&o));
    let n = all.len();
    let Event::Step {
        step: 2,
        state: State::Failed,
        error: Some(step_error),
        ..
    } = &all[n - 2]
    else {
        panic!("no failed step before the end: {all:?}");
    };
    assert!(step_error.contains("cannot create"), "{step_error}");
    let Event::Done {
        ok: false,
        log: Some(_),
        reboot: false,
        failed_step: Some(2),
        error: Some(error),
    } = &all[n - 1]
    else {
        panic!("the last event is not the failure: {all:?}");
    };
    assert!(
        error.contains("step 2 failed: Write the new /boot/armbianEnv.txt"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(boot.join("armbianEnv.txt")).unwrap(),
        ENV
    );
}

#[test]
fn progress_json_ends_a_refused_run_with_done() {
    let d = fake();
    let o = hwtool(
        d.path(),
        &[
            "overlays",
            "enable",
            "--yes",
            "--progress-json",
            "--expect-plan",
            "0000000000000000",
            "--",
            "uart1",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    let all = events(&o);
    let [Event::Done {
        ok: false,
        log: None,
        error: Some(error),
        ..
    }] = all.as_slice()
    else {
        panic!("expected one done event: {all:?}");
    };
    assert!(error.contains("the plan changed"), "{error}");
    assert!(
        text(&o).contains("hwtool: the plan changed"),
        "{}",
        text(&o)
    );
}

#[test]
fn without_progress_json_the_output_is_text() {
    let d = fake();
    let o = hwtool(d.path(), &["overlays", "enable", "uart1", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    let err = String::from_utf8_lossy(&o.stderr);
    for want in [
        "[1/2] Back up /boot/armbianEnv.txt\n",
        "[2/2] Write the new /boot/armbianEnv.txt\n",
        "\nDone.\nReboot for the change to take effect.\n",
    ] {
        assert!(err.contains(want), "{err}");
    }
    assert!(!err.contains("{\"event\""), "{err}");
}
