#!/usr/bin/env bash
set -euo pipefail

VER="$(curl -fsSL https://api.github.com/repos/weavedwires/dumbpipe-over-chatmail/releases/latest \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"

if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
  case "$(uname -m)" in
    aarch64)       PKG="dumbpipe-android-arm64-v8a" ;;
    armv7l|armv8l) PKG="dumbpipe-android-armeabi-v7a" ;;
    x86_64)        PKG="dumbpipe-android-x86_64" ;;
    i686)          PKG="dumbpipe-android-x86" ;;
    *) echo "no Android build for $(uname -m)" >&2; exit 1 ;;
  esac
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fsSLo "$TMP/dumbpipe" \
    "https://github.com/weavedwires/dumbpipe-over-chatmail/releases/download/$VER/dumbpipe-$VER-$PKG"
  chmod +x "$TMP/dumbpipe"
  install -m 755 "$TMP/dumbpipe" "$PREFIX/bin"
  echo "dumbpipe $VER installed to $PREFIX/bin"
  exit 0
fi

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