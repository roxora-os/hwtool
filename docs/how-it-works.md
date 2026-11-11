# How hwtool works

hwtool does not carry its own knowledge of offsets, file names or boot
scripts. Each function wraps a mechanism that the board's Armbian packages
already ship. The references below are to
[armbian/build](https://github.com/armbian/build) at the commit Roxora pins
(`armbian.ref` in roxora-os/build) and to
[armbian/configng](https://github.com/armbian/configng).

Everything hwtool reads is resolved against a root directory, so the tests
run the same code against a fake root filesystem. Every change is first built
as a plan: a list of steps, each a file write or a command. `--dry-run` prints
the plan; otherwise hwtool prints it, asks, and runs it.

## Which board

hwtool reads `BOARD`, `BOARDFAMILY`, `BOOT_SOC` and `KERNEL_TARGET` from
`/etc/armbian-release`, which the board support package installs. If that
file is missing it recognises the board from the installed
`linux-u-boot-<board>-<branch>` package, for the boards in Roxora's
`boards.conf`.

## Device tree overlays

`boot.scr` (`config/bootscripts/boot-rk3576.cmd`) imports
`/boot/armbianEnv.txt` and then:

- for each name in `overlays=`, tries
  `/boot/dtb/rockchip/overlay/<prefix>-<name>.dtbo` for each prefix in
  `overlay_prefix=` (the first match wins), then `<name>.dtbo`;
- for each name in `user_overlays=`, loads `/boot/overlay-user/<name>.dtbo`.

`hwtool overlays list` lists the `.dtbo` files these rules can find.
`enable` and `disable` change only the `overlays=` or `user_overlays=` line;
other lines, comments and their order stay as they are. A user overlay is
compiled and copied to `/boot/overlay-user` with `armbian-add-overlay`, which
the board support package ships; hwtool then enables and disables it like any
other.

The prefixes depend on the kernel branch. Mainline kernels name RK3576
overlays `rockchip-rk3576-*.dtbo`, vendor kernels `rk3576-*.dtbo`
(`rockchip_soc_overlay_prefixes` in
`config/sources/families/include/rockchip64_common.inc`). A board can add its
own prefix in front (`OVERLAY_PREFIX_BOARD`, for example `armsom-cm5-io`).

The `linux-dtb-edge-rockchip64` package for kernel 7.3.4 has no overlays with
an RK3576 prefix, so on edge both supported boards list none.

## Kernel branch

A branch is three packages from the Armbian repository
(`lib/functions/compilation/kernel-debs.sh` and `uboot.sh`):

| Package | Example |
| --- | --- |
| `linux-image-<branch>-<family>` | `linux-image-edge-rockchip64` |
| `linux-dtb-<branch>-<family>` | `linux-dtb-vendor-rk35xx` |
| `linux-u-boot-<board>-<branch>` | `linux-u-boot-radxa-rock-4d-edge` |

plus `linux-headers-<branch>-<family>` where headers are installed. For
RK3576 boards the family is `rockchip64` on `edge` and `current` and `rk35xx`
on `vendor`.

Kernel and device tree packages of different branches can be installed side
by side; the one installed last points `/boot/Image` and `/boot/dtb` at its
files. U-Boot packages all conflict on `armbian-u-boot`, so apt replaces the
old one.

`hwtool kernel switch <branch>`:

1. checks with `apt-cache policy` that every package of the new branch can be
   installed (Roxora pins the Armbian repository so that only these package
   patterns come from it), and stops if one cannot;
2. backs up `/boot/armbianEnv.txt`;
3. saves the packages it is about to replace as `.deb` files in
   `/var/lib/hwtool/packages/<time>/` (see below);
4. runs `apt-get update` and `apt-get install` for the new packages;
5. checks that `/boot/Image` and `/boot/dtb` point at the new branch and that
   the device tree U-Boot loads exists (`fdtfile=` in `armbianEnv.txt`, or
   the U-Boot build's `CONFIG_DEFAULT_FDT_FILE` when the file does not set
   one); if not, it reinstalls the old packages and stops;
6. sets `overlay_prefix=` for the new branch;
7. removes the old kernel, device tree and header packages (`--keep-old`
   keeps them);
8. records the old branch, package versions and saved files in
   `/var/lib/hwtool/kernel-history.json`, and removes the packages saved by
   earlier switches.

`hwtool kernel revert` switches back to the recorded branch. For each package
it asks apt for the recorded version; when the repository no longer offers
it, it installs the saved `.deb` instead. This matters where the repository
never published a package: on 2026-11-11 apt.armbian.com had no
`linux-u-boot-armsom-cm5-io-edge`, so the edge U-Boot of a CM5-IO image
exists only on the board.

### Saving the replaced packages

The switch saves the old U-Boot package (apt removes it, because U-Boot
packages conflict) and the old kernel, device tree and header packages
(unless `--keep-old`). It does this without new dependencies:

- If apt still has the original file in `/var/cache/apt/archives`, hwtool
  copies it. That is the package exactly as it was installed.
- Otherwise hwtool rebuilds the package from the dpkg database, the way
  `dpkg-repack` does: the control fields from `/var/lib/dpkg/status` without
  the install state, the maintainer scripts, conffiles list and md5sums from
  `/var/lib/dpkg/info/`, and the files listed in
  `/var/lib/dpkg/info/<package>.list`, packed with `dpkg-deb`. The files are
  owned by root in the rebuilt package, as they are in these packages.
  Images are built without an apt cache, so this is the usual case.

`--dry-run` shows for each package whether it is copied or rebuilt and how
much space the files take.

The switch installs the new U-Boot package but does not write it to the boot
device; `hwtool boot write-bootloader` does that.

## Writing the bootloader

The U-Boot package installs its binaries in
`/usr/lib/linux-u-boot-<branch>-<board>/` and a shell file,
`/usr/lib/u-boot/platform_install.sh`, that sets `DIR` to that directory and
defines the functions that write them (`uboot.sh`, the part that writes
`platform_install.sh`):

- `write_uboot_platform <dir> <device>` for SD and eMMC;
- `write_uboot_platform_mtd <dir> <mtd device> <log> <mtd list> [image]` for
  SPI flash, on boards whose U-Boot is built for it.

hwtool sources that file and calls these functions. For `--dry-run` and for
the confirmation it first runs them with `dd` and `flashcp` replaced by
functions that only print their arguments, so the plan shows the exact
commands, for example on a Radxa ROCK 4D:

    dd if=/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin of=/dev/mmcblk0 bs=32k seek=1 conv=notrunc status=none
    flashcp -v -p /usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip-spi.bin /dev/mtd0

Before writing to a disk hwtool checks that the target is a whole SD or eMMC
device (the boot ROM does not load U-Boot from NVMe, USB or SATA), that the
write ends before the first partition, and that the target is not the device
the system is running from unless `--allow-booted-device` is given. For SPI
flash it checks that the kernel shows an MTD device and that `flashcp`
(package `mtd-utils`) is installed.

The ArmSoM CM5-IO U-Boot package has no SPI writer, so `--target spi` is not
available there.

## Moving the system to eMMC or NVMe

In armbian/build, `armbian-install` is now a shim that runs
`armbian-config --api module_partitioner`. Roxora images do not ship
armbian-config, so hwtool runs the same steps itself. They follow
`install_run_scenario` in configng's
`tools/modules/functions/module_install_engine.sh`, for an ext4 root:

| configng | hwtool |
| --- | --- |
| pre-flight checks | refuses before anything is written: target is the running disk, mounted, not eMMC or NVMe, too small, a tool or the U-Boot writer is missing |
| `install_plan_layout` | one ext4 partition, starting at 16 MiB on eMMC (room for U-Boot) and 1 MiB on NVMe, same partition table type as the boot disk |
| `install_apply_partitions` | `wipefs`, zero the start of the disk (keeping the Rockchip vendor storage at sectors 7168 to 16383 if it is there), `parted mklabel`, `mkpart`, `legacy_boot` flag |
| `install_make_filesystems` | `mkfs.ext4 -qF` |
| `install_transfer_rootfs` | `rsync -ax --delete` of `/` with Armbian's exclude list, twice |
| `install_populate_boot` | `rsync -aq --exclude 'efi/**' /boot/` |
| `install_gen_fstab` | the `/` line of the copied `/etc/fstab` gets the new UUID; other lines stay |
| `install_rewrite_bootenv` | `rootdev=UUID=...` and `rootfstype=ext4` in the copy's `armbianEnv.txt` |
| `install_write_bootloader` | eMMC: `write_uboot_platform`; NVMe with `--write-spi`: `write_uboot_platform_mtd` |
| `install_verify_boot_dir`, `install_verify_fstab` | the copy's `/boot` has a kernel and `boot.scr`; every UUID in its fstab exists |

The running system and the SD card are not changed. If a step fails, hwtool
unmounts the target and stops.

## The GTK app

`hwtool-gtk` runs as the user. It reads state with `hwtool --json`, shows the
output of `hwtool ... --dry-run` in a confirmation dialog, and runs the
change with `pkexec /usr/bin/hwtool ... --yes`. The polkit action
`io.github.roxora_os.hwtool.run` asks for an administrator password.
