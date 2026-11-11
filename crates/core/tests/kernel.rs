mod common;

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
    /// Packages apt's simulation says it would also remove.
    extra_removals: Vec<String>,
    /// Commands (prefix) whose run fails.
    failing: Vec<String>,
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
        let joined = argv.join(" ");
        if self.failing.iter().any(|f| joined.starts_with(f.as_str())) {
            bail!("{joined}: failed (test)");
        }
        match argv
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["apt-get", "update"] => Ok(()),
            ["bash", "-c", script] => {
                // The cleanup script: pick saved files that exist, else name=version.
                let mut args: Vec<String> = ["apt-get", "install", "-y", "--reinstall"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                for line in script.lines() {
                    let q: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
                    if line.starts_with("if [ -f") {
                        args.push(if self.f.p(q[0]).exists() { q[1] } else { q[2] }.to_string());
                    } else if line.starts_with("pkgs+=") {
                        args.push(q[0].to_string());
                    }
                }
                self.ran.pop();
                self.run(&args)
            }
            ["dpkg-deb", "--root-owner-group", "--build", staging, out] => {
                assert!(std::path::Path::new(staging)
                    .join("DEBIAN/control")
                    .exists());
                std::fs::write(out, "deb").unwrap();
                Ok(())
            }
            ["apt-get", "install", "-y", pkgs @ ..] => {
                for spec in pkgs {
                    if spec.starts_with('-') {
                        continue;
                    }
                    let (name, ver) = match spec.split_once('=') {
                        _ if spec.ends_with(".deb") => {
                            assert!(self.f.p(spec).exists(), "{spec} does not exist");
                            let base = spec.rsplit('/').next().unwrap();
                            let mut it = base.split('_');
                            (
                                it.next().unwrap().to_string(),
                                it.next().unwrap().to_string(),
                            )
                        }
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
            ["apt-get", "remove" | "purge", "-y", pkgs @ ..] => {
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
        let a: Vec<&str> = argv.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["apt-get", "-s", "-y", "install", specs @ ..] => {
                let mut out = String::from("NOTE: This is only a simulation!\n");
                let installed = dpkg::installed(&self.f.root());
                for spec in specs {
                    let name = spec
                        .rsplit('/')
                        .next()
                        .unwrap()
                        .split(['=', '_'])
                        .next()
                        .unwrap();
                    if name.starts_with("linux-u-boot-") {
                        for p in installed
                            .iter()
                            .filter(|p| p.name.starts_with("linux-u-boot-") && p.name != name)
                        {
                            out.push_str(&format!("Remv {} [{}]\n", p.name, p.version));
                        }
                    }
                    out.push_str(&format!("Inst {name} (1 test [arm64])\n"));
                }
                out.push_str(
                    &self
                        .extra_removals
                        .iter()
                        .map(|r| format!("Remv {r} [1]\n"))
                        .collect::<String>(),
                );
                Ok(out)
            }
            ["apt-get", "-s", "-y", "remove", pkgs @ ..] => {
                Ok(pkgs.iter().map(|p| format!("Remv {p} [1]\n")).collect())
            }
            ["dpkg-deb", "--field", file, "Package"] => {
                let base = file.rsplit('/').next().unwrap();
                if std::path::Path::new(file).exists()
                    && std::fs::read_to_string(file).unwrap_or_default() != "broken"
                {
                    Ok(format!("{}\n", base.split('_').next().unwrap()))
                } else {
                    bail!("dpkg-deb: not a debian format archive")
                }
            }
            _ => bail!("unexpected capture {argv:?}"),
        }
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
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
    let text = plan.render();
    assert!(text.contains("$ apt-get install -y linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor"), "{text}");
    assert!(
        text.contains("$ apt-get remove -y linux-image-edge-rockchip64 linux-dtb-edge-rockchip64"),
        "{text}"
    );
    assert!(text.contains("| overlay_prefix=rk3576"), "{text}");
    assert!(plan.reboot_required);

    for pkg in [
        "linux-image-edge-rockchip64",
        "linux-dtb-edge-rockchip64",
        "linux-u-boot-radxa-rock-4d-edge",
    ] {
        assert!(
            text.contains(&format!(
                "Save {pkg} 26.11.0-trunk for 'hwtool kernel revert'"
            )),
            "{text}"
        );
    }
    assert!(
        text.contains("rebuild the package from the dpkg database"),
        "{text}"
    );

    apt.ran.clear();
    plan.execute(&f.root(), &mut apt, "T1").unwrap();
    let repacked = apt
        .ran
        .iter()
        .filter(|c| c.starts_with("dpkg-deb --root-owner-group"))
        .count();
    assert_eq!(repacked, 3);
    apt.ran.retain(|c| !c.starts_with("dpkg-deb"));
    assert_eq!(
        apt.ran,
        vec![
            "apt-get update",
            "apt-get install -y linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor",
            "apt-get remove -y linux-image-edge-rockchip64 linux-dtb-edge-rockchip64",
        ]
    );
    assert_eq!(
        f.read("/var/backups/hwtool/T1/boot/armbianEnv.txt"),
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
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", true, None).unwrap();
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
        extra_removals: vec![],
        failing: vec![],
    };
    let err = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None)
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
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
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
        extra_removals: vec![],
        failing: vec![],
    };
    assert!(
        kernel::switch_plan(&f.root(), &mut apt, "current", false, None)
            .unwrap_err()
            .to_string()
            .contains("supports the kernel branches vendor, edge")
    );
    assert!(
        kernel::switch_plan(&f.root(), &mut apt, "edge", false, None)
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
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
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
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
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

#[test]
fn revert_uses_saved_packages_when_apt_no_longer_has_them() {
    // Like the ArmSoM CM5-IO: the edge U-Boot package is not in the repository.
    let f = cm5();
    // apt still has the original U-Boot .deb in its cache: it is copied as is.
    f.write(
        "/var/cache/apt/archives/linux-u-boot-armsom-cm5-io-edge_26.11.0-trunk_arm64.deb",
        "original",
    );
    let mut apt = FakeApt {
        f: &f,
        available: vec![
            ("linux-image-vendor-rk35xx", "26.5.1"),
            ("linux-dtb-vendor-rk35xx", "26.5.1"),
            ("linux-u-boot-armsom-cm5-io-vendor", "26.5.1"),
        ],
        ran: vec![],
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
    let text = plan.render();
    assert!(text.contains("copy /var/cache/apt/archives/linux-u-boot-armsom-cm5-io-edge_26.11.0-trunk_arm64.deb to /var/lib/hwtool/packages/"), "{text}");
    plan.execute(&f.root(), &mut apt, "T1").unwrap();
    let st = kernel::status(&f.root()).unwrap();
    let saved = &st.history[0].saved;
    assert_eq!(saved.len(), 3);
    let uboot = saved
        .iter()
        .find(|s| s.name == "linux-u-boot-armsom-cm5-io-edge")
        .unwrap();
    assert_eq!(f.read(&uboot.file), "original");

    // Nothing for edge is offered by apt any more (the FakeApt still answers
    // policy for edge packages with the trunk version, so drop that).
    struct NoEdge<'a>(FakeApt<'a>);
    impl Executor for NoEdge<'_> {
        fn run(&mut self, a: &[String]) -> Result<()> {
            self.0.run(a)
        }
        fn capture(&mut self, a: &[String]) -> Result<String> {
            if a.len() == 3 && a[1] == "policy" && a[2].contains("edge") {
                return Ok(policy(None, &[]));
            }
            self.0.capture(a)
        }
    }
    let mut apt = NoEdge(apt);
    let plan = kernel::revert_plan(&f.root(), &mut apt, false).unwrap();
    let text = plan.render();
    for s in saved {
        assert!(text.contains(&s.file), "{text}");
    }
    assert!(text.contains("apt no longer offers this version"), "{text}");
    apt.0.ran.clear();
    plan.execute(&f.root(), &mut apt, "T2").unwrap();
    assert_eq!(
        kernel::status(&f.root()).unwrap().current.as_deref(),
        Some("edge")
    );
    let names: Vec<String> = dpkg::installed(&f.root())
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(
        names.contains(&"linux-u-boot-armsom-cm5-io-edge".to_string()),
        "{names:?}"
    );
    // The revert saved the vendor packages and dropped the older set.
    let sets = f.root().list_dir("/var/lib/hwtool/packages");
    assert_eq!(sets.len(), 1, "{sets:?}");
    let hist = kernel::status(&f.root()).unwrap().history;
    assert!(hist[1].saved.iter().all(|s| s.file.contains(&sets[0])));
    assert!(hist[0].saved.iter().all(|s| !f.p(&s.file).exists()));
}

fn rock4d_apt(f: &Fake) -> FakeApt<'_> {
    FakeApt {
        f,
        available: VENDOR_PKGS.to_vec(),
        ran: vec![],
        extra_removals: vec![],
        failing: vec![],
    }
}

#[test]
fn failed_removal_rolls_everything_back() {
    let f = rock4d();
    let mut apt = rock4d_apt(&f);
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
    apt.failing.push("apt-get remove".into());
    apt.ran.clear();
    let err = format!("{:#}", plan.execute(&f.root(), &mut apt, "R").unwrap_err());
    // armbianEnv.txt is back, the vendor packages are purged, edge is reinstalled.
    assert_eq!(f.read("/boot/armbianEnv.txt"), ROCK4D_ENV);
    assert!(err.contains("done: Put back /boot/armbianEnv.txt"), "{err}");
    assert!(err.contains("done: Purge the vendor packages"), "{err}");
    assert!(err.contains("done: Reinstall the old packages"), "{err}");
    assert!(apt.ran.iter().any(|c| c == "apt-get purge -y linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor"), "{:?}", apt.ran);
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.current.as_deref(), Some("edge"));
    let names: Vec<String> = dpkg::installed(&f.root())
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(!names.iter().any(|n| n.contains("vendor")), "{names:?}");
    assert!(
        names.contains(&"linux-u-boot-radxa-rock-4d-edge".to_string()),
        "{names:?}"
    );
    // The history says the switch did not finish, and revert knows.
    assert_eq!(st.history.last().unwrap().state, "pending");
    let err = kernel::revert_plan(&f.root(), &mut apt, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("did not finish"), "{err}");
}

