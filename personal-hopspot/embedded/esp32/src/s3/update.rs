//! Signed firmware updates over the SoftAP captive portal.
//!
//! The trust flow mirrors the browser flasher's Minisign chain, moved on-device: a standard
//! prehashed Minisign signature (Ed25519 over the BLAKE2b-512 digest of the image) is staged
//! first, the image then streams into the inactive OTA slot one flash sector at a time, and the
//! slot pointer moves only after the signature has verified against the digest of what physically
//! landed in flash. Unsigned or wrongly signed bytes can reach the inactive slot, which is inert,
//! but can never be selected for boot.

use core::cell::RefCell;
use core::fmt::Write as _;

use alloc::boxed::Box;
use alloc::vec::Vec;
use blake2::{Blake2b512, Digest};
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embedded_storage::nor_flash::RmwNorFlashStorage;
use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{self, AppPartitionSubType, PartitionType};
use personal_rns::crypto::{ed25519_verify, Ed25519PublicKey, Ed25519Signature};

use super::captive_portal::tcp_write_all;
use super::*;
use crate::flash::EspRomFlash;
use crate::persistence::S3_LAYOUT;

/// The OTA writer's reach: the full 16 MiB chip of the Heltec V4 and V4-R8, the boards the A/B
/// partition table targets. The identity vault instantiates `EspRomFlash<0xF000>` so its capacity
/// bound protects the head sectors; this instance reaches past them, which is why every slot the
/// writer touches is validated against the identity head and the route journal first.
const OTA_FLASH_CAPACITY: usize = 16 * 1024 * 1024;
const FLASH_SECTOR_LEN: usize = 4096;
/// App partitions live at or above this offset; everything below is bootloader, partition table,
/// otadata, and the identity head that must survive every update.
const APP_SLOT_FLOOR: u32 = 0x10000;
const ROUTE_JOURNAL_START: u32 = S3_LAYOUT.timebase_regions[0];
const ROUTE_JOURNAL_END: u32 = S3_LAYOUT.arenas[1].end;
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
const UPDATE_PROGRESS_LOG_BYTES: usize = 256 * 1024;
const READBACK_YIELD_SECTORS: usize = 16;
/// Core 1 heartbeats (one per second) the fresh boot must accumulate before the running slot is
/// marked valid: proof the engine is alive, not just that the bootloader found an image.
const OTA_VALIDATE_HEARTBEATS: u64 = 30;
const REBOOT_HOLDOFF_MS: u64 = 500;

const HTTP_OK: &str = "200 OK";
const JSON_CONTENT_TYPE: &str = "application/json";

/// The update verification key: the base64 key line (second line) of a standard Minisign public
/// key, compiled in like the `HOPSPOT_WIFI_SSID` fallback. Living inside the image is what makes
/// rotation possible over the air: an update signed by key N ships key N+1. Without it every
/// update endpoint answers with a typed refusal, never an open door.
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

static STAGED_SIGNATURE: BlockingMutex<Mtx, RefCell<Option<StagedSignature>>> =
    BlockingMutex::new(RefCell::new(None));
static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct UpdateGuard;

impl UpdateGuard {
    fn acquire() -> Result<Self, UpdateError> {
        if UPDATE_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(UpdateError::UpdateInProgress);
        }
        Ok(Self)
    }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_IN_PROGRESS.store(false, Ordering::Release);
    }
}

pub(super) enum Route {
    Page,
    Status,
    StageSignature,
    InstallImage,
}

pub(super) struct BodyFraming {
    pub(super) body_start: usize,
    pub(super) buffered_len: usize,
    pub(super) content_length: Option<usize>,
}

pub(super) fn route(method: &str, path: &str) -> Option<Route> {
    match (method, path) {
        ("GET", "/update") => Some(Route::Page),
        ("GET", "/update/status") => Some(Route::Status),
        ("POST", "/update/signature") => Some(Route::StageSignature),
        ("POST", "/update/image") => Some(Route::InstallImage),
        _ => None,
    }
}

