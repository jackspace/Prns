//! 802.11ah (Wi-Fi HaLow) broadcast segment behind a Taixin TX-AH module's AT command UART,
//! generic over any [`embedded_io_async_07`] serial port pair.
//!
//! The module's group mode is a connectionless shared channel: no association, every node hears
//! every frame. One `AT+TXDATA` exchange carries at most 256 bytes (a firmware buffer cap, not a
//! PHY limit), so each Reticulum wire frame is HDLC-encoded ([`rns_serial_framing`]) and chunked
//! across air frames; receivers reassemble **per sender** (the delivery header carries the
//! sender's module MAC) so interleaved transmitters cannot corrupt each other's streams, and the
//! 0x7E flags self-resync after any lost chunk.
//!
//! The module's heap is fragile: parked in the wrong mode or driven with sustained bursts it
//! leaks its skb pool until every send fails — and config writes fail silently in that state.
//! The driver therefore trusts nothing it did not verify: init is reset → await the boot banner →
//! query the config → set only what differs → re-query, and any send failure or unexpected
//! reboot tears the interface down through [`ReconnectPolicy`] backoff into a fresh reset.

use embassy_futures::select::{select3, Either3};
use embassy_time::{with_timeout, Duration, Instant};
use heapless::String as HeaplessString;
use heapless::Vec as HeaplessVec;

use core::fmt::Write as _;

use embedded_io_async_07::{Read, Write};

use prns_core::engine::InstantMillis;
use prns_core::interfaces::halow_at::{
    self, broadcast_header, is_boot_banner, is_error, is_ok, line_contains, parse_mac_after,
    split_rx_frame, AtConsole, AtStep, CHANNEL_TAG_CAP, HALOW_AT_CHUNK_CAP, HALOW_AT_HEADER_LEN,
    HALOW_AT_HW_MTU, HALOW_AT_MAX_WIRE_FRAME_LEN,
};
use prns_core::interfaces::rns_serial_framing::{self, RnsSerialDecoder};
use prns_core::interfaces::{
    BitrateBps, ConnectionState, InterfaceDescriptor, InterfaceId, InterfaceKind,
};
use prns_runtime::manifold::driver::EmbassyInterfaceStatus;
use prns_runtime::manifold::interface_seam::{
    Interface, InterfaceSeam, OutboundDisposition, OutboundDropReason,
};
use prns_runtime::manifold::reconnect::ReconnectPolicy;
use prns_runtime::manifold::throughput::ThroughputLedger;

/// The channel the fleet parks on: 924.0 MHz, chosen away from the LoRa deployment at
/// 906.875 MHz. Bench-persisted on the modules, but init verifies and re-pins it — the driver
/// assumes nothing about module state.
// Refinable: becomes a radio profile once more than one deployment needs a different channel.
const HALOW_AT_CHAN_LIST: &[u8] = b"9240";
/// 2 MHz bandwidth, the bench-qualified operating point (est_rate 25–29 kbps at MCS0).
const HALOW_AT_BSS_BW: &[u8] = b"2";

/// Concurrent senders whose HDLC streams are reassembled in parallel. A slot is ~600 bytes; the
/// oldest stream is evicted when a new sender appears with every slot busy — its next 0x7E flag
/// re-syncs it if it returns.
const PEER_SLOTS: usize = 4;

/// Module reboot takes ~2 s to the banner; generous because the reset is the recovery of last
/// resort and a false timeout only costs another reset.
const BOOT_BANNER_TIMEOUT: Duration = Duration::from_secs(8);
/// After the banner the module dumps its config block; command handling is unreliable until it
/// settles. The drain keeps the UART FIFO from overflowing meanwhile.
const BOOT_SETTLE: Duration = Duration::from_millis(1500);
/// Per-command OK/ERROR window, and the window for one config-dump query to answer.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
/// The two per-chunk waits: the OK that prompts for raw bytes, and the OK/ERROR verdict after.
const TX_STEP_TIMEOUT: Duration = Duration::from_secs(1);

