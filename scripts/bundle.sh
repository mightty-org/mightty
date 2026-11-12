#!/bin/sh
# Builds Mightty.app (in target/release), signed ad hoc so macOS delivers
# its notifications.
#   --universal      for Apple Silicon and Intel both (needs the rustup
#                    targets aarch64-apple-darwin and x86_64-apple-darwin)
#   --sign IDENTITY  signed with IDENTITY ("Developer ID Application: …"),
#                    with the hardened runtime notarization asks for
#   --install [DIR]  then copied to DIR (default ~/Applications), replacing
#                    an older copy
# usage: scripts/bundle.sh [--universal] [--sign IDENTITY] [--install [DIR]]
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

universal= identity=- install= dir="$HOME/Applications"
while [ $# -gt 0 ]; do
  case $1 in
    --universal) universal=1 ;;
    --sign) identity=${2:?--sign needs an identity}; shift ;;
    --install)
      install=1
      if [ $# -gt 1 ] && [ "${2#--}" = "$2" ]; then dir=$2; shift; fi
      ;;
    *) echo "bundle.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

app=target/release/Mightty.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if [ -n "$universal" ]; then
  # The oldest macOS it runs on, as Info.plist says.
  export MACOSX_DEPLOYMENT_TARGET=12.0
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --package mightty --target "$target"
  done
  lipo -create -output "$app/Contents/MacOS/mightty" \
    target/aarch64-apple-darwin/release/mightty target/x86_64-apple-darwin/release/mightty
else
  cargo build --release --package mightty
  cp target/release/mightty "$app/Contents/MacOS/mightty"
fi
cp assets/mightty.icns "$app/Contents/Resources/mightty.icns"
# The themes (iTerm2-Color-Schemes, as Ghostty bundles them), and their licence.
mkdir -p "$app/Contents/Resources/themes"
cp assets/themes/* "$app/Contents/Resources/themes/"
cp assets/THEMES-LICENSE "$app/Contents/Resources/THEMES-LICENSE"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Mightty</string>
  <key>CFBundleExecutable</key><string>mightty</string>
  <key>CFBundleIconFile</key><string>mightty</string>
  <key>CFBundleIdentifier</key><string>org.mightty.Mightty</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Mightty</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 the Mightty contributors. MIT License.</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
EOF
if [ "$identity" = - ]; then
  codesign --force --sign - "$app"
else
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
fi
echo "built $app ($version)"

if [ -n "$install" ]; then
  mkdir -p "$dir"
  # The old lowercase name too, on a case-sensitive disk.
  rm -rf "$dir/mightty.app" "$dir/Mightty.app"
  ditto "$app" "$dir/Mightty.app"
  # Launch the installed copy, not the build: with both registered, macOS
  # may open either, and the Dock can show a stale icon for the one rebuilt
  # in place.
  lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  "$lsregister" -u "$PWD/$app" 2>/dev/null || true
  "$lsregister" -f "$dir/Mightty.app"
  echo "installed $dir/Mightty.app"
  echo "for the command line (mightty ctl, mightty mcp):"
  echo "  ln -s \"$dir/Mightty.app/Contents/MacOS/mightty\" ~/.local/bin/mightty"
fi
