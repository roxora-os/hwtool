## Hardware tool (hwtool)

These steps check the parts of hwtool that CI cannot: anything that boots, and
anything that writes a bootloader or a disk. Run them on each board in
`boards.conf`. Steps marked ROCK 4D need its SPI flash.

### Before you start

- A board running a Roxora image from an SD card, a serial console
  (1500000 baud on the edge branch) and network.
- A second SD card with the same image, to recover if the board stops
  booting. For SPI flash recovery the board may need maskrom mode and
  `rkdeveloptool` on another computer.
- Nothing you want to keep on the eMMC or NVMe drive: the migration steps
  erase it.
- If hwtool is not in the image yet, install the package from the hwtool
  release: `sudo apt install ./roxora-hwtool_<version>_arm64.deb`.

Write down the output of every command marked "record".

### H1. Information (no changes)

    hwtool info                      # record

Pass: the board name and SoC are right, the kernel branch is `edge`, the SD
card is listed as `sd` and "running system", the eMMC as `emmc`, an NVMe
drive as `nvme`. On the ROCK 4D, `/dev/mtd0` is listed as SPI flash.

### H2. Overlays

On the edge kernel the device tree package has no RK3576 overlays, so this
test uses a user overlay that only adds a property to `/chosen`.

    sudo apt install device-tree-compiler
    cat > roxora-test.dtso << 'EOF'
    /dts-v1/;
    /plugin/;
    / {
        fragment@0 {
            target-path = "/chosen";
            __overlay__ {
                roxora-hwtool-test = "ok";
            };
        };
    };
    EOF
    sudo armbian-add-overlay roxora-test.dtso
    sudo hwtool overlays disable roxora-test --dry-run     # record
    hwtool overlays list                                    # record

`armbian-add-overlay` enables the overlay itself. Reboot and check the serial
log shows `Applying user provided DT overlay roxora-test.dtbo`, then:

    cat /proc/device-tree/chosen/roxora-hwtool-test        # prints ok
    sudo hwtool overlays disable roxora-test                # type yes
    ls /var/backups/hwtool/*/boot/armbianEnv.txt     # a backup exists

Reboot. Pass: `/proc/device-tree/chosen/roxora-hwtool-test` no longer exists
and the board booted normally both times.

### H3. Kernel branch switch and back

    sudo hwtool kernel switch vendor --dry-run              # record
    sudo hwtool kernel switch vendor                        # type yes, record
    hwtool kernel status                                    # record

Reboot. Then:

    uname -r                                                # record, ends in -vendor-rk35xx
    hwtool overlays list                                    # record (vendor overlays)
    sudo hwtool kernel revert                               # type yes, record

Reboot. Pass: `uname -r` ends in `-vendor-rk35xx` after the switch and in
`-edge-rockchip64` after the revert, the board boots both times, Ethernet
works both times.

Known limit: on 2026-11-11 the Armbian repository published only vendor
packages for the ArmSoM CM5-IO, so `linux-u-boot-armsom-cm5-io-edge` has no
apt candidate and `hwtool kernel revert` stops there with a message naming
the package. Record what it prints.

### H4. Rewrite the bootloader on the SD card

Use the device name `hwtool info` showed for the SD card (usually
`/dev/mmcblk1`).

    sudo hwtool boot write-bootloader --target /dev/mmcblk1           # must refuse
    sudo hwtool boot write-bootloader --target /dev/mmcblk1 --allow-booted-device --dry-run   # record
    sudo hwtool boot write-bootloader --target /dev/mmcblk1 --allow-booted-device

Reboot. Pass: the first command refuses and names `--allow-booted-device`, the
dry run shows one `dd` line writing `u-boot-rockchip.bin` at 32 KiB, and the
board boots from the SD card afterwards (serial log shows the U-Boot build
date of the installed package).

### H5. SPI flash (ROCK 4D)

    sudo hwtool boot write-bootloader --target spi --dry-run          # record
    sudo hwtool boot write-bootloader --target spi                    # record

Reboot with the SD card inserted. Pass: the dry run shows
`flashcp -v -p .../u-boot-rockchip-spi.bin /dev/mtd0`, `flashcp` reports
verifying, and the board starts (serial log shows U-Boot loaded from SPI,
then `Boot script loaded from mmc`).

On the CM5-IO the same command must refuse: its U-Boot package has no SPI
writer.

### H6. Move the system to eMMC

    sudo hwtool boot migrate --target /dev/mmcblk0 --dry-run          # record
    sudo hwtool boot migrate --target /dev/mmcblk0                    # type yes, record

Power off, remove the SD card, power on. Then:

    findmnt /                     # record, /dev/mmcblk0p1
    cat /etc/fstab                # record, UUID of mmcblk0p1
    cat /boot/armbianEnv.txt      # record, rootdev=UUID of mmcblk0p1
    hwtool info                   # eMMC is the running system

Pass: the board boots from the eMMC without the SD card, `/` is
`mmcblk0p1`, and the SD card still boots the old system when inserted again
(on the ROCK 4D, U-Boot boots the SD card first).

### H7. Move the system to NVMe

ROCK 4D (U-Boot in SPI flash):

    sudo hwtool boot migrate --target /dev/nvme0n1 --write-spi --dry-run   # record
    sudo hwtool boot migrate --target /dev/nvme0n1 --write-spi             # type yes, record

Power off, remove the SD card (and erase or remove anything bootable on the
eMMC), power on. Pass: `findmnt /` shows `/dev/nvme0n1p1`.

CM5-IO (no SPI writer): run the same without `--write-spi`. The board then
needs U-Boot on the eMMC or SD card that looks for `/boot` on NVMe. Record
which device boots with the SD card removed and with it inserted; the result
decides whether hwtool should offer NVMe on this board.

### What to send back

The recorded output of each step, the serial log of each boot that failed,
and for each step pass or fail.
