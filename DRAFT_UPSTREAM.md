# Draft: over-the-air updates for the 16 MB Hopspots, signed uploads over the SoftAP

Status: proof of concept on a local branch, written against a pair of Heltec V4-R8
boards. Not a PR yet; this is the design writeup so the approach can be discussed
before the diff lands, and the bench results section below gets filled in after the
first hardware pass. Happy to split it into reviewable pieces in whatever order suits.

## Why

A Hopspot's first flash always needs a wire. Every flash after that does not have to,
and for nodes handed to neighbors that is most of the value: the update path becomes
"join the node's Wi-Fi, open a page, pick two files" from any phone with a browser,
iPhones included. The SoftAP, DHCP, captive DNS and HTTP server already exist and
already handle the captive-portal dance on both platforms, so the missing pieces were a
second app slot, a POST path, and on-device verification. Web Serial and WebUSB will
never reach iOS Safari; the SoftAP route does not care.

## The partition problem, and a finding worth having anyway

The shipping tables have exactly one app partition and zero free bytes. But two facts
make an A/B layout on the 16 MB boards nearly free:

1. Neither `nvs` nor `phy_init` is referenced anywhere in the firmware; it reads raw
   flash offsets directly. The 12 KiB nvs extent at 0x9000 is dead space, and otadata
   (0x2000) fits inside it. Nothing below 0x10000 moves, so ble_id, hopcfg and node_id
   survive migration by construction, enforced by the same overlap checks the flasher
   and manifest validation already run.
2. `persistence.rs::S3_LAYOUT` hardcodes the route journal at 0x680000..0x800000 on
   every ESP32-S3 board, written through raw ROM SPI calls the partition table never
   sees. On the 16 MB table that range sits inside the declared factory partition: the
   advertised 15.9 MB app ceiling was never real, the true ceiling is 0x670000, and the
   release validator does not enforce it. That is worth fixing on its own, and the A/B
   table fixes it by declaring reality:

        otadata     0x009000  0x002000   (reclaimed nvs; 0xB000 sector reserved)
        ota_0       0x010000  0x670000   6,750,208  same start as factory
        prns_state  0x680000  0x180000   1,572,864  now declared
        ota_1       0x800000  0x800000   8,388,608
        total                           16,777,216  exactly

   The compact app (about 2.19 MB) fills 34% of the binding slot and clears the
   app-plus-1-MiB reserve policy four times over. The same image boots from either slot
   because the flash MMU maps the executing slot; that is the standard IDF A/B design.

Migration is a wired flash, once per node: bootloader, new table, an explicit otadata
selecting ota_0 (relying on bootloader fallback for an erased otadata with no factory
partition is documented behavior for the factory case only, so the PoC writes a valid
entry instead), and the app at its unchanged 0x10000 home. Identity head and journal
untouched. The 8 MB T-Beam is deliberately out of scope: its dual-slot arithmetic only
clears the reserve policy by a couple of kilobytes unless the embedded WASM ships
gzipped first, which is a small win worth taking regardless. The source-archive build
cannot dual-slot anywhere; the clean fix is moving source.zip into its own read-only
data partition shared by both slots, which halves its storage cost too.

## Trust: the existing chain, moved on-device

The browser flasher's chain is minisign all the way down, so the device verifier speaks
minisign rather than inventing anything: prehashed signatures only (Ed25519 over the
BLAKE2b-512 digest, same as `trust.rs` verifying with allow_legacy false), key id
matched against the pinned key, and the global signature over signature plus trusted
comment checked at staging time so the comment arrives authenticated. Ed25519 and the
streaming hash discipline were already on the device via prns-core; BLAKE2b-512 is the
single new dependency (RustCrypto blake2, no_std).

The verification key is a compile-time option in the existing option_env style
(HOPSPOT_OTA_PUBKEY, the base64 line of a minisign public key, parsed by a const fn so
a malformed key is a build error). A firmware built without it refuses updates with a
typed error rather than accepting anything. Because the key ships inside the image, key
rotation works over the air: an update signed by key N carries key N+1. The PoC was
bench-tested with a throwaway keypair from a small test-only signer; release custody
and the pinned key at release/keys are untouched by any of this.

## The update flow

POST the .minisig first; it is staged in RAM after its own verification. Then POST the
image: it streams through one 4 KiB sector buffer into the inactive slot, lazily
erasing, folding a running BLAKE2b-512 as bytes arrive, with writes bounded to the slot
by the partition-table-derived region plus explicit guards that no declared slot
reaches below 0x10000 or into the journal. After the last byte: verify the signature
against the stream digest, then read the slot back out of NOR and hash it again, and
only when both digests match move otadata to the new slot and reboot. Untrusted bytes
never execute; the worst an attacker on the open AP can do is wear the inactive slot.
A boot task marks the running slot valid after the engine has proven itself for thirty
seconds, which gives real fallback if the bundled bootloader has rollback enabled and
is harmless if it does not.

The page is a small self-contained HTML document served from the portal with an upload
progress bar, and it tells iPhone users to leave the captive sheet for Safari, which is
the one sharp edge iOS keeps.

## What this deliberately does not solve yet

- An arming gesture. The endpoint should be live only for a minute after a button hold,
  converting a radio-range nuisance into a physical-access requirement. The button task
  exists; this is the first follow-up.
- A version floor. A validly signed old release can currently be installed; the
  reserved sector at 0xB000 is intended for a monotonic floor. Until then, and in the
  absence of eFuse anti-rollback (the bundled bootloader has no secure boot), all
  anti-rollback is software policy, safe against strangers in radio range and not
  against whoever holds the board and a cable. I think that is the right line for a
  community mesh, but it should be stated, not discovered.
- The release pipeline. The validator, the flasher build, and the web flasher contract
  all assume exactly one factory partition, and an OTA payload is a different artifact
  set than a cable payload (one app image, no bootloader or table). Three known choke
  points, none touched by the PoC.
- Pull updates. The node runs APSTA, so a node with an uplink could fetch a release
  itself over plain HTTP, integrity anchored by the signature rather than the
  transport; the phone then never carries the bytes. Same verify-and-commit core,
  nicer where an uplink exists.

## Bench results

To be filled in after the first hardware pass: migration flash preserving identity,
wall-clock time for a full update from a laptop and from a phone, watchdog behavior
under sustained sector writes, and the refusal drills (unsigned, wrong key, tampered
byte). The claims above about crypto and formats were verified on the host against the
known-good vector in prns-flash-manifest and an independent verifier; nothing on-device
is claimed until the bench says so.

## What I am asking

Does this shape fit the project? Concretely: the A/B table as the shipping 16 MB
layout, minisign prehashed as the on-device trust format, verify-then-commit with
readback as the invariant, and migration as one extra sparse part in a normal
preserve-data install. If yes, I would land it as: the journal-overlap fix and declared
prns_state first, then the table and migration tooling, then the portal endpoint, then
the pipeline work, each small enough to review on its own.
