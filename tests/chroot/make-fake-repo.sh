#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Build a small apt repository with stand-ins for the Armbian hardware
# packages of radxa-rock-4d (edge and vendor branches). They carry the same
# names, relations and the parts of the maintainer scripts that matter for
# a branch switch (from lib/functions/compilation/kernel-debs.sh and
# uboot.sh in armbian/build): linux-image points /boot/Image at its kernel,
# linux-dtb points /boot/dtb at its tree, linux-u-boot packages conflict on
# armbian-u-boot. The Release file says Origin: Armbian, so Roxora's apt
# pinning applies to it. Usage: make-fake-repo.sh <dir>
set -euo pipefail
umask 022

repo="${1:?usage: make-fake-repo.sh <dir>}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$repo"

# deb <name> <version> <dir with files> [control lines]
deb() {
  local name="$1" version="$2" dir="$3" extra="${4:-}"
  mkdir -p "$dir/DEBIAN"
  {
    printf 'Package: %s\nVersion: %s\nArchitecture: arm64\n' "$name" "$version"
    printf 'Maintainer: Test <test@example.invalid>\nSection: kernel\nPriority: optional\n'
    [[ -n "$extra" ]] && printf '%s\n' "$extra"
    printf 'Description: test stand-in for %s\n' "$name"
  } >"$dir/DEBIAN/control"
  dpkg-deb --root-owner-group --build "$dir" "$repo/${name}_${version}_arm64.deb" >/dev/null
}

kernel() {
  local branch="$1" family="$2" kver="$3" version="$4" overlay="$5" d
  d="$work/image-$branch"
  mkdir -p "$d/boot" "$d/lib/modules/$kver"
  echo "kernel $kver" >"$d/boot/vmlinuz-$kver"
  mkdir -p "$d/DEBIAN"
  printf '#!/bin/sh\nset -e\nln -sfv vmlinuz-%s /boot/Image\ntouch /boot/.next\n' "$kver" >"$d/DEBIAN/postinst"
  chmod 0755 "$d/DEBIAN/postinst"
  deb "linux-image-$branch-$family" "$version" "$d" \
    "Provides: linux-image, linux-image-armbian, armbian-$branch"

  d="$work/dtb-$branch"
  mkdir -p "$d/boot/dtb-$kver/rockchip/overlay" "$d/DEBIAN"
  echo dtb >"$d/boot/dtb-$kver/rockchip/rk3576-rock-4d.dtb"
  echo dtbo >"$d/boot/dtb-$kver/rockchip/overlay/$overlay-uart1.dtbo"
  printf '#!/bin/sh\nrm -rf /boot/dtb\n' >"$d/DEBIAN/preinst"
  printf '#!/bin/sh\nset -e\ncd /boot\nln -sfTv dtb-%s dtb\n' "$kver" >"$d/DEBIAN/postinst"
  chmod 0755 "$d/DEBIAN/preinst" "$d/DEBIAN/postinst"
  deb "linux-dtb-$branch-$family" "$version" "$d" \
    "Provides: linux-dtb, linux-dtb-armbian, armbian-$branch"

  d="$work/uboot-$branch"
  mkdir -p "$d/usr/lib/linux-u-boot-$branch-radxa-rock-4d" "$d/usr/lib/u-boot"
  truncate -s 1M "$d/usr/lib/linux-u-boot-$branch-radxa-rock-4d/u-boot-rockchip.bin"
  echo 'CONFIG_DEFAULT_FDT_FILE="rockchip/rk3576-rock-4d.dtb"' \
    >"$d/usr/lib/linux-u-boot-$branch-radxa-rock-4d/u-boot-config-target-1"
  cat >"$d/usr/lib/u-boot/platform_install.sh" <<END
DIR=/usr/lib/linux-u-boot-$branch-radxa-rock-4d
write_uboot_platform ()
{
    dd "if=\$1/u-boot-rockchip.bin" "of=\$2" bs=32k seek=1 conv=notrunc status=none
}
END
  deb "linux-u-boot-radxa-rock-4d-$branch" "$version" "$d" \
    "Provides: armbian-u-boot
Replaces: armbian-u-boot
Conflicts: armbian-u-boot"

  d="$work/headers-$branch"
  mkdir -p "$d/usr/src/linux-headers-$kver"
  deb "linux-headers-$branch-$family" "$version" "$d"
}

kernel edge rockchip64 7.3.4-edge-rockchip64 26.11.0-trunk rockchip-rk3576
kernel vendor rk35xx 6.1.115-vendor-rk35xx 26.5.1 rk3576

# Not a hardware package: the Roxora pinning must keep apt from installing it.
mkdir -p "$work/bsp"
deb armbian-config 26.8.0 "$work/bsp"

(
  cd "$repo"
  dpkg-scanpackages --multiversion . >Packages 2>/dev/null
  apt-ftparchive -o APT::FTPArchive::Release::Origin=Armbian \
    -o APT::FTPArchive::Release::Label=Armbian release . >"$work/Release"
  mv "$work/Release" Release
)
ls "$repo"
