use alloc::string::{String, ToString};
use alloc::vec;

use super::*;
use crate::interfaces::{InterfaceKind, INTERFACE_ID_LEN};

fn id(seed: u8) -> InterfaceId {
    let mut bytes = [seed; INTERFACE_ID_LEN];
    bytes[0] = InterfaceKind::TcpClient as u8;
    InterfaceId::new(bytes)
}

fn accounted(seed: u8) -> RnsInterfaceVitalsEntry {
    RnsInterfaceVitalsEntry {
        name: "halow".to_string(),
        vitals: InterfaceVitals {
            id: id(seed),
            connection: ConnectionState::Connected,
            failure_reason: None,
            rx_bytes: 4_096,
            tx_bytes: 512,
            transfer_rates: Some(TransferRates {
                rx_bps: 1_200,
                tx_bps: 300,
            }),
            frames: Some(FrameAccounting {
                frames_in: 84,
                frames_out: 40,
                malformed: 3,
                undecodable: 7,
                delivered: 74,
            }),
            uptime_ms: Some(1_234_567),
        },
    }
}

fn unaccounted(seed: u8) -> RnsInterfaceVitalsEntry {
    RnsInterfaceVitalsEntry {
        name: "tcp/hertz".to_string(),
        vitals: InterfaceVitals {
            id: id(seed),
            connection: ConnectionState::Degraded,
            failure_reason: None,
            rx_bytes: 0,
            tx_bytes: 0,
            transfer_rates: None,
            frames: None,
            uptime_ms: None,
        },
    }
}

fn round_trip(report: &RnsInterfaceVitalsReport) -> RnsInterfaceVitalsReport {
    let encoded = report.encode_message_pack().expect("encodes");
    RnsInterfaceVitalsReport::decode_message_pack(&encoded).expect("decodes")
}

#[test]
fn a_full_report_survives_the_wire_unchanged() {
    let report = RnsInterfaceVitalsReport::new(vec![accounted(0x11), unaccounted(0x22)]);
    assert_eq!(round_trip(&report), report);
}

#[test]
fn an_empty_report_round_trips() {
    let report = RnsInterfaceVitalsReport::new(vec![]);
    assert_eq!(round_trip(&report), report);
    assert!(round_trip(&report).entries().is_empty());
}

/// The distinction the whole verb exists for: a family that does not account for frames
/// must not arrive looking like one that counted zero of everything.
#[test]
fn unaccounted_frames_do_not_arrive_as_zeroes() {
    let mut zeroed = accounted(0x33);
    zeroed.vitals.frames = Some(FrameAccounting {
        frames_in: 0,
        frames_out: 0,
        malformed: 0,
        undecodable: 0,
        delivered: 0,
    });
    let unaccounted_entry = {
        let mut entry = accounted(0x33);
        entry.vitals.frames = None;
        entry
    };

    let zeroed = round_trip(&RnsInterfaceVitalsReport::new(vec![zeroed]));
    let unaccounted_entry = round_trip(&RnsInterfaceVitalsReport::new(vec![unaccounted_entry]));

    assert_eq!(
        zeroed.entries()[0].vitals.frames,
        Some(FrameAccounting {
            frames_in: 0,
            frames_out: 0,
            malformed: 0,
            undecodable: 0,
            delivered: 0,
        })
    );
    assert_eq!(unaccounted_entry.entries()[0].vitals.frames, None);
    assert_ne!(zeroed, unaccounted_entry);
}

/// `uptime_ms` is what self-dates a relayed sample. Losing it would make a stale report
/// indistinguishable from a fresh one.
#[test]
fn uptime_survives_and_stays_optional() {
    let mut entry = accounted(0x44);
    entry.vitals.uptime_ms = Some(0);
    let decoded = round_trip(&RnsInterfaceVitalsReport::new(vec![entry]));
    assert_eq!(decoded.entries()[0].vitals.uptime_ms, Some(0));

    let mut entry = accounted(0x44);
    entry.vitals.uptime_ms = None;
    let decoded = round_trip(&RnsInterfaceVitalsReport::new(vec![entry]));
    assert_eq!(decoded.entries()[0].vitals.uptime_ms, None);
}

#[test]
fn a_reported_failure_reason_arrives_as_a_flag() {
    let mut entry = accounted(0x55);
    entry.vitals.connection = ConnectionState::Failed;
    entry.vitals.failure_reason = Some("module wedged");
    let decoded = round_trip(&RnsInterfaceVitalsReport::new(vec![entry]));

    assert_eq!(
        decoded.entries()[0].vitals.connection,
        ConnectionState::Failed
    );
    assert_eq!(
        decoded.entries()[0].vitals.failure_reason,
        Some(RELAYED_FAILURE)
    );
}

#[test]
fn an_unnamed_interface_falls_back_to_its_generated_name() {
    let vitals = unaccounted(0x66).vitals;
    let report =
        RnsInterfaceVitalsReport::of(vec![(None, vitals), (Some(String::from("named")), vitals)]);

    assert_eq!(report.entries()[0].name, interface_name(vitals.id));
    assert_eq!(report.entries()[1].name, "named");
}

#[test]
fn a_truncated_reply_is_rejected_rather_than_half_decoded() {
    let encoded = RnsInterfaceVitalsReport::new(vec![accounted(0x77)])
        .encode_message_pack()
        .expect("encodes");
    for length in 0..encoded.len() {
        assert!(
            RnsInterfaceVitalsReport::decode_message_pack(&encoded[..length]).is_err(),
            "a {length}-byte prefix decoded"
        );
    }
    assert!(RnsInterfaceVitalsReport::decode_message_pack(&encoded).is_ok());
}

#[test]
fn trailing_data_is_rejected() {
    let mut encoded = RnsInterfaceVitalsReport::new(vec![])
        .encode_message_pack()
        .expect("encodes");
    encoded.push(0xc0);
    assert_eq!(
        RnsInterfaceVitalsReport::decode_message_pack(&encoded),
        Err(RnsInterfaceVitalsDecodeError::TrailingData)
    );
}

#[test]
fn a_stock_interface_stats_reply_is_not_mistaken_for_a_vitals_reply() {
    let stats = crate::interfaces::rns_management::RnsInterfaceStats::new(vec![])
        .encode_message_pack()
        .expect("encodes");
    assert!(RnsInterfaceVitalsReport::decode_message_pack(&stats).is_err());
}
