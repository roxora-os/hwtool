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
the plan and changes nothing; otherwise hwtool prints it, asks, and runs it.

## How a change runs

- **Log.** Each change writes `/var/log/hwtool/<time>.log` with every step
  and the output of every program it runs.
- **Output.** Programs run in their own process group, and their output goes
  through hwtool to the terminal and the log. When the terminal or the GTK
  app goes away, hwtool's writes fail quietly; the programs never see a
  broken pipe, so a `dd`, `flashcp` or `dpkg` is never cut off by a closed
  window.
- **Signals.** Ctrl-C, SIGHUP and SIGTERM reach hwtool only. It lets the
  running step finish, then stops and runs the cleanup.
- **Cleanup.** A plan can carry cleanup steps for failures in a given range
  of steps (for a kernel switch: from the install on). They all run, even if
  one fails, and hwtool reports each result.
- **Files.** A boot configuration file is copied to
  `/var/backups/hwtool/<time>/` before it changes. Files are replaced by
  writing a new temporary file (never an existing file or a link), flushing
  it to disk, renaming it over the old one and flushing the directory.
- **Same plan.** `--dry-run --json` gives the plan's fingerprint (time stamps
  left out); `--expect-plan <fingerprint>` makes a run refuse if its own plan
  differs. The GTK app confirms a dry run made as the user and passes its
  fingerprint to the run as root.
- **Which disks are in use.** hwtool traces `/` from `/proc/self/mountinfo`
  through its device number in `/sys/dev/block` (and through device-mapper
  slaves) to the disk, and adds the disk of a separate `/boot` and the
  partition U-Boot loaded the boot script from (`ubootpart=` on the kernel
  command line). If `/` cannot be traced, commands that write to disks
  refuse.
- **Test options.** `--root <dir>` (tests) is refused when hwtool runs with
  root rights (euid 0, sudo or pkexec).

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
other lines, comments and their order stay as they are.

`hwtool overlays add FILE.dts|FILE.dtso` does what `armbian-add-overlay`
does, without the C preprocessor: it compiles the file with `dtc -@`
(installing `device-tree-compiler` from Ubuntu first if `dtc` is missing),
checks the `compatible` at the overlay's root against the board's
(`/proc/device-tree/compatible`), installs it as
`/boot/overlay-user/<name>.dtbo` and adds it to `user_overlays=`. Files with
`#include` need the kernel headers and are left to `armbian-add-overlay`.
The plan names the file with a checksum of its content, so a file changed
after the confirmation gives a different plan. `overlays remove NAME`
disables a user overlay and deletes its file after a backup; kernel overlays
can only be disabled.

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
2. asks apt what it would do (`apt-get -s install` and `apt-get -s remove`),
   shows that in the plan, and stops if apt would remove anything other than
   the old U-Boot package and the old kernel packages;
3. backs up `/boot/armbianEnv.txt`;
4. saves the packages it is about to replace as `.deb` files in
   `/var/lib/hwtool/packages/<time>/` (see below);
5. runs `apt-get update` and records the switch as pending in
   `/var/lib/hwtool/kernel-history.json`;
6. runs `apt-get install` for the new packages;
7. checks that `/boot/Image` and `/boot/dtb` point at the new branch and that
   the device tree U-Boot loads exists (`fdtfile=` in `armbianEnv.txt`, or
   the U-Boot build's `CONFIG_DEFAULT_FDT_FILE` when the file does not set
   one);
8. sets `overlay_prefix=` for the new branch;
9. removes the old kernel, device tree and header packages (`--keep-old`
   keeps them);
10. records the switch as done, and removes the packages saved by earlier
    switches.

If step 6, 7, 8 or 9 fails, or hwtool is interrupted there, it puts
`armbianEnv.txt` back from this run's backup, purges the new branch's
packages, reinstalls the old ones (from the saved files when they exist) and
prints where `/boot/Image`, `/boot/dtb` and `/boot/uInitrd` point. The
history entry stays pending; `hwtool kernel status` and `revert` say so.

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

