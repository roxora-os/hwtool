# Changelog

## 0.2.0

A pre-release: not yet tested on hardware.

- UEFI images (`generic-rk3576-uefi`): hwtool recognises them. `hwtool info`
  and the new `hwtool uefi status` show the UEFI firmware, the SoC and
  whether the device tree is the firmware's own or one of the DTB overrides
  on the EFI system partition. Overlays, pin header interfaces, boot
  options, the U-Boot commands, the kernel branch switch and moving the
  system say why they are not available there. The GTK app shows the
  firmware on the Board and Boot pages.
- `hwtool swap off` also stops cloud-init from making the swap file again
  (`/etc/cloud/cloud.cfg.d/99-zz-hwtool-swap.cfg`); `hwtool swap set`
  removes that file.
- The About dialog credits the icon's designers.
- The GTK app's Memory page has its own icon.

## 0.1.0

First version, a pre-release: not yet tested on hardware.

- Device tree overlays: list, enable and disable the board's overlays, and
  add or remove your own from a `.dts` or `.dtso` file.
- Pin header: I2C, SPI (with spidev), UART, PWM and 1-Wire on the 40-pin
  header of the Radxa ROCK 4D and the ArmSoM CM5-IO, checked against the
  boards' device trees; functions that share pins are refused together.
- Kernel branch: switch between `edge` and `vendor` and go back, with the
  replaced packages saved for the way back; install or remove the kernel
  headers; hold the kernel packages.
- Boot: the boot options `boot.scr` reads; write U-Boot to SPI flash, SD or
  eMMC; the U-Boot version on each boot device compared with the package,
  and an update; U-Boot's boot order on the ROCK 4D; move the running
  system to eMMC, NVMe or a USB drive.
- CPU frequency per cluster, LED triggers, temperatures and fans, the Wi-Fi
  country, the swap file and `vm.swappiness`.
- NPU: whether it can run charsiu, and installing or removing charsiu
  (RK3576 only).
- Every change shows its plan first, works with `--dry-run`, asks before it
  runs, backs up the files it changes and writes a log.
- Two Debian packages for arm64: `roxora-hwtool` (command line) and
  `roxora-hwtool-gtk` (GTK app, which asks for the administrator password
  through polkit).