const ENCODE_CAP: usize = rns_serial_framing::max_encoded_len(HALOW_AT_MAX_WIRE_FRAME_LEN);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The serial port itself failed; nothing to do but retry the whole bring-up.
    Uart,
    /// A command was answered with ERROR, timed out, or verification did not match — the
    /// module-heap failure signature. Reset and reconfigure.
    Module,
}

struct Peer {
    mac: [u8; 6],
    decoder: RnsSerialDecoder<HALOW_AT_MAX_WIRE_FRAME_LEN>,
    last_seen: Instant,
}

/// What the config survey saw, and the address it recovered. `addr` is the module's own MAC —
/// the source identity every transmitted header carries.
#[derive(Default)]
struct ConfigSurvey {
    addr: Option<[u8; 6]>,
    role_group: bool,
    channel: bool,
    bandwidth: bool,
}

impl ConfigSurvey {
    fn absorb(&mut self, line: &[u8]) {
        if self.addr.is_none() {
            self.addr = parse_mac_after(line, b"addr:");
        }
        // "join_group:0" also contains "group"; requiring a role token on the same line keeps
        // the check keyed to the field that matters.
        if (line_contains(line, b"role") || line_contains(line, b"ROLE"))
            && line_contains(line, b"group")
        {
            self.role_group = true;
        }
        if line_contains(line, HALOW_AT_CHAN_LIST) {
            self.channel = true;
        }
        if line_contains(line, b"bss_bw:2") || line_contains(line, b"BSS_BW 2") {
            self.bandwidth = true;
        }
    }

    fn configured(&self) -> bool {
        self.addr.is_some() && self.role_group && self.channel && self.bandwidth
    }
}

pub struct HalowAtInterface<'a, R, W> {
    id: InterfaceId,
    uart_rx: R,
    uart_tx: W,
    bitrate: BitrateBps,
    reconnect_policy: ReconnectPolicy,
    tag: HeaplessVec<u8, CHANNEL_TAG_CAP>,
    status: &'a EmbassyInterfaceStatus,
}

impl<'a, R, W> HalowAtInterface<'a, R, W> {
    #[must_use]
    pub fn new(
        uart_rx: R,
        uart_tx: W,
        bitrate: BitrateBps,
        reconnect_policy: ReconnectPolicy,
        status: &'a EmbassyInterfaceStatus,
    ) -> Self {
        Self {
            id: halow_at::interface_id(),
            uart_rx,
            uart_tx,
            bitrate,
            reconnect_policy,
            tag: halow_at::channel_tag(),
            status,
        }
    }

    #[must_use]
    pub fn id(&self) -> InterfaceId {
        self.id
    }

    /// The id this interface will carry — for the caller that stands its
    /// [`EmbassyInterfaceStatus`] up under the same key before building the interface.
    #[must_use]
    pub fn interface_id() -> InterfaceId {
        halow_at::interface_id()
    }
}

