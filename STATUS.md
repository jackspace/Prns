# Remote face PoC: status

Branch `feat/face-poc`, based on trunk `dd200ce`. Four firmware commits plus this
doc pair. Everything lives in `personal-hopspot/embedded/esp32/src/s3/`.

## What was built

A screenless Hopspot's screen, viewed and driven from a phone browser on the
SoftAP captive portal.

- `face.rs` (new): `FaceFrame`, a 64x128 1bpp shadow render target in a
  documented layout (row-major from the top-left, 8 pixels per byte, MSB
  leftmost, 1024 bytes total); `SharedFaceFrame`, the cross-task static the
  render loop publishes into under a critical-section lock; the remote button
  feed with a 150 ms rate cap (`REMOTE_BUTTON_MIN_INTERVAL`).
- `firmware.rs`: the render loop now composes every frame into the shadow
  target first, publishes it, and only then blits to the physical panel when
  one is initialized and awake. A board with no OLED (or a dark one) still has
  a live frame.
- `captive_portal.rs`: three routes, bound exactly like every existing portal
  route (the `http_server_task` pool accepts only on the SoftAP stack, so the
  station interface serves nothing):
  - `GET /face/frame`: the raw 1024-byte frame, `application/octet-stream`,
    `no-store`.
  - `POST /face/button`: one-byte body, `0` short press, `1` long press, into
    `BUTTON_EVENTS` (the same channel GPIO0 feeds). Responses: 200 accepted,
    400 bad byte, 429 rate-capped, 503 channel full.
  - `GET /face`: the viewer page (3.0 KiB, self-contained, inline JS, polls at
    250 ms, canvas scaled with `image-rendering: pixelated`, tap = short,
    500 ms hold = long, mirroring the firmware threshold).
- `face_page.html` (new): the page, embedded with `include_bytes!`.

## Build (human runs these; nothing here has been compiled)

From the repo root:

    cd personal-hopspot/embedded/esp32
    cargo heltec-v4-r8          # build only
    cargo heltec-v4-r8-flash    # build + flash + monitor

The aliases carry `--release --target xtensa-esp32s3-none-elf -Zbuild-std`.
`cargo heltec-v4` for the S3R2 V4 if wanted; the change is board-agnostic
inside the shared s3 core. The C6 has no portal and is untouched.

## Bench pass to run

1. Flash faro-a233 (the unit with a screen) first. Swap radio mode to SoftAP
   from the on-device menu (same flow as provisioning). Join `Hopspot-XXXX`,
   open `http://192.168.4.1/face`.
2. The canvas should mirror the OLED with at most ~250 ms lag, including the
   battery glyph and card animations at the 500 ms render cadence.
3. Tap the canvas: focus moves on both. Hold: the long-press action fires once
   at the threshold, and releasing sends nothing more.
4. Abuse: rapid taps should return 429 without wedging the UI or the portal.
5. Endpoint checks from a joined laptop:

       curl -s http://192.168.4.1/face/frame -o frame.bin; wc -c frame.bin   # 1024
       printf '\x00' | curl -s --data-binary @- http://192.168.4.1/face/button
       printf '\x01' | curl -s --data-binary @- http://192.168.4.1/face/button
       printf 'x'    | curl -s --data-binary @- http://192.168.4.1/face/button  # 400

6. Flash faro-c363 (no screen). Boot log should show
   `display.first-render.unavailable` while `/face/frame` still serves real
   pixels, which is the whole point.

## Known gaps and open questions

- Untested. No cargo run of any kind has happened on this branch; the borrow
  flow in `serve_site_connection` (the mutable re-use of the request buffer for
  the POST body read) is the spot most worth watching on first compile.
- Radio-mode chicken and egg: `/face` only exists in SoftAP mode, and default
  boot mode is BLE. On the screenless unit the swap to AP currently needs the
  blind physical-button dance (or learn the sequence on a233 first). A
  screenless-aware default, or a hopcfg flag, is a follow-up decision.
- `Content-Length` is ignored; the handler takes the first body byte, with one
  bounded follow-up read (750 ms) when a client sends the body in its own
  segment.
- UI Sleep disables the Wi-Fi interface status; whether the SoftAP (and with it
  the remote face) survives the sleep state needs a bench answer.
- The boot splash is drawn directly on the panel before the runtime starts, so
  the remote face is blank until the first render tick.
- The frame keeps publishing while the OLED is off. That is the feature for a
  screenless board and a privacy question for one with a screen turned
  deliberately dark; flag it in review.
- No auth. Anyone who joins the open AP can watch and press. Same trust
  boundary as the existing provisioning portal, but a press can now announce,
  toggle interfaces, or swap radio mode.
