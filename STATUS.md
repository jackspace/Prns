# OTA proof of concept: status

Branch `feat/ota-poc`, based on trunk dd200ce. Nothing here has been compiled or run on
hardware: per the sprint rules no cargo was executed, so the first build is the first
compile. The crypto layer and the signer tool were verified on the host with an
independent Python harness (details below). Everything else is reasoned from the repo
source and the published esp-bootloader-esp-idf 0.5.0 documentation for esp32s3.

## The headline question, answered

Can the bootloader crate switch slots from application code? Yes. esp-bootloader-esp-idf
0.5.0, already a direct dependency, ships `ota_updater::OtaUpdater` with exactly the
needed surface, confirmed from the official 0.5.0 esp32s3 docs at
docs.espressif.com/projects/rust/esp-bootloader-esp-idf/0.5.0/esp32s3/:

- `OtaUpdater::new(flash: &mut F, buffer: &mut [u8; 3072]) -> Result<Self, Error>`
- `next_partition() -> Result<(FlashRegion<'_, F>, AppPartitionSubType), Error>`
- `activate_next_partition() -> Result<(), Error>`
- `set_current_ota_state(OtaImageState) -> Result<(), Error>`
- `ota_data() -> Result<Ota<'_, F>, Error>` with `Ota::set_current_app_partition`

The honest caveat is rollback, not switching: whether the prebuilt IDF second-stage
bootloader that espflash bundles was built with app rollback enabled is unknown. The
implementation writes state New on activation and marks Valid after thirty core 1
heartbeats, which is correct under both configurations: a rollback bootloader gets real
fallback, a rollback-less one ignores the states and a signed-but-broken image would
boot-loop until a wired reflash. Verify-then-commit makes "image is corrupt" impossible;
"image boots but is broken" remains the residual risk and is stated as such.

## What is on the branch

1. `personal-hopspot/embedded/esp32/partitions-hopspot-16mb-ab.csv` plus
   `otadata-select-ota0.bin`. Head offsets identical to the shipping table, otadata in
   the dead nvs extent, ota_0 at 0x10000 for 0x670000, prns_state declared at 0x680000
   (matching what `persistence.rs::S3_LAYOUT` already writes), ota_1 at 0x800000 for
   0x800000. Arithmetic in the commit body.
2. Captive portal refactor: only the header region is decoded as UTF-8 so binary POST
   bodies survive, and the first body bytes that arrive with the headers are carried
   into the body reader instead of dropped.
3. `src/s3/update.rs`: GET /update (page), GET /update/status, POST /update/signature,
   POST /update/image. Streamed sector-at-a-time writes into the inactive slot, Minisign
   prehashed verification (Ed25519 over BLAKE2b-512) against a compile-time key, flash
   readback re-hash, activate only after both digests agree, reboot. Typed errors for
   every refusal. Boot health task marks the running slot valid and repairs an erased
   otadata selection.
4. `tools/ota/sign-test-image.py`: test-only Minisign-compatible signer so the bench
   loop never touches release custody.

## Verified on this machine, without hardware

- The Minisign format understanding was validated against the known-good vector in
  `prns-flash-manifest/src/trust.rs`: prehashed signature verifies over
  BLAKE2b-512(data), global signature verifies over signature || trusted comment.
- The signer's output was verified by an independent harness replicating the device
  logic step by step, and a single flipped image byte is rejected.
- The Rust base64 decoder algorithm (ported line for line to Python) produces
  byte-identical output to a reference decoder on real signature lines, the public key
  line, and rejects padding outside the final group, non-alphabet bytes, and bad length.
- The otadata entry crc32 (0x4743989A for seq=1) was computed with the exact IDF
  algorithm, crc32_le(UINT32_MAX, seq, 4).

## Build and first-compile risk

Build the V4-R8 firmware with the test key baked in (PowerShell):

    cd personal-hopspot/embedded/esp32
    python ../../../tools/ota/sign-test-image.py keygen C:\keys\ota-poc
    $env:HOPSPOT_OTA_PUBKEY = "<the 56 char base64 line keygen printed>"
    cargo heltec-v4-r8

Places most likely to need a touch-up on first compile, in order of likelihood; all are
local to `src/s3/update.rs` unless noted:

- Exact esp-bootloader-esp-idf 0.5.0 item paths and shapes: the `ota_updater` module
  name, whether `OtaUpdater` methods return `partitions::Error` or a crate-level error
  (the `UpdateError::Slots` wrapper type changes accordingly), whether
  `PARTITION_TABLE_MAX_LEN` is exactly 3072, whether `AppPartitionSubType` derives Copy,
  and whether `OtaImageState` is non_exhaustive (the `state_name` match gains a wildcard
  arm if so).
