# B1: halow-only build shape — complete, 2026-08-10

Goal (Doc's tasking B1): a t-halow firmware shape with the 2.4 GHz radios compiled out, so a HaLow
link test cannot be satisfied by ESP-NOW/BLE/WiFi behind its back.

Branch `feat/halow-only-soak` off `feat/halow-at-poc` (0bb10308).

## Gates

All on chido (Windows, esp toolchain), from `personal-hopspot/embedded/esp32`:

| build | result |
|---|---|
| `cargo build -p hopspot-t-halow --no-default-features` (halow-only) | exit 0, **zero warnings** |
| `cargo t-halow` (default, all radios) | exit 0, **zero warnings** |
| `cargo heltec-v4-r8` (shared-tree regression control) | exit 0, **zero warnings** |
| `bash validation/hygiene/fmt-docs.sh` (Git Bash) | three markers, `FMT_DOC_CHECK_GATE_OK` |

The two shipping builds are the control: every file changed here is shared with them, so a green
halow-only build on its own would prove nothing about regressions.

Not run: WSL host tests and clippy (this crate is Xtensa-only and CI never compiles it, so the
bench build *is* the gate), and hardware boot-verify — see "Still owed" below.

## Size delta, default vs halow-only

`xtensa-esp32s3-elf-size` on `target/xtensa-esp32s3-none-elf/release/hopspot-t-halow`:

| shape | text (flash) | data | bss | static RAM (data+bss) |
|---|---|---|---|---|
| default (all radios) | 1,606,043 | 28,384 | 649,196 | 677,580 |
| halow-only | 678,489 | 10,504 | 536,044 | 546,548 |
| delta | **−927,554 (−57.8%)** | −17,880 | −113,152 | **−131,032 (−19.3%)** |

This is the evidence that the radios actually left the image rather than merely being unreferenced.

## How it was done

The tree already had a settled idiom for an optional radio, and Wi-Fi was the one radio breaking
it. Every other radio reaches the renderer as feature-independent values — `Option<InterfaceId>`
and `Option<&'static EmbassyInterfaceStatus>` declared on both `#[cfg]` arms (`firmware.rs`, the
`halow-at` block is the cleanest template) — which is why `build_snapshots`, `build_cards` and
`classify_card` carry no `#[cfg]` at all. Wi-Fi alone was passed as `Option<&AutoWifiStatus<MEMBERS>>`
plus `&HopspotWifiConfig`.

So the change was to make Wi-Fi conform, not to add `#[cfg]` everywhere:

- `s3/display.rs`: `build_snapshots` now takes `Option<&dyn InterfaceStatus>` plus a pre-walked
  `&[&'static dyn InterfaceStatus]` of fleet members; `build_cards` takes the already-resolved
  `wifi_kind: screen::CardKind` instead of the config. Both stay `#[cfg]`-free.
- `s3/firmware.rs`: Wi-Fi/TCP Option shims on both arms; the fleet walk and `wifi_kind` computation
  moved to the caller, where the feature context lives. Concrete Wi-Fi types now appear only inside
  gated blocks, matching how the BLE toggle path was already written.
- `s3/mod.rs`: `LANE_COUNT` follows the feature set (base `1` for USB plus one per selected radio,
  instead of a literal `3` assuming usb+wifi+tcp — arithmetic is unchanged for every existing
  board, which is the check that it is right); `MEMBERS` collapses to 0 without `wifi-auto`.
  `INTERFACE_CAPACITY`'s base was deliberately left alone: over-allocating is harmless, and
  under-allocating panics at lane claim.
- Wi-Fi/TCP-only imports, consts, statics, and the `captive_portal`, `connectivity`,
  `station_recovery`, `wifi_data_path_recovery` modules gated.

## The defect a green build was hiding

The final dispatch in `firmware.rs` is combinatorial on `bluetooth-auto` × `wifi-auto` and had
three arms: BLE-only, Wi-Fi-only, and both. **There was no arm for neither** — exactly the
halow-only shape. It compiled clean while spawning nothing: no HaLow task, no render loop. A board
that boots and does nothing.

Caught only because `halow_seam` and `render` showed up as unused-variable warnings. Added the
fourth arm. This is the "gate that passes without working" class, and it is the reason the
hardware boot-verify below is not optional.

Corrections to earlier guesses in this file's previous revision: every module under `s3/` is
ungated, so `captive_portal` needed a module gate for dead-code reasons only, not to compile; and
the uninhabited stand-in type turned out to be unnecessary — the repo's own Option-shim idiom
covers it.

## Still owed

1. **Boot-verify on hardware**: flash the halow-only image and confirm the console shows the HaLow
   interface up and **no** Wi-Fi/BLE/ESP-NOW interface lines. Until that runs, this is a build
   result, not a working-firmware result.
2. Hand the artifact to Doc — boards now live on the Pis (a3f4 on hertz, 5c10 on marconi), so she
   flashes over SSH. ELF at `target/xtensa-esp32s3-none-elf/release/hopspot-t-halow`, partition
   table `partitions-hopspot-16mb.csv`.

## The soak still needs a control arm

Independent of this work: run the game once with the HaLow module held down to show 2.4 GHz cannot
carry it at the hertz/marconi separation, then run it for real, counting `RNS_HALOW_AT rx/tx`.
The arm that FAILS is what makes the passing arm attributable. The halow-only image makes that
control much stronger, but it does not replace it.
