//! 802.11ah (Wi-Fi HaLow) over a Taixin TX-AH module's AT command UART, in the connectionless
//! group (broadcast) mode the `-wnb` firmware line documents: no access point, no association,
//! every node on the channel hears every frame, like a LoRa channel with two orders of magnitude
//! more throughput. One `AT+TXDATA` exchange carries at most 256 bytes, so Reticulum wire frames
//! ride per-sender HDLC byte streams (keyed by the source MAC in each delivery's pseudo-Ethernet
//! header) chunked across air frames; the 0x7E flags self-resync after a lost chunk.

mod console;
mod policy;
mod protocol;

pub use console::{
    is_boot_banner, is_error, is_ok, line_contains, parse_mac_after, AtConsole, AtStep, AT_LINE_CAP,
};
pub use policy::{
    descriptor, policy_for_bitrate, DEFAULTS, HALOW_AT_BITRATE_BPS, HALOW_AT_HW_MTU,
    HALOW_AT_MAX_WIRE_FRAME_LEN,
};
pub use protocol::{
    broadcast_header, channel_tag, interface_id, split_rx_frame, CHANNEL_TAG_CAP,
    HALOW_AT_AIR_FRAME_CAP, HALOW_AT_BROADCAST_MAC, HALOW_AT_CHUNK_CAP, HALOW_AT_ETHERTYPE,
    HALOW_AT_HEADER_LEN,
};
