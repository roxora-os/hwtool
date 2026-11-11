# hwtool

hwtool changes the hardware settings of a board running a
[Roxora](https://github.com/roxora-os/build) image. It does three things:

1. **Device tree overlays**: list the overlays available for the board, show
   which are enabled, enable or disable them.
2. **Kernel branch**: switch between the kernel branches the board supports
   (for example `edge` and `vendor`), and go back.
3. **Boot storage**: write the bootloader to SPI flash or eMMC, and move the
   running system from an SD card to eMMC or NVMe.

It works with what the board's packages already provide: `/boot/armbianEnv.txt`
read by `boot.scr`, the `linux-image`, `linux-dtb` and `linux-u-boot` packages
from the Armbian repository, and the writer functions the U-Boot package
ships. [docs/how-it-works.md](docs/how-it-works.md) explains each part.

There are two packages:

| Package | Content |
| --- | --- |
| `roxora-hwtool` | `hwtool`, the command line tool. No GUI dependencies. |
| `roxora-hwtool-gtk` | `hwtool-gtk`, a GTK 4 app ("Hardware Tool") that runs `hwtool` through polkit. |

## Using it

```
hwtool info                                  # board, kernel branch, disks
hwtool overlays list
sudo hwtool overlays enable uart1
sudo hwtool overlays disable uart1
hwtool kernel status
sudo hwtool kernel switch vendor
sudo hwtool kernel revert                    # back to the branch before the last switch
hwtool boot status
sudo hwtool boot write-bootloader --target spi
sudo hwtool boot write-bootloader --target /dev/mmcblk0
sudo hwtool boot migrate --target /dev/nvme0n1 --write-spi
```

Every command that changes something:

- prints exactly what it will run and which files it will write, with their
  new content;
- with `--dry-run`, stops there and changes nothing (this works without
  root);
- asks you to type `yes`, unless you add `--yes`; without a terminal it
  refuses unless `--yes` is given;
- copies a boot configuration file to `/var/backups/hwtool/<time>/`
  before changing it;
- for a kernel branch switch, saves the packages it replaces as `.deb` files
  in `/var/lib/hwtool/packages/` so `hwtool kernel revert` works even when
  the repository no longer offers them;
- refuses to write to the device the system is running from, unless that is
  the point of the command (`--allow-booted-device` for a bootloader update
  on the boot device; migration never writes to it).

Overlay and kernel changes take effect after a reboot. `--json` gives
machine-readable output; the GTK app uses it.

## Building

The workspace has three crates: `crates/core` (all logic), `crates/cli`
(`hwtool`) and `crates/gtk` (`hwtool-gtk`).

```
cargo test                            # core and CLI; no GTK needed
cargo build --release -p roxora-hwtool-gtk   # needs libgtk-4-dev
packaging/build-debs.sh target output        # both .deb files into output/
```

CI on GitHub Actions (`ubuntu-24.04-arm`) runs the tests, builds and installs
both packages, starts the GTK app under Xvfb and saves screenshots, and runs
a kernel branch switch with apt and dpkg in an Ubuntu 24.04 chroot
(`tests/chroot/package-switch.sh`). Tags `v*` attach the packages to the
release.

Writing the bootloader and moving the system need real hardware; the steps
are in [docs/hardware-test.md](docs/hardware-test.md).

## License

GPL-2.0-only. See [LICENSE](LICENSE).
