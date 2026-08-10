# B1: halow-only build shape — partial, paused 2026-08-10

Goal (Doc's tasking B1): a t-halow firmware shape with the 2.4 GHz radios compiled out, so a
HaLow link test cannot be satisfied by ESP-NOW/BLE/WiFi behind its back.

## Where it stands

Branch `feat/halow-only-soak` off `feat/halow-at-poc` (0bb10308). **Not finished.**

- `cargo t-halow` (default, all radios): exit 0, **zero warnings**.
- `cargo heltec-v4-r8` (shared-tree regression control): exit 0, **zero warnings**.
- `cargo build -p hopspot-t-halow --no-default-features` (the halow-only shape): **still failing**,
  11 errors — down from the original hard-error, but the cascade is not closed.

So the tree is safe to sit on: every shipping board builds exactly as before. Only the new
not-yet-selected shape is red, and no board package selects it.

## What is done

- `lib.rs`: the S3 hard-error and the `s3` module gate now require only `usb`, the same relaxation
  PR #96 made for `lora`.
- `boards/t-halow/Cargo.toml`: radios moved behind a default-on `radios-2g4` feature, so
  `--no-default-features` is the halow-only shape and the normal build is byte-identical in
  features.
- `s3/mod.rs`: gated the `personal_rns::tcp` and `personal_rns::wifi_auto` imports and
  `WIFI_DRIVER_RESTART_REQUESTED`; un-gated `String`, `AtomicBool`, and the two `WIFI_STATION_*`
  statics that the card renderer reads unconditionally.
- `s3/configuration.rs`: the config *types* (`HopspotWifiConfig`, `HopspotTcpClientConfig`,
  `HopspotTcpClientHost`) are now always compiled — they are pure data the renderer reads — while
  the flash-reading behavior stays gated. `HopspotWifiConfig` gained `Default`.
- `s3/firmware.rs`: an empty `HopspotWifiConfig` on the no-wifi path; `station_configured` folded
  back to one line.
- `s3/connectivity.rs`: `build_tcp` gated on `tcp`.

## What is left (the 11 errors)

The remaining cascade is all *wifi types in signatures*, not logic:

1. `display.rs` `build_snapshots`/`build_cards` take `Option<&AutoWifiStatus<MEMBERS>>`, and
   `firmware.rs` holds `Option<AutoWifi<'static, MEMBERS>>` and `TcpClient<'static>`. Those types
   come from the gated `personal_rns::wifi_auto` / `::tcp`, so the signatures do not exist in the
   halow-only shape. `#[cfg]` on a parameter needs `#[cfg]` on the call site's argument, which Rust
   does not allow, so this needs either duplicated signatures or an uninhabited stand-in type
   (`enum WifiCardStatus {}` with `match *self {}` methods) behind the shim.
2. Module declarations still to gate: `captive_portal` (its `use super::*` goes unused), and check
   `station_recovery` / `wifi_data_path_recovery` reachability.
3. `connectivity::build_tcp` import in `mod.rs:311` needs the same `tcp` gate as the definition.
4. Unused-import warnings to clear once the above lands: `embassy_net::udp::{PacketMetadata,
   UdpSocket}`, the `embassy_net` config group, `MacAddress`, `Fleet`.

Estimated: a few more build cycles. The shape of the answer is known; it is mechanical from here.

## Why it paused

Token budget, not a technical blocker. Resume with
`cargo build --release -p hopspot-t-halow --no-default-features --target xtensa-esp32s3-none-elf
-Zbuild-std=core,alloc` and work the error list top-down.

## Interim answer for the soak

Distance plus a control arm gets honest evidence without this refactor: run the game once with the
HaLow module held down (2.4-GHz-only control) to show the 2.4 path cannot carry it at the
hertz/marconi separation, then run it for real, and count `RNS_HALOW_AT rx/tx` lines per frame.
The arm that FAILS is what makes the passing arm attributable.
