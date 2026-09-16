//! Signed A/B firmware installs, independent of how the bytes arrive.
//!
//! The trust flow mirrors the browser flasher's Minisign chain, moved on-device: a standard
//! prehashed Minisign signature (Ed25519 over the BLAKE2b-512 digest of the image) is staged
//! first, the image then streams into the inactive slot one flash sector at a time, and the boot
//! selection moves only after the signature has verified against the digest of what physically
//! landed in flash. Unsigned or wrongly signed bytes can reach the inactive slot, which is inert,
//! but can never be selected for boot.
//!
//! Nothing here names a socket, a link or an executor. The caller hands over `&[u8]` and gets back
//! typed refusals, so the same engine serves a bench listener, an HTTP endpoint or a remote-control
//! request without changing.
//!
//! Every address comes from the board's compiled [`MemoryProfile`](personal_hopspot_memory::MemoryProfile).
//! The flashed partition table has to agree with it byte for byte or the install is refused: a
//! table that put an application slot over the identity head, the radio profile or the route
//! journal would otherwise be obeyed, not caught.

use core::cell::RefCell;

use alloc::boxed::Box;
use alloc::vec::Vec;
use blake2::{Blake2b512, Digest};
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash, RmwNorFlashStorage};
use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
use esp_bootloader_esp_idf::partitions::{self, AppPartitionSubType};
use portable_atomic::{AtomicBool, Ordering};
use prns_core::crypto::{ed25519_verify, Ed25519PublicKey, Ed25519Signature};

use crate::flash::{EspRomFlash, EspRomFlashError};
use crate::memory::EspFirmwareMemory;

const FLASH_SECTOR_LEN: usize = 4096;
/// First byte of every ESP-IDF application image.
const ESP_IMAGE_MAGIC: u8 = 0xE9;
const IMAGE_MIN_LEN: usize = FLASH_SECTOR_LEN;
const MINISIG_DOCUMENT_MAX: usize = 1024;
const MINISIGN_PUBLIC_KEY_BASE64_LEN: usize = 56;
const MINISIGN_PUBLIC_KEY_RAW_LEN: usize = 42;
const MINISIGN_SIGNATURE_RAW_LEN: usize = 74;
const MINISIGN_GLOBAL_SIGNATURE_RAW_LEN: usize = 64;
const MINISIGN_ED25519_ALGORITHM: &[u8; 2] = b"Ed";
const MINISIGN_ED25519_PREHASHED_ALGORITHM: &[u8; 2] = b"ED";
const BLAKE2B_DIGEST_LEN: usize = 64;
const INSTALL_PROGRESS_LOG_BYTES: usize = 16 * 1024;
const READBACK_YIELD_SECTORS: usize = 16;
/// Core 1 heartbeats (one per second) a fresh boot accumulates before the running slot is marked
/// valid: proof the engine is alive, not just that the bootloader found an image.
pub(crate) const VALIDATE_HEARTBEATS: u64 = 30;

/// Prns declares its own flash slots at application-defined partition types, 0x40 through 0x45,
/// which the ESP-IDF partition format reserves for exactly that use. `partition_type()` in
/// esp-bootloader-esp-idf 0.5.0 reaches `unreachable!()` on any type outside 0..=3, and every
/// table helper in that crate calls it, so `find_partition` panics as soon as it steps over one of
/// ours. The raw type and subtype bytes are public, so the table is searched with those instead.
/// Everything downstream, including all boot-selection handling, is the crate's own.
const RAW_TYPE_APP: u8 = 0x00;
const RAW_TYPE_DATA: u8 = 0x01;
const RAW_SUBTYPE_DATA_OTA: u8 = 0x00;
const RAW_SUBTYPE_OTA_0: u8 = 0x10;
const RAW_SUBTYPE_OTA_1: u8 = 0x11;
/// ota_0 and ota_1. Not counting factory or test slots, which this table does not carry.
const OTA_SLOT_COUNT: usize = 2;

/// The update verification key: the base64 key line (second line) of a standard Minisign public
/// key, compiled in like the `HOPSPOT_WIFI_SSID` fallback. Living inside the image is what makes
/// rotation possible over the air: an update signed by key N ships key N+1. Without it every entry
/// point answers with a typed refusal, never an open door.
const OTA_PUBKEY_BASE64: Option<&str> = option_env!("HOPSPOT_OTA_PUBKEY");

const OTA_VERIFYING_KEY: Option<OtaVerifyingKey> = match OTA_PUBKEY_BASE64 {
    Some(encoded) => Some(parse_ota_verifying_key(encoded)),
    None => None,
};

struct OtaVerifyingKey {
    key_id: [u8; 8],
    public_key: Ed25519PublicKey,
}

#[derive(Clone, Copy)]
struct StagedSignature {
    key_id: [u8; 8],
    signature: Ed25519Signature,
}

