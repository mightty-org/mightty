#!/bin/sh
# Packs Mightty.app into a disk image: opened, it shows the app beside a
# link to Applications, to drag one onto the other. With an identity, the
# image is signed too, as notarization asks.
# usage: scripts/make-dmg.sh [--sign IDENTITY] [APP [DMG]]
#        (default target/release/Mightty.app and target/release/Mightty.dmg)
set -eu
cd "$(dirname "$0")/.."

identity=
if [ "${1:-}" = --sign ]; then
  identity=${2:?--sign needs an identity}
  shift 2
fi
app=${1:-target/release/Mightty.app}
dmg=${2:-target/release/Mightty.dmg}

stage=$(mktemp -d)
trap 'rm -r "$stage"' EXIT
ditto "$app" "$stage/Mightty.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
# Compressed and read-only; HFS+ so macOS 12 opens it.
hdiutil create -quiet -volname Mightty -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg"
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
fi
echo "built $dmg"
