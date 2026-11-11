//! A fake root filesystem shaped like a stock Roxora/Armbian noble minimal
//! image (from the image inventories of radxa-rock-4d and armsom-cm5-io edge
//! builds at armbian/build 3b7191306d36).

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use hwtool_core::Root;
use tempfile::TempDir;

pub const ROCK4D_ENV: &str = "verbosity=1\nbootlogo=false\nconsole=both\noverlay_prefix=rockchip-rk3576\nrootdev=UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908\nrootfstype=ext4\n";
pub const CM5_ENV: &str = "verbosity=1\nbootlogo=false\nconsole=both\noverlay_prefix=armsom-cm5-io rockchip-rk3576\nfdtfile=rockchip/rk3576-armsom-cm5-io.dtb\nrootdev=UUID=237fa9bf-b34e-4ece-bbc6-344533c82f1e\nrootfstype=ext4\n";
pub const EDGE_KERNEL: &str = "7.3.4-edge-rockchip64";
pub const VENDOR_KERNEL: &str = "6.1.115-vendor-rk35xx";

pub struct Fake {
    pub dir: TempDir,
}

impl Fake {
    pub fn root(&self) -> Root {
        Root::new(self.dir.path())
    }

    pub fn p(&self, abs: &str) -> PathBuf {
        self.dir.path().join(abs.trim_start_matches('/'))
    }

    pub fn write(&self, abs: &str, content: &str) {
        let p = self.p(abs);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    pub fn read(&self, abs: &str) -> String {
        fs::read_to_string(self.p(abs)).unwrap()
    }

    pub fn link(&self, abs: &str, target: &str) {
        let p = self.p(abs);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&p);
        symlink(target, p).unwrap();
    }

    pub fn sized(&self, abs: &str, len: u64) {
        let p = self.p(abs);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::File::create(p).unwrap().set_len(len).unwrap();
    }

    pub fn status(&self, pkgs: &[(&str, &str)]) {
        let mut s = String::new();
        for (n, v) in pkgs {
            s.push_str(&format!(
                "Package: {n}\nStatus: install ok installed\nArchitecture: arm64\nVersion: {v}\n\n"
            ));
        }
        self.write("/var/lib/dpkg/status", &s);
        // dpkg's file lists, so packages can be saved (repack.rs).
        for (n, _) in pkgs {
            let doc = format!("/usr/share/doc/{n}");
            self.write(&format!("{doc}/copyright"), "test\n");
            self.write(
                &format!("/var/lib/dpkg/info/{n}.list"),
                &format!("/.\n/usr\n/usr/share\n/usr/share/doc\n{doc}\n{doc}/copyright\n"),
            );
        }
    }

    /// A disk in sysfs. `parts` are (start sector, size sectors).
    pub fn disk(&self, name: &str, mmc_type: Option<&str>, sectors: u64, parts: &[(u64, u64)]) {
        let base = format!("/sys/block/{name}");
        self.write(&format!("{base}/size"), &format!("{sectors}\n"));
        if let Some(t) = mmc_type {
            self.write(&format!("{base}/device/type"), &format!("{t}\n"));
        }
        self.blockdev(name, &format!("../../block/{name}"));
        for (i, (start, size)) in parts.iter().enumerate() {
            let pn = if name.ends_with(|c: char| c.is_ascii_digit()) {
                format!("{name}p{}", i + 1)
            } else {
                format!("{name}{}", i + 1)
            };
            self.write(&format!("{base}/{pn}/partition"), &format!("{}\n", i + 1));
            self.write(&format!("{base}/{pn}/start"), &format!("{start}\n"));
            self.write(&format!("{base}/{pn}/size"), &format!("{size}\n"));
            self.blockdev(&pn, &format!("../../block/{name}/{pn}"));
        }
    }

    /// /sys/dev/block/<major:minor> -> the device, as the kernel links it.
    fn blockdev(&self, name: &str, target: &str) {
        self.link(&format!("/sys/dev/block/{}", majmin(name)), target);
    }

    /// A device-mapper device (LUKS, LVM) on top of `slaves`.
    pub fn dm(&self, name: &str, slaves: &[&str]) {
        for sl in slaves {
            std::fs::create_dir_all(self.p(&format!("/sys/block/{name}/slaves/{sl}"))).unwrap();
        }
        self.write(&format!("/sys/block/{name}/size"), "1000\n");
        self.blockdev(name, &format!("../../devices/virtual/block/{name}"));
    }