pub(super) async fn serve(
    socket: &mut TcpSocket<'static>,
    request_buffer: &mut [u8],
    framing: BodyFraming,
    route: Route,
) -> Result<(), ()> {
    let buffered = &request_buffer[framing.body_start..framing.buffered_len];
    match route {
        Route::Page => {
            send_response(
                socket,
                HTTP_OK,
                "text/html; charset=utf-8",
                UPDATE_PAGE.as_bytes(),
            )
            .await
        }
        Route::Status => {
            let body = status_json();
            send_response(socket, HTTP_OK, JSON_CONTENT_TYPE, body.as_bytes()).await
        }
        Route::StageSignature => {
            match stage_signature(socket, buffered, framing.content_length).await {
                Ok(key_id) => {
                    let body = alloc::format!(
                        "{{\"staged\":true,\"key_id\":\"{}\"}}\n",
                        hex_bytes(&key_id)
                    );
                    send_response(socket, HTTP_OK, JSON_CONTENT_TYPE, body.as_bytes()).await
                }
                Err(error) => {
                    log::warn!("update: signature refused: {error}");
                    send_error(socket, &error).await
                }
            }
        }
        // The guard is taken here rather than inside install_image so it stays held across the
        // success response and the reboot: releasing it earlier would let a second upload target
        // the slot this firmware is executing from during the shutdown window.
        Route::InstallImage => match UpdateGuard::acquire() {
            Ok(_hold) => match install_image(socket, buffered, framing.content_length).await {
                Ok(installed) => finish_and_reboot(socket, installed).await,
                Err(error) => {
                    log::warn!("update: image refused: {error}");
                    send_error(socket, &error).await
                }
            },
            Err(error) => send_error(socket, &error).await,
        },
    }
}

struct InstalledImage {
    slot: AppPartitionSubType,
    image_len: usize,
}

async fn install_image(
    socket: &mut TcpSocket<'static>,
    buffered: &[u8],
    content_length: Option<usize>,
) -> Result<InstalledImage, UpdateError> {
    let Some(expected) = content_length else {
        return Err(UpdateError::LengthRequired);
    };
    if expected < IMAGE_MIN_LEN {
        return Err(UpdateError::ImageTooSmall {
            image_len: expected,
        });
    }
    let Some(key) = OTA_VERIFYING_KEY else {
        return Err(UpdateError::KeyNotConfigured);
    };
    let Some(staged) = STAGED_SIGNATURE.lock(|staged| *staged.borrow()) else {
        return Err(UpdateError::SignatureNotStaged);
    };

    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage = RmwNorFlashStorage::new(
        EspRomFlash::<OTA_FLASH_CAPACITY>::new(),
        merge.as_mut_slice(),
    );
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    {
        let table = partitions::read_partition_table(&mut storage, &mut scratch[..])
            .map_err(UpdateError::Slots)?;
        validated_slot(&table, AppPartitionSubType::Ota0)?;
        validated_slot(&table, AppPartitionSubType::Ota1)?;
    }
    let mut updater = OtaUpdater::new(&mut storage, &mut scratch).map_err(UpdateError::Slots)?;
    let (mut slot_region, target) = updater.next_partition().map_err(UpdateError::Slots)?;
    let slot_len = slot_region.partition_size();
    if expected > slot_len {
        return Err(UpdateError::ImageTooLarge {
            image_len: expected,
            slot_len,
        });
    }

    let mut body = BodyReader {
        socket,
        buffered,
        remaining: expected,
    };
    let mut sector = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut streamed = Blake2b512::new();
    let mut received = 0usize;
    loop {
        let filled = body.fill(&mut sector).await;
        if filled == 0 {
            break;
        }
        if received == 0 && sector[0] != ESP_IMAGE_MAGIC {
            return Err(UpdateError::ImageMagic {
                first_byte: sector[0],
            });
        }
        streamed.update(&sector[..filled]);
        slot_region
            .write(received as u32, &sector[..filled])
            .map_err(UpdateError::Slots)?;
        received += filled;
        if received % UPDATE_PROGRESS_LOG_BYTES == 0 {
            log::info!(
                "update: {received}/{expected} bytes into {}",
                slot_name(target)
            );
        }
    }
    if received != expected {
        return Err(UpdateError::BodyTruncated { received, expected });
    }
    let mut streamed_digest = [0u8; BLAKE2B_DIGEST_LEN];
    streamed_digest.copy_from_slice(&streamed.finalize());
    if ed25519_verify(&key.public_key, &streamed_digest, &staged.signature).is_err() {
        return Err(UpdateError::SignatureRejected);
    }

    // The stream digest covers what arrived on the socket; this second pass covers what NOR
    // actually holds. A dropped write or a worn sector surfaces here and nowhere else.
    let mut readback = Blake2b512::new();
    let mut verified = 0usize;
    while verified < received {
        let take = FLASH_SECTOR_LEN.min(received - verified);
        // Whole-sector reads keep the underlying word alignment; only `take` bytes count.
        slot_region
            .read(verified as u32, &mut sector)
            .map_err(UpdateError::Slots)?;
        readback.update(&sector[..take]);
        verified += take;
        if verified % (READBACK_YIELD_SECTORS * FLASH_SECTOR_LEN) == 0 {
            yield_now().await;
        }
    }
    let mut readback_digest = [0u8; BLAKE2B_DIGEST_LEN];
    readback_digest.copy_from_slice(&readback.finalize());
    if readback_digest != streamed_digest {
        return Err(UpdateError::ReadbackMismatch);
    }

    drop(slot_region);
    updater
        .activate_next_partition()
        .map_err(UpdateError::Slots)?;
    updater
        .set_current_ota_state(OtaImageState::New)
        .map_err(UpdateError::Slots)?;
    STAGED_SIGNATURE.lock(|staged| staged.borrow_mut().take());
    Ok(InstalledImage {
        slot: target,
        image_len: received,
    })
}