static STAGED_SIGNATURE: BlockingMutex<CriticalSectionRawMutex, RefCell<Option<StagedSignature>>> =
    BlockingMutex::new(RefCell::new(None));
static INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Exclusive right to write the inactive slot and move the boot selection.
///
/// The transport takes it before the first byte, hands it to [`begin`], and gets it back inside
/// [`InstalledImage`] so it spans the success reply and the reset as well. Releasing it any earlier
/// would let a second upload start while the first image is still being activated.
pub(crate) struct InstallGuard;

impl InstallGuard {
    pub(crate) fn acquire() -> Result<Self, InstallError> {
        if INSTALL_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(InstallError::InstallInProgress);
        }
        Ok(Self)
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALL_IN_PROGRESS.store(false, Ordering::Release);
    }
}

pub(crate) fn install_in_progress() -> bool {
    INSTALL_IN_PROGRESS.load(Ordering::Acquire)
}

/// Parse and hold a prehashed Minisign document. The image install refuses to start without one,
/// so an unsigned upload never reaches flash at all.
pub(crate) fn stage_signature(document: &str) -> Result<[u8; 8], InstallError> {
    if document.len() > MINISIG_DOCUMENT_MAX {
        return Err(InstallError::SignatureDocumentTooLarge {
            document_len: document.len(),
        });
    }
    let Some(key) = OTA_VERIFYING_KEY else {
        return Err(InstallError::KeyNotConfigured);
    };
    let staged = parse_signature_document(document, &key)?;
    let key_id = staged.key_id;
    STAGED_SIGNATURE.lock(|slot| *slot.borrow_mut() = Some(staged));
    Ok(key_id)
}

pub(crate) fn staged_key_id() -> Option<[u8; 8]> {
    STAGED_SIGNATURE
        .lock(|staged| *staged.borrow())
        .map(|staged| staged.key_id)
}

pub(crate) struct InstalledImage {
    /// The install guard, handed back rather than released. The boot selection now names a slot
    /// that has never run, and the caller still has a reply to send and a reset to perform: a
    /// second upload starting inside that window would target the slot the node is about to boot
    /// from. Keep this value alive until the reset.
    _guard: InstallGuard,
    pub(crate) slot: AppPartitionSubType,
    pub(crate) image_len: usize,
}

pub(crate) struct InstallStatus {
    pub(crate) busy: bool,
    pub(crate) selected: &'static str,
    pub(crate) state: &'static str,
    pub(crate) booted: &'static str,
    pub(crate) signature_staged: bool,
    pub(crate) key_id: Option<[u8; 8]>,
}

/// An install in flight: the target slot, the sector buffer it fills, and the digest of everything
/// handed to it. Dropping one without [`FirmwareInstall::finish`] is the abort path, and it is safe
/// by construction: the boot selection has not moved, so the bytes in the inactive slot are inert.
pub(crate) struct FirmwareInstall {
    _guard: InstallGuard,
    flash: EspRomFlash,
    flash_capacity: usize,
    boot_selection: [u32; 2],
    target: AppPartitionSubType,
    slot_offset: u32,
    slot_len: usize,
    declared_len: usize,
    received: usize,
    buffered: usize,
    sector: Vec<u8>,
    streamed: Blake2b512,
    key: Ed25519PublicKey,
    signature: Ed25519Signature,
}

