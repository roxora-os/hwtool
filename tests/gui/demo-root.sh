#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Create a fake root filesystem shaped like a Radxa ROCK 4D edge image, for
# starting the GTK app in CI without a board. Usage: demo-root.sh <dir>
set -euo pipefail

r="${1:?usage: demo-root.sh <dir>}"
k=7.3.4-edge-rockchip64
mkdir -p "$r/etc" "$r/boot/dtb-$k/rockchip/overlay" "$r/boot/overlay-user" \
  "$r/var/lib/dpkg" "$r/usr/lib/u-boot" "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d" \
  "$r/proc/self" "$r/sys/block/mmcblk1/mmcblk1p1" "$r/sys/block/mmcblk1/device" \
  "$r/sys/block/mmcblk0/device" "$r/sys/block/nvme0n1" "$r/sys/class/mtd/mtd0"

cat > "$r/etc/armbian-release" << 'END'
BOARD=radxa-rock-4d
BOARD_NAME="Rock 4D"
BOARDFAMILY=rk35xx
LINUXFAMILY=rockchip64
BOOT_SOC=rk3576
KERNEL_TARGET=vendor,edge
END
cat > "$r/boot/armbianEnv.txt" << 'END'
verbosity=1
bootlogo=false
console=both
overlay_prefix=rockchip-rk3576
rootdev=UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908
rootfstype=ext4
user_overlays=sample-sensor
END
: > "$r/boot/overlay-user/sample-sensor.dtbo"
: > "$r/boot/overlay-user/sample-display.dtbo"
: > "$r/boot/vmlinuz-$k"
: > "$r/boot/boot.scr"
ln -sfn "vmlinuz-$k" "$r/boot/Image"
ln -sfn "dtb-$k" "$r/boot/dtb"

for p in linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge; do
  printf 'Package: %s\nStatus: install ok installed\nVersion: 26.11.0-trunk\n\n' "$p"
done > "$r/var/lib/dpkg/status"

cat > "$r/usr/lib/u-boot/platform_install.sh" << 'END'
DIR=/usr/lib/linux-u-boot-edge-radxa-rock-4d
write_uboot_platform ()
{
    dd "if=$1/u-boot-rockchip.bin" "of=$2" bs=32k seek=1 conv=notrunc status=none
}
write_uboot_platform_mtd ()
{
    flashcp -v -p "$1/u-boot-rockchip-spi.bin" /dev/mtd0
}
END
truncate -s 9561600 "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin"
truncate -s 1598976 "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip-spi.bin"

echo "/dev/mmcblk1p1 / ext4 rw,relatime 0 0" > "$r/proc/self/mounts"
echo 62333952 > "$r/sys/block/mmcblk1/size"
echo SD > "$r/sys/block/mmcblk1/device/type"
echo 1 > "$r/sys/block/mmcblk1/mmcblk1p1/partition"
echo 32768 > "$r/sys/block/mmcblk1/mmcblk1p1/start"
echo 62300000 > "$r/sys/block/mmcblk1/mmcblk1p1/size"
echo 61071360 > "$r/sys/block/mmcblk0/size"
echo MMC > "$r/sys/block/mmcblk0/device/type"
echo 500118192 > "$r/sys/block/nvme0n1/size"
echo spi5.0 > "$r/sys/class/mtd/mtd0/name"
echo 16777216 > "$r/sys/class/mtd/mtd0/size"
