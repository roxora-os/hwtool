#!/bin/bash
# shellcheck disable=SC2016 # dpkg-query formats use ${...} literally
# SPDX-License-Identifier: GPL-2.0-only
#
# Kernel branch switch in a real Ubuntu 24.04 chroot, with dpkg and apt
# doing the work. Needs root (CI runs it with sudo on an arm64 runner).
#
# Usage: package-switch.sh <roxora-hwtool .deb>
#
# The Armbian packages are replaced by stand-ins from make-fake-repo.sh,
# served as a local repository whose Release says Origin: Armbian, so the
# apt pinning Roxora images ship decides what apt may install.
set -euo pipefail

deb="$(realpath "${1:?usage: package-switch.sh <roxora-hwtool .deb>}")"
here="$(cd "$(dirname "$0")" && pwd)"
r="$(mktemp -d /tmp/hwtool-chroot.XXXXXX)"
mirror="${MIRROR:-http://ports.ubuntu.com/ubuntu-ports}"

cleanup() {
  swapoff "$r/swap.img" 2>/dev/null || true
  for m in sys/firmware dev/pts dev proc sys ""; do
    umount "$r/$m" 2>/dev/null || true
  done
}
trap cleanup EXIT

# Rebuild the index of the fake repository after removing a package.
reindex() {
  (
    cd "$1"
    dpkg-scanpackages --multiversion . >Packages 2>/dev/null
    apt-ftparchive -o APT::FTPArchive::Release::Origin=Armbian \
      -o APT::FTPArchive::Release::Label=Armbian release . >/tmp/Release
    mv /tmp/Release Release
  )
}
step() { printf '\n=== %s\n' "$*"; }
in_root() { chroot "$r" env LC_ALL=C.UTF-8 DEBIAN_FRONTEND=noninteractive "$@"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

step "debootstrap noble"
debootstrap --variant=minbase --arch=arm64 noble "$r" "$mirror" >/tmp/debootstrap.log
# The chroot as a mount of its own, so /proc/self/mountinfo inside it has
# a / entry (hwtool swap reads the root file system's type there).
mount --bind "$r" "$r"
mount -t proc proc "$r/proc"
mount -t sysfs sys "$r/sys"
mount --bind /dev "$r/dev"
mount --bind /dev/pts "$r/dev/pts"
cp /etc/resolv.conf "$r/etc/resolv.conf"

step "fake Armbian repository and Roxora's pinning"
"$here/make-fake-repo.sh" "$r/srv/repo"
echo "deb [trusted=yes] file:/srv/repo ./" >"$r/etc/apt/sources.list.d/armbian.list"
# userpatches/roxora/apt/roxora-hardware.pref.in in roxora-os/build,
# rendered with @ORIGIN@=Armbian, under the name images install it as.
cat >"$r/etc/apt/preferences.d/roxora-hardware.pref" <<'END'
Package: linux-image-* linux-dtb-* linux-headers-* linux-u-boot-* armbian-firmware*
Pin: release o=Armbian
Pin-Priority: 500

Package: charsiu
Pin: release o=Armbian
Pin-Priority: 500

Package: *
Pin: release o=Armbian
Pin-Priority: -1
END
in_root apt-get update -q
if in_root apt-get install -y -q armbian-config >/dev/null 2>&1; then
  fail "the pinning let apt install armbian-config"
fi

step "install hwtool from its package"
cp "$deb" "$r/tmp/"
in_root apt-get install -y -q "/tmp/$(basename "$deb")"

step "image state: radxa-rock-4d on edge"
in_root apt-get install -y -q linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 \
  linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64
cat >"$r/etc/armbian-release" <<'END'
BOARD=radxa-rock-4d
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
END
cp "$r/boot/armbianEnv.txt" /tmp/env.orig
echo script >"$r/boot/boot.scr"

in_root hwtool kernel status
in_root hwtool overlays list

step "overlays: enable uart1 on edge"
in_root hwtool overlays enable uart1 --yes
grep -qx 'overlays=uart1' "$r/boot/armbianEnv.txt" || fail "overlays= not written"
in_root hwtool overlays list | grep -q '\[x\] uart1' || fail "uart1 not shown as enabled"

step "dry run changes nothing"
state() {
  in_root dpkg-query -W -f '${Package} ${Version} ${db:Status-Abbrev}\n' | sort
  find "$r/boot" "$r/var/lib/hwtool" "$r/var/backups/hwtool" "$r/var/log/hwtool" \
    "$r/etc/apt" -printf '%p %s %T@\n' 2>/dev/null | sort || true
}
before="$(state | sha256sum)"
in_root hwtool kernel switch vendor --dry-run
in_root hwtool boot write-bootloader --target spi --dry-run || true
after="$(state | sha256sum)"
[[ "$before" == "$after" ]] || fail "dry run changed something"

step "a switch whose package fails is rolled back"
# A vendor device tree package whose postinst fails, as a broken kernel
# hook would. hwtool must put everything back.
b="$(mktemp -d)"
mkdir -p "$b/boot/dtb-6.1.115-vendor-rk35xx/rockchip" "$b/DEBIAN"
echo dtb >"$b/boot/dtb-6.1.115-vendor-rk35xx/rockchip/rk3576-rock-4d.dtb"
printf 'Package: linux-dtb-vendor-rk35xx\nVersion: 26.5.2\nArchitecture: arm64\nMaintainer: Test <test@example.invalid>\nProvides: linux-dtb, armbian-vendor\nDescription: broken on purpose\n' >"$b/DEBIAN/control"
printf '#!/bin/sh\nrm -rf /boot/dtb\n' >"$b/DEBIAN/preinst"
printf '#!/bin/sh\necho "test: this postinst fails on purpose" >&2\nexit 1\n' >"$b/DEBIAN/postinst"
chmod 0755 "$b/DEBIAN/preinst" "$b/DEBIAN/postinst"
dpkg-deb --root-owner-group --build "$b" "$r/srv/repo/linux-dtb-vendor-rk35xx_26.5.2_arm64.deb" >/dev/null
reindex "$r/srv/repo"
in_root apt-get update -q
cp "$r/boot/armbianEnv.txt" /tmp/env.before-fail
if in_root hwtool kernel switch vendor --yes >/tmp/fail.txt 2>&1; then
  cat /tmp/fail.txt
  fail "the switch with a failing package succeeded"
fi
cat /tmp/fail.txt
grep -q 'done: Put back /boot/armbianEnv.txt' /tmp/fail.txt || fail "armbianEnv.txt not restored"
grep -q 'done: Purge the vendor packages' /tmp/fail.txt || fail "vendor packages not purged"
grep -q 'done: Reinstall the old packages' /tmp/fail.txt || fail "old packages not reinstalled"
grep -q '/boot/Image -> vmlinuz-7.3.4-edge-rockchip64' /tmp/fail.txt || fail "/boot/Image not reported on edge"
diff -u /tmp/env.before-fail "$r/boot/armbianEnv.txt" || fail "armbianEnv.txt differs after the rollback"
[[ "$(readlink "$r/boot/Image")" == vmlinuz-7.3.4-edge-rockchip64 ]] || fail "/boot/Image: $(readlink "$r/boot/Image")"
[[ "$(readlink "$r/boot/dtb")" == dtb-7.3.4-edge-rockchip64 ]] || fail "/boot/dtb: $(readlink "$r/boot/dtb")"
status="$(in_root dpkg-query -W -f '${db:Status-Abbrev} ${Package}\n')"
grep linux- <<<"$status"
for p in linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64; do
  grep -q "^ii  $p\$" <<<"$status" || fail "$p is not installed after the rollback"
done
if grep -E '^[a-z][a-zA-Z] +linux-[a-z-]*vendor' <<<"$status" | grep -qv '^un'; then
  fail "vendor packages left after the rollback"
fi
in_root hwtool kernel status | tee /tmp/status.txt
grep -q 'did not finish' /tmp/status.txt || fail "kernel status does not say the switch did not finish"
ls "$r"/var/log/hwtool/*.log
rm "$r/srv/repo/linux-dtb-vendor-rk35xx_26.5.2_arm64.deb"
reindex "$r/srv/repo"
in_root apt-get update -q

step "no terminal and no --yes: refused"
if in_root hwtool kernel switch vendor </dev/null; then
  fail "switch ran without confirmation"
fi

step "switch to vendor"
# apt uses file: repositories in place and caches nothing, so put the edge
# U-Boot package where apt would have downloaded it. That one is copied;
# the others are rebuilt from the dpkg database.
cp "$r"/srv/repo/linux-u-boot-radxa-rock-4d-edge_*.deb "$r/var/cache/apt/archives/"
in_root hwtool kernel switch vendor --dry-run | tee /tmp/switch-dry.txt
grep -q 'rebuild the package from the dpkg database' /tmp/switch-dry.txt || fail "dry run does not show the rebuild"
grep -q 'copy /var/cache/apt/archives/linux-u-boot-radxa-rock-4d-edge_26.11.0-trunk_arm64.deb' /tmp/switch-dry.txt \
  || fail "dry run does not show the U-Boot copy"
in_root hwtool kernel switch vendor --yes
[[ "$(readlink "$r/boot/Image")" == vmlinuz-6.1.115-vendor-rk35xx ]] || fail "/boot/Image: $(readlink "$r/boot/Image")"
[[ "$(readlink "$r/boot/dtb")" == dtb-6.1.115-vendor-rk35xx ]] || fail "/boot/dtb: $(readlink "$r/boot/dtb")"
grep -qx 'overlay_prefix=rk3576' "$r/boot/armbianEnv.txt" || fail "overlay_prefix not switched"
installed="$(in_root dpkg-query -W -f '${db:Status-Abbrev} ${Package}\n' | awk '$1 == "ii" { print $2 }')"
echo "$installed" | grep -E '^linux-' | sort
for p in linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor linux-headers-vendor-rk35xx; do
  grep -qx "$p" <<<"$installed" || fail "$p is not installed"
done
for p in linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64; do
  grep -qx "$p" <<<"$installed" && fail "$p is still installed"
done
ls "$r"/var/backups/hwtool/*/boot/armbianEnv.txt >/dev/null || fail "no backup of armbianEnv.txt"
in_root hwtool overlays list
in_root hwtool kernel status

step "saved packages"
ls -l "$r"/var/lib/hwtool/packages/*/
for p in linux-image-edge-rockchip64 linux-dtb-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64; do
  f="$(ls "$r"/var/lib/hwtool/packages/*/"${p}"_26.11.0-trunk_arm64.deb)" || fail "$p was not saved"
  dpkg-deb --info "$f" | grep -q "Package: $p" || fail "$f is not a valid package"