impl<R: Read, W: Write> Interface for HalowAtInterface<'_, R, W> {
    const HW_MTU: usize = HALOW_AT_HW_MTU;
    const KIND: InterfaceKind = InterfaceKind::HalowAt;

    fn descriptor(&self) -> InterfaceDescriptor {
        halow_at::descriptor(self.id, self.bitrate)
    }

    fn channel_tag(&self) -> &[u8] {
        &self.tag
    }

    #[allow(clippy::too_many_lines)]
    async fn run<Seam: InterfaceSeam>(self, mut seam: Seam) {
        let HalowAtInterface {
            mut uart_rx,
            mut uart_tx,
            reconnect_policy,
            status,
            ..
        } = self;
        let mut console = AtConsole::new();
        let mut peers: [Option<Peer>; PEER_SLOTS] = [const { None }; PEER_SLOTS];
        let mut reconnect = reconnect_policy.schedule();
        let mut throughput = ThroughputLedger::new();
        let started = Instant::now();
        // The wire frame in custody while its chunks cross the module, and the HDLC scratch.
        let mut custody = [0u8; HALOW_AT_MAX_WIRE_FRAME_LEN];
        let mut encoded = [0u8; ENCODE_CAP];

        loop {
            if !status.is_enabled() {
                status.set_connection(ConnectionState::Disabled);
                status.wait_until_enabled().await;
            }
            status.set_connection(ConnectionState::Initializing);

            let own_mac = match initialize(&mut uart_rx, &mut uart_tx, &mut console).await {
                Ok(mac) => mac,
                Err(fault) => {
                    status.set_connection(ConnectionState::Reconnecting);
                    let delay = reconnect.next_delay(|bytes| seam.fill_entropy(bytes));
                    crate::diagnostic_log::warn!(
                        "RNS_HALOW_AT init failed ({fault:?}); retry in {}ms",
                        delay.as_millis()
                    );
                    embassy_time::Timer::after(Duration::from_millis(delay.as_millis() as u64))
                        .await;
                    continue;
                }
            };
            let header = broadcast_header(own_mac);
            let connected_at = Instant::now();
            status.set_connection(ConnectionState::Connected);
            crate::diagnostic_log::info!(
                "RNS_HALOW_AT up: group mode verified, module mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                own_mac[0], own_mac[1], own_mac[2], own_mac[3], own_mac[4], own_mac[5]
            );

            let fault = 'steady: loop {
                let mut rx_buf = [0u8; 64];
                match select3(
                    uart_rx.read(&mut rx_buf),
                    seam.next_outbound(),
                    status.wait_until_disabled(),
                )
                .await
                {
                    Either3::First(Err(_) | Ok(0)) => break 'steady Some(Fault::Uart),
                    Either3::First(Ok(read)) => {
                        for &byte in &rx_buf[..read] {
                            match console.feed(byte) {
                                AtStep::None => {}
                                AtStep::Line => {
                                    if is_boot_banner(console.line()) {
                                        // The module restarted underneath us; its config may
                                        // not have survived whatever killed it. Re-verify.
                                        break 'steady Some(Fault::Module);
                                    }
                                }
                                AtStep::RxFrame => {
                                    deliver(
                                        console.rx_frame(),
                                        own_mac,
                                        &mut peers,
                                        &mut seam,
                                        status,
                                        &mut throughput,
                                        started,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    Either3::Second(outbound) => {
                        let len = outbound.len().min(custody.len());
                        custody[..len].copy_from_slice(&outbound[..len]);
                        seam.accept_outbound_custody();
                        let sent = transmit(
                            &mut uart_rx,
                            &mut uart_tx,
                            &mut console,
                            &header,
                            &custody[..len],
                            &mut encoded,
                            own_mac,
                            &mut peers,
                            &mut seam,
                            status,
                            &mut throughput,
                            started,
                        )
                        .await;
                        match sent {
                            Ok(()) => seam.complete_outbound(OutboundDisposition::Sent),
                            Err(fault) => {
                                seam.complete_outbound(OutboundDisposition::Dropped(
                                    OutboundDropReason::TransportFailure,
                                ));
                                break 'steady Some(fault);
                            }
                        }
                    }
                    Either3::Third(()) => break 'steady None,
                }
            };

            reconnect.record_connection_lifetime(core::time::Duration::from_millis(
                connected_at.elapsed().as_millis(),
            ));
            if fault.is_some() {
                status.set_connection(ConnectionState::Reconnecting);
                let delay = reconnect.next_delay(|bytes| seam.fill_entropy(bytes));
                embassy_time::Timer::after(Duration::from_millis(delay.as_millis() as u64)).await;
            }
            // Disabled falls straight through: the loop head parks until re-enable, then a fresh
            // init re-verifies the module rather than trusting whatever state it idled in.
        }
    }
}

/// Reset the module, wait out its boot, and bring the config to the group-broadcast operating
/// point — querying first, setting only what differs, and re-querying because a write under
/// module heap pressure fails with nothing but a log line. Returns the module's own MAC.
async fn initialize<R: Read, W: Write>(
    uart_rx: &mut R,
    uart_tx: &mut W,
    console: &mut AtConsole,
) -> Result<[u8; 6], Fault> {
    console.reset();
    send(uart_tx, b"AT+RESET\r\n").await?;
    await_boot_banner(uart_rx, console).await?;
    crate::diagnostic_log::info!("RNS_HALOW_AT module boot banner seen");
    drain(uart_rx, console, BOOT_SETTLE).await?;

    let mut survey = query_config(uart_rx, uart_tx, console).await?;
    crate::diagnostic_log::info!(
        "RNS_HALOW_AT survey: addr={} role_group={} channel={} bandwidth={}",
        survey.addr.is_some(),
        survey.role_group,
        survey.channel,
        survey.bandwidth
    );
    if !survey.configured() {
        if !survey.role_group {
            command(uart_rx, uart_tx, console, b"AT+MODE=group\r\n").await?;
        }
        if !survey.bandwidth {
            let mut set: HeaplessString<24> = HeaplessString::new();
            let _ = write!(
                set,
                "AT+BSS_BW={}\r\n",
                core::str::from_utf8(HALOW_AT_BSS_BW).unwrap_or("2")
            );
            command(uart_rx, uart_tx, console, set.as_bytes()).await?;
        }
        if !survey.channel {
            let mut set: HeaplessString<24> = HeaplessString::new();
            let _ = write!(
                set,
                "AT+CHAN_LIST={}\r\n",
                core::str::from_utf8(HALOW_AT_CHAN_LIST).unwrap_or("9240")
            );
            command(uart_rx, uart_tx, console, set.as_bytes()).await?;
        }
        survey = query_config(uart_rx, uart_tx, console).await?;
    }
    match survey.addr {
        Some(mac) if survey.configured() => Ok(mac),
        _ => Err(Fault::Module),
    }
}

async fn send<W: Write>(uart_tx: &mut W, bytes: &[u8]) -> Result<(), Fault> {
    uart_tx.write_all(bytes).await.map_err(|_| Fault::Uart)?;
    uart_tx.flush().await.map_err(|_| Fault::Uart)
}

async fn await_boot_banner<R: Read>(uart_rx: &mut R, console: &mut AtConsole) -> Result<(), Fault> {
    let deadline = Instant::now() + BOOT_BANNER_TIMEOUT;
    let mut rx_buf = [0u8; 64];
    let mut seen: usize = 0;
    loop {
        let now = Instant::now();
        if now >= deadline {
            // Zero bytes across the whole window is a wiring/level story, not a slow module.
            crate::diagnostic_log::warn!("RNS_HALOW_AT no banner; {seen} bytes seen in window");
            return Err(Fault::Module);
        }
        match with_timeout(deadline - now, uart_rx.read(&mut rx_buf)).await {
            Err(_) => {
                crate::diagnostic_log::warn!("RNS_HALOW_AT no banner; {seen} bytes seen in window");
                return Err(Fault::Module);
            }
            Ok(Err(_)) => return Err(Fault::Uart),
            Ok(Ok(read)) => {
                seen += read;
                for &byte in &rx_buf[..read] {
                    if console.feed(byte) == AtStep::Line && is_boot_banner(console.line()) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Keep the FIFO drained for `period` while the module talks to itself (boot config dump).
async fn drain<R: Read>(
    uart_rx: &mut R,
    console: &mut AtConsole,
    period: Duration,
) -> Result<(), Fault> {
    let deadline = Instant::now() + period;
    let mut rx_buf = [0u8; 64];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        match with_timeout(deadline - now, uart_rx.read(&mut rx_buf)).await {
            Err(_) => return Ok(()),
            Ok(Err(_)) => return Err(Fault::Uart),
            Ok(Ok(read)) => {
                for &byte in &rx_buf[..read] {
                    let _ = console.feed(byte);
                }
            }
        }
    }
}

/// Send one command and skim console noise until its OK or ERROR.
async fn command<R: Read, W: Write>(
    uart_rx: &mut R,
    uart_tx: &mut W,
    console: &mut AtConsole,
    line: &[u8],
) -> Result<(), Fault> {
    send(uart_tx, line).await?;
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut rx_buf = [0u8; 64];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Fault::Module);
        }
        match with_timeout(deadline - now, uart_rx.read(&mut rx_buf)).await {
            Err(_) => return Err(Fault::Module),
            Ok(Err(_)) => return Err(Fault::Uart),
            Ok(Ok(read)) => {
                for &byte in &rx_buf[..read] {
                    if console.feed(byte) == AtStep::Line {
                        if is_ok(console.line()) {
                            return Ok(());
                        }
                        if is_error(console.line()) {
                            return Err(Fault::Module);
                        }
                    }
                }
            }
        }
    }
}

/// Dump the module config and survey it for the operating point and the module's own MAC. The
/// dump's exact layout is firmware noise; the survey keys on tokens. Collection runs the full
/// response window unless everything needed has been seen.
async fn query_config<R: Read, W: Write>(
    uart_rx: &mut R,
    uart_tx: &mut W,
    console: &mut AtConsole,
) -> Result<ConfigSurvey, Fault> {
    send(uart_tx, b"AT+WNBCFG\r\n").await?;
    let mut survey = ConfigSurvey::default();
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut rx_buf = [0u8; 64];
    loop {
        let now = Instant::now();
        if now >= deadline || survey.configured() {
            return Ok(survey);
        }
        match with_timeout(deadline - now, uart_rx.read(&mut rx_buf)).await {
            Err(_) => return Ok(survey),
            Ok(Err(_)) => return Err(Fault::Uart),
            Ok(Ok(read)) => {
                for &byte in &rx_buf[..read] {
                    if console.feed(byte) == AtStep::Line {
                        survey.absorb(console.line());
                    }
                }
            }
        }
    }
}

/// HDLC-encode one wire frame and push it across the module in `AT+TXDATA` chunks: command →
/// OK prompt → header + chunk bytes → OK verdict. Deliveries that interleave with the waits are
/// handed to the seam as usual. Any ERROR, timeout, or surprise reboot aborts the whole frame —
/// the caller resets the module rather than trusting it.
#[allow(clippy::too_many_arguments)]
async fn transmit<R: Read, W: Write, Seam: InterfaceSeam>(
    uart_rx: &mut R,
    uart_tx: &mut W,
    console: &mut AtConsole,
    header: &[u8; HALOW_AT_HEADER_LEN],
    frame: &[u8],
    encoded: &mut [u8; ENCODE_CAP],
    own_mac: [u8; 6],
    peers: &mut [Option<Peer>; PEER_SLOTS],
    seam: &mut Seam,
    status: &EmbassyInterfaceStatus,
    throughput: &mut ThroughputLedger,
    started: Instant,
) -> Result<(), Fault> {
    let Ok(encoded_len) = rns_serial_framing::encode(frame, encoded) else {
        // A frame past the seam's own cap cannot exist; treat as sent-nothing, not module fault.
        return Ok(());
    };
    for chunk in encoded[..encoded_len].chunks(HALOW_AT_CHUNK_CAP) {
        let air_len = HALOW_AT_HEADER_LEN + chunk.len();
        let mut cmd: HeaplessString<24> = HeaplessString::new();
        let _ = write!(cmd, "AT+TXDATA={air_len}\r\n");
        send(uart_tx, cmd.as_bytes()).await?;
        await_tx_ok(
            uart_rx, console, own_mac, peers, seam, status, throughput, started,
        )
        .await?;
        send(uart_tx, header).await?;
        send(uart_tx, chunk).await?;
        await_tx_ok(
            uart_rx, console, own_mac, peers, seam, status, throughput, started,
        )
        .await?;
        let now = InstantMillis(started.elapsed().as_millis());
        status.add_tx(air_len as u64);
        throughput.record_tx(now, air_len as u64);
        status.set_transfer_rates(throughput.rates());
        crate::diagnostic_log::info!("RNS_HALOW_AT tx chunk air_len={air_len}");
    }
    Ok(())
}

/// One OK/ERROR wait inside the TXDATA two-step, still servicing interleaved deliveries and
/// watching for a surprise reboot.
#[allow(clippy::too_many_arguments)]
async fn await_tx_ok<R: Read, Seam: InterfaceSeam>(
    uart_rx: &mut R,
    console: &mut AtConsole,
    own_mac: [u8; 6],
    peers: &mut [Option<Peer>; PEER_SLOTS],
    seam: &mut Seam,
    status: &EmbassyInterfaceStatus,
    throughput: &mut ThroughputLedger,
    started: Instant,
) -> Result<(), Fault> {
    let deadline = Instant::now() + TX_STEP_TIMEOUT;
    let mut rx_buf = [0u8; 64];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Fault::Module);
        }
        match with_timeout(deadline - now, uart_rx.read(&mut rx_buf)).await {
            Err(_) => return Err(Fault::Module),
            Ok(Err(_)) => return Err(Fault::Uart),
            Ok(Ok(read)) => {
                for &byte in &rx_buf[..read] {
                    match console.feed(byte) {
                        AtStep::None => {}
                        AtStep::Line => {
                            if is_ok(console.line()) {
                                return Ok(());
                            }
                            if is_error(console.line()) || is_boot_banner(console.line()) {
                                return Err(Fault::Module);
                            }
                        }
                        AtStep::RxFrame => {
                            deliver(
                                console.rx_frame(),
                                own_mac,
                                peers,
                                seam,
                                status,
                                throughput,
                                started,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }
}

/// Demultiplex one delivery into its sender's HDLC stream and hand every completed wire frame to
/// the seam. Unknown senders take a free slot or evict the longest-quiet stream; the flags
/// re-sync an evicted sender's next frame boundary if it returns.
async fn deliver<Seam: InterfaceSeam>(
    air_frame: &[u8],
    own_mac: [u8; 6],
    peers: &mut [Option<Peer>; PEER_SLOTS],
    seam: &mut Seam,
    status: &EmbassyInterfaceStatus,
    throughput: &mut ThroughputLedger,
    started: Instant,
) {
    let Some((src, payload)) = split_rx_frame(air_frame) else {
        return;
    };
    if src == own_mac {
        return;
    }
    let now = InstantMillis(started.elapsed().as_millis());
    status.add_rx(air_frame.len() as u64);
    throughput.record_rx(now, air_frame.len() as u64);
    status.set_transfer_rates(throughput.rates());
    crate::diagnostic_log::info!(
        "RNS_HALOW_AT rx air_len={} src={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        air_frame.len(),
        src[0],
        src[1],
        src[2],
        src[3],
        src[4],
        src[5]
    );

    let slot = select_peer_slot(peers, src);
    let peer = peers[slot].get_or_insert_with(|| Peer {
        mac: src,
        decoder: RnsSerialDecoder::new(),
        last_seen: Instant::now(),
    });
    if peer.mac != src {
        peer.mac = src;
        peer.decoder.reset();
    }
    peer.last_seen = Instant::now();

    let mut offset = 0;
    while offset < payload.len() {
        match peer.decoder.feed_slice_next(payload, &mut offset) {
            Ok(Some(frame)) => seam.next_inbound(frame).await,
            Ok(None) => break,
            // An overlong stream segment (lost flag between two senders' bursts) drops and the
            // scanner realigns at the next flag.
            Err(_) => {}
        }
    }
}

fn select_peer_slot(peers: &[Option<Peer>; PEER_SLOTS], src: [u8; 6]) -> usize {
    let mut oldest = 0;
    let mut oldest_seen = Instant::MAX;
    for (index, slot) in peers.iter().enumerate() {
        match slot {
            Some(peer) if peer.mac == src => return index,
            Some(peer) => {
                if peer.last_seen < oldest_seen {
                    oldest_seen = peer.last_seen;
                    oldest = index;
                }
            }
            None => return index,
        }
    }
    oldest
}
