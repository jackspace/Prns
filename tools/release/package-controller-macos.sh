#!/usr/bin/env bash
# Build an unsigned macOS PRNS Controller .app with hopspot-flash in Contents/Resources.
#
# This is not Gatekeeper-clean or notarized. Local testers may need:
#   xattr -dr com.apple.quarantine "PRNS Controller.app"
# or right-click → Open the first time.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
controller_dir="$root/personal-hopspot/remote-control-desktop"
hopspot_flash=""
out_dir="$root/target/controller-macos"
firmware_from=""
skip_build_flash=0
dx_bin="${DX:-dx}"

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
usage: tools/release/package-controller-macos.sh [options]

Build an unsigned macOS PRNS Controller .app and embed hopspot-flash in
Contents/Resources (the path the Controller flash resolver already looks for).

The zip is named for the host CPU (aarch64 or x86_64). Build on each runner
natively; do not cross-compile the Dioxus desktop bundle.

options:
  --hopspot-flash PATH   Reuse an existing hopspot-flash binary (skip cargo build)
  --firmware-from DIR    Reuse prebuilt board dirs from DIR/<slug>/ (ESP tools are
                         Linux-only; CI builds firmware on Ubuntu then passes it here)
  --out-dir DIR          Destination for the .app and zip (default: target/controller-macos)
  -h, --help             Show this help

requires: macOS, dioxus-cli 0.7.5 (dx), cargo, rustc
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

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "error: this packager only runs on macOS" >&2
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
echo " PRNS Controller macOS package (UNSIGNED — not notarized)"
echo " Gatekeeper may block first launch; remove quarantine or"
echo " right-click → Open when testing locally."
echo "============================================================"

if [[ "$skip_build_flash" -eq 0 ]]; then
    echo "building hopspot-flash (release)…"
    (
        cd "$root"
        cargo build --release --locked -p hopspot-flash
    )
    cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
    # cargo may report an absolute CARGO_TARGET_DIR; keep relative paths rooted.
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

bundle_staging="$controller_dir/target/controller-bundle-staging"
rm -rf "$bundle_staging"
mkdir -p "$bundle_staging"

echo "bundling PRNS Controller (.app)…"
(
    cd "$controller_dir"
    "$dx_bin" bundle --desktop --release --package-types macos --out-dir "$bundle_staging"
)

app_path=""
while IFS= read -r candidate; do
    if [[ -n "$app_path" ]]; then
        echo "error: multiple .app bundles found under $bundle_staging" >&2
        find "$bundle_staging" -maxdepth 4 -type d -name '*.app' -print >&2
        exit 1
    fi
    app_path="$candidate"
done < <(find "$bundle_staging" -maxdepth 4 -type d -name '*.app' | sort)

if [[ -z "$app_path" ]]; then
    echo "error: dx bundle did not produce a .app under $bundle_staging" >&2
    find "$bundle_staging" -maxdepth 5 -print >&2 || true
    exit 1
fi
app_name="$(basename "$app_path")"

resources="$app_path/Contents/Resources"
macos_dir="$app_path/Contents/MacOS"
if [[ ! -d "$macos_dir" ]]; then
    echo "error: malformed .app (missing Contents/MacOS): $app_path" >&2
    exit 1
fi
mkdir -p "$resources"
cp "$hopspot_flash" "$resources/hopspot-flash"
chmod +x "$resources/hopspot-flash"

flash_version="$("$resources/hopspot-flash" --version 2>/dev/null || echo "hopspot-flash unknown")"
printf '%s\n' "$flash_version" >"$resources/HOPSPOT_FLASH_VERSION.txt"

echo "embedded $flash_version → Contents/Resources/hopspot-flash"

echo "embedding tree firmware for Flash (unsigned / remote-control capable)…"
embed_args=(
    --out-dir "$resources"
    --hopspot-flash "$resources/hopspot-flash"
)
if [[ -n "$firmware_from" ]]; then
    embed_args+=(--firmware-from "$firmware_from")
fi
bash "$root/tools/release/embed-controller-firmware.sh" "${embed_args[@]}"
test -f "$resources/firmware/bundle.json"
test -f "$resources/firmware/heltec-v4/target.json"
test -f "$resources/firmware/heltec-v4-r8/target.json"
test -f "$resources/firmware/mesh-tower-v2/target.json"

