# The `deny` snapshot drift on trunk — 2026-08-06

Notes from reproducing the `deny` failure on run `31069278105`, trunk `91098021`.

Short version: the drift is benign. Nothing shrank that shouldn't have.

## What fails

Advisories, bans, licenses and sources all pass. The failure is the unsafe inventory step:

```text
unsafe dependency snapshot drifted; review and run validation/security/unsafe-audit.py --write
```

## What actually moved

Regenerated in a scratch copy on Ubuntu 24.04 with rustc 1.96.0, matching the job. The delta is
**6 packages, 24 insertions and 50 deletions**, and fourteen of the eighteen graphs are untouched.
Two patterns.

**`proto-ipv4` arrives on the std graphs.** `smoltcp` 0.13.1 and `embassy-net` 0.9.1 gain it on
`android`, `daemon-*`, `desktop-*`, `host-c-*` and `ios`. The four esp32 graphs already had it and
do not move. It follows from `98820b6f`, where `tcp-dns` and `wifi-auto` enable
`embassy-net/proto-ipv4`.

**The esp32 graphs drop the compression chain.** `flate2`, `miniz_oxide`, `simd-adler32` and
`adler2` lose their `esp32-c6`, `esp32-s3-heltec`, `esp32-s3-heltec-r8` and `esp32-s3-tbeam`
entries. Host-side entries for the same crates are unchanged. It follows from `6c7bb4c7` taking the
source archive off the board:

```text
git show 089016c2:personal-hopspot/embedded/esp32/Cargo.lock | grep -c 'name = "flate2"'   → 1
git show 6c7bb4c7:personal-hopspot/embedded/esp32/Cargo.lock | grep -c 'name = "flate2"'   → 0

cargo tree -i flate2 \
  --manifest-path personal-hopspot/embedded/esp32/boards/heltec-v4/Cargo.toml \
  --target xtensa-esp32s3-none-elf
→ error: package ID specification `flate2` did not match any packages

cargo tree -i flate2 --manifest-path personal-hopspot/desktop/Cargo.toml
→ flate2 v1.1.9 └── png └── image └── embedded-graphics-simulator
```

So the embedded side genuinely stopped pulling it in and the desktop side still does. The baseline
getting smaller is the correct consequence of that change, not a control eroding.

Regenerating under both 1.96.0 and stable (1.97.1) gave byte-identical output, so none of this is a
compiler artifact.

**Nothing here has been committed.** The baseline on trunk is untouched.

## One gotcha for anyone reproducing it

`rust-toolchain.toml` pins `channel = "stable"`, currently 1.97.1. The `deny` job sets
`RUSTUP_TOOLCHAIN: 1.96.0` at job level, which overrides it. Locally there is no such override, so a
bare `cargo` inside the repo is not running what CI runs, and nothing says so.