done
dpkg-deb --contents "$r"/var/lib/hwtool/packages/*/linux-image-edge-rockchip64_*.deb | grep -q 'boot/vmlinuz-7.3.4-edge-rockchip64' \
  || fail "the rebuilt kernel package has no kernel"

step "the repository stops offering the edge U-Boot package"
rm "$r"/srv/repo/linux-u-boot-radxa-rock-4d-edge_*.deb
reindex "$r/srv/repo"
in_root apt-get update -q
in_root apt-cache policy linux-u-boot-radxa-rock-4d-edge

step "revert to edge"
in_root hwtool kernel revert --dry-run | tee /tmp/revert-dry.txt
grep -q 'apt no longer offers this version; installing linux-u-boot-radxa-rock-4d-edge' /tmp/revert-dry.txt \
  || fail "revert does not use the saved U-Boot package"
in_root hwtool kernel revert --yes
[[ "$(readlink "$r/boot/Image")" == vmlinuz-7.3.4-edge-rockchip64 ]] || fail "/boot/Image after revert: $(readlink "$r/boot/Image")"
grep -qx 'overlay_prefix=rockchip-rk3576' "$r/boot/armbianEnv.txt" || fail "overlay_prefix not restored"
in_root dpkg-query -W linux-image-edge-rockchip64 linux-u-boot-radxa-rock-4d-edge linux-headers-edge-rockchip64
[[ "$(in_root dpkg-query -W -f '${Version}' linux-u-boot-radxa-rock-4d-edge)" == 26.11.0-trunk ]] \
  || fail "edge U-Boot was not reinstalled"
[[ -f "$r/usr/lib/linux-u-boot-edge-radxa-rock-4d/u-boot-rockchip.bin" ]] || fail "edge U-Boot files missing"
[[ "$(ls "$r/var/lib/hwtool/packages")" != "" ]] || fail "the revert saved nothing"
if in_root dpkg-query -W -f '${db:Status-Abbrev}\n' linux-image-vendor-rk35xx 2>/dev/null | grep -q '^ii'; then
  fail "vendor kernel still installed after revert"
fi
grep -v '^overlays=' "$r/boot/armbianEnv.txt" | diff -u /tmp/env.orig - || fail "armbianEnv.txt differs after the round trip"

step "one backup per change"
ls -d "$r"/var/backups/hwtool/*/
[[ "$(find "$r/var/backups/hwtool" -name armbianEnv.txt | wc -l)" -ge 4 ]] || fail "expected a backup per change"

