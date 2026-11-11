//! Moving the running system from SD to eMMC or NVMe.
//!
//! In armbian/build this is `armbian-install`, which is now a shim for
//! `armbian-config --api module_partitioner`. Roxora does not ship
//! armbian-config, so hwtool runs the same steps itself. They follow
//! `install_run_scenario` in configng's
//! tools/modules/functions/module_install_engine.sh (armbian/configng
//! 79e02f1cc26fafdf848b7bdb5cc0a05a865c31d1), for an ext4 root:
//!
//! | configng                      | here                                              |
//! |-------------------------------|---------------------------------------------------|
//! | pre-flight checks             | [`plan`] refuses before anything is written      |
//! | `install_plan_layout`         | one ext4 partition from 16 MiB (eMMC) or 1 MiB    |
//! | `install_apply_partitions`    | wipefs, zero (keeping DVKR/SSKR), parted          |
//! | `install_make_filesystems`    | `mkfs.ext4 -qF`                                   |
//! | `install_transfer_rootfs`     | `rsync -ax --delete --exclude-from`, twice        |
//! | `install_populate_boot`       | `rsync -aq --exclude 'efi/**' /boot/`             |
//! | `install_gen_fstab`           | the `/` line of the copied fstab gets the new UUID|
//! | `install_rewrite_bootenv`     | `rootdev=UUID=...`, `rootfstype=ext4`             |
//! | `install_write_bootloader`    | the U-Boot package's `write_uboot_platform(_mtd)` |
//! | `install_verify_boot_dir/fstab` | [`Action::VerifyTarget`]                        |
//!
//! Differences: only ext4 (the file system of Roxora images); fstab is
//! edited, not regenerated, so the image's own mount options stay; the
//! "split" and UEFI modes are not offered.

use anyhow::{bail, Context, Result};

use crate::bootloader::{self, Target};
use crate::exec::{argv, Executor};
use crate::plan::{Action, Plan, Step};
use crate::root::Root;
use crate::storage::{self, part_dev};
use crate::ENV_FILE;

/// Where the new root is mounted while it is filled.
pub const MOUNT: &str = "/mnt/hwtool-target";
/// Copied from packages/bsp/common/usr/lib/armbian-install/exclude.txt.
pub const EXCLUDE_FILE: &str = "/run/hwtool/migrate-exclude.txt";
pub const EXCLUDE: &str = "/boot/*\n/dev/*\n/proc/*\n/sys/*\n/media/*\n/mnt/*\n/run/*\n/tmp/*\n/ddbr/*\n/var/log/journal\n";

const TOOLS: &[(&str, &str)] = &[
    ("parted", "parted"),
    ("wipefs", "util-linux"),
    ("mkfs.ext4", "e2fsprogs"),
    ("rsync", "rsync"),
    ("blkid", "util-linux"),
    ("partprobe", "parted"),
    ("udevadm", "udev"),
];

