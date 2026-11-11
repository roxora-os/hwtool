//! Pin header interfaces, bootloader versions and boot order, LEDs, thermal
//! and Wi-Fi country, against a fake root.

mod common;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{bootinfo, cpu, interfaces, leds, swap, thermal, wifi};

// ---- interfaces --------------------------------------------------------------

/// The package's interface data for the board, as build-debs.sh installs it.
fn with_interfaces(f: &Fake, board: &str) {
    let src = format!(
        "{}/../../data/interfaces/{board}",
        env!("CARGO_MANIFEST_DIR")
    );
    let dst = format!("/usr/share/hwtool/interfaces/{board}");
    for e in std::fs::read_dir(&src).unwrap() {
        let n = e.unwrap().file_name().into_string().unwrap();
        if n == "index.json" {
            f.write(
                &format!("{dst}/index.json"),
                &std::fs::read_to_string(format!("{src}/{n}")).unwrap(),
            );
        } else if let Some(stem) = n.strip_suffix(".dtso") {
            f.write(&format!("{dst}/{stem}.dtbo"), "dtbo");
        }
    }
}

#[test]
fn interfaces_list_enable_conflict_disable() {
    let f = rock4d();
    with_interfaces(&f, "radxa-rock-4d");
    let st = interfaces::status(&f.root()).unwrap();
    assert_eq!(st.interfaces.len(), 20);
    assert!(st.interfaces.iter().all(|e| !e.enabled));
    let busy: Vec<u32> = st.busy.iter().map(|(p, _)| *p).collect();
    assert_eq!(busy, vec![8, 10, 27, 28]);
    let spi = st
        .interfaces
        .iter()
        .find(|e| e.interface.name == "spi1-m0")
        .unwrap();
    let pins: Vec<u32> = spi.interface.pins.iter().map(|p| p.pin).collect();
    assert_eq!(pins, vec![19, 21, 23, 24, 26]);
    assert!(spi.conflicts.contains(&"i2c9-m1".to_string()));
    assert!(spi.conflicts.contains(&"uart3-m2".to_string()), "pin 26");

    // Two that share pins at once.
    let err = interfaces::enable_plan(&f.root(), &["spi1-m0".into(), "i2c9-m1".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("share pins"), "{err}");
    assert!(interfaces::enable_plan(&f.root(), &["nope".into()]).is_err());

    let p = interfaces::enable_plan(&f.root(), &["spi1-m0".into()]).unwrap();
    let text = p.render();
    assert!(
        text.contains("$ install -D -m 0644 /usr/share/hwtool/interfaces/radxa-rock-4d/spi1-m0.dtbo /boot/overlay-user/hwtool-spi1-m0.dtbo"),
        "{text}"
    );
    assert!(text.contains("user_overlays=hwtool-spi1-m0"), "{text}");
    assert!(text.contains("/dev/spidev"), "{text}");
    assert!(p.reboot_required);
    let mut ex = RecordingExecutor::default();
    p.execute(&f.root(), &mut ex, "I").unwrap();
    assert!(ex.ran.iter().any(|r| r[0] == "install"));
    // What install would have done.
    f.write("/boot/overlay-user/hwtool-spi1-m0.dtbo", "dtbo");
    assert!(f
        .read("/boot/armbianEnv.txt")
        .contains("user_overlays=hwtool-spi1-m0\n"));

    let st = interfaces::status(&f.root()).unwrap();
    let i2c9 = st
        .interfaces
        .iter()
        .find(|e| e.interface.name == "i2c9-m1")
        .unwrap();
    assert_eq!(i2c9.blocked_by, vec!["spi1-m0".to_string()]);
    let err = interfaces::enable_plan(&f.root(), &["i2c9-m1".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("disable that first"), "{err}");
    // One that shares nothing goes next to it.
    let p = interfaces::enable_plan(&f.root(), &["i2c8-m2".into()]).unwrap();
    assert!(
        p.render()
            .contains("user_overlays=hwtool-spi1-m0 hwtool-i2c8-m2"),
        "{}",
        p.render()
    );

    let p = interfaces::disable_plan(&f.root(), &["spi1-m0".into()]).unwrap();
    assert!(p
        .render()
        .contains("$ rm -f /boot/overlay-user/hwtool-spi1-m0.dtbo"));
    p.execute(&f.root(), &mut ex, "J").unwrap();
    assert!(!f.read("/boot/armbianEnv.txt").contains("user_overlays"));
    assert!(interfaces::disable_plan(&f.root(), &["spi1-m0".into()]).is_err());
}

#[test]
fn interfaces_cm5_and_w1() {
    let f = cm5();
    with_interfaces(&f, "armsom-cm5-io");
    let st = interfaces::status(&f.root()).unwrap();
    assert_eq!(st.interfaces.len(), 10);
    assert!(st.busy.iter().any(|(p, w)| *p == 28 && w.contains("fan")));
    let p = interfaces::enable_plan(&f.root(), &["w1-gpio-pin7".into()]).unwrap();
    assert!(p.render().contains("pull-up"), "{}", p.render());
    // pin 7 is also PWM0 here.
    let err = interfaces::enable_plan(&f.root(), &["w1-gpio-pin7".into(), "pwm0-ch0-m0".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("share pins"), "{err}");
}

#[test]
fn interfaces_missing_data() {
    let f = rock4d();
    let err = interfaces::status(&f.root()).unwrap_err().to_string();
    assert!(err.contains("has no pin header interfaces"), "{err}");
}

// ---- bootloader versions and boot order -----------------------------------------

const PKG_V: &str = "U-Boot 2026.04-armbian-2026.04-S88dc (Nov 10 2026 - 12:27:04 +0000)";
const OLD_V: &str = "U-Boot 2025.10-armbian-2025.10-S1234 (Oct 01 2025 - 08:00:00 +0000)";

fn image(version: &str, len: usize, env: &str) -> Vec<u8> {
    let mut b = vec![0u8; len];
    let spl = b"U-Boot SPL 2026.04 (Nov 10 2026 - 12:27:04 +0000)\0";
    b[0x8000..0x8000 + spl.len()].copy_from_slice(spl);
    let v = format!("{version}\0");
    b[0x60000..0x60000 + v.len()].copy_from_slice(v.as_bytes());
    let e = format!("\0{env}\0\0");
    b[0x70000..0x70000 + e.len()].copy_from_slice(e.as_bytes());
    b
}

const DEFAULT_ENV: &str =
    "arch=arm\0baudrate=1500000\0boot_targets=mmc0 mmc1 scsi nvme usb pxe dhcp\0bootcmd=bootflow scan -lb";

fn rock4d_boot(spi: Option<&str>, sd: Option<&str>) -> Fake {
    let f = rock4d();
    let d = "/usr/lib/linux-u-boot-edge-radxa-rock-4d";
    std::fs::write(
        f.p(&format!("{d}/u-boot-rockchip.bin")),
        image(PKG_V, 0x80000, DEFAULT_ENV),
    )
    .unwrap();
    std::fs::write(
        f.p(&format!("{d}/u-boot-rockchip-spi.bin")),
        image(PKG_V, 0x80000, DEFAULT_ENV),
    )
    .unwrap();
    // From the linux-u-boot-radxa-rock-4d-edge package's u-boot-config-target.
    f.write(
        &format!("{d}/u-boot-config-target-1"),
        "CONFIG_MTD=y\nCONFIG_ENV_IS_IN_SPI_FLASH=y\nCONFIG_ENV_OFFSET=0xc00000\nCONFIG_ENV_SIZE=0x20000\n",
    );
    std::fs::create_dir_all(f.p("/dev")).unwrap();
    f.write("/sys/class/mtd/mtd0/name", "sfc_nor\n");
    f.write("/sys/class/mtd/mtd0/size", "16777216\n");
    f.write("/sys/class/mtd/mtd0/erasesize", "4096\n");
    std::fs::write(
        f.p("/dev/mtd0"),
        match spi {
            Some(v) => image(v, 0x80000, DEFAULT_ENV),
            None => vec![0xff; 0x80000],
        },
    )
    .unwrap();
    // U-Boot at 32 KiB on the SD card, before the first partition (16 MiB).
    std::fs::write(
        f.p("/dev/mmcblk1"),
        match sd {
            Some(v) => image(v, 0x80000, DEFAULT_ENV),
            None => vec![0; 0x80000],
        },
    )
    .unwrap();
    f
}

#[test]
fn bootloader_versions() {
    let f = rock4d_boot(Some(OLD_V), Some(PKG_V));
    f.programs(&["flashcp"]);
    let v = bootinfo::versions(&f.root()).unwrap();
    assert_eq!(v.package.as_deref(), Some(PKG_V));
    assert_eq!(v.media.len(), 2);
    assert_eq!(v.media[0].device, "/dev/mtd0");
    assert_eq!(v.media[0].state, "older");
    assert_eq!(v.media[1].device, "/dev/mmcblk1");
    assert_eq!(v.media[1].state, "up to date");
    assert_eq!(v.starts_from.as_deref(), Some("/dev/mtd0"));

    let p = bootinfo::update_plan(&f.root()).unwrap();
    assert!(
        p.title.contains("Update U-Boot on /dev/mtd0"),
        "{}",
        p.title
    );
    assert!(p.title.contains(OLD_V), "{}", p.title);

    // Nothing on SPI: the board starts from the SD card, which is current.
    let f = rock4d_boot(None, Some(PKG_V));
    let v = bootinfo::versions(&f.root()).unwrap();
    assert_eq!(v.media[0].state, "none");
    assert_eq!(v.starts_from.as_deref(), Some("/dev/mmcblk1"));
    let err = bootinfo::update_plan(&f.root()).unwrap_err().to_string();
    assert!(err.contains("already has the installed U-Boot"), "{err}");
}

#[test]
fn boot_order() {
    let f = rock4d_boot(Some(PKG_V), None);
    let mut ex = RecordingExecutor::default();
    let o = bootinfo::order(&f.root(), &mut ex).unwrap();
    assert_eq!(
        o.default,
        vec!["mmc0", "mmc1", "scsi", "nvme", "usb", "pxe", "dhcp"]
    );
    assert_eq!(
        o.writable.as_deref(),
        Some("/dev/mtd0 at 0xc00000, 0x20000 bytes")
    );
    assert!(bootinfo::order_plan(&f.root(), &mut ex, &["sata".into()]).is_err());

    ex.answer(
        "apt-get -s -y --no-install-recommends install libubootenv-tool",
        "Inst libubootenv-tool (0.3.6-2 Ubuntu:noble [arm64])\n",
    );
    let p = bootinfo::order_plan(&f.root(), &mut ex, &["nvme".into(), "mmc1".into()]).unwrap();
    let text = p.render();
    assert!(text.contains("libubootenv-tool"), "{text}");
    assert!(text.contains("/dev/mtd0 0xc00000 0x20000 0x1000"), "{text}");
    assert!(text.contains("boot_targets nvme mmc1\n"), "{text}");
    assert!(text.contains("baudrate 1500000\n"), "{text}");
    assert!(
        text.contains("$ fw_setenv -c /etc/hwtool/fw_env.config -s /run/hwtool/uboot-env.txt"),
        "{text}"
    );

    f.programs(&["flash_erase"]);
    let text = bootinfo::order_reset_plan(&f.root()).unwrap().render();
    assert!(
        text.contains("$ flash_erase /dev/mtd0 0xc00000 32"),
        "{text}"
    );
}

#[test]
fn boot_order_not_possible_on_cm5() {
    let f = cm5();
    f.write(
        "/usr/lib/linux-u-boot-edge-armsom-cm5-io/u-boot-config-target-1",
        "CONFIG_MTD=y\nCONFIG_ENV_IS_NOWHERE=y\n",
    );
    let o = bootinfo::order(&f.root(), &mut RecordingExecutor::default()).unwrap();
    assert!(o.writable.is_none());
    let r = o.reason.unwrap();
    assert!(r.contains("CONFIG_ENV_IS_NOWHERE"), "{r}");
    assert!(bootinfo::order_plan(
        &f.root(),
        &mut RecordingExecutor::default(),
        &["mmc0".into()]
    )
    .is_err());
}

// ---- LEDs -------------------------------------------------------------------------

fn with_leds(f: &Fake) {
    for (n, t) in [
        ("blue:status", "none timer [heartbeat] default-on mmc0"),
        ("green:power", "[none] timer heartbeat default-on"),
    ] {
        f.write(&format!("/sys/class/leds/{n}/trigger"), &format!("{t}\n"));
        f.write(&format!("/sys/class/leds/{n}/brightness"), "1\n");
        f.write(&format!("/sys/class/leds/{n}/max_brightness"), "1\n");
    }
}

#[test]
fn leds_with_armbian_led_state() {
    let f = rock4d();
    with_leds(&f);
    f.write("/lib/systemd/system/armbian-led-state.service", "[Unit]\n");
    f.write(
        "/etc/armbian-leds.conf",
        "[/sys/class/leds/blue:status]\ntrigger=heartbeat\nbrightness=1\n",
    );
    let st = leds::status(&f.root()).unwrap();
    assert!(st.armbian_led_state);
    let blue = st.leds.iter().find(|l| l.name == "blue:status").unwrap();
    assert_eq!(blue.trigger, "heartbeat");
    assert!(leds::set_plan(&f.root(), "blue:status", "disk-activity").is_err());
    assert!(leds::set_plan(&f.root(), "red", "none").is_err());

    let p = leds::set_plan(&f.root(), "blue:status", "none").unwrap();
    let text = p.render();
    assert!(!text.contains("hwtool-boot.service"), "{text}");
    p.execute(&f.root(), &mut RecordingExecutor::default(), "L")
        .unwrap();
    assert_eq!(f.read("/sys/class/leds/blue:status/trigger"), "none");
    assert_eq!(
        f.read("/etc/armbian-leds.conf"),
        "[/sys/class/leds/blue:status]\ntrigger=none\nbrightness=1\n"
    );
    assert!(!f.p("/etc/hwtool/leds.conf").exists());
}

#[test]
fn leds_without_armbian_led_state() {
    let f = rock4d();
    with_leds(&f);
    let p = leds::set_plan(&f.root(), "green:power", "heartbeat").unwrap();
    let text = p.render();
    assert!(text.contains("hwtool-boot.service"), "{text}");
    p.execute(&f.root(), &mut RecordingExecutor::default(), "L")
        .unwrap();
    assert_eq!(
        f.read("/etc/hwtool/leds.conf"),
        "[/sys/class/leds/green:power]\ntrigger=heartbeat\n"
    );
    assert!(f
        .read(cpu::UNIT)
        .contains("ExecStart=/usr/bin/hwtool apply"));

    // At boot: hwtool apply sets it again.
    f.write(
        "/sys/class/leds/green:power/trigger",
        "[none] timer heartbeat default-on\n",
    );
    let p = cpu::apply_plan(&f.root()).unwrap();
    assert!(
        p.render()
            .contains("Set /sys/class/leds/green:power to heartbeat"),
        "{}",
        p.render()
    );
    p.execute(&f.root(), &mut RecordingExecutor::default(), "A")
        .unwrap();
    assert_eq!(f.read("/sys/class/leds/green:power/trigger"), "heartbeat");

    // cpu reset keeps the unit while LED settings need it.
    let text = cpu::reset_plan(&f.root()).unwrap().render();
    assert!(
        !text.contains("rm -f /etc/systemd/system/hwtool-boot.service"),
        "{text}"
    );
}

// ---- thermal ------------------------------------------------------------------------

#[test]
fn thermal_without_fan() {
    let f = rock4d();
    let z = "/sys/class/thermal/thermal_zone0";
    f.write(&format!("{z}/type"), "package-thermal\n");
    f.write(&format!("{z}/temp"), "45250\n");
    f.write(&format!("{z}/trip_point_0_temp"), "85000\n");
    f.write(&format!("{z}/trip_point_0_type"), "passive\n");
    f.write(&format!("{z}/trip_point_1_temp"), "115000\n");
    f.write(&format!("{z}/trip_point_1_type"), "critical\n");
    for t in ["trip_point_0_temp", "trip_point_1_temp"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.p(&format!("{z}/{t}")),
            std::fs::Permissions::from_mode(0o444),
        )
        .unwrap();
    }
    let c = "/sys/class/thermal/cooling_device0";
    f.write(&format!("{c}/type"), "cpufreq-cpu0\n");
    f.write(&format!("{c}/cur_state"), "0\n");
    f.write(&format!("{c}/max_state"), "9\n");
    let st = thermal::status(&f.root()).unwrap();
    assert_eq!(st.zones[0].temp_mc, Some(45250));
    assert_eq!(st.zones[0].trips.len(), 2);
    assert_eq!(st.zones[0].trips[1].kind, "critical");
    assert!(!st.fan && !st.writable_trips);
    assert!(st.notes.iter().any(|n| n.contains("No fan")));
    assert!(st
        .notes
        .iter()
        .any(|n| n.contains("CONFIG_THERMAL_WRITABLE_TRIPS")));

    // A pwm-fan in the device tree counts as a fan.
    f.write("/sys/firmware/devicetree/base/fan/compatible", "pwm-fan\0");
    assert!(thermal::status(&f.root()).unwrap().fan);
}

// ---- Wi-Fi country ---------------------------------------------------------------------

#[test]
fn wifi_country() {
    let f = rock4d();
    let mut ex = RecordingExecutor::default();
    assert!(wifi::set_plan(&f.root(), &mut ex, "XX", false).is_err());
    assert!(wifi::set_plan(&f.root(), &mut ex, "GER", false).is_err());
    assert!(
        wifi::set_plan(&f.root(), &mut ex, "de", true).is_err(),
        "no aic8800 here"
    );

    ex.answer(
        "apt-get -s -y --no-install-recommends install iw",
        "Inst iw (6.9-1 Ubuntu:noble [arm64])\n",
    );
    let p = wifi::set_plan(&f.root(), &mut ex, "de", false).unwrap();
    let text = p.render();
    assert!(
        text.contains("$ apt-get install -y --no-install-recommends iw"),
        "{text}"
    );
    assert!(
        text.contains("options cfg80211 ieee80211_regdom=DE"),
        "{text}"
    );
    assert!(text.contains("$ iw reg set DE"), "{text}");
    assert!(!text.contains("aic8800"), "{text}");
    p.execute(&f.root(), &mut ex, "W").unwrap();
    assert!(f
        .read(wifi::MODPROBE)
        .contains("options cfg80211 ieee80211_regdom=DE\n"));

    // The ROCK 4D's aic8800 DKMS module.
    f.programs(&["iw"]);
    std::fs::create_dir_all(f.p("/var/lib/dkms/aic8800-usb")).unwrap();
    ex.answer(
        "iw reg get",
        "global\ncountry DE: DFS-ETSI\n\nphy#0 (self-managed)\ncountry 00: DFS-UNSET\n",
    );
    let st = wifi::status(&f.root(), &mut ex).unwrap();
    assert_eq!(st.country.as_deref(), Some("DE"));
    assert_eq!(st.configured.as_deref(), Some("DE"));
    assert_eq!(st.self_managed, vec![("phy#0".into(), "00".into())]);
    assert!(st.aic8800 && !st.aic8800_follows_country);
    let text = wifi::set_plan(&f.root(), &mut ex, "US", false)
        .unwrap()
        .render();
    assert!(text.contains("keeps its own permissive rules"), "{text}");
    assert!(!text.contains("apt-get"), "{text}");
    let p = wifi::set_plan(&f.root(), &mut ex, "US", true).unwrap();
    assert!(
        p.render().contains("options aic8800_fdrv_usb custregd=0"),
        "{}",
        p.render()
    );
    p.execute(&f.root(), &mut ex, "W").unwrap();
    assert!(
        wifi::status(&f.root(), &mut ex)
            .unwrap()
            .aic8800_follows_country
    );
}

// ---- swap -------------------------------------------------------------------------------

/// The state after cloud-init 25.1's first boot: /swap.img in use, its fstab
/// line without comment=cloudconfig, and zram from armbian-zram-config.
fn with_swap(f: &Fake, used_kib: u64, available_kib: u64) {
    f.write(
        "/proc/meminfo",
        &format!("MemTotal:        3956800 kB\nMemFree:  100000 kB\nMemAvailable:    {available_kib} kB\n"),
    );
    f.write(
        "/proc/swaps",
        &format!("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0                              partition\t1978396\t\t0\t\t5\n/swap.img                               file\t\t1048572\t\t{used_kib}\t\t-2\n"),
    );
    f.sized("/swap.img", 1 << 30);
    f.write(
        "/etc/fstab",
        "UUID=db9a14c3 / ext4 defaults,commit=120,errors=remount-ro 0 1\ntmpfs /tmp tmpfs defaults,nosuid 0 0\n/swap.img\tnone\tswap\tsw\t0\t0\n",
    );
    f.write(
        "/lib/systemd/system/armbian-zram-config.service",
        "[Unit]\n",
    );
    f.write("/proc/sys/vm/swappiness", "60\n");
    f.mounts("/dev/mmcblk1p1 / ext4 rw,relatime 0 0\nproc /proc proc rw 0 0\n");
}

/// A 32 GiB root file system with 20 GiB free.
const SPACE: (u64, u64) = (32 << 30, 20 << 30);

#[test]
fn swap_show_resize_off() {
    let f = rock4d();
    with_swap(&f, 2048, 3_000_000);
    let st = swap::status(&f.root()).unwrap();
    assert_eq!(st.file, "/swap.img");
    assert_eq!(st.file_bytes, Some(1 << 30));
    assert_eq!(
        st.fstab_line.as_deref(),
        Some("/swap.img\tnone\tswap\tsw\t0\t0")
    );
    assert!(st.zram && st.zram_service);
    assert_eq!(st.swappiness, Some(60));
    assert_eq!(st.active.len(), 2);

    let p = swap::set_plan_with_space(&f.root(), "64M", false, SPACE).unwrap();
    let text = p.render();
    for want in [
        "$ swapoff /swap.img",
        "$ rm -f /swap.img",
        "$ install -m 0600 /dev/null /swap.img",
        "$ fallocate -l 64M /swap.img",
        "$ mkswap /swap.img",
        "$ swapon /swap.img",
        "/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0",
    ] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    assert!(p
        .cleanup
        .iter()
        .any(|c| c.step.description.contains("unfinished")));
    let mut ex = RecordingExecutor::default();
    p.execute(&f.root(), &mut ex, "S").unwrap();
    let fstab = f.read("/etc/fstab");
    assert_eq!(fstab.matches("/swap.img").count(), 1, "{fstab}");
    assert!(fstab.contains("sw,comment=cloudconfig"));
    assert!(fstab.starts_with("UUID=db9a14c3 / ext4"));
    assert!(f.p("/var/backups/hwtool/S/etc/fstab").exists());

    // A failed mkswap removes the unfinished file and the fstab line.
    let mut ex = RecordingExecutor::default();
    ex.failing.push("mkswap /swap.img".into());
    let p = swap::set_plan_with_space(&f.root(), "128M", false, SPACE).unwrap();
    assert!(p.execute(&f.root(), &mut ex, "T").is_err());
    assert!(ex.ran.iter().any(|r| r.join(" ") == "rm -f /swap.img"));
    assert!(!f.read("/etc/fstab").contains("/swap.img"));

    let f = rock4d();
    with_swap(&f, 2048, 3_000_000);
    let p = swap::off_plan(&f.root()).unwrap();
    let text = p.render();
    assert!(
        text.contains("$ swapoff /swap.img") && text.contains("$ rm -f /swap.img"),
        "{text}"
    );
    assert!(text.contains("zram swap"), "{text}");
    p.execute(&f.root(), &mut RecordingExecutor::default(), "O")
        .unwrap();
    assert!(!f.read("/etc/fstab").contains("swap"));
    assert!(f.read("/etc/fstab").contains("tmpfs /tmp"));
}

#[test]
fn swap_refusals() {
    let f = rock4d();
    // 900 MiB swapped out, 600 MiB of memory available: swapoff would not fit.
    with_swap(&f, 900 * 1024, 600 * 1024);
    let err = swap::set_plan_with_space(&f.root(), "2G", false, SPACE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot be switched off now"), "{err}");
    let err = swap::off_plan(&f.root()).unwrap_err().to_string();
    assert!(err.contains("cannot be switched off now"), "{err}");

    let f = rock4d();
    with_swap(&f, 0, 3_000_000);
    let err = swap::set_plan_with_space(&f.root(), "100T", true, SPACE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not fit"), "{err}");
    // More than twice the 3.8 GiB of memory.
    let err = swap::set_plan_with_space(&f.root(), "9G", false, SPACE)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("more than twice the memory")
            || err.contains("does not fit")
            || err.contains("quarter"),
        "{err}"
    );
    assert!(swap::set_plan_with_space(&f.root(), "8M", false, SPACE).is_err());
    f.mounts("/dev/mmcblk1p1 / btrfs rw 0 0\n");
    let err = swap::set_plan_with_space(&f.root(), "64M", false, SPACE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("btrfs"), "{err}");

    // swappiness
    assert!(swap::swappiness_plan(&f.root(), 201).is_err());
    let p = swap::swappiness_plan(&f.root(), 10).unwrap();
    p.execute(&f.root(), &mut RecordingExecutor::default(), "W")
        .unwrap();
    assert_eq!(f.read("/proc/sys/vm/swappiness"), "10");
    assert_eq!(swap::status(&f.root()).unwrap().swappiness_saved, Some(10));
}

#[test]
fn swap_new_file_without_fstab_line() {
    let f = rock4d();
    f.write(
        "/proc/meminfo",
        "MemTotal: 3956800 kB\nMemAvailable: 3000000 kB\n",
    );
    f.write("/proc/swaps", "Filename Type Size Used Priority\n");
    f.write("/etc/fstab", "UUID=x / ext4 defaults 0 1\n");
    let p = swap::set_plan_with_space(&f.root(), "64M", false, SPACE).unwrap();
    let text = p.render();
    assert!(
        !text.contains("swapoff") && !text.contains("Delete the old"),
        "{text}"
    );
    p.execute(&f.root(), &mut RecordingExecutor::default(), "N")
        .unwrap();
    assert_eq!(
        f.read("/etc/fstab"),
        "UUID=x / ext4 defaults 0 1\n/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0\n"
    );
    let p = swap::off_plan(&f.root()).unwrap();
    assert!(p.render().contains("Remove /swap.img from /etc/fstab"));
}
