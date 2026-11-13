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
user_overlays=sample-sensor hwtool-i2c8-m2 hwtool-fan
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
echo 4096 >"$r/sys/class/mtd/mtd0/erasesize"

# Pin header interfaces from data/interfaces, with I2C8 (M2) on.
ifs="$r/usr/share/hwtool/interfaces/radxa-rock-4d"
mkdir -p "$ifs"
cp "$here/../../data/interfaces/radxa-rock-4d/index.json" "$ifs/"
for f in "$here"/../../data/interfaces/radxa-rock-4d/*.dtso; do
  : >"$ifs/$(basename "$f" .dtso).dtbo"
done
: >"$r/boot/overlay-user/hwtool-i2c8-m2.dtbo"

# U-Boot version strings: the package has a newer build than SPI flash; the
# SD card has the package's. The SPI image carries the default environment.
new="U-Boot 2026.04-armbian-2026.04-S88dc-P1a2b-H7c3d-V1e2f-B9a8b-R448a (Nov 10 2026 - 12:27:04 +0000)"
old="U-Boot 2025.10-armbian-2025.10-S1234-P5678-H9abc-Vdef0-B1234-R448a (Oct 01 2025 - 08:00:00 +0000)"
env='arch=arm\0baudrate=1500000\0boot_targets=mmc0 mmc1 scsi nvme usb pxe dhcp\0bootcmd=bootflow scan -lb\0\0'
ub="$r/usr/lib/linux-u-boot-edge-radxa-rock-4d"
put() { printf "$2" | dd of="$1" bs=1 seek=$(($3)) conv=notrunc status=none; }
for f in u-boot-rockchip.bin u-boot-rockchip-spi.bin; do put "$ub/$f" "$new\\0" 0x60000; done
put "$ub/u-boot-rockchip-spi.bin" "\\0$env" 0x70000
printf 'CONFIG_MTD=y\nCONFIG_ENV_IS_IN_SPI_FLASH=y\nCONFIG_ENV_OFFSET=0xc00000\nCONFIG_ENV_SIZE=0x20000\n' \
  >"$ub/u-boot-config-target-1"
mkdir -p "$r/dev"
truncate -s 1M "$r/dev/mtd0" "$r/dev/mmcblk1" "$r/dev/mmcblk0"
put "$r/dev/mtd0" "$old\\0" 0x60000
put "$r/dev/mmcblk1" "$new\\0" 0x60000

# LEDs and thermal zones as in rk3576-rock-4d.dtb; armbian-led-state keeps them.
for l in "green:status:[none] timer heartbeat default-on mmc0 mmc1" \
  "blue:heartbeat:none timer [heartbeat] default-on mmc0 mmc1"; do
  d="$r/sys/class/leds/${l%:*}"
  mkdir -p "$d"
  echo "${l##*:}" >"$d/trigger"
  echo 1 >"$d/brightness"
  echo 1 >"$d/max_brightness"
done
mkdir -p "$r/lib/systemd/system"
: >"$r/lib/systemd/system/armbian-led-state.service"
n=0
for z in package:46250:115000:critical bigcore:47100:85000:passive littlecore:45300:85000:passive gpu:44800:85000:passive; do
  IFS=: read -r name t trip kind <<<"$z"
  d="$r/sys/class/thermal/thermal_zone$n"
  mkdir -p "$d"
  echo "$name-thermal" >"$d/type"
  echo "$t" >"$d/temp"
  echo "$trip" >"$d/trip_point_0_temp"
  echo "$kind" >"$d/trip_point_0_type"
  chmod 0444 "$d/trip_point_0_temp"
  n=$((n + 1))
done
d="$r/sys/class/thermal/cooling_device0"
mkdir -p "$d"
echo cpufreq-cpu0 >"$d/type"
echo 0 >"$d/cur_state"
echo 8 >"$d/max_state"

# Memory and swap after the first boot: 4 GiB, zram and cloud-init's /swap.img.
printf 'MemTotal:        3956800 kB\nMemFree:          812000 kB\nMemAvailable:    2861200 kB\n' >"$r/proc/meminfo"
printf 'Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0                              partition\t1978396\t\t10240\t\t5\n/swap.img                               file\t\t2097148\t\t0\t\t-2\n' >"$r/proc/swaps"
truncate -s 2G "$r/swap.img"
printf 'UUID=db9a14c3-db24-44e3-b7cc-062ae4e6a908 / ext4 defaults,commit=120,errors=remount-ro 0 1\n/swap.img\tnone\tswap\tsw\t0\t0\n' >"$r/etc/fstab"
mkdir -p "$r/proc/sys/vm"
echo 60 >"$r/proc/sys/vm/swappiness"
: >"$r/lib/systemd/system/armbian-zram-config.service"

# A fan set up with 'hwtool fan enable' on the ROCK 4D's connector, running.
mkdir -p "$r/etc/hwtool" "$r/sys/class/hwmon/hwmon3" "$r/sys/class/thermal/cooling_device1"
: >"$r/boot/overlay-user/hwtool-fan.dtbo"
cat >"$r/etc/hwtool/fan.json" <<'END'
{"pwm": "fan", "node": "pwm2_8ch_5", "pins": [], "curve": [[50, 30], [60, 50], [70, 80], [80, 100]]}
END
echo pwmfan >"$r/sys/class/hwmon/hwmon3/name"
echo 77 >"$r/sys/class/hwmon/hwmon3/pwm1"
echo pwm-fan >"$r/sys/class/thermal/cooling_device1/type"
echo 1 >"$r/sys/class/thermal/cooling_device1/cur_state"
echo 4 >"$r/sys/class/thermal/cooling_device1/max_state"
echo step_wise >"$r/sys/class/thermal/thermal_zone0/policy"
