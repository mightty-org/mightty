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
cp crates/mightty-ffi/include/mightty.h "$prefix/include/"
sed -e "s|@PREFIX@|$prefix|" -e "s|@VERSION@|$version|" crates/mightty-ffi/mightty.pc.in > "$prefix/lib/pkgconfig/mightty.pc"
echo "installed libmightty $version to $prefix"
