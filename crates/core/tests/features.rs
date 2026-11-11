//! Kernel headers, hold, boot options, CPU frequency, user overlays and
//! migration to USB, against a fake root.

mod common;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{bootenv, cpu, kernel, migrate, overlays};

fn edge_with(extra: &[(&str, &str)]) -> Fake {
    let f = rock4d();
    let mut pkgs = vec![
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("linux-u-boot-radxa-rock-4d-edge", "26.11.0-trunk"),
        ("armbian-firmware", "26.11.0-trunk"),
    ];
    pkgs.extend_from_slice(extra);
    f.status(&pkgs);
    f
}

// ---- headers ----------------------------------------------------------------

#[test]
fn headers_install_and_remove() {
    let f = edge_with(&[]);
    let mut ex = RecordingExecutor::default();
    ex.answer(
        "apt-cache policy linux-headers-edge-rockchip64",
        &policy(Some("26.11.0-trunk"), &["26.11.0-trunk"]),
    );
    ex.answer(
        "apt-get -s -y --no-install-recommends install linux-headers-edge-rockchip64",
        "Inst linux-headers-edge-rockchip64 (26.11.0-trunk Armbian:noble [arm64])\n",
    );
    let text = kernel::headers_plan(&f.root(), &mut ex, true)
        .unwrap()
        .render();
    assert!(
        text.contains("$ apt-get install -y --no-install-recommends linux-headers-edge-rockchip64"),
        "{text}"
    );
    assert!(
        text.contains("Inst linux-headers-edge-rockchip64"),
        "{text}"
    );
    assert!(
        kernel::headers_plan(&f.root(), &mut ex, false).is_err(),
        "not installed"
    );

    let f = edge_with(&[("linux-headers-edge-rockchip64", "26.11.0-trunk")]);
    std::fs::create_dir_all(f.p("/var/lib/dkms/aic8800-usb")).unwrap();
    ex.answer(
        "apt-get -s -y --no-install-recommends remove linux-headers-edge-rockchip64",
        "Remv linux-headers-edge-rockchip64 [26.11.0-trunk]\n",
    );
    let text = kernel::headers_plan(&f.root(), &mut ex, false)
        .unwrap()
        .render();
    assert!(
        text.contains("DKMS modules are installed (aic8800-usb)"),
        "{text}"
    );
    assert!(text.contains("ROCK 4D's Wi-Fi"), "{text}");
    ex.answer(
        "apt-get -s -y --no-install-recommends remove linux-headers-edge-rockchip64",
        "Remv aic8800-dkms [1]\nRemv linux-headers-edge-rockchip64 [26.11.0-trunk]\n",
    );
    let err = kernel::headers_plan(&f.root(), &mut ex, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("would also remove aic8800-dkms"), "{err}");
}

// ---- hold --------------------------------------------------------------------

#[test]
fn hold_and_switch_refusal() {
    let f = edge_with(&[]);
    let st = kernel::status(&f.root()).unwrap();
    assert!(st.held.is_empty());
    assert_eq!(st.hardware_packages.len(), 4);
    let text = kernel::hold_plan(&f.root(), true).unwrap().render();
    assert!(
        text.contains("$ apt-mark hold linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge armbian-firmware"),
        "{text}"
    );
    // dpkg records a hold as "hold ok installed".
    let status = f
        .read("/var/lib/dpkg/status")
        .replace("install ok installed", "hold ok installed");
    f.write("/var/lib/dpkg/status", &status);
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.held.len(), 4);
    assert_eq!(
        st.current.as_deref(),
        Some("edge"),
        "held packages still count as installed"
    );
    let err = kernel::switch_plan(
        &f.root(),
        &mut RecordingExecutor::default(),
        "vendor",
        false,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("hwtool kernel unhold"), "{err}");
    let text = kernel::hold_plan(&f.root(), false).unwrap().render();
    assert!(
        text.contains("$ apt-mark unhold linux-image-edge-rockchip64"),
        "{text}"
    );
    assert!(kernel::hold_plan(&f.root(), true).unwrap().is_empty());
}

// ---- boot options ------------------------------------------------------------

