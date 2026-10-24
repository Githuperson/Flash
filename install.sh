#!/bin/sh
# Flash installer (builds from source; there are no prebuilt binaries yet).
#
#   From a checkout:      ./install.sh
#   Straight from GitHub: curl -sSf https://raw.githubusercontent.com/Githuperson/Flash/main/install.sh | sh
#
# Installs into $FLASH_HOME (default ~/.flash):
#   bin/flash  bin/flashpkg  lib/libflash_rt.a  stdlib/  include/
# Needs: a Rust toolchain (https://rustup.rs), git (only for the curl form) and a C compiler (`cc`).
# Options (environment): FLASH_HOME=dir  FLASH_REPO=url  FLASH_REF=branch  FLASH_PROFILE=release|debug
set -eu

FLASH_HOME="${FLASH_HOME:-$HOME/.flash}"
FLASH_REPO="${FLASH_REPO:-https://github.com/Githuperson/Flash}"
FLASH_REF="${FLASH_REF:-main}"
FLASH_PROFILE="${FLASH_PROFILE:-release}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' is required but was not found. $2" >&2; exit 1; }; }
need cargo "Install Rust from https://rustup.rs"
need cc "Install a C compiler (Xcode command line tools on macOS, build-essential on Debian/Ubuntu)."

# Use the checkout we are in, otherwise clone.
SRC=""
if [ -f "$(dirname "$0")/Cargo.toml" ] && [ -d "$(dirname "$0")/crates/flash" ] 2>/dev/null; then
  SRC="$(cd "$(dirname "$0")" && pwd)"
else
  need git "Install git."
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  echo "Cloning $FLASH_REPO ($FLASH_REF)..."
  git clone --quiet --depth 1 --branch "$FLASH_REF" "$FLASH_REPO" "$TMP/flash"
  SRC="$TMP/flash"
fi

echo "Building Flash ($FLASH_PROFILE)... this takes a few minutes the first time."
FLAG="--release"; DIR=release
if [ "$FLASH_PROFILE" = "debug" ]; then FLAG=""; DIR=debug; fi
(cd "$SRC" && cargo build $FLAG -p flash -p flash-rt --quiet)

TARGET="${CARGO_TARGET_DIR:-$SRC/target}/$DIR"
mkdir -p "$FLASH_HOME/bin" "$FLASH_HOME/lib"
cp "$TARGET/flash" "$TARGET/flashpkg" "$FLASH_HOME/bin/"
cp "$TARGET/libflash_rt.a" "$FLASH_HOME/lib/"
rm -rf "$FLASH_HOME/stdlib" "$FLASH_HOME/include"
cp -R "$SRC/stdlib" "$FLASH_HOME/stdlib"
cp -R "$SRC/include" "$FLASH_HOME/include"

echo
echo "Flash installed in $FLASH_HOME"
"$FLASH_HOME/bin/flash" --version
case ":$PATH:" in
  *":$FLASH_HOME/bin:"*) ;;
  *) echo
     echo "Add Flash to your PATH:"
     echo "    export PATH=\"$FLASH_HOME/bin:\$PATH\""
     echo "(put that line in ~/.zshrc or ~/.bashrc to make it permanent)" ;;
esac
echo "Try it:  echo 'say(\"Hello, world!\")' > hello.flash && flash run hello.flash"
