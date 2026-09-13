#!/usr/bin/env bash
set -euo pipefail

VER="$(curl -fsSL https://api.github.com/repos/weavedwires/dumbpipe-over-chatmail/releases/latest \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"

case "$(uname -m)" in
  x86_64) PKG="dumbpipe-linux-x86_64.tar.gz" ;;
  *) echo "no Linux build for $(uname -m)" >&2; exit 1 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSLo "$TMP/dumbpipe.tgz" \
  "https://github.com/weavedwires/dumbpipe-over-chatmail/releases/download/$VER/dumbpipe-$VER-$PKG"
tar -xzf "$TMP/dumbpipe.tgz" -C "$TMP"
sudo install -m 755 "$TMP/dumbpipe" /usr/local/bin
echo "dumbpipe $VER installed to /usr/local/bin"