/// Locate the inactive slot and prove the flashed table is the one this firmware was compiled
/// against, before a single byte is written.
pub(crate) fn begin(
    memory: &EspFirmwareMemory,
    guard: InstallGuard,
    declared_len: usize,
) -> Result<FirmwareInstall, InstallError> {
    let Some(key) = OTA_VERIFYING_KEY else {
        return Err(InstallError::KeyNotConfigured);
    };
    let Some(staged) = STAGED_SIGNATURE.lock(|staged| *staged.borrow()) else {
        return Err(InstallError::SignatureNotStaged);
    };
    if declared_len < IMAGE_MIN_LEN {
        return Err(InstallError::ImageTooSmall {
            image_len: declared_len,
        });
    }
    let Some(update_slot) = memory.update_slot() else {
        return Err(InstallError::NoUpdateSlot);
    };
    let Some(boot_selection) = memory.boot_selection() else {
        return Err(InstallError::NoBootSelection);
    };
    let firmware_owned = memory.firmware_owned();
    let flash_capacity = memory.flash_capacity();

    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage =
        RmwNorFlashStorage::new(EspRomFlash::new(flash_capacity), merge.as_mut_slice());
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let table = partitions::read_partition_table(&mut storage, &mut scratch[..])
        .map_err(InstallError::Partitions)?;

    let ota_0 =
        find_raw(&table, RAW_TYPE_APP, RAW_SUBTYPE_OTA_0).ok_or(InstallError::SlotMissing {
            slot: AppPartitionSubType::Ota0,
        })?;
    let ota_1 =
        find_raw(&table, RAW_TYPE_APP, RAW_SUBTYPE_OTA_1).ok_or(InstallError::SlotMissing {
            slot: AppPartitionSubType::Ota1,
        })?;
    let ota_data = find_raw(&table, RAW_TYPE_DATA, RAW_SUBTYPE_DATA_OTA)
        .ok_or(InstallError::BootSelectionMissing)?;
    // One comparison against the compiled profile replaces a hand-kept list of regions to avoid.
    // Whatever the profile protects, from the identity head to the route journal, is protected
    // here too, including regions added after this file was written.
    check_slot(AppPartitionSubType::Ota0, &ota_0, firmware_owned)?;
    check_slot(AppPartitionSubType::Ota1, &ota_1, update_slot)?;
    check_boot_selection(&ota_data, boot_selection)?;

    // Which slot the MMU is actually executing from, read independently of otadata. The bootloader
    // may have fallen back, or a migration may have left a stale selection behind, and "write the
    // other slot" is only safe if the slot we are running from is known for certain.
    let booted = table
        .booted_partition()
        .map_err(InstallError::Partitions)?
        .ok_or(InstallError::BootedSlotUnknown)?
        .offset();
    let selected = {
        let mut ota = Ota::new(ota_data.as_embedded_storage(&mut storage), OTA_SLOT_COUNT)
            .map_err(InstallError::Partitions)?;
        ota.current_app_partition()
            .map_err(InstallError::Partitions)?
    };
    // An erased otadata reads back as a Factory selection, and this table has no factory row, so
    // "the other slot" of Factory would fall through to ota_0: the exact slot a freshly migrated
    // board is executing from. Refuse rather than guess; the health task repairs an unreadable
    // selection to ota_0 within seconds and the retry then targets ota_1 safely.
    let selected_offset = match selected {
        AppPartitionSubType::Ota0 => ota_0.offset(),
        AppPartitionSubType::Ota1 => ota_1.offset(),
        _ => return Err(InstallError::RunningSlotUnknown),
    };
    if booted != selected_offset {
        return Err(InstallError::BootedSlotDisagrees {
            booted,
            selected: selected_offset,
        });
    }

    let (target, slot_offset, slot_len) = match selected {
        AppPartitionSubType::Ota0 => (AppPartitionSubType::Ota1, ota_1.offset(), ota_1.len()),
        _ => (AppPartitionSubType::Ota0, ota_0.offset(), ota_0.len()),
    };
    let slot_len = slot_len as usize;
    if declared_len > slot_len {
        return Err(InstallError::ImageTooLarge {
            image_len: declared_len,
            slot_len,
        });
    }

    log::info!(
        "update: staging {declared_len} bytes into {} at 0x{slot_offset:X}",
        slot_name(target)
    );
    Ok(FirmwareInstall {
        _guard: guard,
        flash: EspRomFlash::new(flash_capacity),
        flash_capacity,
        boot_selection,
        target,
        slot_offset,
        slot_len,
        declared_len,
        received: 0,
        buffered: 0,
        sector: alloc::vec![0u8; FLASH_SECTOR_LEN],
        streamed: Blake2b512::new(),
        key: key.public_key,
        signature: staged.signature,
    })
}

impl FirmwareInstall {
    pub(crate) fn target_slot(&self) -> AppPartitionSubType {
        self.target
    }

    /// Take the next stretch of the image. Chunk boundaries are the caller's business; flash only
    /// ever sees whole sectors.
    pub(crate) async fn write(&mut self, chunk: &[u8]) -> Result<(), InstallError> {
        let mut chunk = chunk;
        while !chunk.is_empty() {
            let accepted = self.received + self.buffered;
            if accepted >= self.declared_len {
                return Err(InstallError::BodyOverrun {
                    declared: self.declared_len,
                });
            }
            if accepted == 0 && chunk[0] != ESP_IMAGE_MAGIC {
                return Err(InstallError::ImageMagic {
                    first_byte: chunk[0],
                });
            }
            let take = chunk
                .len()
                .min(FLASH_SECTOR_LEN - self.buffered)
                .min(self.declared_len - accepted);
            self.sector[self.buffered..self.buffered + take].copy_from_slice(&chunk[..take]);
            self.buffered += take;
            chunk = &chunk[take..];
            if self.buffered == FLASH_SECTOR_LEN {
                self.flush_sector()?;
                // The erase and write above ran to completion with the other core parked. Hand the
                // rest of the system a turn before asking for the next sector, or a network
                // transport's receive window drains and never refills and the transfer starves
                // itself.
                yield_now().await;
            }
        }
        Ok(())
    }

