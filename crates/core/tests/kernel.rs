mod common;

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use common::*;
use hwtool_core::dpkg;
use hwtool_core::exec::Executor;
use hwtool_core::kernel;

/// Simulates apt on the fake root the way the Armbian packages behave:
/// installing linux-image-* adds /boot/vmlinuz-<ver> and points /boot/Image
/// at it, linux-dtb-* adds /boot/dtb-<ver> and points /boot/dtb at it,
/// linux-u-boot-* replaces any other linux-u-boot-* (Conflicts: armbian-u-boot).
struct FakeApt<'a> {
    f: &'a Fake,
    available: Vec<(&'static str, &'static str)>,
    ran: Vec<String>,
}

fn kver(pkg: &str) -> &'static str {
    if pkg.contains("vendor") {
        VENDOR_KERNEL
    } else {
        EDGE_KERNEL
    }
}

impl FakeApt<'_> {
    fn set_status(&self, f: impl Fn(&mut Vec<(String, String)>)) {
        let mut v: Vec<(String, String)> = dpkg::installed(&self.f.root())
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect();
        f(&mut v);
        let refs: Vec<(&str, &str)> = v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        self.f.status(&refs);
    }
}

impl Executor for FakeApt<'_> {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        self.ran.push(argv.join(" "));
        match argv
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["apt-get", "update"] => Ok(()),
            ["apt-get", "install", "-y", pkgs @ ..] => {
                for spec in pkgs {
                    if spec.starts_with('-') {
                        continue;
                    }
                    let (name, ver) = match spec.split_once('=') {
                        Some((n, v)) => (n.to_string(), v.to_string()),
                        None => {
                            let v = self
                                .available
                                .iter()
                                .find(|(n, _)| n == spec)
                                .map(|(_, v)| v.to_string());
                            match v {
                                Some(v) => (spec.to_string(), v),
                                None => bail!("E: Unable to locate package {spec}"),
                            }
                        }
                    };
                    let k = kver(&name);
                    if name.starts_with("linux-image-") {
                        self.f.write(&format!("/boot/vmlinuz-{k}"), "kernel");
                        self.f.link("/boot/Image", &format!("vmlinuz-{k}"));
                    } else if name.starts_with("linux-dtb-") {
                        self.f
                            .write(&format!("/boot/dtb-{k}/rockchip/rk3576-rock-4d.dtb"), "dtb");
                        self.f.write(
                            &format!("/boot/dtb-{k}/rockchip/rk3576-armsom-cm5-io.dtb"),
                            "dtb",
                        );
                        self.f.write(
                            &format!("/boot/dtb-{k}/rockchip/overlay/rk3576-uart1.dtbo"),
                            "",
                        );
                        self.f.link("/boot/dtb", &format!("dtb-{k}"));
                    }
                    let uboot = name.starts_with("linux-u-boot-");
                    self.set_status(|v| {
                        if uboot {
                            v.retain(|(n, _)| !n.starts_with("linux-u-boot-"));
                        }
                        v.retain(|(n, _)| *n != name);
                        v.push((name.clone(), ver.clone()));
                    });
                }
                Ok(())
            }
            ["apt-get", "remove", "-y", pkgs @ ..] => {
                for p in pkgs {
                    let k = kver(p);
                    if p.starts_with("linux-image-") {
                        let _ = std::fs::remove_file(self.f.p(&format!("/boot/vmlinuz-{k}")));
                    }
                    self.set_status(|v| v.retain(|(n, _)| n != p));
                }
                Ok(())
            }
            other => bail!("unexpected command {other:?}"),
        }
    }

    fn capture(&mut self, argv: &[String]) -> Result<String> {
        self.ran.push(argv.join(" "));
        if argv.len() == 3 && argv[0] == "apt-cache" && argv[1] == "policy" {
            let mut versions: Vec<&str> = self
                .available
                .iter()
                .filter(|(n, _)| *n == argv[2])
                .map(|(_, v)| *v)
                .collect();
            if argv[2].contains("edge") {
                versions.push("26.11.0-trunk");
            }
            return Ok(policy(versions.first().copied(), &versions));
        }
        bail!("unexpected capture {argv:?}")
    }
}

