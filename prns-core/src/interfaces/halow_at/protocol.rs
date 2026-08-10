use heapless::Vec as HeaplessVec;

use crate::interfaces::{InterfaceId, InterfaceKind};

/// The module's hard `AT+TXDATA` ceiling, header included: 256 bytes total. Bench-verified on
/// fresh heaps (256 OK / 258 ERROR) and independent of MCS — it is an AT/UART buffer limit in the
/// Taixin firmware, not a PHY symbol budget.
pub const HALOW_AT_AIR_FRAME_CAP: usize = 256;

/// The pseudo-Ethernet prefix on every `AT+TXDATA` payload and `+RXDATA` delivery:
/// destination MAC (6) ++ source MAC (6) ++ ethertype (2). Bench-confirmed present on group-mode
/// receive, with the source field carrying the sender's real module MAC — the only sender
/// identity the AT layer surfaces.
pub const HALOW_AT_HEADER_LEN: usize = 14;

/// The data budget of one air frame: the hard cap less the header the module requires.
pub const HALOW_AT_CHUNK_CAP: usize = HALOW_AT_AIR_FRAME_CAP - HALOW_AT_HEADER_LEN;

/// The ethertype every delivered frame carries. The firmware rewrites whatever a sender puts in
/// the type field to this on the air ("HI" = Huge-IC), so the field cannot carry signaling; it is
/// only useful as a receive-side sanity mark.
pub const HALOW_AT_ETHERTYPE: [u8; 2] = [0x48, 0x49];

/// The all-stations destination every group-mode frame is addressed to.
pub const HALOW_AT_BROADCAST_MAC: [u8; 6] = [0xFF; 6];

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

/// The header a sender prepends to every chunk: broadcast destination, own module MAC as source,
/// and the ethertype the firmware will rewrite anyway.
#[must_use]
pub fn broadcast_header(own_mac: [u8; 6]) -> [u8; HALOW_AT_HEADER_LEN] {
    let mut header = [0u8; HALOW_AT_HEADER_LEN];
    header[..6].copy_from_slice(&HALOW_AT_BROADCAST_MAC);
    header[6..12].copy_from_slice(&own_mac);
    header[12..].copy_from_slice(&HALOW_AT_ETHERTYPE);
    header
}

/// Split a delivered air frame into the sender's module MAC and the data bytes. Frames shorter
/// than the header are firmware noise, not deliveries. Destination and ethertype are deliberately
/// not enforced: group mode already filters delivery, and the firmware owns the type field.
#[must_use]
pub fn split_rx_frame(frame: &[u8]) -> Option<([u8; 6], &[u8])> {
    if frame.len() < HALOW_AT_HEADER_LEN {
        return None;
    }
    let mut src = [0u8; 6];
    src.copy_from_slice(&frame[6..12]);
    Some((src, &frame[HALOW_AT_HEADER_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chunk_budget_is_the_cap_less_the_header() {
        assert_eq!(HALOW_AT_CHUNK_CAP, 242);
    }

    #[test]
    fn a_broadcast_header_carries_the_senders_mac() {
        let header = broadcast_header([0x12, 0xFD, 0x11, 0x64, 0x98, 0x78]);
        assert_eq!(&header[..6], &[0xFF; 6]);
        assert_eq!(&header[6..12], &[0x12, 0xFD, 0x11, 0x64, 0x98, 0x78]);
        assert_eq!(&header[12..], &[0x48, 0x49]);
    }

    #[test]
    fn an_rx_frame_round_trips_source_and_payload() {
        let mut frame = broadcast_header([0x82, 0x59, 0x13, 0x71, 0x5E, 0xA0]).to_vec();
        frame.extend_from_slice(b"\x7Epayload with \r\n inside\x7E");
        let (src, payload) = split_rx_frame(&frame).expect("frame splits");
        assert_eq!(src, [0x82, 0x59, 0x13, 0x71, 0x5E, 0xA0]);
        assert_eq!(payload, b"\x7Epayload with \r\n inside\x7E");
    }

    #[test]
    fn a_short_frame_is_noise() {
        assert_eq!(split_rx_frame(&[0u8; 13]), None);
        assert!(split_rx_frame(&[0u8; 14]).is_some_and(|(_, p)| p.is_empty()));
    }
}