    /// Verify what physically landed in flash, then move the boot selection. This is the only
    /// place the selection moves, and it happens after both digests agree.
    pub(crate) async fn finish(mut self) -> Result<InstalledImage, InstallError> {
        self.flush_sector()?;
        if self.received != self.declared_len {
            return Err(InstallError::BodyTruncated {
                received: self.received,
                expected: self.declared_len,
            });
        }
        let mut streamed_digest = [0u8; BLAKE2B_DIGEST_LEN];
        streamed_digest.copy_from_slice(&core::mem::take(&mut self.streamed).finalize());
        if ed25519_verify(&self.key, &streamed_digest, &self.signature).is_err() {
            return Err(InstallError::SignatureRejected);
        }

        // The stream digest covers what the caller handed over; this second pass covers what NOR
        // actually holds. A dropped write or a worn sector surfaces here and nowhere else.
        let mut readback = Blake2b512::new();
        let mut verified = 0usize;
        while verified < self.received {
            let take = FLASH_SECTOR_LEN.min(self.received - verified);
            let offset = self.slot_offset + verified as u32;
            // Whole-sector reads keep the underlying word alignment; only `take` bytes count.
            ReadNorFlash::read(&mut self.flash, offset, &mut self.sector)
                .map_err(InstallError::Flash)?;
            readback.update(&self.sector[..take]);
            verified += take;
            if verified % (READBACK_YIELD_SECTORS * FLASH_SECTOR_LEN) == 0 {
                yield_now().await;
            }
        }
        let mut readback_digest = [0u8; BLAKE2B_DIGEST_LEN];
        readback_digest.copy_from_slice(&readback.finalize());
        if readback_digest != streamed_digest {
            return Err(InstallError::ReadbackMismatch);
        }

        select_slot(self.flash_capacity, self.boot_selection, self.target)?;
        STAGED_SIGNATURE.lock(|staged| staged.borrow_mut().take());
        log::info!(
            "update: installed {} bytes into {}",
            self.received,
            slot_name(self.target)
        );
        Ok(InstalledImage {
            slot: self.target,
            image_len: self.received,
            _guard: self._guard,
        })
    }

    fn flush_sector(&mut self) -> Result<(), InstallError> {
        if self.buffered == 0 {
            return Ok(());
        }
        // Erased NOR reads as 0xFF, so a partial final sector is padded rather than left holding
        // whatever the previous image put there. Nothing past `received` is ever digested.
        self.sector[self.buffered..].fill(0xFF);
        let offset = self.slot_offset + self.received as u32;
        debug_assert!(self.received + FLASH_SECTOR_LEN <= self.slot_len);
        NorFlash::erase(&mut self.flash, offset, offset + FLASH_SECTOR_LEN as u32)
            .map_err(InstallError::Flash)?;
        NorFlash::write(&mut self.flash, offset, &self.sector).map_err(InstallError::Flash)?;
        self.streamed.update(&self.sector[..self.buffered]);
        self.received += self.buffered;
        self.buffered = 0;
        if self.received % INSTALL_PROGRESS_LOG_BYTES == 0 {
            log::info!(
                "update: {}/{} bytes into {}",
                self.received,
                self.declared_len,
                slot_name(self.target)
            );
        }
        Ok(())
    }
}

pub(crate) enum SlotHealth {
    NoOtaSlots,
    AlreadyValid,
    MarkedValid,
    SelectionRepaired,
}

/// Confirm the running image once the engine has proven itself, so a bootloader built with
/// rollback support keeps this slot. On a rollback-less bootloader the state write is inert but
/// harmless. An unreadable selection (an erased otadata after a migration flash) is repaired to
/// ota_0, the slot the migration writes the application into.
pub(crate) fn mark_running_slot_valid(
    memory: &EspFirmwareMemory,
) -> Result<SlotHealth, InstallError> {
    let Some(boot_selection) = memory.boot_selection() else {
        return Ok(SlotHealth::NoOtaSlots);
    };
    let flash_capacity = memory.flash_capacity();
    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage =
        RmwNorFlashStorage::new(EspRomFlash::new(flash_capacity), merge.as_mut_slice());
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let Ok(table) = partitions::read_partition_table(&mut storage, &mut scratch[..]) else {
        return Ok(SlotHealth::NoOtaSlots);
    };
    let (Some(ota_data), Some(_)) = (
        find_raw(&table, RAW_TYPE_DATA, RAW_SUBTYPE_DATA_OTA),
        find_raw(&table, RAW_TYPE_APP, RAW_SUBTYPE_OTA_0),
    ) else {
        // A single-slot table is not a fault, it just has nothing to confirm.
        return Ok(SlotHealth::NoOtaSlots);
    };
    check_boot_selection(&ota_data, boot_selection)?;
    let mut ota = Ota::new(ota_data.as_embedded_storage(&mut storage), OTA_SLOT_COUNT)
        .map_err(InstallError::Partitions)?;
    // Factory means both sequence numbers are uninitialized, which is what an erased otadata looks
    // like after a migration flash. The application lives in ota_0, so say so.
    match ota.current_app_partition() {
        Ok(AppPartitionSubType::Ota0 | AppPartitionSubType::Ota1) => {
            match ota.current_ota_state() {
                Ok(OtaImageState::Valid) => Ok(SlotHealth::AlreadyValid),
                Ok(_) | Err(_) => {
                    ota.set_current_ota_state(OtaImageState::Valid)
                        .map_err(InstallError::Partitions)?;
                    Ok(SlotHealth::MarkedValid)
                }
            }
        }
        Ok(_) | Err(_) => {
            ota.set_current_app_partition(AppPartitionSubType::Ota0)
                .map_err(InstallError::Partitions)?;
            ota.set_current_ota_state(OtaImageState::Valid)
                .map_err(InstallError::Partitions)?;
            Ok(SlotHealth::SelectionRepaired)
        }
    }
}

