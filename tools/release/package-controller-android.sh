#!/usr/bin/env bash
# Build an unsigned PRNS Controller APK (no Flash / hopspot-flash).
# Flash remains desktop-only; this package is remote-control only.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
controller_dir="$root/personal-hopspot/remote-control-desktop"
out_dir="$root/target/controller-android"
dx_bin="${DX:-dx}"
target=""

usage() {
    cat <<'EOF'
usage: tools/release/package-controller-android.sh [--arch aarch64] [options]

Build an unsigned PRNS Controller APK for Android arm64-v8a.
Flash is not included (desktop-only).

Dioxus 0.7 / manganis only support 64-bit Android; armv7 is not available.

options:
  --arch aarch64         Android ABI (default: aarch64 → aarch64-linux-android)
  --out-dir DIR          Destination for the APK (default: target/controller-android)
  -h, --help             Show this help

requires: dioxus-cli 0.7.5 (dx), cargo, rustc Android targets, JDK 17,
          ANDROID_HOME + NDK (ndk/27.2.12479018 recommended)
EOF
}

arch="aarch64"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)
            arch="${2:-}"
            if [[ -z "$arch" ]]; then
                echo "error: --arch requires aarch64" >&2
                exit 2
            fi
            shift 2
            ;;
        --out-dir)
            out_dir="${2:-}"
            if [[ -z "$out_dir" ]]; then
                echo "error: --out-dir requires a path" >&2
                exit 2
            fi
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$arch" in
    aarch64)
        target="aarch64-linux-android"
        ;;
    armv7)
        echo "error: armv7 Android is not supported (Dioxus/manganis are 64-bit only)" >&2
        exit 2
        ;;
    *)
        echo "error: unsupported --arch '$arch' (use aarch64)" >&2
        exit 2
        ;;
esac

if ! command -v "$dx_bin" >/dev/null 2>&1; then
    echo "error: dioxus-cli (dx) not found; install dioxus-cli 0.7.5" >&2
    exit 1
fi

dx_version="$("$dx_bin" --version 2>/dev/null || true)"
if [[ "$dx_version" != *"0.7.5"* ]]; then
    echo "error: need dioxus-cli 0.7.5; found: ${dx_version:-unknown}" >&2
    exit 1
fi

: "${ANDROID_HOME:?ANDROID_HOME is not set}"
: "${ANDROID_NDK_HOME:=${ANDROID_HOME}/ndk/27.2.12479018}"
export ANDROID_NDK_HOME
export NDK_HOME="${NDK_HOME:-$ANDROID_NDK_HOME}"

if [[ ! -d "$ANDROID_NDK_HOME" ]]; then
    echo "error: ANDROID_NDK_HOME not found: $ANDROID_NDK_HOME" >&2
    exit 1
fi

if ! rustup target list --installed | grep -qx "$target"; then
    echo "error: rustup target $target is not installed" >&2
    echo "  rustup target add $target" >&2
    exit 1
fi

echo "============================================================"
echo " PRNS Controller Android package (UNSIGNED APK, no Flash)"
echo " arch=$arch  target=$target"
echo "============================================================"

echo "building PRNS Controller (dx build --android --release)…"
(
    cd "$controller_dir"
    "$dx_bin" build --android --release --target "$target" \
        --no-default-features --features mobile
)

# Dioxus 0.7 currently emits app-debug.apk even for --release android builds.
search_roots=(
    "$controller_dir/target/dx"
    "$root/target/dx"
)
cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
if [[ "$cargo_target_dir" != /* ]]; then
    cargo_target_dir="$root/$cargo_target_dir"
fi
search_roots+=("$cargo_target_dir/dx")

apk=""
newest_mtime=0
while IFS= read -r candidate; do
    [[ -f "$candidate" ]] || continue
    mtime=$(stat -c %Y "$candidate" 2>/dev/null || stat -f %m "$candidate" 2>/dev/null || echo 0)
    if (( mtime >= newest_mtime )); then
        newest_mtime=$mtime
        apk="$candidate"
    fi
done < <(
    for search_root in "${search_roots[@]}"; do
        [[ -d "$search_root" ]] || continue
        find "$search_root" -type f \( -name 'app-release*.apk' -o -name 'app-debug.apk' \) 2>/dev/null || true
    done
)

if [[ -z "$apk" || ! -f "$apk" ]]; then
    echo "error: could not find built APK under target/dx" >&2
    exit 1
fi

mkdir -p "$out_dir"
artifact="PRNS-Controller-android-${arch}-unsigned.apk"
dest="$out_dir/$artifact"
cp "$apk" "$dest"

size=$(wc -c <"$dest" | tr -d ' ')
if [[ "$size" -lt 1_000_000 ]]; then
    echo "error: packaged APK is only $size bytes: $dest" >&2
    exit 1
fi

echo "built $dest ($size bytes) from $apk"
echo "install: adb install -r \"$dest\""
echo "Flash is not available on Android; use a desktop Controller package to flash boards."
