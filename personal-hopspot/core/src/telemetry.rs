//! The optional telemetry element of the `lxmf.delivery` announce app_data.
//!
//! A hopspot's delivery announce carries `msgpack([display_name, stamp_cost])`. Appending a third
//! element keeps indexes 0 and 1 byte-identical, so LXMF apps that unpack the array and read those
//! positions are unaffected while a collector reads index 2. The element is
//! `msgpack([format_version, battery_percent, charging, uptime_seconds, reachable_destinations])`
//! with `nil` standing in for an unknown battery: about a dozen bytes riding an announce the node
//! would emit anyway, instead of a second protocol costing its own packets.

use personal_rns::routing::announce::emit::AnnounceAppDataBytes;

use crate::battery::BatteryState;

/// Leading element of the telemetry array, bumped only when the array shape changes so a
/// collector can tell the layouts apart.
pub const TELEMETRY_FORMAT_VERSION: u8 = 0;

const MSGPACK_FIXARRAY_2: u8 = 0x92;
const MSGPACK_FIXARRAY_3: u8 = 0x93;
const MSGPACK_FIXARRAY_5: u8 = 0x95;
const MSGPACK_NIL: u8 = 0xC0;
const MSGPACK_FALSE: u8 = 0xC2;
const MSGPACK_TRUE: u8 = 0xC3;
const MSGPACK_UINT8: u8 = 0xCC;
const MSGPACK_UINT16: u8 = 0xCD;
const MSGPACK_UINT32: u8 = 0xCE;
const MSGPACK_POSITIVE_FIXINT_MAX: u32 = 0x7F;

