#!/bin/sh
# Builds a release archive: dist/flash-<target>.tar.gz (+ .sha256) containing bin/ lib/ stdlib/ include/.
# Usage: scripts/package.sh linux-x86_64        (target name is only a label used by install.sh)
set -eu
TARGET="${1:?usage: package.sh <linux-x86_64|macos-aarch64|macos-x86_64>}"
cd "$(dirname "$0")/.."
cargo build --release -p flash -p flash-rt
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/bin" "$STAGE/lib" dist
cp target/release/flash target/release/flashpkg "$STAGE/bin/"
cp target/release/libflash_rt.a "$STAGE/lib/"
cp -R stdlib include "$STAGE/"
cp LICENSE "$STAGE/"
tar -C "$STAGE" -czf "dist/flash-$TARGET.tar.gz" .
rm -rf "$STAGE"
cd dist
if command -v sha256sum >/dev/null 2>&1; then sha256sum "flash-$TARGET.tar.gz" > "flash-$TARGET.tar.gz.sha256"
else shasum -a 256 "flash-$TARGET.tar.gz" > "flash-$TARGET.tar.gz.sha256"; fi
ls -l "flash-$TARGET.tar.gz"*
