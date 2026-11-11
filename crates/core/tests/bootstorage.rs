mod common;

use common::*;
use hwtool_core::bootloader::{self, Target};
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::migrate;
use hwtool_core::plan::Action;

fn shows(plan: &hwtool_core::plan::Plan) -> Vec<String> {
    plan.steps
        .iter()
        .filter_map(|s| match &s.action {
            Action::Shell { shows, .. } => Some(shows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn package_functions_are_detected() {
    let f = rock4d();
    let u = bootloader::detect(&f.root()).unwrap();
    assert_eq!(u.dir, "/usr/lib/linux-u-boot-edge-radxa-rock-4d");
    assert_eq!(
        u.package.as_deref(),
        Some("linux-u-boot-radxa-rock-4d-edge")
    );
    assert!(u.has("write_uboot_platform") && u.has("write_uboot_platform_mtd"));
    let u = bootloader::detect(&cm5().root()).unwrap();
    assert!(u.has("write_uboot_platform") && !u.has("write_uboot_platform_mtd"));
}

#[test]
fn booted_device_needs_explicit_flag() {
    let f = rock4d();
    let t = Target::Disk("/dev/mmcblk1".into());
    let err = bootloader::write_plan(&f.root(), &t, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("--allow-booted-device"), "{err}");
    let plan = bootloader::write_plan(&f.root(), &t, true).unwrap();
    assert_eq!(
        shows(&plan),
        vec!["dd if=/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin of=/dev/mmcblk1 bs=32k seek=1 conv=notrunc status=none"]
    );
    match &plan.steps[0].action {
        Action::Shell { script, .. } => {
            assert!(script.contains("source /usr/lib/u-boot/platform_install.sh"));
            assert!(script.contains("write_uboot_platform \"$DIR\" /dev/mmcblk1"));
        }
        a => panic!("{a:?}"),
    }
    // Executing runs exactly that script through bash.
    let mut ex = RecordingExecutor::default();
    plan.execute(&f.root(), &mut ex, "T").unwrap();
    assert_eq!(ex.ran.len(), 1);
    assert_eq!(ex.ran[0][0], "bash");
}

#[test]
fn refuses_to_overwrite_partitions() {
    let f = rock4d();
    // eMMC whose first partition starts at 1 MiB: u-boot-rockchip.bin
    // (9,561,600 bytes at 32 KiB) would run into it.
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[(2048, 61_000_000)]);
    let err = bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("past the start of the first partition"),
        "{err}"
    );
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[]);
    std::fs::remove_dir_all(f.p("/sys/block/mmcblk0/mmcblk0p1")).unwrap();
    std::fs::create_dir_all(f.p("/sys/block/mmcblk0")).unwrap();
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[(32768, 61_000_000)]);
    let plan =
        bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false).unwrap();
    assert!(shows(&plan)[0].contains("of=/dev/mmcblk0"));
}

#[test]
fn nvme_is_not_a_bootloader_target() {
    let f = rock4d();
    f.disk("nvme0n1", None, 500_118_192, &[]);
    let err = bootloader::write_plan(&f.root(), &Target::Disk("/dev/nvme0n1".into()), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("only to whole SD or eMMC"), "{err}");
}