const VENDOR_PKGS: &[(&str, &str)] = &[
    ("linux-image-vendor-rk35xx", "26.5.1"),
    ("linux-dtb-vendor-rk35xx", "26.5.1"),
    ("linux-u-boot-radxa-rock-4d-vendor", "26.5.1"),
    ("linux-image-edge-rockchip64", "26.11.0-trunk"),
    ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
    ("linux-u-boot-radxa-rock-4d-edge", "26.11.0-trunk"),
];

#[test]
fn status_reads_branch_from_boot_image() {
    let f = rock4d();
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.board.board, "radxa-rock-4d");
    assert_eq!(st.board.branches, vec!["vendor", "edge"]);
    assert_eq!(st.current.as_deref(), Some("edge"));
    assert_eq!(st.kernel.as_deref(), Some(EDGE_KERNEL));
    let edge = st.branches.iter().find(|b| b.branch == "edge").unwrap();
    assert_eq!(edge.installed.len(), 3);
}

#[test]
fn switch_to_vendor_and_back() {
    let f = rock4d();
    let mut apt = FakeApt {
        f: &f,
        available: VENDOR_PKGS.to_vec(),
        ran: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, &BTreeMap::new()).unwrap();
    let text = plan.render();
    assert!(text.contains("$ apt-get install -y linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor"), "{text}");
    assert!(
        text.contains("$ apt-get remove -y linux-image-edge-rockchip64 linux-dtb-edge-rockchip64"),
        "{text}"
    );
    assert!(text.contains("| overlay_prefix=rk3576"), "{text}");
    assert!(plan.reboot_required);

    apt.ran.clear();
    plan.execute(&f.root(), &mut apt, "T1").unwrap();
    assert_eq!(
        apt.ran,
        vec![
            "apt-get update",
            "apt-get install -y linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor",
            "apt-get remove -y linux-image-edge-rockchip64 linux-dtb-edge-rockchip64",
        ]
    );
    assert_eq!(
        f.read("/var/backups/roxora-hwtool/T1/boot/armbianEnv.txt"),
        ROCK4D_ENV
    );
    assert!(f
        .read("/boot/armbianEnv.txt")
        .contains("overlay_prefix=rk3576\n"));
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.current.as_deref(), Some("vendor"));
    assert_eq!(st.history.len(), 1);
    assert_eq!(st.history[0].from_branch.as_deref(), Some("edge"));
    let names: Vec<&str> = st.history[0]
        .from_packages
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "linux-image-edge-rockchip64",
            "linux-dtb-edge-rockchip64",
            "linux-u-boot-radxa-rock-4d-edge"
        ]
    );
    let installed: Vec<String> = dpkg::installed(&f.root())
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(
        !installed.iter().any(|n| n.contains("edge")),
        "{installed:?}"
    );

    // Way back: pins the recorded versions.
    let plan = kernel::revert_plan(&f.root(), &mut apt, false).unwrap();
    let text = plan.render();
    assert!(
        text.contains("linux-image-edge-rockchip64=26.11.0-trunk"),
        "{text}"
    );
    plan.execute(&f.root(), &mut apt, "T2").unwrap();
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.current.as_deref(), Some("edge"));
    assert_eq!(f.read("/boot/armbianEnv.txt"), ROCK4D_ENV);
}

#[test]
fn keep_old_keeps_kernel_packages() {
    let f = rock4d();
    let mut apt = FakeApt {
        f: &f,
        available: VENDOR_PKGS.to_vec(),
        ran: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", true, &BTreeMap::new()).unwrap();
    assert!(!plan.render().contains("apt-get remove"));
}

#[test]
fn missing_packages_are_reported_before_anything_changes() {
    // CM5-IO edge U-Boot is not published in the Armbian repository.
    let f = cm5();
    let mut apt = FakeApt {
        f: &f,
        available: vec![
            ("linux-image-vendor-rk35xx", "26.5.1"),
            ("linux-dtb-vendor-rk35xx", "26.5.1"),
        ],
        ran: vec![],
    };
    let err = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, &BTreeMap::new())
        .unwrap_err()
        .to_string();
    assert!(err.contains("linux-u-boot-armsom-cm5-io-vendor"), "{err}");
    assert!(apt.ran.iter().all(|c| c.starts_with("apt-cache policy")));
}

