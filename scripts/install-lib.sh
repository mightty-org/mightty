#!/bin/sh
# Builds libmightty and installs the library, header and pkg-config file.
# usage: scripts/install-lib.sh [PREFIX]   (default /usr/local)
set -eu
cd "$(dirname "$0")/.."
prefix="${1:-/usr/local}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release --lib --package mightty-ffi

mkdir -p "$prefix/lib" "$prefix/include" "$prefix/lib/pkgconfig"
for lib in target/release/libmightty.a target/release/libmightty.dylib target/release/libmightty.so; do
  [ -f "$lib" ] && cp "$lib" "$prefix/lib/"
done
# The dylib still names its build directory as where it lives; programs
# linked against it would load it from there. Name the installed copy,
# then sign it again (Apple silicon won't load it otherwise).
if [ -f "$prefix/lib/libmightty.dylib" ] && command -v install_name_tool >/dev/null; then
  install_name_tool -id "$prefix/lib/libmightty.dylib" "$prefix/lib/libmightty.dylib"
  codesign --force --sign - "$prefix/lib/libmightty.dylib" 2>/dev/null
fi
cp crates/mightty-ffi/include/mightty.h "$prefix/include/"
sed -e "s|@PREFIX@|$prefix|" -e "s|@VERSION@|$version|" crates/mightty-ffi/mightty.pc.in > "$prefix/lib/pkgconfig/mightty.pc"
echo "installed libmightty $version to $prefix"