The package is written to a temporary file, checked with `dpkg-deb`, and
only then given its name; a damaged saved file is not used by a revert.
hwtool needs about twice the packages' installed size free before it starts.
`--dry-run` shows for each package whether it is copied or rebuilt and how
much space the files take.

The switch installs the new U-Boot package but does not write it to the boot
device; `hwtool boot write-bootloader` does that.

### Headers and holding

`hwtool kernel headers install|remove` installs or removes
`linux-headers-<branch>-<family>` for the branch in use; Roxora's pinning
allows headers from the Armbian repository. The plan shows `apt-get -s` and
stops on removals other than the headers. DKMS (the ROCK 4D's `aic8800`
Wi-Fi and Bluetooth driver) builds its modules against the headers for each
new kernel, so removing them warns that DKMS modules will be missing from
the next kernel.

`hwtool kernel hold|unhold` runs `apt-mark hold|unhold` on the board's
installed kernel, device tree, header, U-Boot and `armbian-firmware*`
packages. dpkg records a hold as `hold ok installed`; `kernel status` and
the Kernel page list held packages. `kernel switch` refuses while they are
held, rather than releasing them itself, so a hold is only undone on
purpose.

## Boot options

`/boot/boot.cmd` (`config/bootscripts/boot-rk3576.cmd`) imports
`armbianEnv.txt` and builds the kernel command line:

    root=${rootdev} rootwait rootfstype=${rootfstype} ${consoleargs} consoleblank=0
    loglevel=${verbosity} ubootpart=${partuuid} usb-storage.quirks=${usbstoragequirks}
    ${extraargs} ${extraboardargs}

with `consoleargs` from `console` (`serial`: `console=ttyS0,1500000`,
`display`: `console=tty1`, `both`), `earlycon` (`on` adds `earlycon`) and
`bootlogo` (`true`: `splash`, otherwise `splash=verbose`), and the cgroup
options when `docker_optimizations=on`.

`hwtool bootenv` sets only keys that `boot.cmd` reads, each with a check:
`verbosity` 0 to 7, `console` serial, display or both, `bootlogo`
true/false, `earlycon` and `docker_optimizations` on/off, `extraargs` without
quotes, backslashes, `;`, `$`, backquotes or control characters (512
characters at most), `usbstoragequirks` as `VID:PID:FLAGS`. `rootdev`,
`rootfstype`, `fdtfile` and the overlay keys belong to other commands and
are refused. `bootenv show` prints the values, `boot.cmd`'s defaults for
unset keys, and the command line as `boot.scr` builds it. Changes are backed
up and written durably like every other boot configuration change.

## CPU frequency

Each `/sys/devices/system/cpu/cpufreq/policy*` is a cluster (RK3576:
policy0 for the four Cortex-A53, policy4 for the four Cortex-A72; the core
names come from the CPU part in `/proc/cpuinfo`). `hwtool cpu set` accepts
only governors from `scaling_available_governors` and frequencies from
`scaling_available_frequencies`, writes `/etc/hwtool/cpufreq.conf`, installs
and enables `hwtool-boot.service` (a oneshot that runs `hwtool apply` at
boot, which applies the CPU and LED settings), and writes the values to sysfs now (the maximum first when
raising, the minimum first when lowering). `cpu reset` removes the file, and
the unit unless LED settings still need it; the kernel's defaults return at the next boot. Roxora images drop
Armbian's `/etc/default/cpufrequtils` and `armbian-hardware-optimize`, so
nothing else sets the governor. The `performance` governor, or a minimum at
the top frequency, gets a warning about heat.

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
the confirmation it first runs them without writing anything: in restricted
bash (no command names with a slash, no output redirection, no exec, PATH
read-only), as user nobody when hwtool runs as root, with a PATH that holds
only shims. The `dd` and `flashcp` shims print their arguments; a few
read-only tools pass through; any other command, and any error output, makes
hwtool refuse, because it could not show what would happen. Every printed
line must parse completely (all `dd` operands and size suffixes, including
`oflag=seek_bytes`) and must write exactly the requested device. The plan
then shows the exact commands, for example on a Radxa ROCK 4D:

    dd if=/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin of=/dev/mmcblk0 bs=32k seek=1 conv=notrunc status=none
    flashcp -v -p /usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip-spi.bin /dev/mtd0

