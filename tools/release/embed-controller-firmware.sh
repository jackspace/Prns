#!/usr/bin/env bash
# Build (or copy) Controller-bundled board firmware into a firmware/ staging tree.
#
# Layout written to OUT_DIR:
#   firmware/bundle.json
#   firmware/<board-slug>/target.json + artifact files
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out_dir=""
hopspot_flash=""
firmware_from=""
skip_build=0
boards_csv="heltec-v4,heltec-v4-r8,mesh-tower-v2"

usage() {
    cat <<'EOF'
usage: tools/release/embed-controller-firmware.sh --out-dir DIR [options]

Build tree firmware for Controller-bundled boards and stage it under DIR/firmware.

options:
  --out-dir DIR          Destination that will contain firmware/ (required)
  --hopspot-flash PATH   hopspot-flash binary (default: target/release/hopspot-flash)
  --firmware-from DIR    Reuse prebuilt board dirs from DIR/<slug>/ (skip hopspot-flash build)
  --boards LIST          Comma-separated board slugs (default: heltec-v4,heltec-v4-r8,mesh-tower-v2)
  -h, --help             Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out-dir)
            out_dir="${2:-}"
            shift 2
            ;;
        --hopspot-flash)
            hopspot_flash="${2:-}"
            shift 2
            ;;
        --firmware-from)
            firmware_from="${2:-}"
            skip_build=1
            shift 2
            ;;
        --boards)
            boards_csv="${2:-}"
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

if [[ -z "$out_dir" ]]; then
    echo "error: --out-dir is required" >&2
    exit 2
fi

IFS=',' read -r -a boards <<<"$boards_csv"
if [[ ${#boards[@]} -eq 0 ]]; then
    echo "error: --boards produced an empty list" >&2
    exit 2
fi

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
firmware_dir="$out_dir/firmware"
rm -rf "$firmware_dir"
mkdir -p "$firmware_dir"

git_sha="$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
git_sha12="$(printf '%s' "$git_sha" | cut -c1-12)"
version="$(
    cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --no-deps 2>/dev/null \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])' 2>/dev/null \
        || echo "0.0.0"
)"
# Prefer hopspot-flash package version when available.
if [[ -f "$root/personal-hopspot/flasher/Cargo.toml" ]]; then
    # Avoid embedding MSYS paths into native Windows Python (Path("/d/...") breaks).
    flash_ver="$(
        sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
            "$root/personal-hopspot/flasher/Cargo.toml" | head -n 1
    )"
    if [[ -n "$flash_ver" ]]; then
        version="$flash_ver"
    fi
fi

if [[ "$skip_build" -eq 0 ]]; then
    if [[ -z "$hopspot_flash" ]]; then
        cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
        if [[ "$cargo_target_dir" != /* && "$cargo_target_dir" != [A-Za-z]:* ]]; then
            cargo_target_dir="$root/$cargo_target_dir"
        fi
        if [[ -x "$cargo_target_dir/release/hopspot-flash.exe" ]]; then
            hopspot_flash="$cargo_target_dir/release/hopspot-flash.exe"
        else
            hopspot_flash="$cargo_target_dir/release/hopspot-flash"
        fi
    fi
    if [[ ! -f "$hopspot_flash" ]]; then
        echo "error: hopspot-flash not found at $hopspot_flash" >&2
        exit 1
    fi
    hopspot_flash="$(cd "$(dirname "$hopspot_flash")" && pwd)/$(basename "$hopspot_flash")"
fi

build_root="$out_dir/.firmware-build"
rm -rf "$build_root"
mkdir -p "$build_root"

staged_boards=()
for board in "${boards[@]}"; do
    board="$(echo "$board" | tr -d '[:space:]')"
    [[ -n "$board" ]] || continue
    dest="$firmware_dir/$board"
    mkdir -p "$dest"
    if [[ "$skip_build" -eq 1 ]]; then
        src="$firmware_from/$board"
        if [[ ! -f "$src/target.json" ]]; then
            echo "error: --firmware-from missing $src/target.json" >&2
            exit 1
        fi
        echo "copying prebuilt $board from $src"
        cp -R "$src"/. "$dest"/
    else
        # ${board}: macOS bash 3.2 + set -u treats "$board…" as a single unset name.
        echo "building bundled firmware for ${board}…"
        (
            cd "$root"
            "$hopspot_flash" build "$board" --out-root "$build_root"
        )
        # hopspot-flash writes out-root/firmware/hopspot/<board>/<version>/target.json
        artifact=""
        while IFS= read -r candidate; do
            artifact="$candidate"
        done < <(
            find "$build_root" -type f \( \
                -path "*/$board/target.json" -o \
                -path "*/$board/*/target.json" \
            \) | sort
        )
        if [[ -z "$artifact" ]]; then
            # Fallback: any target.json under a path segment named $board
            while IFS= read -r candidate; do
                case "$candidate" in
                    */"$board"/*|*/"$board"/target.json) artifact="$candidate" ;;
                esac
            done < <(find "$build_root" -type f -name target.json | sort)
        fi
        if [[ -z "$artifact" ]]; then
            echo "error: build for $board did not produce target.json under $build_root" >&2
            find "$build_root" -maxdepth 6 -print >&2 || true
            exit 1
        fi
        src_dir="$(dirname "$artifact")"
        echo "staging $board from $src_dir"
        cp -R "$src_dir"/. "$dest"/
    fi
    if [[ ! -f "$dest/target.json" ]]; then
        echo "error: staged $board is missing target.json" >&2
        exit 1
    fi
    staged_boards+=("$board")
done

bundle_json="$firmware_dir/bundle.json"
bundle_json_for_python="$bundle_json"
if command -v cygpath >/dev/null 2>&1; then
    # Native Windows Python cannot open MSYS paths like /d/a/...
    bundle_json_for_python="$(cygpath -w "$bundle_json")"
fi

python3 - "$bundle_json_for_python" "$version" "$git_sha" "$git_sha12" "${staged_boards[@]}" <<'PY'
import json
import sys
from pathlib import Path

out, version, git_sha, git_sha12, *boards = sys.argv[1:]
payload = {
    "schema": 1,
    "kind": "controller-bundled-firmware",
    "version": version,
    "git_sha": git_sha,
    "git_sha12": git_sha12,
    "boards": boards,
    "note": "Unsigned tree builds for Controller Flash until published releases include remote-control support.",
}
path = Path(out)
path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
print(f"wrote {path} boards={boards}")
PY

rm -rf "$build_root"
echo "firmware staged at $firmware_dir"
