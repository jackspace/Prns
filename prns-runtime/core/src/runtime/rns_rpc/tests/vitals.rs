use super::*;

use prns_core::interfaces::rns_management::RnsInterfaceVitalsReport;
use prns_core::interfaces::{FrameAccounting, TransferRates};

/// Carries real `InterfaceVitals` rather than snapshots, which is the only way to prove the
/// verb does not go through the `InterfaceSnapshot` squeeze on its way out.
struct StubVitalsQuery {
    vitals: Vec<(Option<String>, InterfaceVitals)>,
}

impl NodeIntrospection for StubVitalsQuery {
    fn interface_inventory(&self) -> Vec<InterfaceInventoryEntry> {
        std::vec![]
    }

    fn interface_vitals_inventory(&self) -> Vec<(Option<String>, InterfaceVitals)> {
        self.vitals.clone()
    }

    async fn link_count(&self) -> u32 {
        0
    }

    fn packet_phy(&self, _packet_hash: PacketHash) -> Option<PacketPhyStats> {
        None
    }

    async fn announce_rates(&self) -> Vec<AnnounceRateSnapshot> {
        std::vec![]
    }

    async fn routes(&self) -> Vec<RouteSnapshot> {
        std::vec![]
    }

    async fn route(&self, _destination: DestinationHash) -> Option<RouteSnapshot> {
        None
    }
}

fn radio() -> InterfaceVitals {
    InterfaceVitals {
        id: InterfaceId::new([0xD0; 8]),
        connection: ConnectionState::Connected,
        failure_reason: None,
        rx_bytes: 9_001,
        tx_bytes: 640,
        transfer_rates: Some(TransferRates {
            rx_bps: 150,
            tx_bps: 30,
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

/// Only the introspection role varies here, so the remaining roles come from the ordinary
/// stub rather than being reimplemented on a query that never exercises them.
async fn vitals_reply(query: &StubVitalsQuery) -> RnsInterfaceVitalsReport {
    let request = encode_msgpack(Value::Map(std::vec![(
        Value::from("get"),
        Value::from("interface_vitals"),
    )]))
    .unwrap();
    let others = StubQuery {
        links: 0,
        packet_phy: None,
        rates: std::vec![],
        routes: std::vec![],
        interfaces: std::vec![],
    };
    let request = RpcRequest::decode(&request).expect("a decodable vitals request");
    let reply = reply_for_decoded(
        &request,
        query,
        &others,
        &others,
        &others,
        TEST_TRANSPORT_IDENTITY_HASH,
        None,
    )
    .await
    .expect("an encodable vitals reply");
    RnsInterfaceVitalsReport::decode_message_pack(&reply).expect("a decodable vitals reply")
}

#[futures_test::test]
async fn the_verb_returns_the_frame_split_and_uptime_that_interface_stats_drops() {
    let report = vitals_reply(&StubVitalsQuery {
        vitals: std::vec![(Some(String::from("halow0")), radio())],
    })
    .await;

    let entry = &report.entries()[0];
    assert_eq!(entry.name, "halow0");
    assert_eq!(entry.vitals.frames, radio().frames);
    assert_eq!(entry.vitals.uptime_ms, Some(5_820_000));
    assert_eq!(entry.vitals.rx_bytes, 9_001);
}

/// The distinction the verb exists to carry, checked at the dispatch layer rather than only
/// at the codec: an unaccounted interface must not arrive looking like a silent one.
#[futures_test::test]
async fn an_unaccounted_interface_is_distinguishable_from_a_silent_one() {
    let mut silent = radio();
    silent.frames = Some(FrameAccounting {
        frames_in: 0,
        frames_out: 0,
        malformed: 0,
        undecodable: 0,
        delivered: 0,
    });
    let mut unaccounted = radio();
    unaccounted.frames = None;

    let report = vitals_reply(&StubVitalsQuery {
        vitals: std::vec![(Some(String::from("silent")), silent), (None, unaccounted)],
    })
    .await;

    assert_eq!(report.entries()[0].vitals.frames.unwrap().frames_in, 0);
    assert_eq!(report.entries()[1].vitals.frames, None);
}

#[futures_test::test]
async fn a_node_with_no_interfaces_answers_an_empty_report_rather_than_an_error() {
    let report = vitals_reply(&StubVitalsQuery {
        vitals: std::vec![],
    })
    .await;
    assert!(report.entries().is_empty());
}

/// The legacy dialect predates the verb, so a pickle client naming it must be rejected
/// outright rather than served a reply it cannot parse.
#[test]
fn a_pickle_client_cannot_ask_for_vitals() {
    let request = legacy_string_request("get", "interface_vitals");
    assert!(RpcRequest::decode(&request).is_err());
}