Before writing to a disk hwtool checks that the target is a whole SD or eMMC
device (the boot ROM does not load U-Boot from NVMe, USB or SATA), that the
write ends before the first partition, and that the target is not the device
the system is running from unless `--allow-booted-device` is given. For SPI
flash it checks that the kernel shows an MTD device and that `flashcp`
(package `mtd-utils`) is installed.

The ArmSoM CM5-IO U-Boot package has no SPI writer, so `--target spi` is not
available there. On the ROCK 4D the writer always writes `/dev/mtd0`, and
the plan names that device.

The `u-boot-rockchip.bin` of both boards (9.1 MiB, written at 32 KiB) covers
sectors 64 to about 18740. Its bytes for sectors 7168 to 16383, where Rockchip
vendor storage (`DVKR`, `SSKR`) can live, are all 0xff padding: writing U-Boot
to an eMMC overwrites that window, whatever the migration kept before.

## Moving the system to eMMC or NVMe

In armbian/build, `armbian-install` is now a shim that runs
`armbian-config --api module_partitioner`. Roxora images do not ship
armbian-config, so hwtool runs the same steps itself. They follow
`install_run_scenario` in configng's
`tools/modules/functions/module_install_engine.sh`, for an ext4 root:

| configng | hwtool |
| --- | --- |
| pre-flight checks | refuses before anything is written: the running system's disks cannot be traced, the target is one of them, mounted, not eMMC or NVMe, too small (used space of `/` and `/boot`, the space before the first partition, 256 MiB spare; no number, no migration), `/` is not ext4, a tool or the U-Boot writer is missing |
| `install_plan_layout` | one ext4 partition, starting at 16 MiB on eMMC (room for U-Boot) and 1 MiB on NVMe, same partition table type as the boot disk |
| `install_apply_partitions` | `wipefs`, zero the start of the disk (keeping the Rockchip vendor storage at sectors 7168 to 16383 if it is there), `parted mklabel`, `mkpart`, `legacy_boot` flag |
| `install_make_filesystems` | `mkfs.ext4 -qF` |
| `install_transfer_rootfs` | `rsync -ax --delete` of `/` with Armbian's exclude list, twice |
| `install_populate_boot` | `rsync -aq --exclude 'efi/**' /boot/` |
| `install_gen_fstab` | the `/` line of the copied `/etc/fstab` gets the new UUID; other lines stay |
| `install_rewrite_bootenv` | `rootdev=UUID=...` and `rootfstype=ext4` in the copy's `armbianEnv.txt` |
| `install_verify_boot_dir`, `install_verify_fstab` | the copy's `/boot` has a kernel and `boot.scr`; every UUID in its fstab exists |
| `install_write_bootloader` | after the check (configng writes it before): eMMC: `write_uboot_platform`; NVMe with `--write-spi`: `write_uboot_platform_mtd` |

**USB and SATA drives.** U-Boot on the SD card or eMMC checks those first
and starts them, so a USB or SATA drive can hold the system only when U-Boot
runs from SPI flash. hwtool reads the facts from the installed U-Boot
package: the SPI writer (`write_uboot_platform_mtd`), the build
configuration (`u-boot-config-target-*`: `CONFIG_USB_STORAGE=y` for USB,
`CONFIG_AHCI`, `CONFIG_SATA` or `CONFIG_DWC_AHCI` for SATA) and the
`boot_targets` in the SPI image (`usb`, or `scsi` for AHCI). It needs
`--write-spi`, so the U-Boot in SPI flash is the package's. At the pinned
commit:

