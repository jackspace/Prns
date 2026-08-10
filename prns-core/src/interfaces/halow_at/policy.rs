use super::protocol::{HALOW_AT_AIR_MTU, HALOW_AT_HEADER_LEN};

use crate::interfaces::{
    AnnounceBandwidthCap, BitrateBps, ConfiguredInterfacePolicy, EgressCapability,
    IngressCapability, InterfaceCapabilities, InterfaceDefaults, InterfaceDescriptor, InterfaceId,
    InterfaceMode, MtuPolicy, TransportCapability, IFAC_MAX_SIZE,
};

/// The clean-packet MTU we declare: the air ceiling less the pseudo-Ethernet header the module
/// consumes and the largest access tag, so a full frame plus its IFAC code still fits one
/// `AT+TXDATA` exchange.
pub const HALOW_AT_HW_MTU: usize = HALOW_AT_AIR_MTU - HALOW_AT_HEADER_LEN - IFAC_MAX_SIZE;

/// A representative broadcast goodput for announce pacing and the MTU tier — an honest order of
/// magnitude for a 1 MHz / MCS0 link behind the module's default 115200 UART, not a measured peak.
// TODO(bench): revisit once the throughput run lands (Phase 0 test 8) and the UART is rebauded.
pub const HALOW_AT_BITRATE_BPS: BitrateBps = BitrateBps::guess(100_000);

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