#[test]
fn bootenv_show_and_cmdline() {
    let f = rock4d();
    let st = bootenv::status(&f.root()).unwrap();
    let keys: Vec<&str> = st.options.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "verbosity",
            "console",
            "bootlogo",
            "earlycon",
            "docker_optimizations",
            "extraargs",
            "usbstoragequirks"
        ]
    );
    let console = st.options.iter().find(|o| o.key == "console").unwrap();
    assert_eq!(console.value.as_deref(), Some("both"));
    let early = st.options.iter().find(|o| o.key == "earlycon").unwrap();
    assert_eq!(
        (early.value.as_deref(), early.default.as_deref()),
        (None, Some("off"))
    );
    assert_eq!(
        st.cmdline.as_deref(),
        Some(
            "root=UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908 rootwait rootfstype=ext4 splash=verbose \
             console=ttyS0,1500000 console=tty1 consoleblank=0 loglevel=1 \
             ubootpart=<boot partition PARTUUID> usb-storage.quirks= cgroup_enable=cpuset \
             cgroup_memory=1 cgroup_enable=memory"
        )
    );
}

#[test]
fn bootenv_set_unset_and_refusals() {
    let f = rock4d();
    let p = bootenv::set_plan(&f.root(), "verbosity=7").unwrap();
    assert!(p.render().contains("loglevel=7"), "{}", p.render());
    p.execute(&f.root(), &mut RecordingExecutor::default(), "B")
        .unwrap();
    assert!(f.read("/boot/armbianEnv.txt").contains("verbosity=7\n"));
    assert_eq!(
        f.read("/var/backups/hwtool/B/boot/armbianEnv.txt"),
        ROCK4D_ENV
    );

    let p = bootenv::set_plan(&f.root(), "extraargs=usbcore.autosuspend=-1").unwrap();
    assert!(p.render().contains("usbcore.autosuspend=-1"));
    for (bad, want) in [
        ("verbosity=9", "must be one of"),
        ("console=uart", "must be one of"),
        ("extraargs=a\"b", "may not contain"),
        ("extraargs=x; reset", "may not contain"),
        ("rootdev=/dev/sda1", "hwtool boot migrate"),
        ("overlays=x", "hwtool overlays"),
        ("fdtfile=x.dtb", "U-Boot and device tree packages"),
        ("foo=1", "does not set foo"),
        ("verbosity", "expected KEY=VALUE"),
    ] {
        let err = bootenv::set_plan(&f.root(), bad).unwrap_err().to_string();
        assert!(err.contains(want), "{bad}: {err}");
    }
    let p = bootenv::unset_plan(&f.root(), "verbosity").unwrap();
    assert!(
        p.render()
            .contains("boot.scr then uses its default, verbosity=1"),
        "{}",
        p.render()
    );
    assert!(
        bootenv::unset_plan(&f.root(), "earlycon").is_err(),
        "not set"
    );
}

// ---- CPU ---------------------------------------------------------------------

fn cpus(f: &Fake) {
    let mut info = String::new();
    for n in 0..8 {
        let part = if n < 4 { "0xd03" } else { "0xd08" };
        info.push_str(&format!("processor\t: {n}\nCPU part\t: {part}\n\n"));
    }
    f.write("/proc/cpuinfo", &info);
    for (pol, cpus, freqs) in [
        (
            "policy0",
            "0 1 2 3",
            "408000 600000 816000 1008000 1200000 1416000 1608000 1800000 2016000",
        ),
        (
            "policy4",
            "4 5 6 7",
            "408000 600000 816000 1008000 1200000 1416000 1608000 1800000 2016000 2208000",
        ),
    ] {
        let b = format!("/sys/devices/system/cpu/cpufreq/{pol}");
        f.write(&format!("{b}/affected_cpus"), &format!("{cpus}\n"));
        f.write(&format!("{b}/scaling_governor"), "schedutil\n");
        f.write(
            &format!("{b}/scaling_available_governors"),
            "conservative ondemand userspace powersave performance schedutil\n",
        );
        f.write(
            &format!("{b}/scaling_available_frequencies"),
            &format!("{freqs} \n"),
        );
        f.write(&format!("{b}/scaling_min_freq"), "408000\n");
        f.write(
            &format!("{b}/scaling_max_freq"),
            &format!("{}\n", freqs.split(' ').next_back().unwrap()),
        );
    }
}

