#!/usr/bin/env bash
# Build an unsigned portable Windows PRNS Controller directory with hopspot-flash.exe
# beside the Controller executable (Flash sidecar resolver).
#
# Intended for Windows CI (Git Bash / MSYS) or a native Windows shell with bash.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
controller_dir="$root/personal-hopspot/remote-control-desktop"
hopspot_flash=""
out_dir="$root/target/controller-windows"
firmware_from=""
skip_build_flash=0
dx_bin="${DX:-dx}"
product_dir_name="PRNS-Controller"
exe_name="personal-hopspot-remote-control-desktop.exe"
flash_name="hopspot-flash.exe"

# Optional override from CI matrix (--arch). Git Bash on windows-11-arm often
# reports uname -m as x86_64 under emulation, so prefer PROCESSOR_ARCHITECTURE.
arch_override=""

host_arch() {
    if [[ -n "$arch_override" ]]; then
        printf '%s\n' "$arch_override"
        return
    fi
    case "${PROCESSOR_ARCHITECTURE:-}${PROCESSOR_ARCHITEW6432:-}" in
        *ARM64*|*Arm64*) printf '%s\n' aarch64; return ;;
        *AMD64*) printf '%s\n' x86_64; return ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) printf '%s\n' aarch64 ;;
        x86_64|amd64) printf '%s\n' x86_64 ;;
        *)
            echo "error: unsupported CPU architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac
}

usage() {
    cat <<'EOF'
usage: tools/release/package-controller-windows.sh [options]

Build an unsigned portable Windows PRNS Controller folder with hopspot-flash.exe
next to the Controller binary (what the Flash resolver looks for).

The zip is named for the host CPU (aarch64 or x86_64). Build on each runner
natively.

options:
  --hopspot-flash PATH   Reuse an existing hopspot-flash.exe (skip cargo build)
  --firmware-from DIR    Reuse prebuilt board dirs from DIR/<slug>/ (ESP tools are
                         Linux-only; CI builds firmware on Ubuntu then passes it here)
  --out-dir DIR          Destination for the folder and zip (default: target/controller-windows)
  --arch ARCH            Force archive arch label: aarch64 or x86_64 (CI matrix)
  -h, --help             Show this help

requires: Windows (MINGW/MSYS/CYGWIN or similar), dioxus-cli 0.7.5 (dx), cargo, rustc
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --hopspot-flash)
            hopspot_flash="${2:-}"
            if [[ -z "$hopspot_flash" ]]; then
                echo "error: --hopspot-flash requires a path" >&2
                exit 2
            fi
            skip_build_flash=1
            shift 2
            ;;
        --firmware-from)
            firmware_from="${2:-}"
            if [[ -z "$firmware_from" ]]; then
                echo "error: --firmware-from requires a path" >&2
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
        --arch)
            arch_override="${2:-}"
            case "$arch_override" in
                aarch64|x86_64) ;;
                *)
                    echo "error: --arch must be aarch64 or x86_64" >&2
                    exit 2
                    ;;
            esac
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

uname_s="$(uname -s 2>/dev/null || echo unknown)"
case "$uname_s" in
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
        ;;
    *)
        # Some CI images report a Linux-like uname inside containers; also accept
        # explicit override when the host really is Windows.
        if [[ "${OS:-}" != "Windows_NT" && "${RUNNER_OS:-}" != "Windows" ]]; then
            echo "error: this packager only runs on Windows (found uname=$uname_s)" >&2
            exit 1
        fi
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

echo "============================================================"
echo " PRNS Controller Windows package (UNSIGNED portable)"
echo " Layout: PRNS-Controller/<exe> + hopspot-flash.exe beside it"
echo "============================================================"