| Board | U-Boot | USB | SATA | boot_targets |
| --- | --- | --- | --- | --- |
| Radxa ROCK 4D | v2026.04, SPI image | `CONFIG_USB_STORAGE=y`, xHCI, DWC3 | no `CONFIG_AHCI` | `mmc0 mmc1 scsi nvme usb pxe dhcp` |
| ArmSoM CM5-IO | v2026.10, on SD or eMMC only | `CONFIG_USB_STORAGE=y` | no `CONFIG_AHCI` | `mmc1 mmc0 nvme scsi usb pxe dhcp spi` |

So the ROCK 4D can start from USB (after the SD card, the eMMC, SCSI/UFS and
NVMe: anything bootable there starts first), SATA is refused on both, and
the CM5-IO is refused for USB: its U-Boot is built with USB storage, but it
lives on the SD card or eMMC, which it checks before USB.

The running system and the SD card are not changed. If a step fails, hwtool
unmounts the target; if the U-Boot write to the eMMC fails, it zeroes the
range that write covered, so the board does not try to start a partial
U-Boot.

## The NPU and charsiu

charsiu runs language models on the RK3576 NPU through the mainline
`rocket` DRM accel driver. The edge kernel ships it as
`drivers/accel/rocket/rocket.ko`; the device tree has the NPU cores
(`rockchip,rk3576-rknn-core`, under `/soc`, on RK3588
`rockchip,rk3588-rknn-core` at the top level); the bound driver gives
`/dev/accel/accel0`. Vendor kernels have Rockchip's `rknpu` instead, which
charsiu does not use.

`hwtool npu status` looks at the running kernel, or, when the running
kernel is not one of the board's (a chroot, a container), at the kernel
the board boots next. It reports the rocket module (file, built in,
loaded), `rknpu`, the NPU nodes from `/sys/firmware/devicetree/base` (or
`/proc/device-tree`) with their `status`, the `/dev/accel` devices with
mode, owner and group, and charsiu's installed version, candidate and
origin from `apt-cache policy`, and ends in one verdict.

charsiu is a package in the Armbian repository (`noble-utils`), which
Roxora's apt pinning allows by name. charsiu supports only the RK3576, so
`hwtool npu install-charsiu` refuses unless the board's SoC is `rk3576`,
the kernel is not a vendor one, has rocket, and the device tree has an
enabled `rockchip,rk3576-rknn-core` node. On other SoCs (an RK3588 with
rocket, for example) `npu status` still reports the driver and the nodes,
and its verdict is "charsiu supports only the RK3576". Its plan shows `apt-get -s --no-install-recommends
install charsiu` and stops if apt would install or remove anything else;
charsiu's Recommends (curl or wget, whiptail) are named in a notice. It
never downloads a model: `charsiu pull` does that. `remove-charsiu` is the
same in reverse; downloaded models stay. A kernel switch to a vendor
branch notes that an installed charsiu does not work there.

## Pin header interfaces

`data/interfaces/<board>/` holds one overlay source per function and an
`index.json` with its pins, written by `tools/gen-interfaces.py` from these
sources:

- which RK3576 GPIO is on which header pin: Radxa's ROCK 4D page
  (docs.radxa.com, rock4/rock4d/hardware-use/pin-gpio) and ArmSoM's CM5-IO
  table (github.com/ArmSoM/documentation, `docs/main/ArmSoM Product/cm5.md`
  at 90a836e7d7da);
- which function a pin group carries, the order of its pins and the
  controller node: the boards' DTBs from the Roxora images (kernel
  7.3.4-edge-rockchip64), copied to `tests/interfaces/dtb/`;