#[test]
fn spi_uses_the_package_mtd_writer() {
    let f = rock4d();
    let err = bootloader::write_plan(&f.root(), &Target::Spi, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no SPI flash"), "{err}");
    f.write("/sys/class/mtd/mtd0/name", "spi5.0\n");
    f.write("/sys/class/mtd/mtd0/size", "16777216\n");
    let err = bootloader::write_plan(&f.root(), &Target::Spi, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("mtd-utils"), "{err}");
    f.programs(&["flashcp"]);
    let plan = bootloader::write_plan(&f.root(), &Target::Spi, false).unwrap();
    assert_eq!(
        shows(&plan),
        vec!["flashcp -v -p /usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip-spi.bin /dev/mtd0"]
    );
    assert!(!plan.warnings.is_empty());

    let f = cm5();
    f.write("/sys/class/mtd/mtd0/name", "spi\n");
    let err = bootloader::write_plan(&f.root(), &Target::Spi, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not built for SPI flash"), "{err}");
}

#[test]
fn status_reports_what_is_missing() {
    let f = cm5();
    let st = bootloader::status(&f.root());
    assert_eq!(st.root_device.as_deref(), Some("mmcblk1p1"));
    assert!(st.disks[0].holds_root);
    assert!(st
        .missing
        .iter()
        .any(|m| m.contains("write_uboot_platform_mtd")));
    std::fs::remove_file(f.p("/usr/lib/u-boot/platform_install.sh")).unwrap();
    let st = bootloader::status(&f.root());
    assert!(st.missing[0].contains("platform_install.sh is missing"));
    assert!(bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk1".into()), true).is_err());
}

fn migrate_ready(f: &Fake) -> RecordingExecutor {
    f.programs(&[
        "parted",
        "wipefs",
        "mkfs.ext4",
        "rsync",
        "blkid",
        "partprobe",
        "udevadm",
    ]);
    let mut ex = RecordingExecutor::default();
    ex.answer("df -B1 --output=used /", "     Used\n2000000000\n");
    ex.answer("lsblk -ndo PTTYPE /dev/mmcblk1", "gpt\n");
    ex
}

#[test]
fn migrate_to_emmc() {
    let f = rock4d();
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[(2048, 61_000_000)]);
    let mut ex = migrate_ready(&f);
    let plan = migrate::plan(&f.root(), &mut ex, "/dev/mmcblk0", false).unwrap();
    let text = plan.render();
    for want in [
        "$ wipefs -a /dev/mmcblk0",
        "$ parted -s /dev/mmcblk0 mklabel gpt",
        "$ parted -s -a optimal /dev/mmcblk0 mkpart primary ext4 16MiB 100%",
        "$ parted -s /dev/mmcblk0 set 1 legacy_boot on",
        "$ mkfs.ext4 -qF /dev/mmcblk0p1",
        "$ rsync -ax --delete --exclude-from=/run/hwtool/migrate-exclude.txt / /mnt/hwtool-target",
        "$ dd if=/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin of=/dev/mmcblk0 bs=32k seek=1 conv=notrunc status=none",
        "$ umount /mnt/hwtool-target",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert!(text.contains("Everything on /dev/mmcblk0 is erased"));
}

#[test]
fn migrate_to_nvme_and_point_at_new_root() {
    let f = rock4d();
    f.disk("nvme0n1", None, 500_118_192, &[]);
    let mut ex = migrate_ready(&f);
    let plan = migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false).unwrap();
    let text = plan.render();
    assert!(text.contains("mkpart primary ext4 1MiB 100%"), "{text}");
    assert!(
        !text.contains("dd if=/usr/lib"),
        "no bootloader write without --write-spi"
    );
    assert!(text.contains("No bootloader is written"));

    // Simulate the copy, then run only the step that repoints the root.
    let m = migrate::MOUNT;
    f.write(&format!("{m}/etc/fstab"), "UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908 / ext4 defaults,errors=remount-ro 0 1\ntmpfs /tmp tmpfs defaults,nosuid 0 0\n");
    f.write(&format!("{m}/boot/armbianEnv.txt"), ROCK4D_ENV);
    f.write(&format!("{m}/boot/boot.scr"), "");
    f.write(&format!("{m}/boot/vmlinuz-{EDGE_KERNEL}"), "");
    ex.answer("blkid -s UUID -o value /dev/nvme0n1p1", "1111-2222\n");
    ex.answer("blkid -U 1111-2222", "/dev/nvme0n1p1\n");
    let mut sub = hwtool_core::plan::Plan::new("t");
    for s in &plan.steps {
        if matches!(
            s.action,
            Action::PointAtNewRoot { .. } | Action::VerifyTarget { .. }
        ) {
            sub.steps.push(s.clone());
        }
    }
    sub.execute(&f.root(), &mut ex, "T").unwrap();
    assert!(f
        .read(&format!("{m}/etc/fstab"))
        .starts_with("UUID=1111-2222 / ext4 defaults,errors=remount-ro 0 1\ntmpfs"));
    assert!(f
        .read(&format!("{m}/boot/armbianEnv.txt"))
        .contains("rootdev=UUID=1111-2222\n"));
    // The running system's own files are untouched.
    assert_eq!(f.read("/boot/armbianEnv.txt"), ROCK4D_ENV);

    f.write("/sys/class/mtd/mtd0/name", "spi5.0\n");
    f.programs(&["flashcp"]);
    let plan = migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", true).unwrap();
    assert!(plan.render().contains("$ flashcp -v -p"));
}

#[test]
fn migrate_refusals() {
    let f = rock4d();
    let mut ex = migrate_ready(&f);
    let err = migrate::plan(&f.root(), &mut ex, "/dev/mmcblk1", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("holds the running system"), "{err}");
    f.disk("nvme0n1", None, 500_118_192, &[(2048, 1000)]);
    f.mounts("/dev/mmcblk1p1 / ext4 rw 0 0\n/dev/nvme0n1p1 /data ext4 rw 0 0\n");
    let err = migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("in use"), "{err}");
    f.mounts("/dev/mmcblk1p1 / ext4 rw 0 0\n");
    f.disk("sda", None, 1000, &[]);
    let err = migrate::plan(&f.root(), &mut ex, "/dev/sda", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("eMMC or NVMe only"), "{err}");
    // Too small.
    f.disk("mmcblk0", Some("MMC"), 2_000_000, &[]);
    let err = migrate::plan(&f.root(), &mut ex, "/dev/mmcblk0", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("bytes"), "{err}");
    // No U-Boot writer: refuse eMMC before anything is erased.
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[]);
    std::fs::remove_file(f.p("/usr/lib/u-boot/platform_install.sh")).unwrap();
    let err = migrate::plan(&f.root(), &mut ex, "/dev/mmcblk0", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("write_uboot_platform is missing"), "{err}");
}