/// What a transport reports when someone asks before uploading anything.
pub(crate) fn status(memory: &EspFirmwareMemory) -> InstallStatus {
    let signature_staged = staged_key_id().is_some();
    let key_id = OTA_VERIFYING_KEY.map(|key| key.key_id);
    if install_in_progress() {
        return InstallStatus {
            busy: true,
            selected: "unknown",
            state: "unknown",
            booted: "unknown",
            signature_staged,
            key_id,
        };
    }
    let (selected, state, booted) =
        read_slot_status(memory).unwrap_or(("unknown", "unknown", "unknown"));
    InstallStatus {
        busy: false,
        selected,
        state,
        booted,
        signature_staged,
        key_id,
    }
}

fn read_slot_status(
    memory: &EspFirmwareMemory,
) -> Result<(&'static str, &'static str, &'static str), InstallError> {
    let flash_capacity = memory.flash_capacity();
    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage =
        RmwNorFlashStorage::new(EspRomFlash::new(flash_capacity), merge.as_mut_slice());
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let table = partitions::read_partition_table(&mut storage, &mut scratch[..])
        .map_err(InstallError::Partitions)?;
    let booted = match table.booted_partition() {
        Ok(Some(entry)) => {
            let offset = entry.offset();
            if find_raw(&table, RAW_TYPE_APP, RAW_SUBTYPE_OTA_1)
                .is_some_and(|slot| slot.offset() == offset)
            {
                "ota_1"
            } else if find_raw(&table, RAW_TYPE_APP, RAW_SUBTYPE_OTA_0)
                .is_some_and(|slot| slot.offset() == offset)
            {
                "ota_0"
            } else {
                "unknown"
            }
        }
        Ok(None) | Err(_) => "unknown",
    };
    let ota_data = find_raw(&table, RAW_TYPE_DATA, RAW_SUBTYPE_DATA_OTA)
        .ok_or(InstallError::BootSelectionMissing)?;
    let mut ota = Ota::new(ota_data.as_embedded_storage(&mut storage), OTA_SLOT_COUNT)
        .map_err(InstallError::Partitions)?;
    let selected = slot_name(
        ota.current_app_partition()
            .map_err(InstallError::Partitions)?,
    );
    let state = ota
        .current_ota_state()
        .map(state_name)
        .unwrap_or("undefined");
    Ok((selected, state, booted))
}

fn select_slot(
    flash_capacity: usize,
    boot_selection: [u32; 2],
    slot: AppPartitionSubType,
) -> Result<(), InstallError> {
    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage =
        RmwNorFlashStorage::new(EspRomFlash::new(flash_capacity), merge.as_mut_slice());
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let table = partitions::read_partition_table(&mut storage, &mut scratch[..])
        .map_err(InstallError::Partitions)?;
    let ota_data = find_raw(&table, RAW_TYPE_DATA, RAW_SUBTYPE_DATA_OTA)
        .ok_or(InstallError::BootSelectionMissing)?;
    check_boot_selection(&ota_data, boot_selection)?;
    let mut ota = Ota::new(ota_data.as_embedded_storage(&mut storage), OTA_SLOT_COUNT)
        .map_err(InstallError::Partitions)?;
    ota.set_current_app_partition(slot)
        .map_err(InstallError::Partitions)?;
    ota.set_current_ota_state(OtaImageState::New)
        .map_err(InstallError::Partitions)
}

fn find_raw<'a>(
    table: &partitions::PartitionTable<'a>,
    raw_type: u8,
    raw_subtype: u8,
) -> Option<partitions::PartitionEntry<'a>> {
    (0..table.len())
        .filter_map(|index| table.get_partition(index).ok())
        .find(|entry| entry.raw_type() == raw_type && entry.raw_subtype() == raw_subtype)
}