#[test]
fn cpu_show_set_reset() {
    let f = rock4d();
    cpus(&f);
    let st = cpu::status(&f.root()).unwrap();
    assert_eq!(st.policies.len(), 2);
    assert_eq!(st.policies[0].core, "Cortex-A53");
    assert_eq!(st.policies[1].core, "Cortex-A72");
    assert_eq!(st.policies[1].max_khz, 2_208_000);
    assert!(!st.persistent);

    let p = cpu::set_plan(
        &f.root(),
        "policy4",
        Some("ondemand"),
        None,
        Some(1_800_000),
    )
    .unwrap();
    let text = p.render();
    assert!(
        text.contains("$ systemctl enable hwtool-boot.service"),
        "{text}"
    );
    assert!(text.contains("ExecStart=/usr/bin/hwtool apply"), "{text}");
    let mut ex = RecordingExecutor::default();
    p.execute(&f.root(), &mut ex, "C").unwrap();
    assert_eq!(
        f.read("/sys/devices/system/cpu/cpufreq/policy4/scaling_governor"),
        "ondemand"
    );
    assert_eq!(
        f.read("/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
        "1800000"
    );
    assert!(f
        .read("/etc/hwtool/cpufreq.conf")
        .contains("policy4 governor=ondemand max=1800000\n"));
    assert!(cpu::status(&f.root()).unwrap().persistent);

    // A second setting keeps the first; the unit is not written again.
    let p = cpu::set_plan(&f.root(), "policy0", Some("performance"), None, None).unwrap();
    let text = p.render();
    assert!(!text.contains("systemctl enable"), "{text}");
    assert!(text.contains("top frequency all the time"), "{text}");
    p.execute(&f.root(), &mut ex, "C").unwrap();
    let conf = f.read("/etc/hwtool/cpufreq.conf");
    assert!(
        conf.contains("policy4 governor=ondemand max=1800000\n")
            && conf.contains("policy0 governor=performance\n"),
        "{conf}"
    );

    // At boot: the unit applies the file.
    let text = cpu::apply_plan(&f.root()).unwrap().render();
    assert!(
        text.contains("write ondemand to /sys/devices/system/cpu/cpufreq/policy4/scaling_governor"),
        "{text}"
    );

    for (pol, gov, min, max, want) in [
        ("policy9", None, None, Some(1), "no cpufreq policy policy9"),
        ("policy4", Some("turbo"), None, None, "not turbo"),
        ("policy4", None, Some(123), None, "not 123"),
        (
            "policy4",
            None,
            Some(2_208_000),
            Some(408_000),
            "above the maximum",
        ),
        ("policy4", None, None, None, "give --governor"),
    ] {
        let err = cpu::set_plan(&f.root(), pol, gov, min, max)
            .unwrap_err()
            .to_string();
        assert!(err.contains(want), "{err}");
    }
    let text = cpu::reset_plan(&f.root()).unwrap().render();
    assert!(
        text.contains("$ systemctl disable hwtool-boot.service"),
        "{text}"
    );
    assert!(
        text.contains("$ rm -f /etc/systemd/system/hwtool-boot.service"),
        "{text}"
    );
}

#[test]
fn cpu_needs_cpufreq() {
    let f = rock4d();
    assert!(cpu::status(&f.root())
        .unwrap_err()
        .to_string()
        .contains("no policies"));
}

// ---- user overlays -----------------------------------------------------------

const OVERLAY: &str = "/dts-v1/;\n/plugin/;\n\n/ {\n\tcompatible = \"radxa,rock-4d\", \"rockchip,rk3576\";\n\n\tfragment@0 {\n\t\ttarget-path = \"/chosen\";\n\t\t__overlay__ {\n\t\t\tcompatible = \"not,the-root\";\n\t\t\troxora-test = \"ok\";\n\t\t};\n\t};\n};\n";

#[test]
fn overlay_compatibles_are_read_at_the_root() {
    assert_eq!(
        overlays::overlay_compatibles(OVERLAY),
        vec!["radxa,rock-4d", "rockchip,rk3576"]
    );
}

#[test]
fn add_and_remove_user_overlays() {
    let f = rock4d();
    f.write(
        "/proc/device-tree/compatible",
        "radxa,rock-4d\0rockchip,rk3576\0",
    );
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("my-sensor.dtso");
    std::fs::write(&src, OVERLAY).unwrap();
    let file = src.display().to_string();
    f.programs(&["dtc"]);
    let p = overlays::add_plan(&f.root(), &mut RecordingExecutor::default(), &file).unwrap();
    let text = p.render();
    assert!(
        text.contains(&format!(
            "$ dtc -@ -q -I dts -O dtb -o /run/hwtool/my-sensor.dtbo {file}"
        )),
        "{text}"
    );
    assert!(
        text.contains(
            "$ install -D -m 0644 /run/hwtool/my-sensor.dtbo /boot/overlay-user/my-sensor.dtbo"
        ),
        "{text}"
    );
    assert!(text.contains("| user_overlays=my-sensor"), "{text}");
    assert!(!text.contains("device-tree-compiler"));

    // No dtc: the plan installs it from apt first, and refuses removals.
    std::fs::remove_file(f.p("/usr/sbin/dtc")).unwrap();
    let mut ex = RecordingExecutor::default();
    ex.answer(
        "apt-get -s -y --no-install-recommends install device-tree-compiler",
        "Inst device-tree-compiler (1.7.0-2build1 Ubuntu:24.04/noble [arm64])\n",
    );
    let text = overlays::add_plan(&f.root(), &mut ex, &file)
        .unwrap()
        .render();
    assert!(
        text.contains("$ apt-get install -y --no-install-recommends device-tree-compiler"),
        "{text}"
    );
    ex.answer(
        "apt-get -s -y --no-install-recommends install device-tree-compiler",
        "Remv foo [1]\nInst device-tree-compiler (1.7.0)\n",
    );
    assert!(overlays::add_plan(&f.root(), &mut ex, &file).is_err());
    f.programs(&["dtc"]);

    // For another board: refused.
    std::fs::write(
        &src,
        OVERLAY.replace("radxa,rock-4d\", \"rockchip,rk3576", "rockchip,rk3588"),
    )
    .unwrap();
    let err = overlays::add_plan(&f.root(), &mut ex, &file)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is for rockchip,rk3588"), "{err}");
    for (body, want) in [
        ("/dts-v1/;\n/ { };\n", "no /plugin/"),
        (
            "/dts-v1/;\n/plugin/;\n#include <dt-bindings/gpio/gpio.h>\n",
            "#include",
        ),
    ] {
        std::fs::write(&src, body).unwrap();
        let err = overlays::add_plan(&f.root(), &mut ex, &file)
            .unwrap_err()
            .to_string();
        assert!(err.contains(want), "{err}");
    }
    let bad = dir.path().join("-x.dtso");
    std::fs::write(&bad, OVERLAY).unwrap();
    assert!(overlays::add_plan(&f.root(), &mut ex, &bad.display().to_string()).is_err());

    // Remove: disables it and deletes the file after a backup.
    f.write("/boot/overlay-user/my-sensor.dtbo", "dtbo");
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}user_overlays=my-sensor other\n"),
    );
    std::fs::write(&src, OVERLAY).unwrap();
    assert!(overlays::add_plan(&f.root(), &mut ex, &file)
        .unwrap_err()
        .to_string()
        .contains("already exists"));
    let p = overlays::remove_plan(&f.root(), "my-sensor").unwrap();
    let text = p.render();
    assert!(text.contains("| user_overlays=other"), "{text}");
    assert!(
        text.contains("$ rm -f /boot/overlay-user/my-sensor.dtbo"),
        "{text}"
    );
    p.execute(&f.root(), &mut RecordingExecutor::default(), "O")
        .unwrap();
    assert_eq!(
        f.read("/var/backups/hwtool/O/boot/overlay-user/my-sensor.dtbo"),
        "dtbo"
    );
    assert!(overlays::remove_plan(&f.root(), "uart1").is_err());
}

