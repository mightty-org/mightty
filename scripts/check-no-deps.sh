#!/bin/sh
# Fails if any package in Cargo.lock comes from outside this workspace.
set -eu
cd "$(dirname "$0")/.."
cargo generate-lockfile --offline >/dev/null 2>&1 || cargo generate-lockfile >/dev/null
if grep -q '^source = ' Cargo.lock; then
  echo "error: third-party dependencies found:" >&2
  grep -B2 '^source = ' Cargo.lock | grep '^name = ' >&2
  exit 1
fi
echo "ok: no third-party dependencies"