if [[ "$skip_build_flash" -eq 0 ]]; then
    echo "building hopspot-flash (release)…"
    (
        cd "$root"
        cargo build --release --locked -p hopspot-flash
    )
    cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
    # Windows paths from env may be absolute already.
    if [[ "$cargo_target_dir" != /* && "$cargo_target_dir" != [A-Za-z]:* ]]; then
        cargo_target_dir="$root/$cargo_target_dir"
    fi
    hopspot_flash="$cargo_target_dir/release/$flash_name"
fi

if [[ ! -f "$hopspot_flash" ]]; then
    echo "error: hopspot-flash binary not found at $hopspot_flash" >&2
    exit 1
fi
hopspot_flash="$(cd "$(dirname "$hopspot_flash")" && pwd)/$(basename "$hopspot_flash")"

echo "building PRNS Controller (dx build --desktop --release)…"
(
    cd "$controller_dir"
    "$dx_bin" build --desktop --release
)

search_roots=("$controller_dir/target")
cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
if [[ "$cargo_target_dir" != /* && "$cargo_target_dir" != [A-Za-z]:* ]]; then
    cargo_target_dir="$root/$cargo_target_dir"
fi
search_roots+=("$cargo_target_dir")

controller_exe=""
newest_mtime=0
while IFS= read -r candidate; do
    [[ -f "$candidate" ]] || continue
    mtime=$(stat -c %Y "$candidate" 2>/dev/null || stat -f %m "$candidate" 2>/dev/null || echo 0)
    if (( mtime >= newest_mtime )); then
        newest_mtime=$mtime
        controller_exe="$candidate"
    fi
done < <(
    for root_dir in "${search_roots[@]}"; do
        [[ -d "$root_dir" ]] || continue
        find "$root_dir" -type f -name "$exe_name" \( -path '*/release/*' -o -path '*/desktop-release/*' \) 2>/dev/null
    done | sort -u
)

if [[ -z "$controller_exe" ]]; then
    echo "error: dx build did not produce $exe_name under release/desktop-release" >&2
    for root_dir in "${search_roots[@]}"; do
        echo "searched: $root_dir" >&2
    done
    exit 1
fi

echo "controller exe: $controller_exe"

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
rm -rf "$out_dir/$product_dir_name" "$out_dir"/*.zip
dest_dir="$out_dir/$product_dir_name"
mkdir -p "$dest_dir"

cp "$controller_exe" "$dest_dir/$exe_name"
cp "$hopspot_flash" "$dest_dir/$flash_name"

flash_version="$("$dest_dir/$flash_name" --version 2>/dev/null || echo "hopspot-flash unknown")"
printf '%s\n' "$flash_version" >"$dest_dir/HOPSPOT_FLASH_VERSION.txt"
echo "embedded $flash_version → $product_dir_name/$flash_name"

echo "embedding tree firmware for Flash (unsigned / remote-control capable)…"
embed_args=(
    --out-dir "$dest_dir"
    --hopspot-flash "$dest_dir/$flash_name"
)
if [[ -n "$firmware_from" ]]; then
    embed_args+=(--firmware-from "$firmware_from")
fi
bash "$root/tools/release/embed-controller-firmware.sh" "${embed_args[@]}"
test -f "$dest_dir/firmware/bundle.json"
test -f "$dest_dir/firmware/heltec-v4/target.json"
test -f "$dest_dir/firmware/heltec-v4-r8/target.json"
test -f "$dest_dir/firmware/mesh-tower-v2/target.json"

arch="$(host_arch)"
archive_name="PRNS-Controller-windows-${arch}-unsigned.zip"
rm -f "$out_dir"/PRNS-Controller-windows-*-unsigned.zip
(
    cd "$out_dir"
    if command -v zip >/dev/null 2>&1; then
        zip -r "$archive_name" "$product_dir_name"
    elif command -v powershell.exe >/dev/null 2>&1; then
        # Git Bash on windows-latest usually lacks zip(1); Compress-Archive
        # produces a real zip. Prefer that over `tar -a`, which may emit a
        # GNU tar stream with a .zip name.
        powershell.exe -NoProfile -Command \
            "Compress-Archive -Path '$product_dir_name' -DestinationPath '$archive_name' -Force"
    elif command -v powershell >/dev/null 2>&1; then
        powershell -NoProfile -Command \
            "Compress-Archive -Path '$product_dir_name' -DestinationPath '$archive_name' -Force"
    else
        echo "error: need zip or PowerShell Compress-Archive to build $archive_name" >&2
        exit 1
    fi
)

# Sanity-check: reject a mislabeled tar pretending to be zip.
if ! command -v unzip >/dev/null 2>&1; then
    :
elif ! unzip -t "$out_dir/$archive_name" >/dev/null 2>&1; then
    echo "error: $archive_name is not a readable zip archive" >&2
    exit 1
fi

echo "dir:  $dest_dir"
echo "arch: $arch"
echo "zip:  $out_dir/$archive_name"
echo "done (unsigned)."
