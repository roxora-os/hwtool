#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Build the two Debian packages from binaries cargo has already built:
#
#   roxora-hwtool      /usr/bin/hwtool, no GUI dependencies
#   roxora-hwtool-gtk  /usr/bin/hwtool-gtk, desktop file, polkit policy
#
# Usage: packaging/build-debs.sh [target dir] [output dir]
# The target dir holds release/hwtool and release/hwtool-gtk (default: target).
set -euo pipefail
umask 022

here="$(cd "$(dirname "$0")" && pwd)"
top="$(dirname "$here")"
target="${1:-$top/target}"
out="${2:-$top/output}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$top/Cargo.toml" | head -1)"
arch="$(dpkg --print-architecture)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

# Shared-library dependencies of a binary, as dpkg-shlibdeps computes them.
shlibs() {
  local d="$work/shlibs"
  mkdir -p "$d/debian"
  printf 'Source: x\n\nPackage: x\nArchitecture: any\n' >"$d/debian/control"
  (cd "$d" && dpkg-shlibdeps -O "$1" 2>/dev/null) | sed -n 's/^shlibs:Depends=//p'
}

# build_deb <package> <depends> <recommends> <description> <long description>
build_deb() {
  local pkg="$1" depends="$2" recommends="$3" desc="$4" long="$5" root="$work/$1"
  local size
  mkdir -p "$root/DEBIAN" "$root/usr/share/doc/$pkg"
  install -m 0644 "$here/copyright" "$root/usr/share/doc/$pkg/copyright"
  size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"
  cat >"$root/DEBIAN/control" <<END
Package: $pkg
Version: $version
Architecture: $arch
Maintainer: Roxora Project <oss@gahingwoo.com>
Installed-Size: $size
Depends: $depends
${recommends:+Recommends: $recommends
}Section: admin
Priority: optional
Homepage: https://github.com/roxora-os/hwtool
Description: $desc
$(printf '%s\n' "$long" | fold -s -w 72 | sed 's/ *$//; s/^/ /')
END
  dpkg-deb --root-owner-group -Zxz --build "$root" "$out/${pkg}_${version}_${arch}.deb"
}

cli="$work/roxora-hwtool"
install -D -m 0755 "$target/release/hwtool" "$cli/usr/bin/hwtool"
# mtd-utils provides flashcp, needed only to write SPI flash.
build_deb roxora-hwtool \
  "$(shlibs "$cli/usr/bin/hwtool"), apt, util-linux, e2fsprogs, parted, rsync, udev" \
  "mtd-utils" \
  "device tree overlays, kernel branch and boot storage for Roxora boards" \
  "hwtool enables and disables device tree overlays, switches between the kernel branches the board supports, writes the bootloader with the board's U-Boot package and moves the running system from SD to eMMC or NVMe. Every change can be shown with --dry-run first."

if [[ -x "$target/release/hwtool-gtk" ]]; then
  gtk="$work/roxora-hwtool-gtk"
  install -D -m 0755 "$target/release/hwtool-gtk" "$gtk/usr/bin/hwtool-gtk"
  install -D -m 0644 "$here/io.github.roxora_os.HwTool.desktop" \
    "$gtk/usr/share/applications/io.github.roxora_os.HwTool.desktop"
  install -D -m 0644 "$here/io.github.roxora_os.hwtool.policy" \
    "$gtk/usr/share/polkit-1/actions/io.github.roxora_os.hwtool.policy"
  build_deb roxora-hwtool-gtk \
    "$(shlibs "$gtk/usr/bin/hwtool-gtk"), roxora-hwtool (= $version), pkexec" \
    "" \
    "GTK app for hwtool" \
    "Hardware Tool shows and changes device tree overlays, the kernel branch and boot storage of a Roxora board. It runs as the user and asks for the administrator password through polkit when it changes something."
else
  echo "build-debs: $target/release/hwtool-gtk not built, skipping roxora-hwtool-gtk" >&2
fi
ls -l "$out"/*.deb
