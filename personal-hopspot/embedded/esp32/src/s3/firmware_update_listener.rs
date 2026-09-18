//! A bench transport for [`crate::firmware_update`]: one signed firmware image per TCP connection.
//!
//! This exists so the install engine can be exercised on real hardware while the question of how
//! Hopspot should really take an update stays open. It is deliberately the smallest thing that can
//! carry an image: a fixed twelve byte header, the `.minisig` document, then the image, and one
//! line of reply. No HTTP, no framing library, nothing to negotiate.
//!
//! It is not a shipping surface and the feature that compiles it is off in every board package.
//! Anyone on the same link can open the port and write bytes into the slot the node is not running
//! from, or hold the install guard by connecting and stalling. What they cannot do is get those
//! bytes selected for boot: the compiled Minisign key gates the only place the boot selection
//! moves. Treat the port as bench equipment, not as a door with a lock on it.
//!
//! Wire format, all integers big endian:
//!
//! ```text
//! "HSFW1\0" | u16 signature_len | u32 image_len | signature bytes | image bytes
//! ```
//!
//! `signature_len = 0` with `image_len = 0` asks for status instead of installing. The reply is a
//! single line: `ok slot=... bytes=...`, `status ...`, or `err <code> <detail>`, where the code is
//! the engine's own refusal code.

use super::*;

use crate::firmware_update::{self, InstallError, InstallGuard, InstallStatus, InstalledImage};
use crate::memory::EspFirmwareMemory;

/// Next to the 42699 Wi-Fi Auto rendezvous, so the two bench ports sit together.
const FIRMWARE_UPDATE_PORT: u16 = 42700;
const REQUEST_MAGIC: &[u8; 6] = b"HSFW1\0";
const REQUEST_HEADER_LEN: usize = REQUEST_MAGIC.len() + 2 + 4;
/// The engine's own document limit. Enforced here too so an absurd length is refused before the
/// bytes are read rather than after.
const SIGNATURE_DOCUMENT_MAX: usize = 1024;
/// A firmware upload is the only request that streams megabytes at this firmware, and on the old
/// bench branch it starved itself at 4 KiB: every sector erase and write parks the executor, the
/// receive window drains and never refills. 16 KiB and not more — 64 KiB per socket corrupted the
/// internal heap the radio stack shares.
const SOCKET_RX_BUFFER_BYTES: usize = 16 * 1024;
const SOCKET_TX_BUFFER_BYTES: usize = 1024;
/// One sector's worth of image per read, which is exactly what the engine buffers before it
/// touches flash.
const UPLOAD_CHUNK_BYTES: usize = 4096;
/// Ten minutes. A whole image over a poor link has been measured at a few KB/s, and a timeout
/// firing mid transfer looks exactly like a device fault when it is really a slow radio.
const SOCKET_TIMEOUT_SECS: u64 = 600;
/// The header and the signature document together are under a kilobyte, so they get a short
/// deadline rather than the image's. This is what stops an unauthenticated peer on the bench port
/// from holding the install guard for the full socket timeout by connecting and stalling.
const HANDSHAKE_TIMEOUT_SECS: u64 = 20;
/// Long enough for the success line to leave the board before the reset takes the link down.
const REBOOT_HOLDOFF_MS: u64 = 500;
/// One listener per stack: the station uplink and the SoftAP.
const LISTENER_POOL: usize = 2;