fn check_slot(
    slot: AppPartitionSubType,
    entry: &partitions::PartitionEntry<'_>,
    region: [u32; 2],
) -> Result<(), InstallError> {
    if entry.offset() == region[0] && entry.len() == region[1] - region[0] {
        return Ok(());
    }
    Err(InstallError::SlotOutsideProfile {
        slot,
        offset: entry.offset(),
        len: entry.len(),
        expected: region,
    })
}

fn check_boot_selection(
    entry: &partitions::PartitionEntry<'_>,
    region: [u32; 2],
) -> Result<(), InstallError> {
    if entry.offset() == region[0] && entry.len() == region[1] - region[0] {
        return Ok(());
    }
    Err(InstallError::BootSelectionOutsideProfile {
        offset: entry.offset(),
        len: entry.len(),
        expected: region,
    })
}

pub(crate) fn slot_name(slot: AppPartitionSubType) -> &'static str {
    match slot {
        AppPartitionSubType::Factory => "factory",
        AppPartitionSubType::Ota0 => "ota_0",
        AppPartitionSubType::Ota1 => "ota_1",
        _ => "ota_n",
    }
}

fn state_name(state: OtaImageState) -> &'static str {
    match state {
        OtaImageState::New => "new",
        OtaImageState::PendingVerify => "pending-verify",
        OtaImageState::Valid => "valid",
        OtaImageState::Invalid => "invalid",
        OtaImageState::Aborted => "aborted",
        OtaImageState::Undefined => "undefined",
    }
}

fn parse_signature_document(
    document: &str,
    key: &OtaVerifyingKey,
) -> Result<StagedSignature, InstallError> {
    let mut lines = document.lines();
    let untrusted = lines
        .next()
        .ok_or(InstallError::SignatureDocumentMalformed)?;
    if !untrusted.starts_with("untrusted comment:") {
        return Err(InstallError::SignatureDocumentMalformed);
    }
    let mut raw = [0u8; MINISIGN_SIGNATURE_RAW_LEN];
    let encoded = lines
        .next()
        .ok_or(InstallError::SignatureDocumentMalformed)?;
    let raw_len =
        decode_base64(encoded, &mut raw).ok_or(InstallError::SignatureDocumentMalformed)?;
    if raw_len != MINISIGN_SIGNATURE_RAW_LEN {
        return Err(InstallError::SignatureDocumentMalformed);
    }
    let algorithm = [raw[0], raw[1]];
    if algorithm == *MINISIGN_ED25519_ALGORITHM {
        return Err(InstallError::SignatureNotPrehashed);
    }
    if algorithm != *MINISIGN_ED25519_PREHASHED_ALGORITHM {
        return Err(InstallError::SignatureDocumentMalformed);
    }
    let mut key_id = [0u8; 8];
    key_id.copy_from_slice(&raw[2..10]);
    if key_id != key.key_id {
        return Err(InstallError::SignatureKeyMismatch);
    }
    let mut signature = [0u8; Ed25519Signature::LEN];
    signature.copy_from_slice(&raw[10..MINISIGN_SIGNATURE_RAW_LEN]);
    let trusted = lines
        .next()
        .ok_or(InstallError::SignatureDocumentMalformed)?
        .strip_prefix("trusted comment: ")
        .ok_or(InstallError::SignatureDocumentMalformed)?;
    let mut global = [0u8; MINISIGN_GLOBAL_SIGNATURE_RAW_LEN];
    let encoded = lines
        .next()
        .ok_or(InstallError::SignatureDocumentMalformed)?;
    let global_len =
        decode_base64(encoded, &mut global).ok_or(InstallError::SignatureDocumentMalformed)?;
    if global_len != MINISIGN_GLOBAL_SIGNATURE_RAW_LEN {
        return Err(InstallError::SignatureDocumentMalformed);
    }
    // Minisign's global signature covers signature || trusted comment, so a verified document
    // carries an authenticated comment, not just an authenticated payload digest.
    let mut message = Vec::with_capacity(signature.len() + trusted.len());
    message.extend_from_slice(&signature);
    message.extend_from_slice(trusted.as_bytes());
    if ed25519_verify(&key.public_key, &message, &Ed25519Signature(global)).is_err() {
        return Err(InstallError::TrustedCommentRejected);
    }
    Ok(StagedSignature {
        key_id,
        signature: Ed25519Signature(signature),
    })
}

