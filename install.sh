#!/bin/sh
# Flash installer.
#   curl form: downloads the prebuilt archive for your platform from the latest GitHub release
#              (checksum verified); falls back to building from source if none is available.
#   checkout:  builds from source.  Force source build with FLASH_FROM_SOURCE=1.
#
#   From a checkout:      ./install.sh
#   Straight from GitHub: curl -sSf https://raw.githubusercontent.com/Githuperson/Flash/main/install.sh | sh
#
# Installs into $FLASH_HOME (default ~/.flash):
#   bin/flash  bin/flashpkg  lib/libflash_rt.a  stdlib/  include/
# Needs: a Rust toolchain (https://rustup.rs), git (only for the curl form) and a C compiler (`cc`).
# Options (environment): FLASH_HOME=dir  FLASH_REPO=url  FLASH_REF=branch  FLASH_PROFILE=release|debug
#   FLASH_RELEASE_URL=base url of the release files   FLASH_FROM_SOURCE=1
set -eu

FLASH_HOME="${FLASH_HOME:-$HOME/.flash}"
FLASH_REPO="${FLASH_REPO:-https://github.com/Githuperson/Flash}"
FLASH_REF="${FLASH_REF:-main}"
FLASH_PROFILE="${FLASH_PROFILE:-release}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' is required but was not found. $2" >&2; exit 1; }; }
FLASH_RELEASE_URL="${FLASH_RELEASE_URL:-$FLASH_REPO/releases/latest/download}"

in_checkout=0
if [ -f "$(dirname "$0")/Cargo.toml" ] && [ -d "$(dirname "$0")/crates/flash" ] 2>/dev/null; then in_checkout=1; fi

finish() {
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
  echo "(Installing flash needs a C compiler on your machine to link programs: cc)"
  exit 0
}

# Prebuilt binary (curl form only).
if [ "$in_checkout" = 0 ] && [ -z "${FLASH_FROM_SOURCE:-}" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) PLAT=linux-x86_64 ;;
    Darwin-arm64) PLAT=macos-aarch64 ;;
    Darwin-x86_64) PLAT=macos-x86_64 ;;
    *) PLAT="" ;;
  esac
  if [ -n "$PLAT" ] && command -v curl >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
    DL="$(mktemp -d)"
    if curl -fsSL "$FLASH_RELEASE_URL/flash-$PLAT.tar.gz" -o "$DL/f.tar.gz" && curl -fsSL "$FLASH_RELEASE_URL/flash-$PLAT.tar.gz.sha256" -o "$DL/f.sha256"; then
      want="$(cut -d' ' -f1 "$DL/f.sha256")"
      if command -v sha256sum >/dev/null 2>&1; then got="$(sha256sum "$DL/f.tar.gz" | cut -d' ' -f1)"; else got="$(shasum -a 256 "$DL/f.tar.gz" | cut -d' ' -f1)"; fi
      [ "$want" = "$got" ] || { echo "error: checksum mismatch for the downloaded archive; refusing to install." >&2; rm -rf "$DL"; exit 1; }
      mkdir -p "$FLASH_HOME"
      rm -rf "$FLASH_HOME/bin" "$FLASH_HOME/lib" "$FLASH_HOME/stdlib" "$FLASH_HOME/include"
      tar -xzf "$DL/f.tar.gz" -C "$FLASH_HOME"
      rm -rf "$DL"
      finish
    fi
    rm -rf "$DL"
    echo "No prebuilt archive for $PLAT found at $FLASH_RELEASE_URL; building from source instead."
  fi
fi

need cargo "Install Rust from https://rustup.rs (or install a release archive)."
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
cp "$TARGET/libflash_rt.a" "$TARGET/flash-stub" "$FLASH_HOME/lib/"
rm -rf "$FLASH_HOME/stdlib" "$FLASH_HOME/include"
cp -R "$SRC/stdlib" "$FLASH_HOME/stdlib"
cp -R "$SRC/include" "$FLASH_HOME/include"

finish