// ---- migration to USB --------------------------------------------------------

/// /sys/block/sda as the kernel shows it: a link into the device tree of
/// sysfs, whose path names the bus (usb, ata).
fn attach_sda(f: &Fake, bus_path: &str) {
    let dir = format!("/sys/devices/{bus_path}/block/sda");
    f.write(&format!("{dir}/size"), "250069680\n");
    let _ = std::fs::remove_file(f.p("/sys/block/sda"));
    f.link(
        "/sys/block/sda",
        &format!("../devices/{bus_path}/block/sda"),
    );
}

fn usb_ready(f: &Fake) -> RecordingExecutor {
    f.programs(&[
        "parted",
        "wipefs",
        "mkfs.ext4",
        "rsync",
        "blkid",
        "partprobe",
        "udevadm",
        "flashcp",
    ]);
    f.write("/sys/class/mtd/mtd0/name", "spi5.0\n");
    f.write("/sys/class/mtd/mtd0/size", "16777216\n");
    attach_sda(
        f,
        "platform/usbdrd3_0/xhci-hcd.0.auto/usb2/2-1/2-1:1.0/host0/target0:0:0/0:0:0:0",
    );
    let mut ex = RecordingExecutor::default();
    ex.answer("df -B1 --output=used /", "Used\n2000000000\n");
    ex.answer("lsblk -ndo PTTYPE /dev/mmcblk1", "gpt\n");
    ex
}

