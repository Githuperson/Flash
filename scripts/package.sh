#!/bin/sh
# Builds a release archive containing bin/ stdlib/ include/ and the licence:
#   dist/flash-<target>.tar.gz (+ .sha256)      or, for windows-* targets, dist/flash-<target>.zip (+ .sha256)
# Usage: scripts/package.sh linux-x86_64        (target name is only a label used by install.sh)
set -eu
TARGET="${1:?usage: package.sh <linux-x86_64|linux-aarch64|macos-aarch64|windows-x86_64>}"
cd "$(dirname "$0")/.."
cargo build --release -p flash
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/bin" dist
case "$TARGET" in
  windows-*) EXE=".exe" ;;
  *) EXE="" ;;
esac
cp "target/release/flash$EXE" "target/release/flashpkg$EXE" "$STAGE/bin/"
cp -R stdlib include "$STAGE/"
cp LICENSE "$STAGE/"
case "$TARGET" in
  windows-*) cp install.ps1 uninstall.ps1 install.bat "$STAGE/" ;;
esac
case "$TARGET" in
  windows-*)
    ARCHIVE="flash-$TARGET.zip"
    # PowerShell's Compress-Archive ships with every Windows runner; `zip` may not exist
    (cd "$STAGE" && powershell -NoProfile -Command "Compress-Archive -Path * -DestinationPath '$(cd - >/dev/null && pwd -W 2>/dev/null || pwd)/dist/$ARCHIVE' -Force")
    ;;
  *)
    ARCHIVE="flash-$TARGET.tar.gz"
    tar -C "$STAGE" -czf "dist/$ARCHIVE" .
    ;;
esac
rm -rf "$STAGE"
cd dist
if command -v sha256sum >/dev/null 2>&1; then sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
else shasum -a 256 "$ARCHIVE" > "$ARCHIVE.sha256"; fi
ls -l "$ARCHIVE"*