#[test]
fn migrate_needs_tools() {
    let f = rock4d();
    f.disk("nvme0n1", None, 500_118_192, &[]);
    let mut ex = RecordingExecutor::default();
    let err = migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing programs: parted (parted)"), "{err}");
}

// ---- which disks the running system uses (M1) ----------------------------

fn holds_root(f: &Fake) -> Vec<String> {
    hwtool_core::storage::disks(&f.root())
        .into_iter()
        .filter(|d| d.holds_root)
        .map(|d| d.name)
        .collect()
}

#[test]
fn root_on_device_mapper_protects_the_disks_below() {
    let f = rock4d();
    f.disk(
        "nvme0n1",
        None,
        500_118_192,
        &[(2048, 1000), (4096, 400_000_000)],
    );
    f.dm("dm-0", &["nvme0n1p2"]);
    f.mounts("/dev/mapper/root / ext4\n");
    // The kernel shows /dev/mapper/root in mountinfo; the number is dm-0's.
    let mi = f
        .read("/proc/self/mountinfo")
        .replace(&majmin("root"), &majmin("dm-0"));
    f.write("/proc/self/mountinfo", &mi);
    assert_eq!(holds_root(&f), vec!["nvme0n1"]);
    let sys = hwtool_core::storage::system(&f.root()).unwrap();
    assert_eq!(sys.root_device, "dm-0");
}

#[test]
fn ubootpart_and_boot_mount_are_protected_too() {
    let f = rock4d();
    f.disk("nvme0n1", None, 500_118_192, &[(2048, 400_000_000)]);
    f.mounts("/dev/nvme0n1p1 / ext4\n");
    // U-Boot loaded boot.scr from the SD card.
    f.write("/proc/cmdline", "root=UUID=x ubootpart=1234-01 rootwait\n");
    f.link("/dev/disk/by-partuuid/1234-01", "../../mmcblk1p1");
    assert_eq!(holds_root(&f), vec!["mmcblk1", "nvme0n1"]);
    let err = bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk1".into()), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("--allow-booted-device"), "{err}");
    // A separate /boot counts as well.
    f.write("/proc/cmdline", "root=UUID=x rootwait\n");
    f.mounts("/dev/nvme0n1p1 / ext4\n/dev/mmcblk1p1 /boot ext4\n");
    assert_eq!(holds_root(&f), vec!["mmcblk1", "nvme0n1"]);
}

#[test]
fn unknown_root_fails_closed() {
    let f = rock4d();
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[(32768, 61_000_000)]);
    f.disk("nvme0n1", None, 500_118_192, &[]);
    // / from a device sysfs does not know (or no mountinfo at all).
    f.mounts("/dev/root / ext4\n");
    assert!(holds_root(&f).is_empty());
    let err =
        bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false).unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot tell which disk"),
        "{err:#}"
    );
    let mut ex = migrate_ready(&f);
    let err = migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false).unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot find the block device"),
        "{err:#}"
    );
}