async fn finish_and_reboot(
    socket: &mut TcpSocket<'static>,
    installed: InstalledImage,
) -> Result<(), ()> {
    let body = alloc::format!(
        "{{\"installed\":true,\"slot\":\"{}\",\"bytes\":{},\"rebooting\":true}}\n",
        slot_name(installed.slot),
        installed.image_len
    );
    let _ = send_response(socket, HTTP_OK, JSON_CONTENT_TYPE, body.as_bytes()).await;
    socket.close();
    let _ = with_timeout(Duration::from_secs(2), socket.flush()).await;
    log::info!(
        "update: installed {} bytes into {}; rebooting",
        installed.image_len,
        slot_name(installed.slot)
    );
    Timer::after(Duration::from_millis(REBOOT_HOLDOFF_MS)).await;
    esp_hal::system::software_reset()
}

async fn stage_signature(
    socket: &mut TcpSocket<'static>,
    buffered: &[u8],
    content_length: Option<usize>,
) -> Result<[u8; 8], UpdateError> {
    let Some(expected) = content_length else {
        return Err(UpdateError::LengthRequired);
    };
    if expected == 0 {
        return Err(UpdateError::SignatureDocumentMalformed);
    }
    if expected > MINISIG_DOCUMENT_MAX {
        return Err(UpdateError::SignatureDocumentTooLarge {
            document_len: expected,
        });
    }
    let Some(key) = OTA_VERIFYING_KEY else {
        return Err(UpdateError::KeyNotConfigured);
    };
    let mut document = alloc::vec![0u8; expected];
    let mut body = BodyReader {
        socket,
        buffered,
        remaining: expected,
    };
    let mut received = 0usize;
    while received < expected {
        let filled = body.fill(&mut document[received..]).await;
        if filled == 0 {
            break;
        }
        received += filled;
    }
    if received != expected {
        return Err(UpdateError::BodyTruncated { received, expected });
    }
    let document =
        core::str::from_utf8(&document).map_err(|_| UpdateError::SignatureDocumentMalformed)?;
    let staged = parse_signature_document(document, &key)?;
    let key_id = staged.key_id;
    STAGED_SIGNATURE.lock(|slot| *slot.borrow_mut() = Some(staged));
    Ok(key_id)
}

