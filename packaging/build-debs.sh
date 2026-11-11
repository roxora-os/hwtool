#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Build the two Debian packages from binaries cargo has already built:
#
#   roxora-hwtool      /usr/bin/hwtool and its overlays, no GUI dependencies
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
# Pin header interface overlays (data/interfaces, written by
# tools/gen-interfaces.py and checked by tests/interfaces/check.py).
for dir in "$top"/data/interfaces/*/; do
  board="$(basename "$dir")"
  dest="$cli/usr/share/hwtool/interfaces/$board"
  install -D -m 0644 "$dir/index.json" "$dest/index.json"
  for src in "$dir"/*.dtso; do
    dtc -q -@ -I dts -O dtb -o "$dest/$(basename "$src" .dtso).dtbo" "$src"
    chmod 0644 "$dest/$(basename "$src" .dtso).dtbo"
  done
done
# mtd-utils provides flashcp, needed only to write SPI flash.
build_deb roxora-hwtool \
  "$(shlibs "$cli/usr/bin/hwtool"), apt, util-linux, e2fsprogs, parted, rsync, udev" \
  "mtd-utils" \
  "device tree overlays, kernel branch and boot storage for Roxora boards" \
  "hwtool enables and disables device tree overlays and the 40-pin header's I2C, SPI, UART, PWM and 1-Wire, switches between the kernel branches the board supports, writes and updates the bootloader with the board's U-Boot package, moves the running system from SD to eMMC, NVMe or USB, and sets boot options, CPU frequency, LED triggers and the Wi-Fi country. Every change can be shown with --dry-run first."

if [[ -x "$target/release/hwtool-gtk" ]]; then
  gtk="$work/roxora-hwtool-gtk"
  install -D -m 0755 "$target/release/hwtool-gtk" "$gtk/usr/bin/hwtool-gtk"
  data="$top/data"
  app=io.github.roxora_os.HwTool
  install -D -m 0644 "$data/$app.desktop" "$gtk/usr/share/applications/$app.desktop"
  install -D -m 0644 "$data/$app.metainfo.xml" "$gtk/usr/share/metainfo/$app.metainfo.xml"
  # Icons from roxora-os/design (data/icons/README.md). The hicolor icon
  # cache and the desktop database are updated by the dpkg file triggers of
  # libgtk-4-bin/gtk-update-icon-cache and desktop-file-utils.
  for png in "$data"/icons/png/roxora-hwtool-*.png; do
    n="${png##*-}"
    n="${n%.png}"
    install -D -m 0644 "$png" "$gtk/usr/share/icons/hicolor/${n}x${n}/apps/$app.png"
  done
  install -D -m 0644 "$data/icons/roxora-hwtool.svg" "$gtk/usr/share/icons/hicolor/scalable/apps/$app.svg"
  install -D -m 0644 "$data/icons/roxora-hwtool-symbolic.svg" \
    "$gtk/usr/share/icons/hicolor/symbolic/apps/$app-symbolic.svg"
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
