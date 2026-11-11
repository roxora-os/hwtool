//! Runs the hwtool binary against a small fake root.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output};

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
