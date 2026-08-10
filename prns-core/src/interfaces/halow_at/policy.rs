use crate::interfaces::{
    AnnounceBandwidthCap, BitrateBps, ConfiguredInterfacePolicy, EgressCapability,
    IngressCapability, InterfaceCapabilities, InterfaceDefaults, InterfaceDescriptor, InterfaceId,
    InterfaceMode, MtuPolicy, TransportCapability, IFAC_MAX_SIZE,
};

/// The MTU this interface declares: the RNS minimum. One air frame carries at most
/// [`HALOW_AT_CHUNK_CAP`](super::HALOW_AT_CHUNK_CAP) data bytes, so the driver fragments each
/// wire frame across a per-sender HDLC byte stream — the declared MTU is a promise the
/// fragmentation layer keeps, not an air-frame property.
pub const HALOW_AT_HW_MTU: usize = 500;

/// The largest wire frame the seam can hand the driver: a full MTU packet plus the largest
/// access tag. Sizes the reassembly buffers and the outbound encode scratch.
pub const HALOW_AT_MAX_WIRE_FRAME_LEN: usize = HALOW_AT_HW_MTU + IFAC_MAX_SIZE;

/// Bench-measured broadcast goodput: the module's own `est_rate` read 25–29 kbps at
/// 2 MHz / MCS0, behind a 115200 UART. Refinable once the sustained-throughput ceiling and the
/// 400K rebaud land on the bench.
pub const HALOW_AT_BITRATE_BPS: BitrateBps = BitrateBps::guess(25_000);

#[must_use]
pub fn descriptor(id: InterfaceId, bitrate: BitrateBps) -> InterfaceDescriptor {
    policy_for_bitrate(bitrate).descriptor(id)
}

#[must_use]
pub fn policy_for_bitrate(bitrate: BitrateBps) -> crate::interfaces::EffectiveInterfacePolicy {
    DEFAULTS.configured(ConfiguredInterfacePolicy {
        bitrate: Some(bitrate),
        ..ConfiguredInterfacePolicy::default()
    })
}

pub const DEFAULTS: InterfaceDefaults = InterfaceDefaults {
    capabilities: InterfaceCapabilities {
        ingress: IngressCapability::Enabled,
        egress: EgressCapability::Enabled(TransportCapability::SameInterfaceRepeat),
    },
    mode: InterfaceMode::Full,
    gravity: crate::interfaces::InterfaceGravity::ZERO,
    bitrate: HALOW_AT_BITRATE_BPS,
    mtu: MtuPolicy::fixed(HALOW_AT_HW_MTU),
    announce_rate_limit: None,
    announce_bandwidth_cap: AnnounceBandwidthCap::RNS_DEFAULT,
    airtime_duty_cycle: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_the_selected_phy_bitrate() {
        let id = InterfaceId::new([11; 8]);
        let bitrate = BitrateBps::guess(2_000_000);

        assert_eq!(descriptor(id, bitrate).bitrate, bitrate);
    }
}