/// Accept one install at a time, forever. `link` names which stack this listener is on, because on
/// an APSTA board both are up and a bench log that cannot tell them apart is not evidence.
#[embassy_executor::task(pool_size = LISTENER_POOL)]
pub(super) async fn firmware_update_listener_task(
    stack: Stack<'static>,
    profile: &'static personal_hopspot_memory::MemoryProfile,
    link: &'static str,
) -> ! {
    // Socket storage is ordinary software state; PSRAM keeps the scarce internal SRAM for the
    // radio, the same reasoning as every other socket on these stacks.
    let rx_buffer = crate::storage::allocate_psram_slice(SOCKET_RX_BUFFER_BYTES, 0u8);
    let tx_buffer = crate::storage::allocate_psram_slice(SOCKET_TX_BUFFER_BYTES, 0u8);
    let signature_buffer = crate::storage::allocate_psram_slice(SIGNATURE_DOCUMENT_MAX, 0u8);
    let chunk_buffer = crate::storage::allocate_psram_slice(UPLOAD_CHUNK_BYTES, 0u8);
    let memory = EspFirmwareMemory::new(profile);
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(SOCKET_TIMEOUT_SECS)));
    log::info!("update: listening on {link} port {FIRMWARE_UPDATE_PORT}");

    loop {
        if let Err(error) = socket.accept(FIRMWARE_UPDATE_PORT).await {
            log::warn!("update: {link} accept failed: {error:?}");
            Timer::after(Duration::from_millis(250)).await;
            continue;
        }
        let peer = socket.remote_endpoint();
        let served = serve(
            &mut socket,
            &memory,
            signature_buffer,
            chunk_buffer,
            link,
            peer,
        )
        .await;
        let reply = match &served {
            Ok(Served::Status(status)) => status_line(status),
            Ok(Served::Installed(installed)) => alloc::format!(
                "ok slot={} bytes={}\n",
                firmware_update::slot_name(installed.slot),
                installed.image_len
            ),
            Err(error) => alloc::format!("err {} {error}\n", error.code()),
        };
        let written = write_all(&mut socket, reply.as_bytes()).await;
        socket.close();
        let flush = with_timeout(Duration::from_secs(2), socket.flush()).await;
        match &served {
            Ok(Served::Status(_)) => log::info!("update: {link} status to {peer:?}"),
            Ok(Served::Installed(installed)) => {
                // The guard inside `installed` is still held, so nothing can start a second
                // install into the slot this node is about to boot from.
                log::info!(
                    "update: {link} installed {} bytes into {}, rebooting in {REBOOT_HOLDOFF_MS} ms (reply_written={written} flush={flush:?})",
                    installed.image_len,
                    firmware_update::slot_name(installed.slot)
                );
                Timer::after(Duration::from_millis(REBOOT_HOLDOFF_MS)).await;
                esp_hal::system::software_reset();
            }
            Err(error) => log::warn!(
                "update: {link} refused {peer:?}: {} ({error})",
                error.code()
            ),
        }
        socket.abort();
        Timer::after(Duration::from_millis(50)).await;
    }
}

enum Served {
    Status(InstallStatus),
    Installed(InstalledImage),
}

async fn serve(
    socket: &mut TcpSocket<'static>,
    memory: &EspFirmwareMemory,
    signature_buffer: &mut [u8],
    chunk_buffer: &mut [u8],
    link: &'static str,
    peer: Option<embassy_net::IpEndpoint>,
) -> Result<Served, ListenerError> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    // The ten minute socket timeout exists for a whole image over a poor link. Applying it to the
    // handshake as well lets anyone who can reach the port hold the install guard for ten minutes
    // by connecting and saying nothing, so the header and the signature get a short deadline of
    // their own. Both are tiny and arrive together in practice.
    with_timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        read_exact(socket, &mut header),
    )
    .await
    .map_err(|_| ListenerError::HandshakeTimeout)??;
    if &header[..REQUEST_MAGIC.len()] != REQUEST_MAGIC {
        return Err(ListenerError::RequestMagic);
    }
    let signature_len = usize::from(u16::from_be_bytes([header[6], header[7]]));
    let image_len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    if signature_len == 0 && image_len == 0 {
        return Ok(Served::Status(firmware_update::status(memory)));
    }
    if signature_len == 0 {
        return Err(ListenerError::SignatureMissing);
    }
    if signature_len > SIGNATURE_DOCUMENT_MAX {
        return Err(ListenerError::SignatureDocumentTooLarge {
            document_len: signature_len,
        });
    }

    // Taken before the signature is even read: staging writes engine state, and a second
    // connection arriving mid install must be refused rather than allowed to overwrite it.
    let guard = InstallGuard::acquire().map_err(ListenerError::Install)?;
    with_timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        read_exact(socket, &mut signature_buffer[..signature_len]),
    )
    .await
    .map_err(|_| ListenerError::HandshakeTimeout)??;
    let document = core::str::from_utf8(&signature_buffer[..signature_len])
        .map_err(|_| ListenerError::SignatureNotText)?;
    firmware_update::stage_signature(document).map_err(ListenerError::Install)?;

    let mut install =
        firmware_update::begin(memory, guard, image_len).map_err(ListenerError::Install)?;
    // The engine logs the slot and the length; only the transport knows who is sending them.
    log::info!(
        "update: {link} upload from {peer:?} targets {}",
        firmware_update::slot_name(install.target_slot())
    );
    let mut remaining = image_len;
    while remaining > 0 {
        let take = chunk_buffer.len().min(remaining);
        match socket.read(&mut chunk_buffer[..take]).await {
            Ok(0) => {
                log::warn!("update: {link} upload ended early with {remaining} bytes outstanding");
                break;
            }
            Ok(read) => {
                install
                    .write(&chunk_buffer[..read])
                    .await
                    .map_err(ListenerError::Install)?;
                remaining -= read;
            }
            Err(error) => {
                // Why it stopped matters: a peer hanging up, a socket timing out and a link
                // dropping under us need different fixes and look identical in a length alone.
                log::warn!(
                    "update: {link} upload read failed with {remaining} bytes outstanding: {error:?}"
                );
                break;
            }
        }
    }
    install
        .finish()
        .await
        .map(Served::Installed)
        .map_err(ListenerError::Install)
}