- which header pins the board uses itself: the enabled nodes of those DTBs
  (ROCK 4D: pins 8 and 10 for the console UART0, 27 and 28 for I2C6 and the
  board EEPROM; CM5-IO: 8 and 10 for the console, 16 and 18 for the
  `gpio-leds` LEDs), and on the CM5-IO pin 28, which ArmSoM's table notes
  shares the fan control signal.

Radxa's table names several functions per pin; where its text is ambiguous
the DTB's pin groups decide. Functions whose pins could not be confirmed in
both sources are left out (on the CM5-IO only 10 functions remain, against
20 on the ROCK 4D). `tests/interfaces/check.py` checks every entry: the pin
groups exist and their pins, in order, are the GPIOs `index.json` puts on
those header pins; the overlay compiles with `dtc -@` and applies with
`fdtoverlay` to the board's DTB; in the result the controller is `okay` with
the right `pinctrl-0`; no function uses a busy pin. CI runs it, and applies
the installed `.dtbo` files again after the package is installed.

Each overlay sets `pinctrl-0` and `status = "okay"` on the controller
(`&i2c5`, `&spi1`, `&uart3`, `&pwm1_6ch_0`, ...). SPI overlays add
children on each chip select with `compatible = "armbian,spi-dev",
"rockchip,spidev"`, which the kernel's spidev driver (CONFIG_SPI_SPIDEV=m)
binds to `/dev/spidevB.C`. The edge kernel's spidev lists `armbian,spi-dev`
and not `rockchip,spidev` (`modinfo spidev`, 7.3.3-edge-rockchip64): with
`rockchip,spidev` alone it created `spi1.0` and `spi1.1` on a ROCK 4D but no
`/dev/spidev1.*`. `rockchip,spidev` stays second for kernels that list it.
1-Wire adds a `w1-gpio` node on header pin 7 (CONFIG_W1_MASTER_GPIO=m) and
needs an external pull-up of about 4.7 kOhm.

`hwtool interfaces enable NAME` installs
`/usr/share/hwtool/interfaces/<board>/NAME.dtbo` as
`/boot/overlay-user/hwtool-NAME.dtbo` and adds `hwtool-NAME` to
`user_overlays` in `armbianEnv.txt` (backed up first), which `boot.scr`
applies at the next boot. It refuses two functions that share a pin or the
same controller in another pin mux (`i2c8-m1` and `i2c8-m2`), whether they
are enabled together or one is already on. `disable` removes both again.

## Bootloader version and boot order

U-Boot images carry their version as a string such as `U-Boot
2026.04-armbian-... (Nov 10 2026 - 12:27:04 +0000)` (the SPL has its own,
`U-Boot SPL ...`, which is skipped). `hwtool boot version` reads it from
each SPI flash MTD device (the first 16 MiB) and from the start of each SD
and eMMC device up to its first partition, which needs root, and from the
installed package's `u-boot-rockchip.bin` and `u-boot-rockchip-spi.bin`.
Each device is "up to date", "older", "newer" or "different" (compared by
the release in the version string, then the whole string), or "none". The
device the board starts from is the first that has U-Boot in the order
SPI flash, eMMC, SD. `hwtool info` shows the same summary.

`hwtool boot update-bootloader` writes the package's U-Boot to that device
with the package's writer (see below), only when it differs, and with the
same checks as `write-bootloader --allow-booted-device`.