step "kernel headers, hold, boot options, user overlay"
# Headers: edge headers are installed from the image state; remove, then
# install again.
in_root hwtool kernel headers remove --dry-run
in_root hwtool kernel headers remove --yes
if in_root dpkg-query -W -f '${db:Status-Abbrev}\n' linux-headers-edge-rockchip64 2>/dev/null | grep -q '^ii'; then
  fail "headers still installed"
fi
in_root hwtool kernel headers install --yes
in_root dpkg-query -W linux-headers-edge-rockchip64
# Hold: apt-mark records it, switch refuses, unhold releases.
in_root hwtool kernel hold --yes
in_root apt-mark showhold | tee /tmp/held.txt
grep -qx linux-image-edge-rockchip64 /tmp/held.txt || fail "image not held"
in_root hwtool kernel status | grep -q 'Held packages: linux-' || fail "status does not show the hold"
if in_root hwtool kernel switch vendor --dry-run 2>/tmp/held-switch.txt; then
  fail "switch did not refuse while held"
fi
grep -q 'hwtool kernel unhold' /tmp/held-switch.txt || fail "refusal does not name unhold"
in_root hwtool kernel unhold --yes
[[ -z "$(in_root apt-mark showhold)" ]] || fail "packages still held"
# Boot options: the image's boot.cmd is a stand-in here, so give it the real
# script's bootargs line.
cp "$here/../../crates/core/tests/fixtures/boot-rk3576.cmd" "$r/boot/boot.cmd"
in_root hwtool bootenv show
in_root hwtool bootenv set verbosity=7 --yes
grep -qx verbosity=7 "$r/boot/armbianEnv.txt" || fail "verbosity not set"
if in_root hwtool bootenv set 'extraargs=a;reboot' --yes; then fail "unsafe extraargs accepted"; fi
in_root hwtool bootenv unset verbosity --yes
# A user overlay: device-tree-compiler comes from Ubuntu, dtc compiles it.
cat >"$r/tmp/roxora-test.dtso" <<'END'
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
END
in_root hwtool overlays add /tmp/roxora-test.dtso --dry-run | tee /tmp/ov-dry.txt
grep -q 'Install the device tree compiler' /tmp/ov-dry.txt || fail "dry run does not install dtc"
in_root hwtool overlays add /tmp/roxora-test.dtso --yes
[[ -s "$r/boot/overlay-user/roxora-test.dtbo" ]] || fail "overlay not installed"
grep -q '^user_overlays=.*roxora-test' "$r/boot/armbianEnv.txt" || fail "overlay not enabled"
fdtdump "$r/boot/overlay-user/roxora-test.dtbo" 2>/dev/null | grep -q 'roxora-hwtool-test' \
  || in_root dtc -I dtb -O dts "/boot/overlay-user/roxora-test.dtbo" | grep -q roxora-hwtool-test \
  || fail "compiled overlay does not contain the property"