- `embedded_storage::nor_flash::RmwNorFlashStorage::new(flash, merge_buffer)` shape in
  embedded-storage 0.3, and that `FlashRegion` exposes `partition_size()` plus the
  ReadStorage and Storage impls when F: Storage.
- blake2 0.10 pulls digest 0.10 alongside the tree's sha2 0.11 (digest 0.11); both
  digest majors coexisting in one lockfile is expected but unproven here.
- The borrow sequencing in `serve_site_connection` (immutable header borrow ends before
  the update handler takes the buffer mutably) is standard NLL but untested.

## Bench procedure

Use faro-a233 (the board with the screen). The portal only exists in SoftAP radio mode
and swapping modes is an OLED menu action (SwapRadioMode), which faro-c363 cannot do
headless; do not use c363 for the first pass.

Step 1, migration, wired, once. This rewrites the partition table and otadata and must
not touch 0xC000..0x10000 or 0x680000..0x800000; espflash only writes the regions it is
given, so the identity head and journal survive:

    espflash write-bin 0x9000 otadata-select-ota0.bin
    espflash flash --chip esp32s3 --partition-table partitions-hopspot-16mb-ab.csv `
      --target-app-partition ota_0 --flash-mode dio --flash-freq 40mhz --flash-size 16mb `
      --monitor target/xtensa-esp32s3-none-elf/release/hopspot-heltec-v4-r8

  Watch the monitor: the node must boot normally, keep its node identity (destination
  hash unchanged on the screen), and after about thirty seconds log
  "update: running image marked valid" (or "ota selection repaired to ota_0" if the
  write-bin step was skipped). If espflash rejects --target-app-partition, update
  espflash; the flag exists from espflash 3.x.

Step 2, produce a second, visibly different build to send over the air. Any new commit
works (the /update/status commit field is the tell). Then:

    cargo heltec-v4-r8
    espflash save-image --chip esp32s3 --flash-mode dio --flash-freq 40mhz `
      --flash-size 16mb target/xtensa-esp32s3-none-elf/release/hopspot-heltec-v4-r8 application.bin
    python ../../../tools/ota/sign-test-image.py sign C:\keys\ota-poc\ota-test.key application.bin

Step 3, first OTA from the laptop. Put the node in AccessPoint mode from the OLED menu,
join the Hopspot-XXXX network, open http://192.168.4.1/update, pick application.bin and
application.bin.minisig, Install. Expected: progress bar, a JSON success naming ota_1,
reboot, and /update/status afterwards showing the new commit, slot ota_1, state new then
valid within a minute. Budget 30 to 60 seconds of wall clock for the upload and flash.

Step 4, the same from a phone. Android Chrome should behave like the laptop. On an
iPhone, do not use the captive portal sheet; close it and open the address in Safari
(the page says this too). This is the deliverable the sprint exists for.

Step 5, refusal drills, any browser:

- POST an image without staging a signature: expect 409 signature-not-staged.
- Sign with a second, different test key and stage that .minisig: expect 400
  signature-key-mismatch.
- Flip one byte of application.bin after signing (or upload a different file than the
  one signed): expect 403 signature-rejected after the upload completes, and the node
  keeps running the old build; otadata must not have moved.
- Upload the same signed image again afterwards: expect success, proving a rejected
  attempt leaves the slot reusable.

## Known gaps and untested territory

- Watchdog under sustained flash writes: each sector erase parks core 1 inside a
  critical section and the RWDT is only fed while core 1 heartbeats advance. The stream
  interleaves socket awaits between sectors, which should be enough headroom against the
  15 s budget, but this is the first thing to watch on the bench. If the node resets mid
  upload, the writer needs explicit feeds or more yields.
- Wi-Fi and flash-cache contention during a 2 MB sustained write is untested; the ROM
  flash calls are IRAM-resident and the journal already writes under radio, but not at
  this duty cycle.
- The transient heap usage of an update is roughly 11 KiB (merge buffer, table scratch,
  sector buffer) from the internal heap on the V4-R8; fragmentation under long uptimes
  is unmeasured.
- No arming gesture: any client on the open SoftAP can attempt an upload. Verification
  makes that a denial-of-service and flash-wear vector only, but the button-hold arming
  described in the upstream draft should land before any real deployment.
- No version floor: a validly signed older release can be installed (deliberate for the
  PoC, the rollback-attack surface is documented in the draft).
- Status reads of otadata can race a concurrent install's commit window; harmless in
  practice, unguarded in code.
- The release pipeline (validator, flasher build, web flasher contract) still assumes a
  single factory partition; this branch changes none of it, so OTA-capable builds are
  bench-only until that lands.
- T-Beam Supreme (8 MB) is out of scope: its dual-slot layout only clears the 1 MiB
  reserve policy if the embedded WASM is gzipped first.
