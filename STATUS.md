# Status: per-node naming + identity home card (feat/name-poc)

## What this branch does

- One resolver for the node name (`personal-hopspot/core/src/naming.rs`):
  hopcfg override wins, otherwise `NODE_BASE_NAME` + first four hex chars of
  the `lxmf.delivery` destination hash (`Hopspot-a233`). Hash-derived, not
  MAC-derived.
- The delivery announce app_data (`msgpack([name, nil])`) and the node-page
  app_data are composed at boot from that name; the three S3 boards dropped
  their baked byte literals (a core test pins the composed bytes to the old
  HeltecV4-R8 literal).
- hopcfg format v1 gains an optional name field: length byte at offset 368
  (after the TCP hostname region), up to 32 bytes UTF-8 at 369. Length 0 or
  erased 0xFF = no override. No version bump; old slots and old firmware are
  both unaffected. Writer + validation in `prns-flash-manifest`
  (`ProvisioningAction::Configure { wifi, tcp_client, node_name }`), parser in
  `embedded/esp32/src/s3/configuration.rs`.
- Flasher CLI: `--node-name` alongside `--wifi configure`; rejected in
  preserve/clear modes like the other configure-only inputs.
- Boot logs one line: `node-identity name=... delivery=...` so a screenless
  board (faro-c363) can be labeled from serial.
- New OLED home card (core/src/screen): sits right under the Menu row, shows
  the node name and the full 32-hex delivery address wrapped 12 chars/line in
  FONT_5X8. It is a focus slot like the docs footer; long-press on it does
  nothing. Faces that do not surface identity (desktop, mobile, T-Echo) pass
  `node_identity: None` and behave exactly as before.

## Commits

1. `Resolve the node name in one place` (core naming module + hash derive)
2. `Screen: home card with the node's name and full delivery address`
3. `hopcfg: optional node name override after the TCP client region`
4. `S3 boot: derive the node name, compose announces, light the home card`

## Build and test (human runs these; nothing on this branch has been compiled yet)

```
cargo test -p personal-hopspot-core
cargo test -p prns-flash-manifest
cargo test -p hopspot-flash
cargo check -p personal-hopspot-desktop
cargo run -p hopspot-flash -- flash heltec-v4-r8 --local-build --yes --port COMx --monitor
```

Expected on the flashed V4-R8:

- serial: `node-identity name=Hopspot-xxxx delivery=<32 hex>` right after the
  identity bootstrap lines; on faro-a233 the suffix should read `a233` if that
  label came from the delivery hash.
- OLED: Menu row, then the home card (name centered, address in three lines),
  then LoRa/Wi-Fi/... cards one slot lower than before.
- A peer (MeshChat/Sideband) that hears an announce lists `Hopspot-xxxx`.

Override path:

```
cargo run -p hopspot-flash -- flash heltec-v4-r8 --local-build --yes --port COMx \
  --wifi configure --wifi-ssid <ssid> --wifi-password-stdin --node-name "Faro Lakeside"
```

Then confirm log + card + announce all show the override, and that a later
plain reflash (default `--wifi preserve`) keeps it.

## Untested / known gaps

- Nothing compiled: cargo is not run from this session. Screen pixel tests
  were written against the documented layout constants; if an assert misses by
  a pixel, trust the failure output and adjust the test coordinates.
- Web flasher UI has no name field yet; it passes `node_name: None` and
  produces byte-identical slots for the flows it already had.
- No release-manifest capability gate for the name field (TCP client has one).
  Writing a name into a slot for firmware that predates the field is harmless
  (old parsers stop before offset 368), but a gate should exist before this
  ships in signed releases.
- Flasher guided/interactive mode does not prompt for a name; flag only.
- The hopcfg parse lives in the wifi-auto path, so non-wifi S3 builds always
  derive; same for c6 and T-Echo, which also keep their const announce names
  and have no home card yet.
- `ProvisioningAction::Configure` changed from a newtype to a struct variant;
  its serde shape changed with it. Nothing in-repo persists that JSON, and CLI
  + website compile against the same crate, but any out-of-tree user of the
  crate API will need the one-line construction update.
- Home card strings are runtime data only (name, hex address); nothing new to
  fold into the locale/Phrase table when that branch merges.

## Fleet notes

- faro-a233 (screen): use for the OLED + announce checks.
- faro-c363 (no screen): the serial identity line is the whole point; label
  the case from it.
- hopcfg (0xD000) and node_id (0xE000) survive reflashes; the override test
  depends on that, do not erase those sectors.
