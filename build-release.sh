#!/usr/bin/env bash
set -euo pipefail

# Build release binaries for:
#   - desktop Linux x86_64          (x86_64-unknown-linux-gnu)
#   - Android arm64-v8a  (armv8)    (aarch64-linux-android)
#   - Android armeabi-v7a (armv7)   (armv7-linux-androideabi)
#   - Android x86_64     (x64)      (x86_64-linux-android)
#   - Android x86        (x86)      (i686-linux-android)
#
# Artifacts are copied to ./dist/.
#
# Requirements:
#   - Linux x86_64 host
#   - Rust toolchain with the rustup targets installed:
#       rustup target add x86_64-unknown-linux-gnu \
#           aarch64-linux-android armv7-linux-androideabi \
#           x86_64-linux-android i686-linux-android
#   - Android NDK with a Linux x86_64 toolchain. Location is auto-detected
#     from (in order): --ndk <path>, $ANDROID_NDK_ROOT, $NDK_HOME, then the
#     newest version under ~/Android/Sdk/ndk.
#
# Usage:
#   ./build-release.sh [--ndk <path>] [--api <level>]
#     --api   Android API level for the binaries (default: 24)

API_LEVEL="24"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ndk)
            NDK="$2"
            shift 2
            ;;
        --api)
            API_LEVEL="$2"
            shift 2
            ;;
        -h | --help)
            sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

if [[ "${NDK:-}" = "" ]]; then
    for cand in "${ANDROID_NDK_ROOT:-}" "${NDK_HOME:-}"; do
        if [[ -n "$cand" ]] && [[ -f "$cand/source.properties" ]]; then
            NDK="$cand"
            break
        fi
    done
fi

if [[ "${NDK:-}" = "" ]] && [[ -d "$HOME/Android/Sdk/ndk" ]]; then
    NDK="$(find "$HOME/Android/Sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
fi

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: only a Linux x86_64 host is supported (NDK prebuilt 'linux-x86_64')" >&2
    exit 1
fi

TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
BIN="$TC/bin"

[[ -d "$TC" ]] || { echo "error: NDK toolchain not found at $TC" >&2; exit 1; }
for tool in "$BIN/llvm-ar" "$BIN/ld.lld"; do
    [[ -x "$tool" ]] || { echo "error: missing NDK tool: $tool" >&2; exit 1; }
done

# cargo target triple -> NDK clang name (armv7 uses the 'armv7a' prefix)
declare -A ANDROID_TARGETS=(
    [aarch64-linux-android]="aarch64-linux-android${API_LEVEL}-clang"
    [armv7-linux-androideabi]="armv7a-linux-androideabi${API_LEVEL}-clang"
    [x86_64-linux-android]="x86_64-linux-android${API_LEVEL}-clang"
    [i686-linux-android]="i686-linux-android${API_LEVEL}-clang"
)

missing=""
for t in "${!ANDROID_TARGETS[@]}"; do
    rustup target list --installed | grep -qx "$t" || missing="$missing $t"
done
if [[ -n "$missing" ]]; then
    echo "error: missing rustup targets:$missing" >&2
    echo "  run: rustup target add$missing" >&2
    exit 1
fi

mkdist() {
    mkdir -p dist
}

build_android() {
    local target="$1" clang="$2"
    [[ -x "$BIN/$clang" ]] || { echo "error: missing $BIN/$clang (wrong API level?)" >&2; exit 1; }
    local env_key="${target//-/_}" # armv7_linux_androideabi
    local env_key_up="${env_key^^}" # ARMV7_LINUX_ANDROIDEABI (cargo wants uppercase)
    export "CARGO_TARGET_${env_key_up}_LINKER=$BIN/$clang"
    export "CC_${env_key}=$BIN/$clang"     # cc-rs wants lowercase target
    export "AR_${env_key}=$BIN/llvm-ar"
    echo "==> building $target"
    cargo build --release --target "$target"
}

echo "==> building desktop x86_64-unknown-linux-gnu"
cargo build --release

for target in "${!ANDROID_TARGETS[@]}"; do
    build_android "$target" "${ANDROID_TARGETS[$target]}"
done

mkdist
cp target/release/dumbpipe                    dist/dumbpipe-linux-x86_64
cp target/aarch64-linux-android/release/dumbpipe dist/dumbpipe-android-arm64-v8a
cp target/armv7-linux-androideabi/release/dumbpipe dist/dumbpipe-android-armeabi-v7a
cp target/x86_64-linux-android/release/dumbpipe   dist/dumbpipe-android-x86_64
cp target/i686-linux-android/release/dumbpipe      dist/dumbpipe-android-x86

echo "==> done. artifacts in ./dist/:"
ls -lh dist/