fn parse_signature_document(
    document: &str,
    key: &OtaVerifyingKey,
) -> Result<StagedSignature, UpdateError> {
    let mut lines = document.lines();
    let untrusted = lines
        .next()
        .ok_or(UpdateError::SignatureDocumentMalformed)?;
    if !untrusted.starts_with("untrusted comment:") {
        return Err(UpdateError::SignatureDocumentMalformed);
    }
    let mut raw = [0u8; MINISIGN_SIGNATURE_RAW_LEN];
    let encoded = lines
        .next()
        .ok_or(UpdateError::SignatureDocumentMalformed)?;
    let raw_len =
        decode_base64(encoded, &mut raw).ok_or(UpdateError::SignatureDocumentMalformed)?;
    if raw_len != MINISIGN_SIGNATURE_RAW_LEN {
        return Err(UpdateError::SignatureDocumentMalformed);
    }
    let algorithm = [raw[0], raw[1]];
    if algorithm == *MINISIGN_ED25519_ALGORITHM {
        return Err(UpdateError::SignatureNotPrehashed);
    }
    if algorithm != *MINISIGN_ED25519_PREHASHED_ALGORITHM {
        return Err(UpdateError::SignatureDocumentMalformed);
    }
    let mut key_id = [0u8; 8];
    key_id.copy_from_slice(&raw[2..10]);
    if key_id != key.key_id {
        return Err(UpdateError::SignatureKeyMismatch);
    }
    let mut signature = [0u8; Ed25519Signature::LEN];
    signature.copy_from_slice(&raw[10..MINISIGN_SIGNATURE_RAW_LEN]);
    let trusted = lines
        .next()
        .ok_or(UpdateError::SignatureDocumentMalformed)?
        .strip_prefix("trusted comment: ")
        .ok_or(UpdateError::SignatureDocumentMalformed)?;
    let mut global = [0u8; MINISIGN_GLOBAL_SIGNATURE_RAW_LEN];
    let encoded = lines
        .next()
        .ok_or(UpdateError::SignatureDocumentMalformed)?;
    let global_len =
        decode_base64(encoded, &mut global).ok_or(UpdateError::SignatureDocumentMalformed)?;
    if global_len != MINISIGN_GLOBAL_SIGNATURE_RAW_LEN {
        return Err(UpdateError::SignatureDocumentMalformed);
    }
    // Minisign's global signature covers signature || trusted comment, so a verified document
    // carries an authenticated comment, not just an authenticated payload digest.
    let mut message = Vec::with_capacity(signature.len() + trusted.len());
    message.extend_from_slice(&signature);
    message.extend_from_slice(trusted.as_bytes());
    if ed25519_verify(&key.public_key, &message, &Ed25519Signature(global)).is_err() {
        return Err(UpdateError::TrustedCommentRejected);
    }
    Ok(StagedSignature {
        key_id,
        signature: Ed25519Signature(signature),
    })
}

struct BodyReader<'a> {
    socket: &'a mut TcpSocket<'static>,
    buffered: &'a [u8],
    remaining: usize,
}

impl BodyReader<'_> {
    /// Fill `chunk` with body bytes, draining the header read's remainder before touching the
    /// socket. Returns 0 once the declared body is complete or the peer stops sending; the caller
    /// detects truncation by comparing totals.
    async fn fill(&mut self, chunk: &mut [u8]) -> usize {
        let want = chunk.len().min(self.remaining);
        let mut filled = 0;
        while filled < want {
            if !self.buffered.is_empty() {
                let take = self.buffered.len().min(want - filled);
                chunk[filled..filled + take].copy_from_slice(&self.buffered[..take]);
                self.buffered = &self.buffered[take..];
                filled += take;
                continue;
            }
            match self.socket.read(&mut chunk[filled..want]).await {
                Ok(0) | Err(_) => break,
                Ok(read) => filled += read,
            }
        }
        self.remaining -= filled;
        filled
    }
}

