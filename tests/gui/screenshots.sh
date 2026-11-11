#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Screenshots of every page of the GTK app, wide and narrow, light and dark,
# plus the busy state, a confirmation dialog and an unknown board. Needs a
# hwtool-gtk built with the test-hooks feature and a display (CI runs this
# under xvfb-run; GDK_BACKEND=broadway works too).
#
# Usage: screenshots.sh <hwtool> <hwtool-gtk with test-hooks> <output dir>
set -euo pipefail

cli="$(realpath "${1:?usage: screenshots.sh <hwtool> <hwtool-gtk> <dir>}")"
gui="$(realpath "${2:?}")"
out="${3:?}"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
"$here/demo-root.sh" "$work/demo"
mkdir -p "$work/unknown" "$work/config/gtk-4.0"
# A fixed 96 dpi, no animations (a dialog is captured fully shown) and no
# user settings, so every display server renders alike.
printf '[Settings]\ngtk-xft-dpi=98304\ngtk-enable-animations=false\n' >"$work/config/gtk-4.0/settings.ini"
export XDG_CONFIG_HOME="$work/config"

shot() { # shot <file> <root> [VAR=value...]
  local file="$1" root="$2"
  shift 2
  env HWTOOL_CLI="$cli" HWTOOL_ROOT="$root" HWTOOL_GTK_SCREENSHOT="$out/$file.png" "$@" \
    timeout 60 "$gui" >/dev/null 2>&1 || true
  if [[ -s "$out/$file.png" ]]; then echo "$out/$file.png"; else
    echo "no screenshot: $file" >&2
    return 1
  fi
}

for theme in light dark; do
  dark=0
  [[ $theme == dark ]] && dark=1
  for page in board overlays kernel boot cpu npu; do
    shot "$page-wide-$theme" "$work/demo" HWTOOL_GTK_PAGE=$page HWTOOL_GTK_DARK=$dark HWTOOL_GTK_SIZE=900x700
    shot "$page-narrow-$theme" "$work/demo" HWTOOL_GTK_PAGE=$page HWTOOL_GTK_DARK=$dark HWTOOL_GTK_SIZE=360x700
  done
  for page in board boot npu kernel; do
    shot "$page-tall-$theme" "$work/demo" HWTOOL_GTK_PAGE=$page HWTOOL_GTK_DARK=$dark HWTOOL_GTK_SIZE=900x1200
  done
  shot "busy-$theme" "$work/demo" HWTOOL_GTK_PAGE=kernel HWTOOL_GTK_DARK=$dark HWTOOL_GTK_DEMO_BUSY=1
  shot "confirm-$theme" "$work/demo" HWTOOL_GTK_PAGE=boot HWTOOL_GTK_DARK=$dark HWTOOL_GTK_DEMO_CONFIRM=1
  shot "confirm-narrow-$theme" "$work/demo" HWTOOL_GTK_PAGE=boot HWTOOL_GTK_DARK=$dark HWTOOL_GTK_DEMO_CONFIRM=1 HWTOOL_GTK_SIZE=360x700
  shot "unknown-board-$theme" "$work/unknown" HWTOOL_GTK_PAGE=board HWTOOL_GTK_DARK=$dark
done
