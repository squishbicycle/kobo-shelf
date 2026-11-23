#!/bin/sh
# Regenerate AppIcon.icns from icon.svg (run after editing the SVG).
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
swift render.swift icon.svg "$tmp/1024.png"
set=$tmp/AppIcon.iconset
mkdir "$set"
for s in 16 32 128 256 512; do
  sips -z $s $s "$tmp/1024.png" --out "$set/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$tmp/1024.png" --out "$set/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set" -o AppIcon.icns
rm -rf "$tmp"
echo "Wrote icon/AppIcon.icns"