/// One beacon's worth of node health, gathered by the firmware and packed into the announce.
pub struct TelemetryReading {
    pub battery: BatteryState,
    pub uptime_seconds: u32,
    pub reachable_destinations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryAppDataError {
    /// The registered app_data does not start with the two-element msgpack array this extends.
    RegisteredNotAnLxmfPair,
    /// The extended app_data would not fit the announce app-data budget.
    BudgetExceeded,
}

/// Rewrite a registered `msgpack([display_name, stamp_cost])` app_data into
/// `msgpack([display_name, stamp_cost, telemetry])`: the pair's own bytes are reused verbatim,
/// only the array header grows by one element.
pub fn delivery_app_data_with_telemetry(
    registered: &[u8],
    reading: &TelemetryReading,
) -> Result<AnnounceAppDataBytes, TelemetryAppDataError> {
    let (head, pair_body) = registered
        .split_first()
        .ok_or(TelemetryAppDataError::RegisteredNotAnLxmfPair)?;
    if *head != MSGPACK_FIXARRAY_2 {
        return Err(TelemetryAppDataError::RegisteredNotAnLxmfPair);
    }
    let mut app_data = AnnounceAppDataBytes::new();
    push(&mut app_data, MSGPACK_FIXARRAY_3)?;
    extend(&mut app_data, pair_body)?;
    push(&mut app_data, MSGPACK_FIXARRAY_5)?;
    push_unsigned(&mut app_data, u32::from(TELEMETRY_FORMAT_VERSION))?;
    match reading.battery {
        BatteryState::Unknown => {
            push(&mut app_data, MSGPACK_NIL)?;
            push(&mut app_data, MSGPACK_FALSE)?;
        }
        BatteryState::Level(percent) => {
            push_unsigned(&mut app_data, u32::from(percent.get()))?;
            push(&mut app_data, MSGPACK_FALSE)?;
        }
        BatteryState::Charging(percent) => {
            push_unsigned(&mut app_data, u32::from(percent.get()))?;
            push(&mut app_data, MSGPACK_TRUE)?;
        }
    }
    push_unsigned(&mut app_data, reading.uptime_seconds)?;
    push_unsigned(&mut app_data, reading.reachable_destinations)?;
    Ok(app_data)
}

fn push(out: &mut AnnounceAppDataBytes, byte: u8) -> Result<(), TelemetryAppDataError> {
    out.push(byte)
        .map_err(|_| TelemetryAppDataError::BudgetExceeded)
}

fn extend(out: &mut AnnounceAppDataBytes, bytes: &[u8]) -> Result<(), TelemetryAppDataError> {
    out.extend_from_slice(bytes)
        .map_err(|()| TelemetryAppDataError::BudgetExceeded)
}

fn push_unsigned(out: &mut AnnounceAppDataBytes, value: u32) -> Result<(), TelemetryAppDataError> {
    if value <= MSGPACK_POSITIVE_FIXINT_MAX {
        return push(out, value as u8);
    }
    if let Ok(value) = u8::try_from(value) {
        push(out, MSGPACK_UINT8)?;
        return push(out, value);
    }
    if let Ok(value) = u16::try_from(value) {
        push(out, MSGPACK_UINT16)?;
        return extend(out, &value.to_be_bytes());
    }
    push(out, MSGPACK_UINT32)?;
    extend(out, &value.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::BatteryPercent;
    use personal_rns::routing::announce::emit::MAX_ANNOUNCE_APP_DATA_LEN;

    const REGISTERED: &[u8] = b"\x92\xc4\x07Hopspot\xc0";

    #[test]
    fn telemetry_extends_the_lxmf_pair_into_a_parseable_triple() {
        let reading = TelemetryReading {
            battery: BatteryState::Level(BatteryPercent::saturating(87)),
            uptime_seconds: 3_661,
            reachable_destinations: 5,
        };

        let app_data = delivery_app_data_with_telemetry(REGISTERED, &reading).unwrap();

        assert_eq!(
            app_data.as_slice(),
            b"\x93\xc4\x07Hopspot\xc0\x95\x00\x57\xc2\xcd\x0e\x4d\x05"
        );
    }

    #[test]
    fn an_unknown_battery_rides_as_nil_and_not_charging() {
        let reading = TelemetryReading {
            battery: BatteryState::Unknown,
            uptime_seconds: 9,
            reachable_destinations: 0,
        };

        let app_data = delivery_app_data_with_telemetry(REGISTERED, &reading).unwrap();

        assert_eq!(
            app_data.as_slice(),
            b"\x93\xc4\x07Hopspot\xc0\x95\x00\xc0\xc2\x09\x00"
        );
    }

    #[test]
    fn a_charging_battery_sets_the_boolean_element() {
        let reading = TelemetryReading {
            battery: BatteryState::Charging(BatteryPercent::saturating(100)),
            uptime_seconds: 0,
            reachable_destinations: 1,
        };

        let app_data = delivery_app_data_with_telemetry(REGISTERED, &reading).unwrap();

        assert_eq!(
            app_data.as_slice(),
            b"\x93\xc4\x07Hopspot\xc0\x95\x00\x64\xc3\x00\x01"
        );
    }

    #[test]
    fn unsigned_values_take_their_smallest_msgpack_encoding() {
        let uptime_tail = |uptime_seconds: u32| {
            let reading = TelemetryReading {
                battery: BatteryState::Unknown,
                uptime_seconds,
                reachable_destinations: 0,
            };
            let app_data = delivery_app_data_with_telemetry(REGISTERED, &reading).unwrap();
            let mut tail = [0u8; 6];
            let encoded = &app_data.as_slice()[15..app_data.len() - 1];
            tail[..encoded.len()].copy_from_slice(encoded);
            (tail, encoded.len())
        };

        assert_eq!(uptime_tail(127), (*b"\x7f\0\0\0\0\0", 1));
        assert_eq!(uptime_tail(128), (*b"\xcc\x80\0\0\0\0", 2));
        assert_eq!(uptime_tail(255), (*b"\xcc\xff\0\0\0\0", 2));
        assert_eq!(uptime_tail(256), (*b"\xcd\x01\x00\0\0\0", 3));
        assert_eq!(uptime_tail(65_535), (*b"\xcd\xff\xff\0\0\0", 3));
        assert_eq!(uptime_tail(65_536), (*b"\xce\x00\x01\x00\x00\0", 5));
        assert_eq!(uptime_tail(u32::MAX), (*b"\xce\xff\xff\xff\xff\0", 5));
    }

    #[test]
    fn registered_app_data_that_is_not_an_lxmf_pair_is_rejected() {
        let reading = TelemetryReading {
            battery: BatteryState::Unknown,
            uptime_seconds: 0,
            reachable_destinations: 0,
        };

        assert_eq!(
            delivery_app_data_with_telemetry(b"Personal Hopspot C6", &reading),
            Err(TelemetryAppDataError::RegisteredNotAnLxmfPair)
        );
        assert_eq!(
            delivery_app_data_with_telemetry(b"", &reading),
            Err(TelemetryAppDataError::RegisteredNotAnLxmfPair)
        );
    }

    #[test]
    fn a_pair_already_at_the_announce_budget_cannot_grow() {
        let registered = [MSGPACK_FIXARRAY_2; MAX_ANNOUNCE_APP_DATA_LEN];
        let reading = TelemetryReading {
            battery: BatteryState::Unknown,
            uptime_seconds: 0,
            reachable_destinations: 0,
        };

        assert_eq!(
            delivery_app_data_with_telemetry(&registered, &reading),
            Err(TelemetryAppDataError::BudgetExceeded)
        );
    }
}