fn validated_slot(
    table: &partitions::PartitionTable<'_>,
    slot: AppPartitionSubType,
) -> Result<(), UpdateError> {
    let entry = table
        .find_partition(PartitionType::App(slot))
        .map_err(UpdateError::Slots)?
        .ok_or(UpdateError::SlotMissing { slot })?;
    let offset = entry.offset();
    let len = entry.len();
    if offset < APP_SLOT_FLOOR {
        return Err(UpdateError::SlotBelowIdentityHead { slot, offset });
    }
    let end = offset.saturating_add(len);
    if offset < ROUTE_JOURNAL_END && end > ROUTE_JOURNAL_START {
        return Err(UpdateError::SlotOverlapsRouteJournal { slot, offset, len });
    }
    Ok(())
}

pub(super) enum SlotHealth {
    NoOtaSlots,
    AlreadyValid,
    MarkedValid,
    SelectionRepaired,
}

/// Confirm the running image once the engine has proven itself, so a bootloader built with
/// rollback support keeps this slot. On a rollback-less bootloader the state write is inert but
/// harmless. An unreadable selection (an erased otadata after a partial migration) is repaired to
/// ota_0, the slot the migration flash writes the application into.
#[embassy_executor::task]
pub(super) async fn ota_health_task() {
    loop {
        Timer::after(Duration::from_secs(1)).await;
        if CORE_ONE_HEARTBEAT.load(Ordering::Relaxed) < OTA_VALIDATE_HEARTBEATS {
            continue;
        }
        // An install owns otadata from its first byte to the reset that follows success. Firing
        // inside its post-activation window would mark the freshly selected slot valid before
        // that image has ever booted, erasing the rollback evidence, so the install is waited
        // out: success ends in a reset and this task with it, failure releases the guard and the
        // next tick proceeds against the slot that is still running. The write below cannot race
        // the check because mark_running_slot_valid never yields and installs only start at
        // await points of this executor.
        if UPDATE_IN_PROGRESS.load(Ordering::Acquire) {
            continue;
        }
        break;
    }
    match mark_running_slot_valid() {
        Ok(SlotHealth::NoOtaSlots) => {
            log::debug!("update: no ota slots on this partition table");
        }
        Ok(SlotHealth::AlreadyValid) => {}
        Ok(SlotHealth::MarkedValid) => log::info!("update: running image marked valid"),
        Ok(SlotHealth::SelectionRepaired) => {
            log::info!("update: ota selection repaired to ota_0");
        }
        Err(error) => log::warn!("update: could not mark the running image valid: {error}"),
    }
}

fn mark_running_slot_valid() -> Result<SlotHealth, UpdateError> {
    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage = RmwNorFlashStorage::new(
        EspRomFlash::<OTA_FLASH_CAPACITY>::new(),
        merge.as_mut_slice(),
    );
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let Ok(mut updater) = OtaUpdater::new(&mut storage, &mut scratch) else {
        return Ok(SlotHealth::NoOtaSlots);
    };
    match updater.selected_partition() {
        Ok(_) => match updater.current_ota_state() {
            Ok(OtaImageState::Valid) => Ok(SlotHealth::AlreadyValid),
            Ok(_) | Err(_) => {
                updater
                    .set_current_ota_state(OtaImageState::Valid)
                    .map_err(UpdateError::Slots)?;
                Ok(SlotHealth::MarkedValid)
            }
        },
        Err(_) => {
            let mut ota = updater.ota_data().map_err(UpdateError::Slots)?;
            ota.set_current_app_partition(AppPartitionSubType::Ota0)
                .map_err(UpdateError::Slots)?;
            ota.set_current_ota_state(OtaImageState::Valid)
                .map_err(UpdateError::Slots)?;
            Ok(SlotHealth::SelectionRepaired)
        }
    }
}

