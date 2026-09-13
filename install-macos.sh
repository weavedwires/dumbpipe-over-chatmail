#!/usr/bin/env bash
set -euo pipefail

VER="$(curl -fsSL https://api.github.com/repos/weavedwires/dumbpipe-over-chatmail/releases/latest \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"

case "$(uname -m)" in
  arm64)  BIN_DIR="/opt/homebrew/bin"; PKG="dumbpipe-macos-aarch64.tar.gz" ;;
  x86_64) BIN_DIR="/usr/local/bin";  PKG="dumbpipe-macos-x86_64.tar.gz" ;;
  *) echo "no macOS build for $(uname -m)" >&2; exit 1 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSLo "$TMP/dumbpipe.tgz" \
  "https://github.com/weavedwires/dumbpipe-over-chatmail/releases/download/$VER/dumbpipe-$VER-$PKG"
tar -xzf "$TMP/dumbpipe.tgz" -C "$TMP"
sudo install -m 755 "$TMP/dumbpipe" "$BIN_DIR"
echo "dumbpipe $VER installed to $BIN_DIR"