pub fn plan(root: &Root, exec: &mut dyn Executor, target: &str, write_spi: bool) -> Result<Plan> {
    let disk = storage::find_disk(root, target)
        .with_context(|| format!("{target} is not a disk on this system"))?;
    if disk.holds_root {
        bail!("{target} holds the running system; choose another disk to move it to");
    }
    if !disk.mounted.is_empty() {
        bail!(
            "{target} is in use ({} mounted or used as swap); unmount it first",
            disk.mounted.join(", ")
        );
    }
    let mode = match disk.kind.as_str() {
        "emmc" => "emmc",
        "nvme" => "nvme",
        k => bail!("{target} is a {k} device; hwtool moves the system to eMMC or NVMe only"),
    };
    let missing: Vec<String> = TOOLS
        .iter()
        .filter(|(prog, _)| !root.has_program(prog))
        .map(|(prog, pkg)| format!("{prog} ({pkg})"))
        .collect();
    if !missing.is_empty() {
        bail!("missing programs: {}", missing.join(", "));
    }
    if !root.exists(ENV_FILE) || !root.exists("/boot/boot.scr") {
        bail!(
            "this system does not boot with /boot/boot.scr and {ENV_FILE}; hwtool cannot move it"
        );
    }
    let uboot = bootloader::detect(root);
    if mode == "emmc"
        && !uboot
            .as_ref()
            .is_some_and(|u| u.has("write_uboot_platform"))
    {
        bail!(
            "the U-Boot package's write_uboot_platform is missing ({}), so the eMMC could not be made \
             bootable; nothing was changed",
            bootloader::PLATFORM_INSTALL
        );
    }
    // Plan the bootloader write now, so an impossible write stops us before the disk is wiped.
    let boot_write = match mode {
        "emmc" => Some(bootloader::write_plan_with(
            root,
            &Target::Disk(disk.path.clone()),
            false,
            Some(16 << 20),
        )),
        _ if write_spi => Some(bootloader::write_plan(root, &Target::Spi, false)),
        _ => None,
    }
    .transpose()?;

    let root_part = storage::root_device(root).context("cannot find the device of /")?;
    let root_disk = storage::disks(root)
        .into_iter()
        .find(|d| d.holds_root)
        .context("cannot find the disk that holds /")?;
    let used: u64 = exec
        .capture(&argv(&["df", "-B1", "--output=used", "/"]))?
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .next()
        .unwrap_or(0);
    if used > 0 && disk.size_bytes < used + 256 * 1024 * 1024 {
        bail!(
            "{target} has {} bytes; the running system uses {used} bytes plus room for the copy",
            disk.size_bytes
        );
    }
    // Same partition table as the disk the board boots from
    // (install_source_table_type): Rockchip U-Boot reads GPT.
    let table = match exec
        .capture(&argv(&["lsblk", "-ndo", "PTTYPE", &root_disk.path]))
        .unwrap_or_default()
        .trim()
    {
        "dos" => "msdos",
        _ => "gpt",
    };
    let start = if mode == "emmc" { 16 } else { 1 };
    let part = part_dev(&disk.path, 1);
    let t = disk.path.as_str();

    let mut p = Plan::new(format!(
        "Move the running system from /dev/{root_part} to {t} ({mode}); everything on {t} is erased"
    ));
    p.step(
        "Write the rsync exclude list",
        Action::WriteFile {
            path: EXCLUDE_FILE.into(),
            content: EXCLUDE.into(),
        },
    );
    p.run(
        format!("Erase the file system signatures on {t}"),
        &["wipefs", "-a", t],
    );
    p.step(
        format!("Zero the start of {t}, keeping the Rockchip vendor storage (sectors 7168-16383) if it is there"),
        Action::Shell {
            script: format!(
                "set -e\nd={t}\nkeep=no\n\
                 [ \"$(dd if=$d bs=1 skip=$((7168*512)) count=4 2>/dev/null | tr -d '\\0')\" = DVKR ] && keep=yes\n\
                 [ \"$(dd if=$d bs=1 skip=$((8192*512)) count=4 2>/dev/null | tr -d '\\0')\" = SSKR ] && keep=yes\n\
                 if [ $keep = yes ]; then\n  dd if=/dev/zero of=$d bs=512 count=7168 conv=notrunc\n  \
                 dd if=/dev/zero of=$d bs=512 seek=16384 count=4096 conv=notrunc\nelse\n  \
                 dd if=/dev/zero of=$d bs=1M count=10 conv=notrunc\nfi\n"
            ),
            shows: vec![
                format!("if DVKR at sector 7168 or SSKR at sector 8192: dd if=/dev/zero of={t} bs=512 count=7168 conv=notrunc; dd if=/dev/zero of={t} bs=512 seek=16384 count=4096 conv=notrunc"),
                format!("otherwise: dd if=/dev/zero of={t} bs=1M count=10 conv=notrunc"),
            ],
        },
    );
    p.run(
        format!("Create a {table} partition table"),
        &["parted", "-s", t, "mklabel", table],
    );
    let start_s = format!("{start}MiB");
    p.run(
        format!("Create one ext4 partition from {start} MiB to the end"),
        &[
            "parted", "-s", "-a", "optimal", t, "mkpart", "primary", "ext4", &start_s, "100%",
        ],
    );
    let flag = if table == "gpt" {
        "legacy_boot"
    } else {
        "boot"
    };
    p.run(
        "Mark it bootable",
        &["parted", "-s", t, "set", "1", flag, "on"],
    );
    p.run("Reread the partition table", &["partprobe", t]);
    p.run("Wait for the partition device", &["udevadm", "settle"]);
    p.run(
        format!("Create the ext4 file system on {part}"),
        &["mkfs.ext4", "-qF", &part],
    );
    p.run("Create the mount point", &["mkdir", "-p", MOUNT]);
    p.run(format!("Mount {part} on {MOUNT}"), &["mount", &part, MOUNT]);
    let ex = format!("--exclude-from={EXCLUDE_FILE}");
    p.run(
        "Copy the running system",
        &["rsync", "-ax", "--delete", &ex, "/", MOUNT],
    );
    p.run(
        "Copy again to catch files that changed during the first copy",
        &["rsync", "-ax", "--delete", &ex, "/", MOUNT],
    );
    let mboot = format!("{MOUNT}/boot/");
    p.run(
        "Copy /boot",
        &["rsync", "-aq", "--exclude", "efi/**", "/boot/", &mboot],
    );
    p.step(
        format!("Point the copy's fstab and armbianEnv.txt at {part}"),
        Action::PointAtNewRoot {
            partition: part.clone(),
            mount: MOUNT.into(),
        },
    );
    if let Some(bw) = &boot_write {
        for s in &bw.steps {
            p.steps.push(s.clone());
        }
        p.warnings.extend(bw.warnings.iter().cloned());
    }
    p.step(
        "Check the copy can boot",
        Action::VerifyTarget {
            mount: MOUNT.into(),
        },
    );
    p.run("Flush the writes", &["sync"]);
    p.run(format!("Unmount {MOUNT}"), &["umount", MOUNT]);
    p.cleanup.push(Step {
        description: "Unmount the target".into(),
        action: Action::Run {
            argv: argv(&["umount", "-R", MOUNT]),
        },
    });

    p.warnings.push(format!("Everything on {t} is erased."));
    match mode {
        "emmc" => p.notices.push(
            "Power off, remove the SD card and start the board: it boots from the eMMC. \
             The SD card is not changed and still boots the old copy."
                .to_string(),
        ),
        _ => p.notices.push(if write_spi {
            "U-Boot is written to SPI flash. Power off, remove the SD card and start the board: \
             U-Boot in SPI flash looks for /boot on the NVMe drive."
                .to_string()
        } else {
            "No bootloader is written. The board needs a U-Boot that looks for /boot on NVMe, \
             in SPI flash ('hwtool boot write-bootloader --target spi', or add --write-spi) or on \
             the SD card or eMMC it boots from now. While the SD card is inserted and its U-Boot \
             boots first, the SD copy may still be the one that starts."
                .to_string()
        }),
    }
    p.reboot_required = true;
    Ok(p)
}