fn status_json() -> String {
    let staged = STAGED_SIGNATURE.lock(|staged| staged.borrow().is_some());
    let key_id = match OTA_VERIFYING_KEY {
        Some(key) => alloc::format!("\"{}\"", hex_bytes(&key.key_id)),
        None => String::from("null"),
    };
    let (busy, slot, state) = if UPDATE_IN_PROGRESS.load(Ordering::Acquire) {
        (true, "unknown", "unknown")
    } else {
        match read_slot_status() {
            Ok((slot, state)) => (false, slot, state),
            Err(_) => (false, "unknown", "unknown"),
        }
    };
    alloc::format!(
        "{{\"version\":\"{}\",\"commit\":\"{}\",\"busy\":{busy},\"slot\":\"{slot}\",\"state\":\"{state}\",\"signature_staged\":{staged},\"key_id\":{key_id}}}\n",
        env!("HOPSPOT_BUILD_VERSION"),
        env!("HOPSPOT_BUILD_COMMIT_SHORT"),
    )
}

fn read_slot_status() -> Result<(&'static str, &'static str), UpdateError> {
    let mut merge = alloc::vec![0u8; FLASH_SECTOR_LEN];
    let mut storage = RmwNorFlashStorage::new(
        EspRomFlash::<OTA_FLASH_CAPACITY>::new(),
        merge.as_mut_slice(),
    );
    let mut scratch = Box::new([0u8; partitions::PARTITION_TABLE_MAX_LEN]);
    let mut updater = OtaUpdater::new(&mut storage, &mut scratch).map_err(UpdateError::Slots)?;
    let slot = updater.selected_partition().map_err(UpdateError::Slots)?;
    let state = updater
        .current_ota_state()
        .map(state_name)
        .unwrap_or("undefined");
    Ok((slot_name(slot), state))
}

fn slot_name(slot: AppPartitionSubType) -> &'static str {
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

enum UpdateError {
    KeyNotConfigured,
    UpdateInProgress,
    LengthRequired,
    SignatureNotStaged,
    SignatureDocumentTooLarge { document_len: usize },
    SignatureDocumentMalformed,
    SignatureNotPrehashed,
    SignatureKeyMismatch,
    SignatureRejected,
    TrustedCommentRejected,
    ImageTooSmall { image_len: usize },
    ImageMagic { first_byte: u8 },
    ImageTooLarge { image_len: usize, slot_len: usize },
    BodyTruncated { received: usize, expected: usize },
    SlotMissing { slot: AppPartitionSubType },
    SlotBelowIdentityHead { slot: AppPartitionSubType, offset: u32 },
    SlotOverlapsRouteJournal { slot: AppPartitionSubType, offset: u32, len: u32 },
    ReadbackMismatch,
    Slots(partitions::Error),
}

impl UpdateError {
    fn http_status(&self) -> &'static str {
        match self {
            Self::KeyNotConfigured => "503 Service Unavailable",
            Self::UpdateInProgress | Self::SignatureNotStaged | Self::SlotMissing { .. } => {
                "409 Conflict"
            }
            Self::LengthRequired => "411 Length Required",
            Self::SignatureDocumentTooLarge { .. } | Self::ImageTooLarge { .. } => {
                "413 Payload Too Large"
            }
            Self::SignatureDocumentMalformed
            | Self::SignatureNotPrehashed
            | Self::SignatureKeyMismatch
            | Self::ImageTooSmall { .. }
            | Self::ImageMagic { .. }
            | Self::BodyTruncated { .. } => "400 Bad Request",
            Self::SignatureRejected | Self::TrustedCommentRejected => "403 Forbidden",
            Self::SlotBelowIdentityHead { .. }
            | Self::SlotOverlapsRouteJournal { .. }
            | Self::ReadbackMismatch
            | Self::Slots(_) => "500 Internal Server Error",
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::KeyNotConfigured => "key-not-configured",
            Self::UpdateInProgress => "update-in-progress",
            Self::LengthRequired => "length-required",
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
            Self::BodyTruncated { .. } => "body-truncated",
            Self::SlotMissing { .. } => "slot-missing",
            Self::SlotBelowIdentityHead { .. } => "slot-below-identity-head",
            Self::SlotOverlapsRouteJournal { .. } => "slot-overlaps-route-journal",
            Self::ReadbackMismatch => "readback-mismatch",
            Self::Slots(_) => "ota-partition-access",
        }
    }
}

