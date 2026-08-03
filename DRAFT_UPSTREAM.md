# Draft upstream write-up: Heltec Mesh Node T114 port

Held until the review queue has room. The branch is three logical changes; the first stands on
its own as a PR, the second and third are the port and should land as a pair once a T-Echo has
been reflashed to prove the refactor is a move and not a rewrite.

---

## Part 1, PR: Take UF2 bootloader identity from the board catalog instead of a constant

A refactor. No behaviour changes for the T-Echo. It removes the two places where a specific
board's name is written into code that is supposed to be about a class of boards, and puts both
facts where the rest of the board's data already lives.

I am doing this now because I am bringing up a second nRF52840 UF2 board and I ran into both of
them in the first hour. Rather than bury the fix inside a large firmware change, it is here on
its own so the firmware PR can be about firmware.

**The bootloader volume label.** `prns-flash-manifest/src/catalog.rs` validates every UF2
catalog entry, and one of its clauses was `build.mount_label != "TECHOBOOT"`. `Uf2Build` already
carries `mount_label` as data, so the validator was checking a per-board value against a
hardcoded copy of that same value. The clause is now a non-empty check. The strict volume-label
shape already lives at the boundary that needs it, `web-flasher/src/core.js`, where it validates
an untrusted bridge request; a second copy of that rule in Rust with nothing tying the two
together is how they drift.

**The bootloader Board-ID prefix.** The flasher identified the target drive by requiring the
`INFO_UF2.TXT` Board-ID to start with the literal `nrf52840-techo-v`. That is per-board data,
and it was the only board fact in the whole flasher not read from the catalog. `Uf2Build` gains
`board_id_prefix`, and a new `Uf2BoardIdPrefix` domain value owns the folding rule (lowercase,
underscores to hyphens) and requires the catalog to store the already-folded form, so the
trusted side of the comparison is never normalised at runtime. `validate_transport` checks it
the same way it already checks `BoardId`, `ChipFamily`, and the reset strategies.

Detection now has two named entry points, and the difference is a safety property. `flash`
narrows to the selected board's prefix, so with a second UF2 board cataloged an image can never
be delivered to a different board's bootloader drive that happens to be mounted. `doctor` with
no board argument scans for everything cataloged, because it has nothing to narrow to. The
revision-shape rule is preserved verbatim: the text after the prefix must still be a non-empty
alphanumeric-or-dot token, so a bare prefix, a generic UF2 drive, or a coincidental mount label
still fails identity.

What this deliberately does not do: rename `PreparationProfile::TechoUf2` (the wire spelling
`techo-uf2` is baked into the minisign-signed 0.2.6 browser fixture, which cannot be re-signed
from this repo, and a neutral variant name emitting a board-named string would need a comment to
explain itself); rename the `techo_mounts` field in the schema-1 `doctor --json` output (public
contract, should move with the next schema bump); rename the `HOPSPOT_TECHOBOOT` environment
variable (operator escape hatch); or touch `SHIPPING_BOARD_SLUGS` (adding a board should stay a
conscious edit).

One open question I would rather ask than pre-empt: the preparation profile is fully determined
by the transport, and the repo asserts that correspondence in four places. Deriving the profile
from the transport and dropping the wire field looks like the real fix, but it needs a manifest
schema bump and the 0.2.6 fixture retired, so it is not something to slip into a refactor.

## Part 2: Split the nRF52840 crate into a shared library and per-board packages

Mirrors the ESP32 layout: `personal-hopspot-nrf52840` is a library holding everything true of
any nRF52840 under the S140 SoftDevice, `boards/t-echo` is a thin binary crate, and
`src/boards/t_echo.rs` owns every T-Echo fact. The seam is the `Nrf52840Board` trait, the
platform layer's counterpart to `Esp32S3Board`. One deliberate difference from the ESP32 crate:
no silicon-family submodule, because this crate has one family and `nrf52840/src/nrf52840/`
would be a fake namespace.