The boot order is U-Boot's `boot_targets` variable. hwtool changes it only
where the U-Boot build keeps a writable environment, read from the package's
`u-boot-config-target-*`: the ROCK 4D edge U-Boot has
`CONFIG_ENV_IS_IN_SPI_FLASH=y`, `CONFIG_ENV_OFFSET=0xc00000`,
`CONFIG_ENV_SIZE=0x20000` and no redundant copy. The CM5-IO build has no
environment store, so U-Boot always uses its built-in order and hwtool says
so. `boot order set` installs `libubootenv-tool` if needed, writes
`/etc/hwtool/fw_env.config` for that location, and saves U-Boot's built-in
environment (read from the package's SPI image, where it is stored as
NUL-separated `key=value` strings) with only `boot_targets` changed, using
`fw_setenv -s`. Every target must be one of the built-in ones
(`mmc0 mmc1 scsi nvme usb pxe dhcp` on the ROCK 4D; in U-Boot's device tree
`mmc0` is the SD card and `mmc1` the eMMC). `boot order reset` erases the
saved environment with `flash_erase`, so the built-in one applies again.

## LEDs

`hwtool leds list` reads `/sys/class/leds/*/trigger` (the current trigger is
the one in brackets). `leds set NAME TRIGGER` accepts only triggers that LED
offers and writes it to sysfs now. The BSP's `armbian-led-state.service`
saves every LED's trigger to `/etc/armbian-leds.conf` at shutdown and
restores it at boot; hwtool does not add a second mechanism next to it: it
sets `trigger=` in that LED's `[/sys/class/leds/NAME]` section of the same
file (backed up first), so the setting also survives a power cut before
the next clean shutdown. Without that service the trigger goes to
`/etc/hwtool/leds.conf`, which `hwtool-boot.service` applies at boot.

## Temperatures and fans

`hwtool fan show` lists `/sys/class/thermal/thermal_zone*` with their
temperatures and trip points, the cooling devices, and the fan's speed (the
`pwm1` of the pwm-fan driver's hwmon device, 0 to 255). Neither board's
device tree has a `pwm-fan` node, so out of the box the cooling maps only
throttle the CPU and GPU. Trip points can be changed at run time only in
kernels built with CONFIG_THERMAL_WRITABLE_TRIPS, which the edge kernel is
not.

`hwtool fan enable [--pwm NAME] [--curve 50:30,60:50,70:80,80:100]` adds a
fan through a user overlay, compiled with `dtc -@` from
`/etc/hwtool/fan.dtso` into `/boot/overlay-user/hwtool-fan.dtbo` and added to
`user_overlays` (a reboot applies it). The overlay

- adds a `pwm-fan` node (CONFIG_SENSORS_PWM_FAN=m in the edge kernel) on the
  chosen PWM channel, with `cooling-levels` 0 and then each point's
  percentage as 0 to 255;