    /// Mounts as "device mountpoint fstype" lines, written as
    /// /proc/self/mountinfo with the devices' major:minor numbers.
    pub fn mounts(&self, text: &str) {
        let mut out = String::new();
        for (i, l) in text.lines().enumerate() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 {
                continue;
            }
            let name = f[0].rsplit('/').next().unwrap();
            let mm = if f[0].starts_with("/dev/") {
                majmin(name)
            } else {
                "0:99".into()
            };
            out.push_str(&format!(
                "{} 1 {mm} / {} rw,relatime - {} {} rw\n",
                20 + i,
                f[1],
                f[2],
                f[0]
            ));
        }
        self.write("/proc/self/mountinfo", &out);
    }

    pub fn programs(&self, names: &[&str]) {
        for n in names {
            self.write(&format!("/usr/sbin/{n}"), "#!/bin/sh\n");
        }
    }
}

/// A stable made-up major:minor number for a device name.
pub fn majmin(name: &str) -> String {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(16777619);
    }
    format!("259:{}", h % 1_000_000)
}

fn fixture(name: &str) -> String {
    fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn base(board: &str, env: &str) -> Fake {
    let f = Fake {
        dir: TempDir::new().unwrap(),
    };
    f.write(
        "/etc/armbian-release",
        &fixture(&format!("armbian-release-{board}")),
    );
    f.write("/boot/armbianEnv.txt", env);
    f.write("/boot/boot.cmd", "setenv overlay_error \"false\"\n");
    f.write("/boot/boot.scr", "script");
    f.write(&format!("/boot/vmlinuz-{EDGE_KERNEL}"), "kernel");
    f.link("/boot/Image", &format!("vmlinuz-{EDGE_KERNEL}"));
    f.link("/boot/dtb", &format!("dtb-{EDGE_KERNEL}"));
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay/README.rockchip-overlays"),
        "",
    );
    // A few of the 139 overlays linux-dtb-edge-rockchip64 7.3.4 ships; none is for rk3576.
    for o in [
        "rockchip-rk3588-uart1-m1",
        "rockchip-rk3568-hk-uart1",
        "rk3308-uart1",
    ] {
        f.write(
            &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay/{o}.dtbo"),
            "",
        );
    }
    f.write("/proc/cmdline", "root=UUID=db9a14c3 rootwait\n");
    f.mounts("/dev/mmcblk1p1 / ext4 rw,relatime 0 0\nproc /proc proc rw 0 0\n");
    f.disk("mmcblk1", Some("SD"), 62_333_952, &[(32768, 62_300_000)]);
    f
}

pub fn rock4d() -> Fake {
    let f = base("radxa-rock-4d", ROCK4D_ENV);
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/rk3576-rock-4d.dtb"),
        "dtb",
    );
    f.write(
        "/usr/lib/u-boot/platform_install.sh",
        &fixture("platform_install-radxa-rock-4d-edge.sh"),
    );
    let d = "/usr/lib/linux-u-boot-edge-radxa-rock-4d";
    f.sized(&format!("{d}/u-boot-rockchip.bin"), 9_561_600);
    f.sized(&format!("{d}/u-boot-rockchip-spi.bin"), 1_598_976);
    f.write(
        &format!("{d}/u-boot-config-target-1"),
        "CONFIG_MTD=y\nCONFIG_DEFAULT_FDT_FILE=\"rockchip/rk3576-rock-4d.dtb\"\n",
    );
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("linux-u-boot-radxa-rock-4d-edge", "26.11.0-trunk"),
        ("armbian-firmware", "26.11.0-trunk"),
    ]);
    f
}

pub fn cm5() -> Fake {
    let f = base("armsom-cm5-io", CM5_ENV);
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/rk3576-armsom-cm5-io.dtb"),
        "dtb",
    );
    f.write(
        "/usr/lib/u-boot/platform_install.sh",
        &fixture("platform_install-armsom-cm5-io-edge.sh"),
    );
    let d = "/usr/lib/linux-u-boot-edge-armsom-cm5-io";
    f.sized(&format!("{d}/u-boot-rockchip.bin"), 9_000_000);
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("linux-u-boot-armsom-cm5-io-edge", "26.11.0-trunk"),
    ]);
    f
}

pub fn policy(candidate: Option<&str>, versions: &[&str]) -> String {
    let mut s = format!(
        "pkg:\n  Installed: (none)\n  Candidate: {}\n  Version table:\n",
        candidate.unwrap_or("(none)")
    );
    for v in versions {
        s.push_str(&format!(
            "     {v} 500\n        500 http://apt.armbian.com noble/main arm64 Packages\n"
        ));
    }
    s
}
