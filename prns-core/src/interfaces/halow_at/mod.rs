//! 802.11ah (Wi-Fi HaLow) over a Taixin TX-AH module's AT command UART, in the connectionless
//! group (broadcast) mode the `-wnb` firmware line documents: no access point, no association,
//! every node on the channel hears every frame, like a LoRa channel with two orders of magnitude
//! more throughput. The AT firmware packetizes with `AT+TXDATA` / `+RXDATA`, so one Reticulum
//! wire frame maps to one exchange and no serial framing layer rides the air.

mod policy;
mod protocol;

pub use policy::{descriptor, policy_for_bitrate, DEFAULTS, HALOW_AT_BITRATE_BPS, HALOW_AT_HW_MTU};
pub use protocol::{
    channel_tag, interface_id, CHANNEL_TAG_CAP, HALOW_AT_AIR_MTU, HALOW_AT_HEADER_LEN,
};