- enables that PWM controller with its pin group;
- adds one `active` trip point per curve point to `package-thermal` (the
  SoC package sensor; the zone names are labels in the boards' DTBs), each
  with a 5 C hysteresis, and a cooling map that puts the fan in state k at
  trip k, so the kernel's step_wise governor turns it up and down; it sets
  the zone's `polling-delay` to 1 s. The existing critical trip stays.

A curve has 1 to 8 points; temperatures go up within 30 to 100 C and the
speed (0 to 100 %) does not go down. The PWM sources:

- `fan` (the default), the board's fan connector. ROCK 4D: a 2-pin 1.25 mm
  5 V connector (Radxa docs, rock4/rock4d/hardware-use/fan.md); Radxa's
  kernel drives it from `pwm2_8ch_5`, pin group `pwm2m1_ch5` (GPIO4_C5, not
  on the header), 60 us period (radxa/kernel linux-6.1-stan-rkr5.1,
  `rk3576-rock-4d.dts`). CM5-IO: connector CN32 (5 V, GND and a return
  switched by Q45/Q46), controlled by PWM2_CH6_M2, which is also header pin
  28 (ArmSoM's cm5.md); ArmSoM's kernel uses `pwm2_8ch_6`, `pwm2m2_ch6`,
  50 us (ArmSoM/rockchip-kernel e47316851307, `rk3576-armsom-cm5-io.dts`).
- a header PWM function from `hwtool interfaces list`, at 25 kHz (the
  frequency of 4-pin PC fans), for a fan driven through a transistor.

Neither connector has a tachometer line, so hwtool adds no `fan-tach`. A
fan holds its PWM channel and pins: `interfaces list` shows them as used by
the fan, and header functions on them are refused, as is `fan enable` on a
channel a header function uses. `hwtool fan disable` removes the overlay.
The tests compile every generated overlay and apply it with `fdtoverlay`
to the boards' DTBs.

`hwtool fan set PERCENT` runs the fan at a fixed speed until the next boot:
it sets the `package-thermal` governor to `user_space` (so the temperature
no longer changes the speed; the critical trip still works) and writes the
speed to `pwm1`. `hwtool fan auto` sets the governor back to `step_wise`.
On UEFI images `fan enable` and `disable` refuse (the firmware owns the
device tree); `show`, `set` and `auto` work when a pwm-fan device exists.

## Wi-Fi country

`hwtool wifi country` shows the country from `iw reg get`, including wiphys
that manage their own rules. `wifi country XX` accepts ISO 3166-1 alpha-2
codes (and 00), writes `options cfg80211 ieee80211_regdom=XX` to
`/etc/modprobe.d/hwtool-wifi.conf` (for every later boot) and runs
`iw reg set XX` now, installing `iw` first if it is missing.

The ROCK 4D's Wi-Fi uses the aic8800 driver from the DKMS package
`aic8800-usb` (module `aic8800_fdrv_usb`). Its source
(`rwnx_mod_params.c`) turns its `custregd` parameter on by default: the
wiphy is then `REGULATORY_WIPHY_SELF_MANAGED` with the driver's own
permissive rules for its compiled-in country (`CONFIG_COUNTRY_CODE "00"`)
and logs "CAUTION: USING PERMISSIVE CUSTOM REGULATORY RULES"; its
`country_code` parameter is overwritten at load. So the cfg80211 setting
does not reach it. `--aic8800-follow-country` also writes
`options aic8800_fdrv_usb custregd=0`, which makes it leave the rules to
cfg80211. That is opt-in because it has not been tried on a board.

## Swap

Roxora images get `/swap.img` at the first boot from cloud-init's mounts
module (roxora-os/build `userpatches/roxora/cloud-init/99-roxora.cfg`:
`swap: filename /swap.img, size auto`). cloud-init's `cc_mounts.py` writes
the fstab line `/swap.img none swap sw 0 0`; cloud-init 24.1 adds
`,comment=cloudconfig` to its options, 25.1 and later do not. When the
module runs again (a new instance id) it drops every fstab line with
`comment=cloudconfig` and appends its swap line for the file. hwtool writes
the line with `comment=cloudconfig`, so cloud-init replaces it instead of
adding a second one, and replaces cloud-init's line (with or without the
comment) rather than adding its own.

`hwtool swap show` reads `/proc/swaps` (what `swapon --show` prints),
`/proc/meminfo`, the file and its fstab line, and whether zram is in use
(the BSP's `armbian-zram-config.service`, enabled by default, puts swap on
zram at priority 5; the swap file has a lower priority, so zram is used
first). `swap set SIZE` (for example `2G` or `512M`, whole MiB):

- refuses a file that does not fit, and, unless `--force`, one that leaves
  less than a quarter of the root file system free or is more than twice the
  memory; only ext4 and xfs root file systems are accepted;
- if the file is in use, runs `swapoff` first, but only if the memory
  available can take what is swapped out to it (plus 256 MiB); otherwise it
  refuses and says how much is in use;
- creates the file mode 600 (`install -m 0600 /dev/null`), `fallocate`s it,
  runs `mkswap` and `swapon`, then writes fstab (backed up first). If a step
  that makes the new file fails, the cleanup deletes the half-made file and
  its fstab line.

`swap off` runs `swapoff` (with the same memory check), removes the fstab
line and deletes the file. It also writes
`/etc/cloud/cloud.cfg.d/99-zz-hwtool-swap.cfg` with `swap: {size: 0}`, so a
later run of cloud-init's mounts module does not make the file again:
cloud-init reads `cloud.cfg.d` in reverse lexical order and keeps the first
value of each key (`util.read_conf_d`, `mergemanydict`), so this file wins
over `99-roxora.cfg`. `swap set` removes it again. `swap swappiness N` (0 to 200) sets
`/proc/sys/vm/swappiness` now and writes
`/etc/sysctl.d/60-hwtool-swap.conf`.

## UEFI images

roxora-os/build also makes `generic-rk3576-uefi`, one image for RK3576
boards with UEFI firmware such as edk2-rk3576: GPT with an EFI system
partition at `/boot/efi`, Ubuntu's `grub-efi-arm64` (settings in
`/etc/default/grub.d/90-roxora.cfg`, applied with `update-grub`), the edge
kernel, and no U-Boot package, `boot.scr` or `armbianEnv.txt`. The firmware
gives the kernel its own device tree, or, with its "DTB override" option,
one of Armbian's rk3576 DTBs, which `/etc/kernel/postinst.d/zz-roxora-efi-dtb`
copies to `/boot/efi/dtb/rockchip/` with every kernel.

