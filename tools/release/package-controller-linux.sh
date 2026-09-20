#!/usr/bin/env bash
# Build an unsigned portable Linux PRNS Controller directory with hopspot-flash
# beside the Controller executable (Flash sidecar resolver).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
controller_dir="$root/personal-hopspot/remote-control-desktop"
hopspot_flash=""
out_dir="$root/target/controller-linux"
firmware_from=""
skip_build_flash=0
dx_bin="${DX:-dx}"
product_dir_name="PRNS-Controller"
exe_name="personal-hopspot-remote-control-desktop"

host_arch() {
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
usage: tools/release/package-controller-linux.sh [options]

Build an unsigned portable Linux PRNS Controller folder with hopspot-flash
next to the Controller binary (what the Flash resolver looks for).

The tarball is named for the host CPU (aarch64 or x86_64). Build on each
runner natively; WebKit/GTK desktop bundles are not cross-compiled here.

options:
  --hopspot-flash PATH   Reuse an existing hopspot-flash binary (skip cargo build)
  --firmware-from DIR    Reuse prebuilt board dirs from DIR/<slug>/ (ESP tools are
                         x86_64-oriented; CI builds firmware on Ubuntu then passes it)
  --out-dir DIR          Destination for the folder and archive (default: target/controller-linux)
  -h, --help             Show this help

requires: Linux, dioxus-cli 0.7.5 (dx), cargo, rustc, tar
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

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: this packager only runs on Linux" >&2
    exit 1
fi

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
echo " PRNS Controller Linux package (UNSIGNED portable)"
echo " Layout: PRNS-Controller/<exe> + hopspot-flash beside it"
echo "============================================================"

if [[ "$skip_build_flash" -eq 0 ]]; then
    echo "building hopspot-flash (release)…"
    (
        cd "$root"
        cargo build --release --locked -p hopspot-flash
    )
    cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
    if [[ "$cargo_target_dir" != /* ]]; then
        cargo_target_dir="$root/$cargo_target_dir"
    fi
    hopspot_flash="$cargo_target_dir/release/hopspot-flash"
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

# Prefer the newest release desktop binary under known target roots.
search_roots=("$controller_dir/target")
cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
if [[ "$cargo_target_dir" != /* ]]; then
    cargo_target_dir="$root/$cargo_target_dir"
fi
search_roots+=("$cargo_target_dir")

controller_exe=""
newest_mtime=0
while IFS= read -r candidate; do
    [[ -f "$candidate" && -x "$candidate" ]] || continue
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
rm -rf "$out_dir/$product_dir_name" "$out_dir"/*.tar.gz
dest_dir="$out_dir/$product_dir_name"
mkdir -p "$dest_dir"

cp "$controller_exe" "$dest_dir/$exe_name"
chmod +x "$dest_dir/$exe_name"
cp "$hopspot_flash" "$dest_dir/hopspot-flash"
chmod +x "$dest_dir/hopspot-flash"

flash_version="$("$dest_dir/hopspot-flash" --version 2>/dev/null || echo "hopspot-flash unknown")"
printf '%s\n' "$flash_version" >"$dest_dir/HOPSPOT_FLASH_VERSION.txt"
echo "embedded $flash_version → $product_dir_name/hopspot-flash"

echo "embedding tree firmware for Flash (unsigned / remote-control capable)…"
embed_args=(
    --out-dir "$dest_dir"
    --hopspot-flash "$dest_dir/hopspot-flash"
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
archive_name="PRNS-Controller-linux-${arch}-unsigned.tar.gz"
rm -f "$out_dir"/PRNS-Controller-linux-*-unsigned.tar.gz
(
    cd "$out_dir"
    tar -czf "$archive_name" "$product_dir_name"
)

echo "dir:  $dest_dir"
echo "arch: $arch"
echo "archive: $out_dir/$archive_name"
echo "done (unsigned)."