in_root hwtool overlays list | grep -q 'roxora-test'
in_root hwtool overlays remove roxora-test --yes
[[ ! -e "$r/boot/overlay-user/roxora-test.dtbo" ]] || fail "overlay not removed"

step "NPU status and charsiu"
# The NPU as on a ROCK 4D: hwtool reads the device tree from
# /sys/firmware/devicetree/base and the rocket module of the kernel the board
# boots next (the chroot's running kernel is the runner's).
npu_fake=no
if mount -t tmpfs tmpfs "$r/sys/firmware" 2>/dev/null; then
  for n in npu@27700000 npu@27708000; do
    mkdir -p "$r/sys/firmware/devicetree/base/soc/$n"
    printf 'rockchip,rk3576-rknn-core\0' >"$r/sys/firmware/devicetree/base/soc/$n/compatible"
    printf 'okay\0' >"$r/sys/firmware/devicetree/base/soc/$n/status"
  done
  mkdir -p "$r/lib/modules/7.3.4-edge-rockchip64/kernel/drivers/accel/rocket"
  : >"$r/lib/modules/7.3.4-edge-rockchip64/kernel/drivers/accel/rocket/rocket.ko"
  npu_fake=yes
else
  echo "cannot mount a tmpfs on /sys/firmware in the chroot; the install path is not tested"