The two-phase bring-up keeps its exact shape. `claim` runs before `Softdevice::enable` and is a
script the board writes out of shared helpers, because `embassy_nrf::Peripherals` cannot be
passed on after a partial move, so a shared skeleton calling into a board hook cannot be written
at all; Rust's partial-move rules pick the design, and the design happens to preserve the
T-Echo's seven bring-up steps in order. `finish` moves whole: same SPIM configs, same 4 MHz on
both buses, same 150 ms settle, same radio `BoardConfig`, same button pull.

Each board's display becomes one owned type implementing `DrawTarget<Color = BinaryColor>`,
which is the only contract core imposes. `TechoDisplay` folds the old borrow wrapper, the panel
buffer, the refresh policy, the displayed-frame hash, and the panel rail into one value, and the
shared render loop's forty lines of e-ink policy move behind `Nrf52840Board::present`. The loop
keeps only the refresh urgency, because only the loop knows why it woke.

Three deltas I can name, none reachable on working hardware: the linker assert message no longer
names a board; a panel that fails to initialise now drops its rail instead of holding a rail for
a panel that failed; and the render stall matches on the whole display rather than the driver
inside it. Everything else is a move, and the commit's rename detection shows it.

The sharpest edge was not in the firmware: three build paths ran `cargo build` in the crate
directory with no package selector, which silently builds only the library once the crate is a
workspace. The flasher now passes `-p` from the catalog's package field, and the validation and
CI scripts name the board packages.

## Part 3: The Heltec Mesh Node T114 board and an ST7789 driver

The T114 is an HT-n5262: the same SX1262 arrangement as the T-Echo (TCXO on DIO3 at 1.8 V, DIO2
as the antenna switch, radio DC-DC fitted), a 240x135 ST7789V2 colour TFT instead of e-ink, a
gated battery divider, and the same one-button interface. Every pin in the board module is
confirmed by at least two independent sources: Heltec's published MeshNode-T114 schematics (all
three revisions agree on every net), their Rev 2.0 pin map, the Meshtastic
`heltec_mesh_node_t114` variant, Heltec's nRF52 BSP, and the community bootloader port. The one
single-source number, the 10 ms settle after raising the divider gate, is marked PROVISIONAL at
its definition.

The driver is written in the `ssd1681.rs` mould: embedded-hal 1.0 traits, named command
constants, `cmd` and `cmd_data` as the only two things that touch the bus. The panel facts a
board must state are required constructor inputs, not defaults: rotation, colour order,
inversion, and the visible window's origin inside the controller's 240x320 frame memory, whose
fit is a build-time assert. The frame lives as a 4,050 byte packed shadow and expands to RGB565
eight rows at a time inside the driver, because a full colour frame is 64,800 bytes against
204,800 bytes of application RAM with 68 KiB reserved for stack. On SPIM3 at 32 MHz, the only
instance on this part above 8 MHz, a full push is about 16 ms of bus time.

Two trait members arrive only now, because with one board each had one possible value: the
animation clock (e-ink pins it to zero so the charge glyph never costs a refresh; a backlit
panel runs it), and the panel light's lit level (the T114 backlight sits behind an active-low
P-FET where the T-Echo frontlight is active high).

**Why the board is not in the shipping catalog yet.** Heltec's stock bootloader carries S140
6.1.1 with the application base at 0x26000; both facts read directly out of the vendor's shipped
bootloader image, not inferred. `nrf-softdevice` supports S140 v7 only, so a Prns UF2 copied
onto a stock T114 is silently dead. The T-Echo has the same shape of problem and the same known
fix, a re-bootloader to an Adafruit build carrying S140 7.3.0, at which point the base is
0x27000 and the T114 shares the T-Echo's memory layout file. Until that procedure is written up
and a panel has shown first light, the board builds with `cargo heltec-t114` and stays out of
the release gates. Related and worth knowing early: the T114's Board-ID is exactly `HT-n5262`
with no revision suffix, so the flasher's revision-shape rule will need an exact-match allowance
when the board is cataloged.

One warning that belongs in every T114 document: never chip-erase this board. UICR holds the
bootloader start pointer and the reset-pin selection, and a chip erase removes the reset button
and the bootloader in one stroke, recoverable only over SWD.