#[test]
fn migrate_to_usb_needs_spi_uboot_with_usb() {
    let f = rock4d();
    // Facts from the ROCK 4D U-Boot package: USB storage built in, usb in
    // the SPI image's boot_targets.
    let d = "/usr/lib/linux-u-boot-edge-radxa-rock-4d";
    f.write(
        &format!("{d}/u-boot-config-target-1"),
        "CONFIG_USB_STORAGE=y\nCONFIG_SCSI=y\nCONFIG_DEFAULT_FDT_FILE=\"rockchip/rk3576-rock-4d.dtb\"\n",
    );
    let mut spi = vec![0u8; 4096];
    spi.extend_from_slice(b"boot_targets=mmc0 mmc1 scsi nvme usb pxe dhcp\0");
    std::fs::write(f.p(&format!("{d}/u-boot-rockchip-spi.bin")), &spi).unwrap();
    let mut ex = usb_ready(&f);
    assert_eq!(
        hwtool_core::storage::find_disk(&f.root(), "/dev/sda")
            .unwrap()
            .kind,
        "usb"
    );
    let err = migrate::plan(&f.root(), &mut ex, "/dev/sda", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("add --write-spi"), "{err}");
    let text = migrate::plan(&f.root(), &mut ex, "/dev/sda", true)
        .unwrap()
        .render();
    assert!(text.contains("mkpart primary ext4 1MiB 100%"), "{text}");
    assert!(text.contains("$ flashcp -v -p"), "{text}");
    assert!(text.contains("before USB"), "{text}");

    // Without USB storage in U-Boot: refused, naming the setting.
    f.write(&format!("{d}/u-boot-config-target-1"), "CONFIG_SCSI=y\n");
    let err = migrate::plan(&f.root(), &mut ex, "/dev/sda", true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("CONFIG_USB_STORAGE=y not set"), "{err}");

    // A SATA disk: the ROCK 4D U-Boot has no AHCI.
    f.link(
        "/sys/block/sda",
        "../devices/platform/fe210000.sata/ata1/host0/target0:0:0/0:0:0:0/block/sda",
    );
    let err = migrate::plan(&f.root(), &mut ex, "/dev/sda", true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("without SATA support"), "{err}");
}

#[test]
fn cm5_refuses_usb() {
    let f = cm5();
    let mut ex = usb_ready(&f);
    let err = migrate::plan(&f.root(), &mut ex, "/dev/sda", true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no SPI writer"), "{err}");
    assert!(
        migrate::boot_targets(b"xx\0boot_targets=mmc1 mmc0 nvme scsi usb pxe dhcp spi\0yy")
            .is_some_and(|t| t == "mmc1 mmc0 nvme scsi usb pxe dhcp spi")
    );
}