fi
# charsiu from apt.armbian.com, behind the same pin, if the runner reaches it;
# otherwise a stand-in of origin Armbian in the local repository.
source=apt.armbian.com
if curl -fsSL --max-time 60 https://apt.armbian.com/armbian.key | gpg --dearmor >/tmp/armbian.gpg 2>/dev/null \
  && [[ -s /tmp/armbian.gpg ]]; then
  install -m 0644 /tmp/armbian.gpg "$r/usr/share/keyrings/armbian-test.gpg"
  echo "deb [signed-by=/usr/share/keyrings/armbian-test.gpg] http://apt.armbian.com noble noble-utils" \
    >"$r/etc/apt/sources.list.d/armbian-utils.list"
  in_root apt-get update -q || source=local
  in_root apt-cache policy charsiu | grep -q 'apt.armbian.com' || source=local
else
  source=local
fi
if [[ $source == local ]]; then
  rm -f "$r/etc/apt/sources.list.d/armbian-utils.list"
  c="$(mktemp -d)"
  mkdir -p "$c/usr/bin" "$c/DEBIAN"
  printf '#!/bin/sh\necho charsiu stand-in\n' >"$c/usr/bin/charsiu"
  chmod 0755 "$c/usr/bin/charsiu"
  printf 'Package: charsiu\nVersion: 0.1.0\nArchitecture: arm64\nMaintainer: Test <test@example.invalid>\nDepends: libc6\nRecommends: curl | wget, whiptail\nDescription: stand-in\n' >"$c/DEBIAN/control"
  dpkg-deb --root-owner-group --build "$c" "$r/srv/repo/charsiu_0.1.0_arm64.deb" >/dev/null
  reindex "$r/srv/repo"
  in_root apt-get update -q
