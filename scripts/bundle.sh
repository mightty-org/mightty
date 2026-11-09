#!/bin/sh
# Builds mightty.app (in target/release), signed ad hoc so macOS delivers
# its notifications. With --install, copies it to DIR (default
# ~/Applications), replacing an older copy.
# usage: scripts/bundle.sh [--install [DIR]]
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release --package mightty

app=target/release/mightty.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/mightty "$app/Contents/MacOS/mightty"
cp assets/mightty.icns "$app/Contents/Resources/mightty.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>mightty</string>
  <key>CFBundleExecutable</key><string>mightty</string>
  <key>CFBundleIconFile</key><string>mightty</string>
  <key>CFBundleIdentifier</key><string>app.mightty</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>mightty</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
EOF
codesign --force --sign - "$app"
echo "built $app ($version)"

if [ "${1:-}" = "--install" ]; then
  dir="${2:-$HOME/Applications}"
  mkdir -p "$dir"
  rm -rf "$dir/mightty.app"
  ditto "$app" "$dir/mightty.app"
  # Launch the installed copy, not the build: with both registered, macOS
  # may open either, and the Dock can show a stale icon for the one rebuilt
  # in place.
  lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  "$lsregister" -u "$PWD/$app" 2>/dev/null || true
  "$lsregister" -f "$dir/mightty.app"
  echo "installed $dir/mightty.app"
  echo "for the command line (mightty ctl, mightty mcp):"
  echo "  ln -s \"$dir/mightty.app/Contents/MacOS/mightty\" ~/.local/bin/mightty"
fi