impl core::fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::KeyNotConfigured => {
                write!(
                    formatter,
                    "this firmware was built without an update verification key"
                )
            }
            Self::UpdateInProgress => write!(formatter, "another update is already running"),
            Self::LengthRequired => write!(formatter, "the request must carry a Content-Length"),
            Self::SignatureNotStaged => {
                write!(formatter, "stage the .minisig signature before the image")
            }
            Self::SignatureDocumentTooLarge { document_len } => {
                write!(
                    formatter,
                    "signature document of {document_len} bytes exceeds the {MINISIG_DOCUMENT_MAX} byte limit"
                )
            }
            Self::SignatureDocumentMalformed => {
                write!(formatter, "the body is not a minisign signature document")
            }
            Self::SignatureNotPrehashed => {
                write!(
                    formatter,
                    "legacy non-prehashed minisign signatures are not accepted"
                )
            }
            Self::SignatureKeyMismatch => {
                write!(
                    formatter,
                    "the signature was made with a different key than this firmware trusts"
                )
            }
            Self::SignatureRejected => {
                write!(formatter, "the image does not match the staged signature")
            }
            Self::TrustedCommentRejected => {
                write!(formatter, "the trusted comment failed verification")
            }
            Self::ImageTooSmall { image_len } => {
                write!(formatter, "{image_len} bytes is too small for an app image")
            }
            Self::ImageMagic { first_byte } => {
                write!(
                    formatter,
                    "first byte 0x{first_byte:02X} is not an ESP application image"
                )
            }
            Self::ImageTooLarge {
                image_len,
                slot_len,
            } => {
                write!(
                    formatter,
                    "image of {image_len} bytes exceeds the {slot_len} byte slot"
                )
            }
            Self::BodyTruncated { received, expected } => {
                write!(
                    formatter,
                    "received {received} of the declared {expected} bytes"
                )
            }
            Self::SlotMissing { slot } => {
                write!(
                    formatter,
                    "partition table has no {} slot; flash the A/B migration first",
                    slot_name(*slot)
                )
            }
            Self::SlotBelowIdentityHead { slot, offset } => {
                write!(
                    formatter,
                    "{} at 0x{offset:X} reaches into the identity head",
                    slot_name(*slot)
                )
            }
            Self::SlotOverlapsRouteJournal { slot, offset, len } => {
                write!(
                    formatter,
                    "{} at 0x{offset:X}+0x{len:X} overlaps the route journal",
                    slot_name(*slot)
                )
            }
            Self::ReadbackMismatch => {
                write!(formatter, "flash readback does not match the received image")
            }
            Self::Slots(error) => write!(formatter, "ota partition access failed: {error:?}"),
        }
    }
}

async fn send_response(
    socket: &mut TcpSocket<'static>,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), ()> {
    let header = alloc::format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tcp_write_all(socket, header.as_bytes()).await?;
    tcp_write_all(socket, body).await
}