#[test]
fn nothing_is_rolled_back_before_the_install_starts() {
    let f = rock4d();
    let mut apt = rock4d_apt(&f);
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
    apt.failing.push("apt-get update".into());
    apt.ran.clear();
    let err = format!("{:#}", plan.execute(&f.root(), &mut apt, "R").unwrap_err());
    assert!(!err.contains("cleanup"), "{err}");
    assert!(
        !apt.ran
            .iter()
            .any(|c| c.contains("purge") || c.contains("--reinstall")),
        "{:?}",
        apt.ran
    );
}

#[test]
fn unexpected_removals_stop_the_plan() {
    let f = rock4d();
    let mut apt = rock4d_apt(&f);
    apt.extra_removals.push("some-dkms-module".into());
    let err = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("would also remove some-dkms-module"), "{err}");
    // The dry run shows what apt resolved.
    apt.extra_removals.clear();
    let text = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None)
        .unwrap()
        .render();
    assert!(
        text.contains("apt resolves (apt-get -s -y install"),
        "{text}"
    );
    assert!(
        text.contains("Remv linux-u-boot-radxa-rock-4d-edge"),
        "{text}"
    );
}

#[test]
fn a_damaged_saved_package_is_not_used() {
    let f = cm5();
    let mut apt = FakeApt {
        f: &f,
        available: vec![
            ("linux-image-vendor-rk35xx", "26.5.1"),
            ("linux-dtb-vendor-rk35xx", "26.5.1"),
            ("linux-u-boot-armsom-cm5-io-vendor", "26.5.1"),
        ],
        ran: vec![],
        extra_removals: vec![],
        failing: vec![],
    };
    let plan = kernel::switch_plan(&f.root(), &mut apt, "vendor", false, None).unwrap();
    plan.execute(&f.root(), &mut apt, "T1").unwrap();
    let st = kernel::status(&f.root()).unwrap();
    let uboot = st.history[0]
        .saved
        .iter()
        .find(|s| s.name.starts_with("linux-u-boot"))
        .unwrap();
    f.write(&uboot.file, "broken");
    // apt has no edge packages either: the revert must stop, not install a broken file.
    struct NoEdgeApt<'a>(FakeApt<'a>);
    impl Executor for NoEdgeApt<'_> {
        fn run(&mut self, a: &[String]) -> Result<()> {
            self.0.run(a)
        }
        fn capture(&mut self, a: &[String]) -> Result<String> {
            if a.len() == 3 && a[1] == "policy" && a[2].contains("edge") {
                return Ok(policy(None, &[]));
            }
            self.0.capture(a)
        }
    }
    let mut apt = NoEdgeApt(apt);
    let err = format!(
        "{:#}",
        kernel::revert_plan(&f.root(), &mut apt, false).unwrap_err()
    );
    assert!(err.contains("linux-u-boot-armsom-cm5-io-edge"), "{err}");
}