fn status_line(status: &InstallStatus) -> alloc::string::String {
    let mut line = alloc::format!(
        "status busy={} selected={} state={} booted={} signature={}",
        status.busy,
        status.selected,
        status.state,
        status.booted,
        if status.signature_staged {
            "staged"
        } else {
            "none"
        }
    );
    match status.key_id {
        // Minisign prints a key id as its bytes reversed, which is what the bench signer writes
        // into the public key comment, so print it the same way or the two cannot be compared.
        Some(key_id) => {
            line.push_str(" key=");
            for byte in key_id.iter().rev() {
                let _ = core::fmt::Write::write_fmt(&mut line, format_args!("{byte:02X}"));
            }
        }
        None => line.push_str(" key=none"),
    }
    line.push('\n');
    line
}

async fn read_exact(
    socket: &mut TcpSocket<'static>,
    buffer: &mut [u8],
) -> Result<(), ListenerError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match socket.read(&mut buffer[filled..]).await {
            Ok(0) => return Err(ListenerError::RequestTruncated),
            Ok(read) => filled += read,
            Err(error) => {
                log::warn!("update: request read failed: {error:?}");
                return Err(ListenerError::RequestReadFailed);
            }
        }
    }
    Ok(())
}

async fn write_all(socket: &mut TcpSocket<'static>, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        match socket.write(bytes).await {
            Ok(0) | Err(_) => return false,
            Ok(written) => bytes = &bytes[written..],
        }
    }
    true
}

/// Everything the transport can refuse on its own, plus the engine's refusals passed through
/// unchanged so a bench log reads the same code the engine raised.
enum ListenerError {
    RequestTruncated,
    RequestReadFailed,
    HandshakeTimeout,
    RequestMagic,
    SignatureMissing,
    SignatureDocumentTooLarge { document_len: usize },
    SignatureNotText,
    Install(InstallError),
}

impl ListenerError {
    fn code(&self) -> &'static str {
        match self {
            Self::RequestTruncated => "request-truncated",
            Self::RequestReadFailed => "request-read-failed",
            Self::HandshakeTimeout => "handshake-timeout",
            Self::RequestMagic => "request-magic",
            Self::SignatureMissing => "signature-missing",
            Self::SignatureDocumentTooLarge { .. } => "signature-document-too-large",
            Self::SignatureNotText => "signature-not-text",
            Self::Install(error) => error.code(),
        }
    }
}

impl core::fmt::Display for ListenerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::RequestTruncated => {
                write!(formatter, "the request ended before the header was complete")
            }
            Self::RequestReadFailed => write!(formatter, "the link failed while reading the request"),
            Self::HandshakeTimeout => write!(
                formatter,
                "the header and signature did not arrive within {HANDSHAKE_TIMEOUT_SECS}s"
            ),
            Self::RequestMagic => write!(formatter, "this is not a firmware update request"),
            Self::SignatureMissing => {
                write!(formatter, "an image needs a signature document ahead of it")
            }
            Self::SignatureDocumentTooLarge { document_len } => write!(
                formatter,
                "signature document of {document_len} bytes exceeds the {SIGNATURE_DOCUMENT_MAX} byte limit"
            ),
            Self::SignatureNotText => {
                write!(formatter, "the signature document is not valid text")
            }
            Self::Install(error) => write!(formatter, "{error}"),
        }
    }
}