// ---- the U-Boot package's writer (M4, M9) --------------------------------

fn with_writer(body: &str) -> Fake {
    let f = rock4d();
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[(32768, 61_000_000)]);
    f.write(
        "/usr/lib/u-boot/platform_install.sh",
        &format!("DIR=/usr/lib/linux-u-boot-edge-radxa-rock-4d\nwrite_uboot_platform ()\n{{\n{body}\n}}\n"),
    );
    f
}

fn emmc_err(f: &Fake) -> String {
    format!(
        "{:#}",
        bootloader::write_plan(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false).unwrap_err()
    )
}

#[test]
fn writer_lines_must_parse_completely() {
    // Generic rockchip64 layout: idbloader at sector 64, u-boot.itb at 8 MiB.
    let f = with_writer(
        "    dd if=$1/idbloader.img of=$2 seek=64 conv=notrunc status=none\n    dd if=$1/u-boot.itb of=$2 seek=16384 conv=notrunc status=none",
    );
    let d = "/usr/lib/linux-u-boot-edge-radxa-rock-4d";
    f.sized(&format!("{d}/idbloader.img"), 200_000);
    f.sized(&format!("{d}/u-boot.itb"), 1_000_000);
    let p =
        bootloader::write_plan_with(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false, None)
            .unwrap();
    let w: Vec<(u64, u64)> = p.writes.iter().map(|w| (w.offset, w.len)).collect();
    assert_eq!(w, vec![(32768, 200_000), (8 << 20, 1_000_000)]);
    // oflag=seek_bytes, count and a size suffix.
    let f = with_writer("    dd if=$1/u-boot-rockchip.bin of=$2 bs=1M count=2 seek=32768 oflag=seek_bytes conv=notrunc,fsync");
    let p =
        bootloader::write_plan_with(&f.root(), &Target::Disk("/dev/mmcblk0".into()), false, None)
            .unwrap();
    assert_eq!((p.writes[0].offset, p.writes[0].len), (32768, 2 << 20));
    // Past the first partition (16 MiB): refused.
    let f = with_writer("    dd if=$1/u-boot-rockchip.bin of=$2 bs=1M seek=10");
    assert!(emmc_err(&f).contains("past the start of the first partition"));
    // Things hwtool cannot fully read: refused.
    for bad in [
        "    dd if=$1/u-boot-rockchip.bin of=$2 bs=2x512",
        "    dd if=$1/u-boot-rockchip.bin of=$2 conv=excl",
        "    dd if=$1/u-boot-rockchip.bin of=$2 oflag=append",
        "    dd if=$1/u-boot-rockchip.bin of=$2 iflag=nonblock",
        "    dd $1/u-boot-rockchip.bin $2",
    ] {
        let f = with_writer(bad);
        assert!(
            emmc_err(&f).contains("is not supported") || emmc_err(&f).contains("cannot read"),
            "{bad}: {}",
            emmc_err(&f)
        );
    }
}

#[test]
fn writer_must_write_only_the_requested_device() {
    let f = with_writer("    dd if=$1/u-boot-rockchip.bin of=/dev/mmcblk1 bs=32k seek=1");
    assert!(
        emmc_err(&f).contains("writes to /dev/mmcblk1, not to /dev/mmcblk0"),
        "{}",
        emmc_err(&f)
    );
}

#[test]
fn writer_side_effects_are_refused_and_never_happen() {
    let marker = "/tmp/hwtool-should-not-exist";
    let host_marker = std::path::Path::new(marker);
    let _ = std::fs::remove_file(host_marker);
    for (body, want) in [
        ("    blkdiscard $2", "runs blkdiscard"),
        ("    command blkdiscard $2", "runs blkdiscard"),
        (&*format!("    /usr/bin/touch {marker}"), "cannot show"),
        (&*format!("    echo x > {marker}"), "cannot show"),
        (
            &*format!("    rm -f {marker}; dd if=$1/u-boot-rockchip.bin of=$2 bs=32k seek=1"),
            "runs rm",
        ),
        ("    exec dd if=$1/u-boot-rockchip.bin of=$2", "cannot show"),
        (
            &*format!("    PATH=/usr/bin; touch {marker}"),
            "cannot show",
        ),
    ] {
        let f = with_writer(body);
        let err = emmc_err(&f);
        assert!(err.contains(want), "{body}: {err}");
        assert!(!host_marker.exists(), "{body} wrote a file");
    }
}

