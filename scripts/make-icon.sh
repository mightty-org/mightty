#!/bin/sh
# Regenerates assets/mightty.icns from scripts/make-icon.py.
# usage: scripts/make-icon.sh
set -eu
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
python3 -I scripts/make-icon.py "$work/icon.png"
set="$work/mightty.iconset"
mkdir "$set"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work/icon.png" --out "$set/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$work/icon.png" --out "$set/icon_${size}x${size}@2x.png" >/dev/null
done
mkdir -p assets
iconutil -c icns "$set" -o assets/mightty.icns
echo "wrote assets/mightty.icns"
