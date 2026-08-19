use super::*;

use personal_rns::interfaces::rns_management::RnsInterfaceVitalsReport;
use personal_rns::interfaces::{
    ConnectionState, FrameAccounting, InterfaceId, InterfaceVitals, TransferRates,
};

/// Shaped like a row the USB host stores for a remote radio: it carries the frame split and
/// the reporting board's own uptime, neither of which a local read produces.
fn relayed_remote_row() -> InterfaceVitals {
    InterfaceVitals {
        id: InterfaceId::new([0xD0; 8]),
        connection: ConnectionState::Connected,
        failure_reason: None,
        rx_bytes: 12_480,
        tx_bytes: 960,
        transfer_rates: Some(TransferRates {
            rx_bps: 208,
            tx_bps: 16,
        }),
        frames: Some(FrameAccounting {
            frames_in: 110,
            frames_out: 15,
            malformed: 2,
            undecodable: 5,
            delivered: 103,
        }),
        uptime_ms: Some(5_820_000),
    }
}

fn local_row() -> InterfaceVitals {
    InterfaceVitals {
        id: InterfaceId::new([0x0a; 8]),
        connection: ConnectionState::Connected,
        failure_reason: None,
        rx_bytes: 64,
        tx_bytes: 64,
        transfer_rates: None,
        frames: None,
        uptime_ms: None,
    }
}

/// Renders through the real wire codec rather than straight off the in-memory value, so the
/// test covers the path the command actually takes: daemon encodes, client decodes, CLI prints.
fn render_over_the_wire(rows: Vec<(Option<String>, InterfaceVitals)>) -> serde_json::Value {
    let encoded = RnsInterfaceVitalsReport::of(rows)
        .encode_message_pack()
        .expect("the daemon encodes the report");
    let decoded =
        RnsInterfaceVitalsReport::decode_message_pack(&encoded).expect("the client decodes it");
    serde_json::from_str(&render(&decoded).expect("the CLI renders it")).expect("valid JSON")
}

#[test]
fn a_stored_remote_report_survives_the_wire_and_reaches_the_json() {
    let rendered = render_over_the_wire(std::vec![(
        Some(String::from("t-halow/halow0")),
        relayed_remote_row(),
    )]);

    let row = &rendered["interfaces"][0];
    assert_eq!(row["name"], "t-halow/halow0");
    assert_eq!(row["id"], "d0d0d0d0d0d0d0d0");
    assert_eq!(row["connection"], "Connected");
    assert_eq!(row["rx_bytes"], 12_480);
    assert_eq!(row["frames"]["frames_in"], 110);
    assert_eq!(row["frames"]["frames_out"], 15);
    assert_eq!(row["frames"]["malformed"], 2);
    assert_eq!(row["frames"]["undecodable"], 5);
    assert_eq!(row["frames"]["delivered"], 103);
    assert_eq!(row["uptime_ms"], 5_820_000u64);
}

/// The question the command was built for: a reader must be able to tell an interface that
/// does not account for frames from one that counted none.
#[test]
fn an_unaccounted_interface_renders_null_frames_not_zeroes() {
    let mut silent = relayed_remote_row();
    silent.frames = Some(FrameAccounting {
        frames_in: 0,
        frames_out: 0,
        malformed: 0,
        undecodable: 0,
        delivered: 0,
    });

    let rendered = render_over_the_wire(std::vec![
        (Some(String::from("silent")), silent),
        (Some(String::from("unaccounted")), local_row()),
    ]);

    assert_eq!(rendered["interfaces"][0]["frames"]["frames_in"], 0);
    assert!(rendered["interfaces"][1]["frames"].is_null());
    assert!(rendered["interfaces"][1]["uptime_ms"].is_null());
    assert!(rendered["interfaces"][1]["rx_bps"].is_null());
}

/// One object per line is what makes the output appendable to a JSONL timeline.
#[test]
fn the_render_is_a_single_line_object() {
    let encoded =
        RnsInterfaceVitalsReport::of(std::vec![(None, relayed_remote_row()), (None, local_row()),])
            .encode_message_pack()
            .expect("encodes");
    let decoded = RnsInterfaceVitalsReport::decode_message_pack(&encoded).expect("decodes");
    let rendered = render(&decoded).expect("renders");

    assert!(!rendered.contains('\n'));
    assert!(rendered.starts_with('{'));
    assert!(rendered.ends_with('}'));
}

#[test]
fn a_node_with_no_interfaces_still_renders_an_object() {
    let rendered = render_over_the_wire(std::vec![]);
    assert_eq!(rendered["interfaces"].as_array().map(Vec::len), Some(0));
}
