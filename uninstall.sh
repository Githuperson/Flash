#!/bin/sh
# Removes the Flash installation made by install.sh.
set -eu
FLASH_HOME="${FLASH_HOME:-$HOME/.flash}"
rm -rf "$FLASH_HOME" "${XDG_CACHE_HOME:-$HOME/.cache}/flash"
echo "Removed $FLASH_HOME and the build cache. Remove the PATH line from your shell profile by hand."