const fn base64_sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn decode_base64(encoded: &str, out: &mut [u8]) -> Option<usize> {
    let encoded = encoded.trim_end().as_bytes();
    if encoded.is_empty() || encoded.len() % 4 != 0 {
        return None;
    }
    let groups = encoded.len() / 4;
    let mut written = 0;
    for (index, group) in encoded.chunks_exact(4).enumerate() {
        let padding = group.iter().filter(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return None;
        }
        let mut sextets = [0u8; 4];
        for (at, byte) in group.iter().enumerate() {
            if *byte == b'=' {
                if at < 4 - padding {
                    return None;
                }
                continue;
            }
            sextets[at] = base64_sextet(*byte)?;
        }
        let word = ((sextets[0] as u32) << 18)
            | ((sextets[1] as u32) << 12)
            | ((sextets[2] as u32) << 6)
            | (sextets[3] as u32);
        let bytes = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
        let produce = 3 - padding;
        if written + produce > out.len() {
            return None;
        }
        out[written..written + produce].copy_from_slice(&bytes[..produce]);
        written += produce;
    }
    Some(written)
}

const fn parse_ota_verifying_key(encoded: &str) -> OtaVerifyingKey {
    let encoded = encoded.as_bytes();
    if encoded.len() != MINISIGN_PUBLIC_KEY_BASE64_LEN {
        panic!(
            "HOPSPOT_OTA_PUBKEY must be the 56 character base64 key line of a minisign public key"
        );
    }
    let mut raw = [0u8; MINISIGN_PUBLIC_KEY_RAW_LEN];
    let mut group = 0;
    while group < MINISIGN_PUBLIC_KEY_BASE64_LEN / 4 {
        let word = ((const_sextet(encoded[group * 4]) as u32) << 18)
            | ((const_sextet(encoded[group * 4 + 1]) as u32) << 12)
            | ((const_sextet(encoded[group * 4 + 2]) as u32) << 6)
            | const_sextet(encoded[group * 4 + 3]) as u32;
        raw[group * 3] = (word >> 16) as u8;
        raw[group * 3 + 1] = (word >> 8) as u8;
        raw[group * 3 + 2] = word as u8;
        group += 1;
    }
    if raw[0] != MINISIGN_ED25519_ALGORITHM[0] || raw[1] != MINISIGN_ED25519_ALGORITHM[1] {
        panic!("HOPSPOT_OTA_PUBKEY is not an Ed25519 minisign public key");
    }
    let mut key_id = [0u8; 8];
    let mut at = 0;
    while at < key_id.len() {
        key_id[at] = raw[2 + at];
        at += 1;
    }
    let mut public_key = [0u8; Ed25519PublicKey::LEN];
    let mut at = 0;
    while at < public_key.len() {
        public_key[at] = raw[10 + at];
        at += 1;
    }
    OtaVerifyingKey {
        key_id,
        public_key: Ed25519PublicKey(public_key),
    }
}

const fn const_sextet(byte: u8) -> u8 {
    match base64_sextet(byte) {
        Some(value) => value,
        None => panic!("HOPSPOT_OTA_PUBKEY contains a character outside the base64 alphabet"),
    }
}

pub(crate) enum InstallError {
    KeyNotConfigured,
    InstallInProgress,
    NoUpdateSlot,
    NoBootSelection,
    SignatureNotStaged,
    SignatureDocumentTooLarge {
        document_len: usize,
    },
    SignatureDocumentMalformed,
    SignatureNotPrehashed,
    SignatureKeyMismatch,
    SignatureRejected,
    TrustedCommentRejected,
    ImageTooSmall {
        image_len: usize,
    },
    ImageMagic {
        first_byte: u8,
    },
    ImageTooLarge {
        image_len: usize,
        slot_len: usize,
    },
    BodyOverrun {
        declared: usize,
    },
    BodyTruncated {
        received: usize,
        expected: usize,
    },
    SlotMissing {
        slot: AppPartitionSubType,
    },
    SlotOutsideProfile {
        slot: AppPartitionSubType,
        offset: u32,
        len: u32,
        expected: [u32; 2],
    },
    BootSelectionMissing,
    BootSelectionOutsideProfile {
        offset: u32,
        len: u32,
        expected: [u32; 2],
    },
    RunningSlotUnknown,
    BootedSlotUnknown,
    BootedSlotDisagrees {
        booted: u32,
        selected: u32,
    },
    ReadbackMismatch,
    Flash(EspRomFlashError),
    Partitions(partitions::Error),
}

