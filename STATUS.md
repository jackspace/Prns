# Telemetry beacon PoC, branch feat/telem-poc

## What this branch adds

- `personal-hopspot/core/src/telemetry.rs`: the encoder that rewrites a registered
  `msgpack([display_name, stamp_cost])` delivery app_data into
  `msgpack([display_name, stamp_cost, telemetry])`, where telemetry is
  `[format_version, battery_percent, charging, uptime_seconds, reachable_destinations]`.
  Hand-encoded msgpack matching the app_data const idiom, typed errors, byte-exact tests.
  Exported from `personal_hopspot_core` as `delivery_app_data_with_telemetry`,
  `TelemetryReading`, `TelemetryAppDataError`, `TELEMETRY_FORMAT_VERSION`.
- `personal-hopspot/embedded/esp32/src/s3/telemetry.rs`: firmware side, behind a new
  `telemetry` cargo feature (off by default).
  - `TELEMETRY_SHARED`: two atomics the render loop writes each tick (smoothed battery
    state, sum of per-interface routing-table destination counts from the snapshots).
  - `telemetry_beacon_task`: waits `FIRST_BEACON_DELAY` (120 s), then every
    `BEACON_INTERVAL` (600 s) packs the freshest reading plus
    `embassy_time::Instant::now().as_secs()` uptime and issues
    `AnnounceNow { app_data: AnnounceAppData::Data(...) }` on the delivery destination,
    all interfaces. Logs `telemetry-beacon queued=...` each cycle.
- `personal-hopspot/embedded/esp32/src/s3/firmware.rs`: two cfg-gated additions, the
  render-loop `record(...)` call and the task spawn (after the watchdog spawn).
- Features: `telemetry = []` in `personal-hopspot-esp32`, forwarded as
  `telemetry = ["personal-hopspot-esp32/telemetry"]` in the `hopspot-heltec-v4-r8`
  board package only (our fleet). Other boards can forward the same one line.

Outside the worktree, chido side:

- `telemetry_listener.py` (kept outside this repo):
  Python listener on the rns API. Registers an announce handler for `lxmf.delivery`,
  prints one status line per beacon, `--all` also prints telemetry-free announces.

## What is verified and what is not

Nothing on this branch has been compiled or run. This machine does not build; you do.
Specifically untested:

- `cargo test -p personal-hopspot-core` (the six telemetry tests; expected byte vectors
  were computed by hand against the msgpack spec).
- The firmware build with and without `--features telemetry`.
- The listener is UNTESTED and stays untested until the firmware side runs; it also
  assumes `RNS.vendor.umsgpack` and the three-argument `received_announce` signature.

## Build commands

Host tests, from the repo root:

    cargo test -p personal-hopspot-core
    cargo clippy -p personal-hopspot-core

Firmware, from `personal-hopspot/embedded/esp32` (needs the esp toolchain and the
embedded site bundle already staged; running the dev flasher once on this branch does
both, then the direct command picks the feature up):

    cargo build --release --locked --package hopspot-heltec-v4-r8 --bin hopspot-heltec-v4-r8 --target xtensa-esp32s3-none-elf -Zbuild-std=core,alloc --features telemetry

Note `hopspot-flash -- build heltec-v4-r8` does not forward extra features today, so a
fleet image needs either the direct command above or a temporary
`default = ["telemetry"]` in the board package while building.

The default (no `--features telemetry`) build must stay byte-identical in behavior:
no store writes, no task, no new statics referenced.

## On-air verification plan

1. Flash faro-a233 (screen) with a telemetry build.
2. On chido, with an RNS config that reaches the hopspot (TCP via the LAN node, or the
   green RNode on COM4 on the same channel):
   `python3 telemetry_listener.py`
3. Expect the first line about 2 minutes after boot, then every 10 minutes. Battery
   percent should track the OLED battery glyph; `reach` should track the destination
   counts summed across the cards.
4. Confirm a stock LXMF client (Sideband/MeshChat) still resolves the display name from
   the same announce. That is the compatibility claim; it needs a real check.

## Known gaps

- `reachable_destinations` sums per-interface routing counts, so a destination reachable
  via two interfaces counts twice. Honest enough for a health beacon; noted in the draft.
- Heltec V4-R8 has no charge-status pin; `charging` is the voltage-trend inference from
  `HeltecR8Battery::is_charging`, which fades once the cell is full.
- The beacon replaces the announce's app_data only for its own emissions; the button
  announce still sends the registered pair without telemetry. Deliberate for the PoC.
- No cadence jitter. Two nodes booted together will beacon in near lockstep on LoRa.
  Worth a small random offset before this leaves PoC.
- The listener keys on array shape and version only; there is no namespace claim on
  element index 2. Raised as an open question in the draft.
- The upstream PR will need the listener carried into the repo (or an equivalent
  example) and a decision on which board packages forward the feature.