fi
echo "charsiu source: $source"
in_root apt-cache policy charsiu
in_root hwtool npu status
in_root hwtool --json npu status >/tmp/npu.json
grep -q '"candidate": "0.1.0"' /tmp/npu.json || fail "no charsiu candidate"
if [[ $npu_fake == yes ]]; then
  grep -q '"can_install": true' /tmp/npu.json || fail "hwtool does not see the NPU"
  in_root hwtool npu install-charsiu --dry-run | tee /tmp/charsiu-dry.txt
  grep -q 'Inst charsiu' /tmp/charsiu-dry.txt || fail "dry run does not show apt's resolution"
  grep -q 'charsiu pull' /tmp/charsiu-dry.txt || fail "dry run does not mention charsiu pull"
  in_root hwtool npu install-charsiu --yes
  in_root dpkg-query -W charsiu
  in_root hwtool npu status | tee /tmp/npu.txt
  grep -q 'charsiu: 0.1.0 installed' /tmp/npu.txt || fail "status does not show charsiu installed"
  in_root hwtool kernel switch vendor --dry-run | tee /tmp/vendor-dry.txt
  grep -q 'charsiu 0.1.0 is installed. It needs the edge kernel' /tmp/vendor-dry.txt \
    || fail "switch dry run does not warn about charsiu"
  in_root hwtool npu remove-charsiu --yes
  if in_root dpkg-query -W -f '${db:Status-Abbrev}\n' charsiu 2>/dev/null | grep -q '^ii'; then
    fail "charsiu still installed after remove-charsiu"
  fi
  umount "$r/sys/firmware"
  # No NPU node: install is refused before apt runs.
  if in_root hwtool npu install-charsiu --dry-run; then
    fail "install-charsiu did not refuse without an NPU"
  fi
fi

step "swap file: make, resize, swappiness, off"
# What cloud-init 25.1 writes at the first boot, before the file exists.
printf '/swap.img\tnone\tswap\tsw\t0\t0\n' >>"$r/etc/fstab"
in_root hwtool swap show
in_root hwtool swap set 64M --dry-run
in_root hwtool swap set 64M --yes
in_root grep -q '^/swap.img ' /proc/swaps || fail "/swap.img is not in use"
test "$(stat -c %a "$r/swap.img")" = 600 || fail "/swap.img is not mode 600"
test "$(stat -c %s "$r/swap.img")" = $((64 << 20)) || fail "/swap.img is not 64 MiB"
test "$(grep -c swap.img "$r/etc/fstab")" = 1 || fail "fstab has not exactly one swap.img line"
grep -qP '^/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0$' "$r/etc/fstab" \
  || fail "fstab line is not in cloud-init's format"
in_root hwtool swap set 128M --yes
test "$(stat -c %s "$r/swap.img")" = $((128 << 20)) || fail "/swap.img was not resized"
test "$(grep -c swap.img "$r/etc/fstab")" = 1 || fail "resize duplicated the fstab line"
in_root hwtool swap show | tee /tmp/swap.txt
grep -q '^Active swap: /swap.img (file)' /tmp/swap.txt || fail "show does not list the active file"
if in_root hwtool swap set 100T --force --yes 2>/tmp/swap-big.txt; then fail "100T accepted"; fi
grep -q 'does not fit' /tmp/swap-big.txt || fail "100T refusal unclear"
old_swappiness="$(cat /proc/sys/vm/swappiness)"
in_root hwtool swap swappiness 20 --yes
test "$(cat /proc/sys/vm/swappiness)" = 20 || fail "swappiness not set"
grep -qx 'vm.swappiness = 20' "$r/etc/sysctl.d/60-hwtool-swap.conf" || fail "swappiness not kept"
echo "$old_swappiness" >/proc/sys/vm/swappiness
in_root hwtool swap off --yes
test ! -e "$r/swap.img" || fail "/swap.img still there"
! grep -q swap.img "$r/etc/fstab" || fail "fstab still names /swap.img"
! in_root grep -q '^/swap.img ' /proc/swaps || fail "/swap.img still in use"
ls "$r/var/backups/hwtool/"*/etc/fstab >/dev/null || fail "no fstab backup"
grep -q 'size: 0' "$r/etc/cloud/cloud.cfg.d/99-zz-hwtool-swap.cfg" || fail "cloud-init swap not turned off"
in_root hwtool swap set 64M --yes
test ! -e "$r/etc/cloud/cloud.cfg.d/99-zz-hwtool-swap.cfg" || fail "swap set left the cloud-init override"
in_root hwtool swap off --yes

step "boot status"
in_root hwtool boot status
echo "PASS"