#[test]
fn board_prefix_is_kept_for_cm5() {
    let f = cm5();
    let mut apt = FakeApt {
        f: &f,
        available: vec![
            ("linux-image-vendor-rk35xx", "26.5.1"),
            ("linux-dtb-vendor-rk35xx", "26.5.1"),
            ("linux-u-boot-armsom-cm5-io-vendor", "26.5.1"),
        ],
        ran: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, &BTreeMap::new()).unwrap();
    plan.execute(&f.root(), &mut apt, "T").unwrap();
    assert!(f
        .read("/boot/armbianEnv.txt")
        .contains("overlay_prefix=armsom-cm5-io rk3576\n"));
    assert!(f
        .read("/boot/armbianEnv.txt")
        .contains("fdtfile=rockchip/rk3576-armsom-cm5-io.dtb\n"));
}

#[test]
fn refuses_unsupported_and_same_branch() {
    let f = rock4d();
    let mut apt = FakeApt {
        f: &f,
        available: VENDOR_PKGS.to_vec(),
        ran: vec![],
    };
    assert!(
        kernel::switch_plan(&f.root(), &mut apt, "current", false, &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("supports the kernel branches vendor, edge")
    );
    assert!(
        kernel::switch_plan(&f.root(), &mut apt, "edge", false, &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("already runs")
    );
    assert!(kernel::revert_plan(&f.root(), &mut apt, false).is_err());
}

#[test]
fn missing_device_tree_stops_before_old_kernel_is_removed() {
    let f = rock4d();
    // fdtfile names a device tree the vendor package does not ship.
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}fdtfile=rockchip/rk3576-rock-4d-spi.dtb\n"),
    );
    let mut apt = FakeApt {
        f: &f,
        available: VENDOR_PKGS.to_vec(),
        ran: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, &BTreeMap::new()).unwrap();
    apt.ran.clear();
    let err = format!("{:#}", plan.execute(&f.root(), &mut apt, "T").unwrap_err());
    assert!(err.contains("rk3576-rock-4d-spi.dtb"), "{err}");
    assert!(
        !apt.ran.iter().any(|c| c.starts_with("apt-get remove")),
        "{:?}",
        apt.ran
    );
    // Cleanup reinstalled the old packages, so /boot points at edge again.
    assert!(
        apt.ran
            .last()
            .unwrap()
            .starts_with("apt-get install -y --reinstall"),
        "{:?}",
        apt.ran
    );
    assert_eq!(
        kernel::status(&f.root()).unwrap().current.as_deref(),
        Some("edge")
    );
}

#[test]
fn fdtfile_falls_back_to_uboot_default() {
    let f = rock4d();
    assert_eq!(
        kernel::fdtfile(&f.root()).as_deref(),
        Some("rockchip/rk3576-rock-4d.dtb")
    );
    let f = cm5();
    assert_eq!(
        kernel::fdtfile(&f.root()).as_deref(),
        Some("rockchip/rk3576-armsom-cm5-io.dtb")
    );
}

#[test]
fn headers_follow_the_branch_when_installed() {
    let f = rock4d();
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("linux-headers-edge-rockchip64", "26.11.0-trunk"),
        ("linux-u-boot-radxa-rock-4d-edge", "26.11.0-trunk"),
    ]);
    let mut avail = VENDOR_PKGS.to_vec();
    avail.push(("linux-headers-vendor-rk35xx", "26.5.1"));
    let mut apt = FakeApt {
        f: &f,
        available: avail,
        ran: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, &BTreeMap::new()).unwrap();
    let text = plan.render();
    assert!(
        text.contains("linux-u-boot-radxa-rock-4d-vendor linux-headers-vendor-rk35xx\n"),
        "{text}"
    );
    assert!(text.contains("$ apt-get remove -y linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-headers-edge-rockchip64\n"), "{text}");
    plan.execute(&f.root(), &mut apt, "T").unwrap();
    let names: Vec<String> = dpkg::installed(&f.root())
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(
        names.contains(&"linux-headers-vendor-rk35xx".to_string()),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.contains("edge")), "{names:?}");
}
