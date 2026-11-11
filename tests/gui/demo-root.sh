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

cat >"$r/etc/armbian-release" <<'END'
BOARD=radxa-rock-4d
BOARD_NAME="Rock 4D"
BOARDFAMILY=rk35xx
LINUXFAMILY=rockchip64
BOOT_SOC=rk3576
KERNEL_TARGET=vendor,edge
END
cat >"$r/boot/armbianEnv.txt" <<'END'
verbosity=1
bootlogo=false
console=both
overlay_prefix=rockchip-rk3576
rootdev=UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908
rootfstype=ext4
user_overlays=sample-sensor
END
: >"$r/boot/overlay-user/sample-sensor.dtbo"
: >"$r/boot/overlay-user/sample-display.dtbo"
: >"$r/boot/vmlinuz-$k"
: >"$r/boot/boot.scr"
here="$(cd "$(dirname "$0")" && pwd)"
cp "$here/../../crates/core/tests/fixtures/boot-rk3576.cmd" "$r/boot/boot.cmd"
ln -sfn "vmlinuz-$k" "$r/boot/Image"
ln -sfn "dtb-$k" "$r/boot/dtb"

for p in linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64; do
  printf 'Package: %s\nStatus: install ok installed\nVersion: 26.11.0-trunk\n\n' "$p"
done >"$r/var/lib/dpkg/status"

cat >"$r/usr/lib/u-boot/platform_install.sh" <<'END'
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
mkdir -p "$r/usr/sbin"
printf '#!/bin/sh\n' >"$r/usr/sbin/flashcp"
truncate -s 9561600 "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin"
truncate -s 1598976 "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip-spi.bin"

echo "22 1 179:97 / / rw,relatime - ext4 /dev/mmcblk1p1 rw" >"$r/proc/self/mountinfo"
mkdir -p "$r/sys/dev/block"
ln -sfn ../../block/mmcblk1/mmcblk1p1 "$r/sys/dev/block/179:97"
ln -sfn ../../block/mmcblk1 "$r/sys/dev/block/179:96"
ln -sfn ../../block/mmcblk0 "$r/sys/dev/block/179:0"
ln -sfn ../../block/nvme0n1 "$r/sys/dev/block/259:0"
echo 62333952 >"$r/sys/block/mmcblk1/size"
echo SD >"$r/sys/block/mmcblk1/device/type"
echo 1 >"$r/sys/block/mmcblk1/mmcblk1p1/partition"
echo 32768 >"$r/sys/block/mmcblk1/mmcblk1p1/start"
echo 62300000 >"$r/sys/block/mmcblk1/mmcblk1p1/size"
echo 61071360 >"$r/sys/block/mmcblk0/size"
echo MMC >"$r/sys/block/mmcblk0/device/type"
echo 500118192 >"$r/sys/block/nvme0n1/size"
echo spi5.0 >"$r/sys/class/mtd/mtd0/name"

# The NPU, as on a ROCK 4D running edge: rocket loaded, two NPU cores,
# /dev/accel/accel0.
mkdir -p "$r/proc/sys/kernel"
echo "$k" >"$r/proc/sys/kernel/osrelease"
mkdir -p "$r/lib/modules/$k/kernel/drivers/accel/rocket" "$r/dev/accel"
: >"$r/lib/modules/$k/kernel/drivers/accel/rocket/rocket.ko"
echo "rocket 49152 0 - Live 0x0000000000000000" >"$r/proc/modules"
for n in npu@27700000 npu@27708000; do
  mkdir -p "$r/proc/device-tree/soc/$n"
  printf 'rockchip,rk3576-rknn-core\0' >"$r/proc/device-tree/soc/$n/compatible"
  printf 'okay\0' >"$r/proc/device-tree/soc/$n/status"
done
: >"$r/dev/accel/accel0"
chmod 0660 "$r/dev/accel/accel0"
# Two CPU clusters as on RK3576: four Cortex-A53 (policy0), four Cortex-A72
# (policy4).
for n in 0 1 2 3 4 5 6 7; do
  part=0xd03
  [[ $n -ge 4 ]] && part=0xd08
  printf 'processor\t: %s\nCPU part\t: %s\n\n' "$n" "$part"
done >"$r/proc/cpuinfo"
for p in policy0 policy4; do
  d="$r/sys/devices/system/cpu/cpufreq/$p"
  mkdir -p "$d"
  if [[ $p == policy0 ]]; then
    echo "0 1 2 3" >"$d/affected_cpus"
    f="408000 600000 816000 1008000 1200000 1416000 1608000 1800000 2016000"
  else
    echo "4 5 6 7" >"$d/affected_cpus"
    f="408000 600000 816000 1008000 1200000 1416000 1608000 1800000 2016000 2208000"
  fi
  echo "$f" >"$d/scaling_available_frequencies"
  echo "conservative ondemand userspace powersave performance schedutil" >"$d/scaling_available_governors"
  echo schedutil >"$d/scaling_governor"
  echo 408000 >"$d/scaling_min_freq"
  echo "${f##* }" >"$d/scaling_max_freq"
done
mkdir -p "$r/var/lib/dkms/aic8800-usb"

# Name the demo files' owner and group as on a board.
echo "root:x:$(id -u):$(id -g)::/root:/bin/sh" >"$r/etc/passwd"
echo "render:x:$(id -g):" >"$r/etc/group"
echo 16777216 >"$r/sys/class/mtd/mtd0/size"
