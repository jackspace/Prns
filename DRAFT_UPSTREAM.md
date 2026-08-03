# Draft upstream write-up (hold until the review queue has room)

Title: Per-node naming: derived defaults, a hopcfg override, and an identity home card

Every Hopspot of a given board model currently announces the same display
name. Two Heltec V4-R8s on one desk both show up as "Personal Hopspot
HeltecV4-R8" in LXMF clients, and the only way to tell them apart is to read
their destination hashes somewhere else. This branch gives each node a name of
its own and finally lands the old backlog item: a home card on the OLED that
shows the node's own address.

## What a node is called

At boot the firmware resolves one name and uses it everywhere: the
lxmf.delivery announce, the node-page announce, the serial log, and the
screen.

- Default: the board's base name plus the first four hex chars of the
  lxmf.delivery destination hash, e.g. `Hopspot-a233`. The suffix is
  hash-derived on purpose. The codebase already keeps Wi-Fi identity separate
  from mesh identity (the SoftAP SSID is random precisely so it leaks no
  device identity), and a hash suffix carries nothing an announce does not
  already broadcast. It is also stable across reboots and reflashes, since the
  identity vault survives both.
- Override: hopcfg gains an optional name field. A length byte at offset 368
  (right after the TCP client hostname region) followed by up to 32 bytes of
  UTF-8. Length 0 and the erased 0xFF both mean "no override", so every
  existing v1 slot reads back unchanged, firmware that predates the field
  never looks at it, and a stale byte can never rename a node. No version
  bump; a layout test pins the offsets.

The announce app_data is no longer a compile-time msgpack literal with a
hand-counted length byte per board. One function composes
`fixarray(2) | bin8(name) | nil` from the resolved name, and a test pins its
output to the byte literal it replaced. The boards now carry only a
`NODE_BASE_NAME`.

## Writing the override

`ProvisioningAction` folds its two configure variants into one:

```rust
Configure {
    wifi: WifiCredentials,
    tcp_client: Option<TcpClientEndpoint>,
    node_name: Option<String>,
}
```

which keeps name-with-and-without-TCP from doubling the variant count and
makes Preserve/Clear-with-a-name unrepresentable. Names are validated in
bytes (non-empty, max 32, no control characters). The CLI takes `--node-name`
next to `--wifi configure`; the web flasher is unchanged for now and writes
byte-identical slots for the flows it already had.

## Seeing who you are

- Boot logs `node-identity name=... delivery=...` once, so a screenless board
  can be labeled from a serial tail in seconds.
- A new home card sits under the Menu row, ahead of the interface cards: the
  node name over the full 32-hex delivery address, wrapped at 12 chars per
  FONT_5X8 line so the whole address is on screen. It is a focus slot like
  the docs footer; the focus arithmetic moved behind one helper
  (`card_focus_base`) and `selected_card` now takes the screen content, so
  there is a single source of truth for what sits ahead of the cards. Faces
  that do not surface identity yet (desktop, mobile, T-Echo) pass `None` and
  render exactly as before.

## Scope and follow-ups

- S3 boards only for the boot-side naming; c6 and T-Echo keep their const
  names for now and are mechanical follow-ups.
- The web flasher should grow a name field.
- Signed releases should gate the name field on a release capability the way
  the TCP client target is gated, before this rides a public release.
- The home card renders runtime data only, so there are no new strings for
  the localization table.

Tests: naming unit tests (derived name, override precedence, composed
app_data vs the retired literal, hex spelling), provisioning layout and
validation tests, and screen tests for the home card render, the inverted
selection, and the shifted focus order end to end.
