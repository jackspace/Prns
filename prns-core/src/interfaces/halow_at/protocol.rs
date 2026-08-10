use heapless::Vec as HeaplessVec;

use crate::interfaces::{InterfaceId, InterfaceKind};

/// The largest `AT+TXDATA` exchange we will hand the module, header included. The Taixin AT
/// firmware documents no fixed cap; the real ceiling is the PHY symbol budget and so depends on
/// the configured bandwidth and MCS (a 1 MHz / MCS0 link rejects frames well under the Ethernet
/// MTU with an lmac "too long" error). This floor is provisional until the bench sweep measures
/// the true cap at our operating point.
// TODO(bench): replace with the measured TXDATA length cap (Phase 0 test 6).
pub const HALOW_AT_AIR_MTU: usize = 600;

/// The pseudo-Ethernet prefix the 1-to-many AT firmware puts on every `AT+TXDATA` payload and
/// `+RXDATA` delivery: destination MAC (6) ++ source MAC (6) ++ ethertype (2). The source MAC is
/// the only sender identity the AT layer surfaces.
// TODO(bench): confirm the header is present in group mode (Phase 0 test 3).
pub const HALOW_AT_HEADER_LEN: usize = 14;

const CHANNEL_TAG: &[u8] = b"halow-at";

pub const CHANNEL_TAG_CAP: usize = CHANNEL_TAG.len();

#[must_use]
pub fn channel_tag() -> HeaplessVec<u8, CHANNEL_TAG_CAP> {
    let mut tag = HeaplessVec::new();
    let _ = tag.extend_from_slice(CHANNEL_TAG);
    tag
}

#[must_use]
pub fn interface_id() -> InterfaceId {
    InterfaceId::from_channel_tag(InterfaceKind::HalowAt, CHANNEL_TAG)
}
