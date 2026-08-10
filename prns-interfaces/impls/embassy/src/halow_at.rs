//! 802.11ah (Wi-Fi HaLow) broadcast segment behind a Taixin TX-AH module's AT command UART,
//! generic over any [`embedded_io_async_07`] serial port. The module's `-wnb` firmware line
//! packetizes with `AT+TXDATA` / `+RXDATA`, so this interface hands whole Reticulum wire frames
//! across like [`esp_now`](prns_core::interfaces::esp_now) rather than running a serial decoder.
//!
//! Phase 1 skeleton: the driver compiles, holds the port, and reports [`ConnectionState::Initializing`]
//! while refusing traffic. The AT state machine (init sequence, response vs URC parsing, binary
//! `+RXDATA` chunk reads interleaved with line-oriented responses) lands once the bench numbers
//! fix the protocol constants.
// TODO(bench): AT init sequence (probe → bandwidth → frequency → MCS → group mode → group join)
// pends the Phase 0 command transcript, and the +RXDATA pseudo-Ethernet header handling pends
// Phase 0 test 3.

use embassy_futures::select::{select3, Either3};
use heapless::Vec as HeaplessVec;

use embedded_io_async_07::{Read, Write};

use prns_core::interfaces::halow_at::{self, CHANNEL_TAG_CAP, HALOW_AT_HW_MTU};
use prns_core::interfaces::{
    BitrateBps, ConnectionState, InterfaceDescriptor, InterfaceId, InterfaceKind,
};
use prns_runtime::manifold::driver::EmbassyInterfaceStatus;
use prns_runtime::manifold::interface_seam::{
    Interface, InterfaceSeam, OutboundDisposition, OutboundDropReason,
};

/// How much of the module's UART output the skeleton drains per read while the AT parser is
/// pending. Sized for a response line, not a data frame; the real parser owns frame-scale buffers.
const SKELETON_DRAIN_LEN: usize = 128;

pub struct HalowAtInterface<'a, R, W> {
    id: InterfaceId,
    uart_rx: R,
    uart_tx: W,
    bitrate: BitrateBps,
    tag: HeaplessVec<u8, CHANNEL_TAG_CAP>,
    status: &'a EmbassyInterfaceStatus,
}

impl<'a, R, W> HalowAtInterface<'a, R, W> {
    #[must_use]
    pub fn new(
        uart_rx: R,
        uart_tx: W,
        bitrate: BitrateBps,
        status: &'a EmbassyInterfaceStatus,
    ) -> Self {
        Self {
            id: halow_at::interface_id(),
            uart_rx,
            uart_tx,
            bitrate,
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

    async fn run<Seam: InterfaceSeam>(self, mut seam: Seam) {
        let HalowAtInterface {
            mut uart_rx,
            uart_tx,
            status,
            ..
        } = self;
        // The TX half idles until the AT state machine claims it for the init sequence.
        let mut _uart_tx = uart_tx;
        let mut drain = [0u8; SKELETON_DRAIN_LEN];
        status.set_connection(ConnectionState::Initializing);
        crate::diagnostic_log::info!("RNS_HALOW_AT skeleton up: AT state machine pending bench");

        loop {
            if !status.is_enabled() {
                status.set_connection(ConnectionState::Disabled);
                status.wait_until_enabled().await;
                status.set_connection(ConnectionState::Initializing);
            }

            match select3(
                uart_rx.read(&mut drain),
                seam.next_outbound(),
                status.wait_until_disabled(),
            )
            .await
            {
                Either3::First(_read) => {
                    // TODO(bench): feed the AT response / +RXDATA parser. Discarded until the
                    // group-mode header layout is confirmed on hardware.
                }
                Either3::Second(_outbound) => {
                    // TODO(bench): AT+TXDATA with the pseudo-Ethernet broadcast header. Refused
                    // rather than silently queued so the engine's backpressure stays honest.
                    seam.complete_outbound(OutboundDisposition::Dropped(
                        OutboundDropReason::TransportFailure,
                    ));
                }
                Either3::Third(()) => {}
            }
        }
    }
}