async fn send_error(socket: &mut TcpSocket<'static>, error: &UpdateError) -> Result<(), ()> {
    let body = alloc::format!("{{\"error\":\"{}\",\"detail\":\"{error}\"}}\n", error.code());
    send_response(socket, error.http_status(), JSON_CONTENT_TYPE, body.as_bytes()).await
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
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
        let word = (sextets[0] as u32) << 18
            | (sextets[1] as u32) << 12
            | (sextets[2] as u32) << 6
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
        panic!("HOPSPOT_OTA_PUBKEY must be the 56 character base64 key line of a minisign public key");
    }
    let mut raw = [0u8; MINISIGN_PUBLIC_KEY_RAW_LEN];
    let mut group = 0;
    while group < MINISIGN_PUBLIC_KEY_BASE64_LEN / 4 {
        let word = (const_sextet(encoded[group * 4]) as u32) << 18
            | (const_sextet(encoded[group * 4 + 1]) as u32) << 12
            | (const_sextet(encoded[group * 4 + 2]) as u32) << 6
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
    let mut public_key = [0u8; 32];
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

const UPDATE_PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Hopspot firmware update</title>
<style>
body{font:16px system-ui,sans-serif;background:#071014;color:#eef6f7;margin:0;padding:24px;display:grid;place-items:start center}
main{width:min(420px,100%)}
h1{font-size:20px;margin:0 0 4px}
p{color:#9ab0b7;font-size:14px;margin:6px 0}
label{display:block;margin:14px 0 4px;font-size:14px}
input[type=file]{width:100%;color:#9ab0b7}
button{margin-top:18px;width:100%;padding:12px;font-size:16px;font-weight:600;color:#071014;background:#49d2a9;border:0;border-radius:8px}
button:disabled{opacity:.5}
progress{width:100%;margin-top:14px;height:8px}
pre{white-space:pre-wrap;word-break:break-word;color:#9ab0b7;font:13px ui-monospace,monospace;margin-top:14px}
#meta{font:12px ui-monospace,monospace;color:#6f858c}
</style>
</head>
<body>
<main>
<h1>Firmware update</h1>
<p id="meta">reading node status...</p>
<p>Pick a signed firmware image and its .minisig signature. The node verifies the signature before it switches slots, then reboots. If this page opened inside the small sign-in window, close it and open http://192.168.4.1/update in your regular browser first.</p>
<label>Firmware image (.bin)<input type="file" id="image"></label>
<label>Signature (.minisig)<input type="file" id="sig"></label>
<button id="go">Install</button>
<progress id="bar" max="100" value="0" hidden></progress>
<pre id="out"></pre>
</main>
<script>
const out = (text) => { document.getElementById('out').textContent = text; };
const meta = document.getElementById('meta');
fetch('/update/status').then((r) => r.json()).then((s) => {
  meta.textContent = 'running ' + s.version + ' (' + s.commit + ') slot ' + s.slot + ' state ' + s.state;
}).catch(() => { meta.textContent = 'status unavailable'; });
document.getElementById('go').onclick = async () => {
  const image = document.getElementById('image').files[0];
  const sig = document.getElementById('sig').files[0];
  const button = document.getElementById('go');
  if (!image || !sig) { out('Choose both the firmware image and its .minisig signature.'); return; }
  button.disabled = true;
  try {
    out('Staging signature...');
    const staged = await fetch('/update/signature', { method: 'POST', body: sig });
    const stagedBody = await staged.json();
    if (!staged.ok) { out('Signature rejected: ' + stagedBody.detail); button.disabled = false; return; }
    out('Uploading ' + image.size + ' bytes. Keep this window open and stay on the Hopspot network.');
    const bar = document.getElementById('bar');
    bar.hidden = false;
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/update/image');
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) { bar.value = Math.round(100 * event.loaded / event.total); }
    };
    xhr.onload = () => {
      let body = null;
      try { body = JSON.parse(xhr.responseText); } catch (error) { body = null; }
      if (xhr.status === 200 && body && body.installed) {
        out('Installed into ' + body.slot + '. The node is rebooting; rejoin its network in about 30 seconds and reload this page to confirm the new build.');
      } else {
        out('Update failed: ' + (body && body.detail ? body.detail : 'status ' + xhr.status));
        button.disabled = false;
      }
    };
    xhr.onerror = () => {
      out('The connection dropped before the node answered. If the upload had finished, the node may be verifying and rebooting; rejoin its network and reload this page to check. Nothing is activated without a verified signature.');
      button.disabled = false;
    };
    xhr.send(image);
  } catch (error) {
    out('Update failed: ' + error);
    button.disabled = false;
  }
};
</script>
</body>
</html>
"##;