# TCC: Finder/`open` aborts without these (SIGABRT / namespace TCC). dx bundle
# does not emit them from Dioxus.toml today, so inject after bundling.
plist="$app_path/Contents/Info.plist"
if [[ ! -f "$plist" ]]; then
    echo "error: malformed .app (missing Info.plist): $app_path" >&2
    exit 1
fi
bt_usage="PRNS Controller uses Bluetooth to discover and pair with Personal Hopspot devices."
lan_usage="PRNS Controller uses the local network to find and talk to Personal Hopspot devices over Wi-Fi."
plist_set_string() {
    local key="$1" value="$2"
    if plutil -extract "$key" raw "$plist" >/dev/null 2>&1; then
        plutil -replace "$key" -string "$value" "$plist"
    else
        plutil -insert "$key" -string "$value" "$plist"
    fi
}
plist_set_string NSBluetoothAlwaysUsageDescription "$bt_usage"
plist_set_string NSBluetoothPeripheralUsageDescription "$bt_usage"
plist_set_string NSLocalNetworkUsageDescription "$lan_usage"
plist_set_string CFBundleDisplayName "PRNS Controller"
plist_set_string CFBundleName "PRNS Controller"
# Must match Apple DNS-SD browse/publish types in prns-ffi mdns/macos.rs
# (_reticulum._tcp / _reticulum._udp). Wrong or missing entries prevent Local
# Network privacy from applying and Bonjour returns NoAuth.
bonjour_services='["_reticulum._tcp","_reticulum._udp","_prns._tcp"]'
if plutil -extract NSBonjourServices raw "$plist" >/dev/null 2>&1; then
    plutil -replace NSBonjourServices -json "$bonjour_services" "$plist"
else
    plutil -insert NSBonjourServices -json "$bonjour_services" "$plist"
fi
# Re-sign after mutating Info.plist / Resources. Prefer an Apple-issued identity:
# ad-hoc GUI apps often get Bonjour NoAuth with no Local Network prompt (TN3179).
codesign_identity="${CODESIGN_IDENTITY:-}"
if [[ -z "$codesign_identity" ]]; then
    codesign_identity="$(
        security find-identity -v -p codesigning 2>/dev/null \
            | awk -F'"' '/Developer ID Application|Apple Development|Mac Developer/{print $2; exit}'
    )"
fi
if [[ -n "$codesign_identity" ]]; then
    codesign --force --deep --options runtime --sign "$codesign_identity" "$app_path"
    echo "injected TCC + Bonjour + signed with: $codesign_identity"
else
    codesign --force --deep --sign - "$app_path"
    echo "injected TCC + Bonjour + ad-hoc re-sign" >&2
    echo "warning: no Apple codesigning identity; .app Local Network prompt/list may not work (TN3179). Set CODESIGN_IDENTITY or install a Development/Developer ID cert. For wifi-auto debug, run the MacOS binary from Terminal instead." >&2
fi

# Dioxus names the .app from the Cargo package; ship the product display name.
product_app_name="PRNS Controller.app"
if [[ "$app_name" != "$product_app_name" ]]; then
    renamed="$(dirname "$app_path")/$product_app_name"
    rm -rf "$renamed"
    mv "$app_path" "$renamed"
    app_path="$renamed"
    app_name="$product_app_name"
fi

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
# Drop stale names from earlier package runs (Cargo package .app vs product name).
rm -rf "$out_dir"/*.app "$out_dir"/*.zip
dest_app="$out_dir/$app_name"
ditto "$app_path" "$dest_app"

arch="$(host_arch)"
zip_name="PRNS-Controller-macos-${arch}-unsigned.zip"
rm -f "$out_dir"/PRNS-Controller-macos-*-unsigned.zip
(
    cd "$out_dir"
    ditto -c -k --sequesterRsrc --keepParent "$app_name" "$zip_name"
)

echo "app:  $dest_app"
echo "arch: $arch"
echo "zip:  $out_dir/$zip_name"
echo "done (unsigned)."