#[test]
fn spi_plan_shows_the_device_the_writer_uses() {
    let f = rock4d();
    f.write("/sys/class/mtd/mtd0/name", "spi5.0\n");
    f.write("/sys/class/mtd/mtd0/size", "16777216\n");
    f.programs(&["flashcp"]);
    let p = bootloader::write_plan(&f.root(), &Target::Spi, false).unwrap();
    assert!(p.title.contains("SPI flash /dev/mtd0"), "{}", p.title);
    // A SPI image larger than the flash is refused.
    f.write("/sys/class/mtd/mtd0/size", "1048576\n");
    let err = format!(
        "{:#}",
        bootloader::write_plan(&f.root(), &Target::Spi, false).unwrap_err()
    );
    assert!(err.contains("more than the 1048576 bytes"), "{err}");
}

// ---- migration order and checks (M6, L1, L2) -----------------------------

#[test]
fn migration_verifies_before_the_bootloader_and_zeroes_it_on_failure() {
    let f = rock4d();
    f.disk("mmcblk0", Some("MMC"), 61_071_360, &[]);
    let mut ex = migrate_ready(&f);
    let p = migrate::plan(&f.root(), &mut ex, "/dev/mmcblk0", false).unwrap();
    let pos =
        |pred: &dyn Fn(&Action) -> bool| p.steps.iter().position(|s| pred(&s.action)).unwrap();
    let verify = pos(&|a| matches!(a, Action::VerifyTarget { .. }));
    let uboot = pos(
        &|a| matches!(a, Action::Shell { shows, .. } if shows.iter().any(|s| s.contains("u-boot-rockchip.bin"))),
    );
    assert!(verify < uboot, "verify {verify} uboot {uboot}");
    let zero = p
        .cleanup
        .iter()
        .find(|c| c.step.description.starts_with("Zero the U-Boot area"))
        .unwrap();
    assert_eq!((zero.from, zero.to), (uboot, uboot));
    let text = p.render();
    assert!(
        text.contains(
            "dd if=/dev/zero of=/dev/mmcblk0 bs=512 seek=64 count=18675 conv=notrunc,fsync"
        ),
        "{text}"
    );

    // The bootloader write fails: the U-Boot area is zeroed and the target unmounted.
    ex.answer("blkid -s UUID -o value /dev/mmcblk0p1", "1111\n");
    ex.answer("blkid -U 1111", "/dev/mmcblk0p1\n");
    let m = migrate::MOUNT;
    f.write(&format!("{m}/etc/fstab"), "UUID=x / ext4 defaults 0 1\n");
    f.write(&format!("{m}/boot/armbianEnv.txt"), ROCK4D_ENV);
    f.write(&format!("{m}/boot/boot.scr"), "");
    f.write(&format!("{m}/boot/vmlinuz-{EDGE_KERNEL}"), "");
    ex.failing
        .push("bash -c set -e -o pipefail\nsource /usr/lib/u-boot/platform_install.sh".into());
    ex.ran.clear();
    let err = format!("{:#}", p.execute(&f.root(), &mut ex, "M").unwrap_err());
    assert!(err.contains("done: Zero the U-Boot area"), "{err}");
    assert!(err.contains("done: Unmount the target"), "{err}");
}

#[test]
fn migration_needs_ext4_and_a_known_size() {
    let f = rock4d();
    f.disk("nvme0n1", None, 500_118_192, &[]);
    f.mounts("/dev/mmcblk1p1 / btrfs\n");
    let mut ex = migrate_ready(&f);
    let err = format!(
        "{:#}",
        migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false).unwrap_err()
    );
    assert!(err.contains("not ext4"), "{err}");
    f.mounts("/dev/mmcblk1p1 / ext4\n");
    ex.answers.remove("df -B1 --output=used /");
    let err = format!(
        "{:#}",
        migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false).unwrap_err()
    );
    assert!(err.contains("cannot tell how much of / is used"), "{err}");
    ex.answer("df -B1 --output=used /", "Used\n0\n");
    assert!(migrate::plan(&f.root(), &mut ex, "/dev/nvme0n1", false).is_err());
}
