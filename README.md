# hwtool

hwtool changes the hardware settings of a board running a
[Roxora](https://github.com/roxora-os/build) image:

1. **Device tree overlays**: list the overlays available for the board, show
   which are enabled, enable or disable them.
2. **Kernel branch**: switch between the kernel branches the board supports
   (for example `edge` and `vendor`), and go back.
3. **Boot storage**: write the bootloader to SPI flash or eMMC, and move the
   running system from an SD card to eMMC, NVMe or (with U-Boot in SPI
   flash) a USB drive.
4. **Kernel headers and holding**: install or remove the headers DKMS
   needs, and hold the kernel, device tree, U-Boot and firmware packages.
5. **Boot options**: the `armbianEnv.txt` keys `boot.scr` reads (log
   level, console, splash, extra kernel arguments), checked.
6. **CPU frequency**: governor and frequency range per CPU cluster,
   applied at every boot.
7. **Pin header**: I2C, SPI (with spidev), UART, PWM and 1-Wire on the
   40-pin header, as overlays that hwtool ships and checks against the
   board's device tree; functions that share pins are refused together.
8. **Bootloader version and boot order**: the U-Boot version on SPI flash,
   eMMC and SD compared with the installed package, an update of the device
   the board starts from, and U-Boot's boot order where it can be saved.
9. **LEDs, temperatures and fans**: LED triggers kept across reboots,
   thermal zones, trip points and cooling devices.
10. **Wi-Fi country**: the regulatory country, set now and at every boot.
11. **Swap**: the swap file cloud-init makes at the first boot: show,
    resize, remove, and `vm.swappiness`.
12. **UEFI images**: on the `generic-rk3576-uefi` image, `hwtool uefi status`
    shows the firmware and whether the device tree is the firmware's or an
    override from the EFI system partition; what needs U-Boot or `boot.scr`
    is refused with the reason.
13. **NPU**: show whether the NPU can run
   [charsiu](https://github.com/gahingwoo/charsiu) (the `rocket` driver, the
   NPU device tree nodes, `/dev/accel`), and install or remove charsiu
   (RK3576 only).

It works with what the board's packages already provide: `/boot/armbianEnv.txt`
read by `boot.scr`, the `linux-image`, `linux-dtb` and `linux-u-boot` packages
from the Armbian repository, and the writer functions the U-Boot package
ships. [docs/how-it-works.md](docs/how-it-works.md) explains each part.

There are two packages:

| Package | Content |
| --- | --- |
| `roxora-hwtool` | `hwtool`, the command line tool. No GUI dependencies. |
| `roxora-hwtool-gtk` | `hwtool-gtk`, a GTK 4 and libadwaita app ("Hardware Tool") that runs `hwtool` through polkit. |

## Using it

```
hwtool info                                  # board, kernel branch, disks
hwtool overlays list
sudo hwtool overlays enable uart1
sudo hwtool overlays disable uart1
sudo hwtool overlays add my-sensor.dtso        # compile, install, enable
sudo hwtool overlays remove my-sensor
hwtool kernel status
sudo hwtool kernel switch vendor
sudo hwtool kernel revert                    # back to the branch before the last switch
sudo hwtool kernel headers install           # or remove
sudo hwtool kernel hold                      # or unhold
hwtool bootenv show
sudo hwtool bootenv set verbosity=7          # or: bootenv unset verbosity
hwtool cpu show
sudo hwtool cpu set --policy policy4 --governor ondemand --max 1800000
sudo hwtool cpu reset
hwtool boot status
sudo hwtool boot write-bootloader --target spi
sudo hwtool boot write-bootloader --target /dev/mmcblk0
sudo hwtool boot migrate --target /dev/nvme0n1 --write-spi
sudo hwtool boot migrate --target /dev/sda --write-spi   # USB drive, ROCK 4D
hwtool interfaces list                       # 40-pin header functions and pins
sudo hwtool interfaces enable i2c8-m2 spi1-m0
sudo hwtool interfaces disable spi1-m0
sudo hwtool boot version                     # U-Boot on SPI, eMMC, SD vs the package
sudo hwtool boot update-bootloader           # only if the boot device differs
sudo hwtool boot order show
sudo hwtool boot order set nvme mmc0 mmc1    # ROCK 4D; or: boot order reset
hwtool leds list
sudo hwtool leds set green:status heartbeat
hwtool fan show                              # temperatures, trip points, fans
hwtool wifi country
sudo hwtool wifi country DE
hwtool swap show
sudo hwtool swap set 2G                      # or: swap off
sudo hwtool swap swappiness 20
hwtool uefi status                           # UEFI image: firmware, device tree source
hwtool npu status
sudo hwtool npu install-charsiu                # then: charsiu pull
sudo hwtool npu remove-charsiu
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
- writes a log to `/var/log/hwtool/<time>.log`, and if a step fails or
  hwtool is interrupted (Ctrl-C, a closed terminal), runs the plan's cleanup
  and reports it; a failed kernel switch is rolled back;
- for a kernel branch switch, saves the packages it replaces as `.deb` files
  in `/var/lib/hwtool/packages/` so `hwtool kernel revert` works even when
  the repository no longer offers them;
- refuses to write to the device the system is running from, unless that is
  the point of the command (`--allow-booted-device` for a bootloader update
  on the boot device; migration never writes to it).

Overlay, interface, kernel and boot order changes take effect after a reboot. `--json` gives
machine-readable output; the GTK app uses it.

## Building

The workspace has three crates: `crates/core` (all logic), `crates/cli`
(`hwtool`) and `crates/gtk` (`hwtool-gtk`).

```
cargo test                            # core and CLI; no GTK needed
cargo build --release -p roxora-hwtool-gtk   # needs libgtk-4-dev
packaging/build-debs.sh target output        # both .deb files into output/
```

CI on GitHub Actions (`ubuntu-24.04-arm`) runs the tests, checks every pin
header overlay against the boards' DTBs (`tests/interfaces/check.py`: pin
groups, `dtc -@`, `fdtoverlay`), builds and installs both packages, starts
the GTK app under Xvfb and saves screenshots, and runs a kernel branch
switch with apt and dpkg and a swap file with `mkswap` and `swapon` in an
Ubuntu 24.04 chroot (`tests/chroot/package-switch.sh`). Tags `v*` attach the packages to the
release.

Writing the bootloader and moving the system need real hardware; the steps
are in [docs/hardware-test.md](docs/hardware-test.md).

## Credits

The app icon is by [DavidQyn](https://github.com/DavidQyn) and Ga Hing
Woo, from [roxora-os/design](https://github.com/roxora-os/design).

## License

GPL-2.0-only. See [LICENSE](LICENSE).