hwtool takes an image for UEFI when `/etc/armbian-release` names a
`*-uefi` board, or when no U-Boot package is installed and the system has
`/boot/efi/EFI` and GRUB. There:

- `hwtool info` and `hwtool uefi status` show "UEFI image (GRUB)", the
  firmware vendor, version and date from DMI (`/sys/class/dmi/id/bios_*`;
  `/sys/firmware/efi/fw_vendor` is only an address), whether
  `/sys/firmware/efi` exists, and the model and SoC from
  `/proc/device-tree`;
- the device tree source: hwtool compares every property of the running
  tree (`/proc/device-tree`, readable without root) with each DTB on the
  ESP, leaving out `/chosen`, memory nodes, `__symbols__` and phandles,
  which the firmware and dtc fill in differently. If one DTB has at least
  98% of the properties with the same value, the tree is that override;
  otherwise it is the firmware's own. The share is printed;
- overlays, pin header interfaces, boot options, the bootloader commands
  (write, update, version, order), the kernel branch switch and revert (only
  edge boots with UEFI here) and migration (not supported on UEFI images
  yet) refuse with the reason; overlays
  would have to be applied by the firmware;
- CPU, LEDs, temperatures, Wi-Fi, swap, NPU, kernel status, headers and
  hold work as on the other images.

The GTK app shows the firmware and the device tree on the Board and Boot
pages, and a status page with the reason on Overlays and Pin Header.

## The GTK app

`hwtool-gtk` is a GTK 4 and libadwaita 1.5 app laid out like GNOME
Settings: a sidebar with the pages Board, Overlays, Pin Header, Kernel,
Boot, CPU, Memory, Devices and NPU (an AdwNavigationSplitView that shows one
pane at a time on narrow screens), destructive actions in their own groups,
confirmations as dialogs that show the plan, results as toasts, and a
status page for boards it does not recognise. It runs as the user and
refuses to run as root. It reads state
with `hwtool --json`, shows the output of `hwtool ... --dry-run` in a
confirmation dialog, and runs the change with
`pkexec /usr/bin/hwtool ... --yes --expect-plan <fingerprint>`, so what runs
is what was confirmed. The polkit action `io.github.roxora_os.hwtool.run`
asks for an administrator password. The U-Boot versions on the boot
devices are read the same way, after a Check button, because reading the
devices needs root. While a change runs, the pages are
insensitive, a banner says not to switch the board off, and the window
cannot be closed. Names (overlays, branches) are passed after `--`.

The variables HWTOOL_CLI, HWTOOL_ROOT and HWTOOL_GTK_NO_PKEXEC exist only in
builds with the `test-hooks` feature, which CI uses for screenshots and
release packages do not.
