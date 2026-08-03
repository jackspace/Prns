# Draft: telemetry beacons in the announce you already send

Status: local proof of concept on `feat/telem-poc`, not yet posted. Post as a discussion
first (it touches wire-visible app_data), with the PR ready behind it. Fill in the
on-air numbers from the fleet before posting; the airtime figures below are computed,
not measured.

---

I run a small fleet of Hopspots that live on roofs. Once a node is up a ladder, the
questions are always the same three: how is the battery, how long since it last
rebooted, and can it actually see anybody. Today answering them means climbing or
polling. I wanted the node to volunteer the answers on a cadence, cheaply enough that
nobody minds it exists.

## The vehicle

A Hopspot's `lxmf.delivery` announce already carries
`msgpack([display_name, stamp_cost])`. This PoC appends one element:

    msgpack([display_name, stamp_cost, telemetry])

    telemetry = [format_version, battery_percent, charging, uptime_seconds, reachable_destinations]

- `format_version`: positive fixint, currently 0. Bumped only if the shape changes.
- `battery_percent`: 0 to 100 fixint, or nil when the board cannot read a battery.
- `charging`: bool.
- `uptime_seconds`: minimal-width msgpack uint.
- `reachable_destinations`: minimal-width msgpack uint, the routing-table destination
  counts summed across interfaces, the same figure the face cards show.

Existing parsers keep working because the pair's bytes are untouched: the array header
grows from fixarray(2) to fixarray(3) and the two elements are reused verbatim, so
anything that unpacks the list and reads positions 0 and 1 (which is what the shipped
LXMF display-name and stamp-cost helpers do) never sees a difference. An observer that
knows about index 2 reads it; everyone else ignores bytes they were already skipping.

## Why not an LXMF message to a collector

I considered the obvious alternative, a periodic LXMF message to a configured collector
destination, and rejected it for this use:

- Airtime. A collector message is a whole extra packet per beacon per node, plus proof
  traffic on a ProveAll destination. The announce extension is 10 to 13 bytes riding a
  packet that was going out anyway: zero additional packets. On LongFast an announce is
  already roughly 250 bytes on the wire; the element adds about five percent to that one
  packet and nothing else.
- Configuration. A collector destination has to be provisioned into every node.
  The announce path needs nothing: any listener that can hear announces is a collector.
- Reach. An addressed message serves exactly one consumer. Announces flood, so a
  neighborhood can watch its own infrastructure passively.

Sideband already does rich, private telemetry over LXMF messages, and that is the right
tool for people and phones. This is deliberately the tin-can version for infrastructure
nodes: tiny, broadcast, ignorable.

The honest flip side of riding announces: the element travels as far as the announce
does, which is the whole network, not just RF neighbors. It carries battery, uptime, and
a peer count, nothing positional, and it is off by default (below), but it is public by
construction and the write-up should say so plainly.

## Firmware side

Everything is behind a new `telemetry` cargo feature on `personal-hopspot-esp32`,
default off, forwarded by the board package. A stock build is unchanged.

- The encoder lives in `personal-hopspot-core` (`telemetry.rs`) next to the battery
  gauge it reads from: hand-encoded msgpack like the app_data consts, heapless output
  sized to the announce app-data budget, typed errors, byte-exact tests.
- The render loop already samples `BatteryGauge` and builds the interface snapshots
  every tick; with the feature on it also stores the latest battery state and summed
  destination count into two atomics.
- A beacon task waits 120 s after boot, then every 600 s (named consts) packs the
  freshest reading plus `embassy_time::Instant` uptime and issues the existing
  `AnnounceNow` command with `AnnounceAppData::Data(...)` on the delivery destination.
  No new engine surface: the command, the budget checks, and the ratchet accounting are
  all the ones announces already use.
- The button announce still sends the plain registered pair. Only the beacon emits the
  extended form.

A reference listener (Python, rns) registers an announce handler on `lxmf.delivery` and
prints one line per beacon:

    2026-08-02 20:41:07  <a2 33 ...>  Personal Hopspot HeltecV4-R8  batt 87%  up 3h12m  reach 5

## Open questions for the maintainer

1. Is element index 2 of the delivery announce acceptable to claim, or should the
   element be a map keyed by a short namespace so other extensions can coexist at the
   same index? The array is smaller; the map is politer. I went with the array for the
   PoC and would happily switch.
2. `reachable_destinations` double counts a destination reachable via two interfaces.
   Summing per-interface counts is what the face cards do, so the beacon matches the
   screen, but a deduplicated figure would need a routing-table walk. Preference?
3. Cadence. 600 s suits a three-node rooftop fleet on LongFast. If this graduates past
   PoC it likely wants jitter and maybe a config knob rather than a const.
4. Which board packages should forward the feature. The PoC wires only heltec-v4-r8
   because that is the hardware I can verify on.

## Evidence to attach before posting

- `cargo test -p personal-hopspot-core` output.
- Serial log of a beacon cycle (`telemetry-beacon queued=true ...`).
- Listener capture showing two nodes over an hour, next to the OLED battery reading.
- A stock Sideband/MeshChat screenshot resolving the display name from an extended
  announce, backing the compatibility claim.