impl InstallError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::KeyNotConfigured => "key-not-configured",
            Self::InstallInProgress => "install-in-progress",
            Self::NoUpdateSlot => "no-update-slot",
            Self::NoBootSelection => "no-boot-selection",
            Self::SignatureNotStaged => "signature-not-staged",
            Self::SignatureDocumentTooLarge { .. } => "signature-document-too-large",
            Self::SignatureDocumentMalformed => "signature-document-malformed",
            Self::SignatureNotPrehashed => "signature-not-prehashed",
            Self::SignatureKeyMismatch => "signature-key-mismatch",
            Self::SignatureRejected => "signature-rejected",
            Self::TrustedCommentRejected => "trusted-comment-rejected",
            Self::ImageTooSmall { .. } => "image-too-small",
            Self::ImageMagic { .. } => "image-magic",
            Self::ImageTooLarge { .. } => "image-too-large",
            Self::BodyOverrun { .. } => "body-overrun",
            Self::BodyTruncated { .. } => "body-truncated",
            Self::SlotMissing { .. } => "slot-missing",
            Self::SlotOutsideProfile { .. } => "slot-outside-profile",
            Self::BootSelectionMissing => "boot-selection-missing",
            Self::BootSelectionOutsideProfile { .. } => "boot-selection-outside-profile",
            Self::RunningSlotUnknown => "running-slot-unknown",
            Self::BootedSlotUnknown => "booted-slot-unknown",
            Self::BootedSlotDisagrees { .. } => "booted-slot-disagrees",
            Self::ReadbackMismatch => "readback-mismatch",
            Self::Flash(_) => "flash-access",
            Self::Partitions(_) => "partition-access",
        }
    }
}

impl core::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::KeyNotConfigured => write!(
                formatter,
                "this firmware was built without an update verification key"
            ),
            Self::InstallInProgress => write!(formatter, "another install is already running"),
            Self::NoUpdateSlot => write!(
                formatter,
                "this build's memory profile has no firmware update slot"
            ),
            Self::NoBootSelection => write!(
                formatter,
                "this build's memory profile has no boot selection region"
            ),
            Self::SignatureNotStaged => {
                write!(formatter, "stage the .minisig signature before the image")
            }
            Self::SignatureDocumentTooLarge { document_len } => write!(
                formatter,
                "signature document of {document_len} bytes exceeds the {MINISIG_DOCUMENT_MAX} byte limit"
            ),
            Self::SignatureDocumentMalformed => {
                write!(formatter, "the body is not a minisign signature document")
            }
            Self::SignatureNotPrehashed => write!(
                formatter,
                "legacy non-prehashed minisign signatures are not accepted"
            ),
            Self::SignatureKeyMismatch => write!(
                formatter,
                "the signature was made with a different key than this firmware trusts"
            ),
            Self::SignatureRejected => {
                write!(formatter, "the image does not match the staged signature")
            }
            Self::TrustedCommentRejected => {
                write!(formatter, "the trusted comment failed verification")
            }
            Self::ImageTooSmall { image_len } => {
                write!(formatter, "{image_len} bytes is too small for an app image")
            }
            Self::ImageMagic { first_byte } => write!(
                formatter,
                "first byte 0x{first_byte:02X} is not an ESP application image"
            ),
            Self::ImageTooLarge {
                image_len,
                slot_len,
            } => write!(
                formatter,
                "image of {image_len} bytes exceeds the {slot_len} byte slot"
            ),
            Self::BodyOverrun { declared } => write!(
                formatter,
                "more bytes arrived than the declared {declared} byte image"
            ),
            Self::BodyTruncated { received, expected } => write!(
                formatter,
                "received {received} of the declared {expected} bytes"
            ),
            Self::SlotMissing { slot } => write!(
                formatter,
                "partition table has no {} slot; flash the A/B table first",
                slot_name(*slot)
            ),
            Self::SlotOutsideProfile {
                slot,
                offset,
                len,
                expected,
            } => write!(
                formatter,
                "{} at 0x{offset:X}+0x{len:X} is not the 0x{:X}..0x{:X} this firmware was built for",
                slot_name(*slot),
                expected[0],
                expected[1]
            ),
            Self::BootSelectionMissing => write!(
                formatter,
                "partition table has no otadata slot; flash the A/B table first"
            ),
            Self::BootSelectionOutsideProfile {
                offset,
                len,
                expected,
            } => write!(
                formatter,
                "otadata at 0x{offset:X}+0x{len:X} is not the 0x{:X}..0x{:X} this firmware was built for",
                expected[0], expected[1]
            ),
            Self::RunningSlotUnknown => write!(
                formatter,
                "the boot selection is unreadable; the node repairs it on its own, retry in a few seconds"
            ),
            Self::BootedSlotUnknown => write!(
                formatter,
                "the running slot could not be read from the flash mapping"
            ),
            Self::BootedSlotDisagrees { booted, selected } => write!(
                formatter,
                "the node is running 0x{booted:X} but the boot selection names 0x{selected:X}"
            ),
            Self::ReadbackMismatch => {
                write!(formatter, "flash readback does not match the received image")
            }
            Self::Flash(error) => write!(formatter, "slot flash access failed: {error}"),
            Self::Partitions(error) => write!(formatter, "partition access failed: {error:?}"),
        }
    }
}
