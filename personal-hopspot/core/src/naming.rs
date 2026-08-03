//! One place resolves what a Hopspot is called and what its announces carry. The name feeds the
//! `lxmf.delivery` announce, the node-page announce, the boot log, and the on-screen home card, so
//! every surface agrees.

use core::fmt::Write as _;

use heapless::{String as HString, Vec as HVec};
use personal_rns::wire::{DestinationHash, TRUNCATED_HASH_BYTE_LEN};

/// Byte cap for a node's display name, matching the hopcfg override field the flasher writes.
pub const NODE_NAME_MAX_BYTES: usize = 32;
pub type NodeName = HString<NODE_NAME_MAX_BYTES>;

/// Hex chars in a fully spelled destination hash.
pub const DESTINATION_HEX_CHARS: usize = TRUNCATED_HASH_BYTE_LEN * 2;
pub type DestinationHex = HString<DESTINATION_HEX_CHARS>;

/// Leading destination-hash bytes folded into a derived name: two bytes, four hex chars.
const DERIVED_NAME_SUFFIX_BYTES: usize = 2;

const MSGPACK_FIXARRAY_TWO: u8 = 0x92;
const MSGPACK_BIN8: u8 = 0xc4;
const MSGPACK_NIL: u8 = 0xc0;
/// fixarray marker, bin8 marker, bin8 length byte, nil stamp cost.
const DELIVERY_APP_DATA_OVERHEAD: usize = 4;
pub const DELIVERY_ANNOUNCE_APP_DATA_MAX_BYTES: usize =
    NODE_NAME_MAX_BYTES + DELIVERY_APP_DATA_OVERHEAD;
pub type DeliveryAnnounceAppData = HVec<u8, DELIVERY_ANNOUNCE_APP_DATA_MAX_BYTES>;

/// The single decision point for what this node is called. A provisioned override wins; otherwise
/// the name is the board's base name plus the first four hex chars of the delivery destination
/// hash. Hash-derived rather than MAC-derived on purpose: mesh identity stays separate from Wi-Fi
/// identity, and a hash suffix leaks nothing an announce does not already carry.
#[must_use]
pub fn resolve_node_name(
    configured: Option<&str>,
    base: &str,
    delivery: &DestinationHash,
) -> NodeName {
    let mut name = NodeName::new();
    match configured {
        Some(configured) if !configured.is_empty() => {
            for c in configured.chars() {
                if name.push(c).is_err() {
                    break;
                }
            }
        }
        _ => {
            let _ = write!(name, "{base}-");
            for byte in delivery.as_bytes().iter().take(DERIVED_NAME_SUFFIX_BYTES) {
                let _ = write!(name, "{byte:02x}");
            }
        }
    }
    name
}

/// This node's `lxmf.delivery` announce app_data: `msgpack([display_name, stamp_cost])`
/// = `fixarray(2)` ‖ `bin8(name)` ‖ `nil`, the shape LXMF apps parse. Composed from the resolved
/// node name at boot; boards no longer bake the length byte into a byte-string literal.
#[must_use]
pub fn delivery_announce_app_data(name: &NodeName) -> DeliveryAnnounceAppData {
    let mut app_data = DeliveryAnnounceAppData::new();
    let _ = app_data.push(MSGPACK_FIXARRAY_TWO);
    let _ = app_data.push(MSGPACK_BIN8);
    let _ = app_data.push(name.len() as u8);
    let _ = app_data.extend_from_slice(name.as_bytes());
    let _ = app_data.push(MSGPACK_NIL);
    app_data
}

/// The full destination hash as lowercase hex, for the boot log and the home card.
#[must_use]
pub fn destination_hex(destination: &DestinationHash) -> DestinationHex {
    let mut hex = DestinationHex::new();
    for byte in destination.as_bytes() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash() -> DestinationHash {
        DestinationHash::new([
            0xa2, 0x33, 0x5c, 0x1f, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
            0xaa, 0xff,
        ])
    }

    #[test]
    fn derived_name_is_base_plus_first_four_hash_hex_chars() {
        assert_eq!(
            resolve_node_name(None, "Hopspot", &hash()).as_str(),
            "Hopspot-a233"
        );
    }

    #[test]
    fn configured_name_wins_and_empty_falls_back_to_derived() {
        assert_eq!(
            resolve_node_name(Some("Lighthouse"), "Hopspot", &hash()).as_str(),
            "Lighthouse"
        );
        assert_eq!(
            resolve_node_name(Some(""), "Hopspot", &hash()).as_str(),
            "Hopspot-a233"
        );
    }

    #[test]
    fn composed_app_data_matches_the_retired_board_literal() {
        let mut name = NodeName::new();
        name.push_str("Personal Hopspot HeltecV4-R8").unwrap();
        assert_eq!(
            delivery_announce_app_data(&name).as_slice(),
            b"\x92\xc4\x1cPersonal Hopspot HeltecV4-R8\xc0"
        );
    }

    #[test]
    fn destination_hex_spells_every_byte() {
        let hex = destination_hex(&hash());
        assert_eq!(hex.len(), DESTINATION_HEX_CHARS);
        assert_eq!(hex.as_str(), "a2335c1f00112233445566778899aaff");
    }
}
