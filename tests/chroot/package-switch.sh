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
  for m in dev/pts dev proc sys; do
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
mount -t proc proc "$r/proc"
mount -t sysfs sys "$r/sys"
mount --bind /dev "$r/dev"
mount --bind /dev/pts "$r/dev/pts"
cp /etc/resolv.conf "$r/etc/resolv.conf"

step "fake Armbian repository and Roxora's pinning"
"$here/make-fake-repo.sh" "$r/srv/repo"
echo "deb [trusted=yes] file:/srv/repo ./" >"$r/etc/apt/sources.list.d/armbian.list"
# The same records as userpatches/roxora/apt/roxora-armbian.pref in
# roxora-os/build (linux-headers-* added there too).
cat >"$r/etc/apt/preferences.d/roxora-armbian.pref" <<'END'
Package: linux-image-* linux-dtb-* linux-headers-* linux-u-boot-* armbian-firmware*
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
before="$(in_root dpkg-query -W -f '${Package} ${Version}\n' | sort | sha256sum)"
in_root hwtool kernel switch vendor --dry-run
after="$(in_root dpkg-query -W -f '${Package} ${Version}\n' | sort | sha256sum)"
[[ "$before" == "$after" ]] || fail "dry run changed packages"

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
[[ "$(find "$r/var/backups/hwtool" -name armbianEnv.txt | wc -l)" -eq 3 ]] || fail "expected 3 backups"

step "boot status"
in_root hwtool boot status
echo "PASS"
