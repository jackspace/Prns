# Draft discussion post: the remote face

(Do not post until the maintainer's queue has room; #40 and #43 are still
pending and #42/#44 are open. Fill in the bench results section before
posting.)

---

Title: A screenless Hopspot's screen: the face in the phone browser

I have two V4-R8 units and only one of them has a panel. The screenless one is
fully functional and completely mute, which felt wrong given that the firmware
already composes a UI every 500 ms and already runs an HTTP server on the
SoftAP for provisioning. So this branch teaches the captive portal to serve the
screen itself.

## What it does

Join the SoftAP, open `http://192.168.4.1/face`, and you get the OLED in the
browser: live view, and taps that behave exactly like the physical button. Tap
for a short press, hold 500 ms for a long press (the same threshold the
firmware uses, with the same fire-at-threshold semantics). It works identically
on a board that has no panel at all, which is the point.

## How it works

The render loop used to draw straight into the SSD1306 buffer, so the composed
screen only existed when a working panel was awake to hold it. Now every pass
renders into a shadow target first: 64x128, one bit per pixel, row-major from
the top-left, MSB leftmost, 1024 bytes. That frame is published to a shared
static under a critical-section lock (one 1 KiB copy), and the panel, when
present and awake, is a blit of the same pixels. The blit is per pixel, the
same path the UI's text already takes through the buffered driver, one extra
8192-pixel pass per 500 ms tick.

Three portal routes on top of that:

- `GET /face/frame` returns the latest frame raw. No PNG, no encoding work on
  the device; 1024 bytes is already smaller than most encoders' output for
  this content, and the browser unpacks it into a canvas in a dozen lines.
- `POST /face/button` takes a one-byte body, 0 short or 1 long, and lands it
  in the same channel the GPIO0 task feeds, so a remote press wakes the
  screen, moves focus, and opens menus through the identical UI path. Remote
  presses are rate-capped at one per 150 ms (a harder floor than the 25 ms
  physical debounce) with a compare-exchange, so the pooled HTTP tasks race
  for one slot; losers get a 429, and a full channel gets a 503 instead of a
  blocked server task.
- `GET /face` serves one self-contained 3.0 KiB page, inline styles and
  script, no external assets, because on the captive AP nothing else is
  reachable. It polls at 250 ms, twice the render cadence, so a change is at
  most one poll late.

The whole addition is about 460 lines including the page.

## Security posture

The routes bind exactly as every existing portal route does: the server task
pool only ever accepts on the SoftAP stack, and the station interface serves
nothing. I want to be explicit that I kept it that way on purpose. Exposing
the face on the station side would make every phone on the joined LAN a
viewer and a button, and that is a policy call that belongs to the project,
not a default smuggled in with a feature. Within the AP the trust boundary is
unchanged from the provisioning portal (anyone who joins can act), but a
button press can announce or toggle interfaces, so it deserves a look.

## Bench results

(placeholder: fill in after the hardware pass on both units; frame latency
observed, 429 behavior under rapid taps, screenless boot log showing
display.first-render.unavailable alongside a live /face, any surprises)

## Open questions

- Default radio mode is BLE, so a factory screenless board cannot reach the
  face without the blind button dance to enter SoftAP mode first. A
  screenless-aware default (no panel detected at boot, prefer AP) would make
  this self-serve, but that changes boot behavior and I did not want to bundle
  it here.
- The frame keeps publishing while a physical OLED is deliberately switched
  off. Feature for the screenless case, arguably a privacy leak for the other;
  happy to gate it if you see it that way.
- Whether UI Sleep should also stop the face is the same question in another
  costume.
