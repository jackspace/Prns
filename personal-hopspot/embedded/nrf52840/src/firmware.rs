use embassy_executor::Spawner;
use embassy_futures::join::{join, join3, join5};
use embassy_futures::select::{select3, Either3};
use embassy_nrf::gpio::{Input, Output};
use embassy_nrf::spim::Spim;
use embassy_time::{Delay, Duration, Timer};
use embassy_usb::{Builder, Config as UsbConfig};
use static_cell::{ConstStaticCell, StaticCell};

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_hal_bus::spi::ExclusiveDevice;

use nrf_softdevice::ble::l2cap;
use nrf_softdevice::{Flash, Softdevice};

use personal_hopspot_core as hopspot;
use personal_rns::bluetooth_auto::{BluetoothAuto, BluetoothAutoStatus};
use personal_rns::engine::{AnnounceAppData, AnnounceNow, AnnounceTarget, PrnsCommand};
use personal_rns::interfaces::bluetooth_auto::{Endpoint, LinkCapabilities, Nrf52Host, BLE_HW_MTU};
use personal_rns::interfaces::lora::{AirtimePolicy, DEFAULT_915_PROFILE};
use personal_rns::interfaces::usb_auto::{WEBUSB_PRODUCT_ID, WEBUSB_VENDOR_ID};
use personal_rns::interfaces::{ConnectionState, InterfaceStatus};
use personal_rns::lora::{
    LoRaInterface, LoRaInterfaceInput, LoRaSpectrumStatus, LORA_TX_QUEUE_BYTES,
};
use personal_rns::manifold::embassy::{EmbassyHost, EmbassyInterfaceStatus};
use personal_rns::manifold::interface_seam::Interface;
use personal_rns::runtime::{Fleet, PrnsEvent, PrnsNode, PrnsNodeHandle, PrnsNodeRecipe};
use personal_rns::storage::StorageLayout;
use personal_rns::usb_auto::{UsbAutoDevice, UsbAutoDeviceInput};
use personal_rns::usb_auto::{WebUsbAutoClass, WebUsbAutoState, WEBUSB_AUTO_PACKET_SIZE};

use crate::bluetooth_auto::{
    acceptor, scanner, serve_slot, softdevice_config, softdevice_task, usb_vbus_present,
    L2capPacket, NrfBleBackend, Server, BLE_SHARED, BLE_SUPERVISOR_ID, HUB, MEMBERS, OUTBOUND_WAKE,
    POOL,
};
use crate::board::{EarlyHardware, Nrf52840Board, RuntimeHardware, UsbHardware};
use crate::display::{build_cards, build_snapshots};
use crate::input;
use crate::node::*;

const STATS_POLL: Duration = Duration::from_secs(1);
const NOTICE_MS: u64 = 900;

#[embassy_executor::task]
async fn manifold_task(
    node: &'static mut Node,
    persistence: &'static mut crate::persistence::Nrf52840Persistence,
) {
    let _ = node.restore_embedded_persistence(persistence).await;
    node.run_manifold_with_persistence_and_interface_store(&INTERFACE_STORE, persistence)
        .await
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn run<B: Nrf52840Board>(spawner: Spawner) -> ! {
    let ((node_bootstrap, ble_bootstrap), early_hardware) = B::claim(|nvmc, rng| {
        let mut fill_entropy = |bytes: &mut [u8]| rng.blocking_fill_bytes(bytes);
        let node_bootstrap = crate::identity::bootstrap_node_identity(nvmc, &mut fill_entropy);
        let ble_bootstrap = crate::identity::bootstrap_ble_identity(nvmc, &mut fill_entropy);
        (node_bootstrap, ble_bootstrap)
    });
    let identity_startup_notice =
        crate::identity::startup_notice(node_bootstrap.persistence(), ble_bootstrap.persistence());
    let node_identity = node_bootstrap.into_identity();
    let ble_identity = Some(ble_bootstrap.into_identity());

    let EarlyHardware {
        usb,
        battery,
        status_led,
        deferred,
    } = early_hardware;
    let UsbHardware {
        driver: usb_driver,
        vbus,
    } = usb;
    let mut usb_config = UsbConfig::new(WEBUSB_VENDOR_ID, WEBUSB_PRODUCT_ID);
    usb_config.manufacturer = Some("Stay Personal");
    usb_config.product = Some(B::USB_PRODUCT);
    usb_config.serial_number = Some(B::USB_SERIAL_NUMBER);
    usb_config.max_packet_size_0 = 64;
    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    let mut builder = Builder::new(
        usb_driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );
    builder.msos_descriptor(0x0603_0000, 0x20);
    static USB_STATE: StaticCell<WebUsbAutoState> = StaticCell::new();
    let class = WebUsbAutoClass::new(
        &mut builder,
        USB_STATE.init(WebUsbAutoState::new()),
        WEBUSB_AUTO_PACKET_SIZE,
    );
    let mut usb = builder.build();

    let sd = Softdevice::enable(&softdevice_config());
    static SERVER: StaticCell<Server> = StaticCell::new();
    let server: &'static Server = SERVER.init(Server::new(sd).unwrap());
    static L2CAP: StaticCell<l2cap::L2cap<L2capPacket>> = StaticCell::new();
    let l2cap: &'static l2cap::L2cap<L2capPacket> = L2CAP.init(l2cap::L2cap::init(sd));
    let sd: &'static Softdevice = sd;
    spawner.spawn(softdevice_task(sd, vbus).expect("softdevice task fits"));
    let flash = Flash::take(sd);
    if let Some(identity) = ble_identity {
        crate::bluetooth_auto::set_columba_identity(server, identity);
    }
    if ble_identity.is_some() {
        for idx in 0..POOL {
            spawner.spawn(serve_slot(idx, sd, l2cap, server, &HUB).expect("serve slot fits"));
        }
    }

    let RuntimeHardware {
        radio,
        display,
        button,
        illumination,
    } = B::finish(deferred).await;
    let mut led = status_led;

    let transport_secret = node_identity.transport_secret();
    let destination_secret = node_identity.into_destination_secret();
    let destination_hashes = hopspot::HopspotDestinationSet::new(
        destination_secret.clone(),
        B::ANNOUNCE_APP_DATA,
        B::NODE_ANNOUNCE_APP_DATA,
    )
    .destination_hashes()
    .expect("the hopspot destination names are valid");
    let self_destination = destination_hashes.delivery;
    let node_page_destination = destination_hashes.node_page;
    let seed = self_destination.as_bytes();
    ENTROPY_STATE.store(
        u32::from_le_bytes([seed[0], seed[1], seed[2], seed[3]]) | 1,
        core::sync::atomic::Ordering::Relaxed,
    );
    let mut manifold_lanes = ManifoldLanes::new();
    let lora_profile = DEFAULT_915_PROFILE;
    let lora_id = LoRaInterface::<
        ExclusiveDevice<Spim<'static>, Output<'static>, Delay>,
        Input<'static>,
        Input<'static>,
        Output<'static>,
        Delay,
    >::interface_id(&lora_profile);
    static LORA_STATUS: StaticCell<EmbassyInterfaceStatus> = StaticCell::new();
    let lora_status: &'static EmbassyInterfaceStatus = LORA_STATUS.init(
        EmbassyInterfaceStatus::new(lora_id, ConnectionState::Initializing),
    );
    static LORA_SPECTRUM: StaticCell<LoRaSpectrumStatus> = StaticCell::new();
    let lora_spectrum: &'static LoRaSpectrumStatus = LORA_SPECTRUM.init(LoRaSpectrumStatus::new());
    static LORA_TX_QUEUE: ConstStaticCell<[u8; LORA_TX_QUEUE_BYTES]> =
        ConstStaticCell::new([0; LORA_TX_QUEUE_BYTES]);
    let lora_tx_queue = LORA_TX_QUEUE.take();
    let lora = match LoRaInterface::new(LoRaInterfaceInput {
        radio,
        profile: lora_profile,
        airtime_policy: AirtimePolicy::Regional,
        tx_queue: lora_tx_queue,
        control: &LORA_CONTROL,
        status: lora_status,
        spectrum: lora_spectrum,
        lifecycle: LIFECYCLE.dyn_sender(),
    }) {
        Ok(lora) => lora,
        Err(_) => panic!("the built-in LoRa profile and regional policy must be valid"),
    };

    let (usb_tx, usb_rx) = class.split();
    static USB_STATUS: StaticCell<EmbassyInterfaceStatus> = StaticCell::new();
    let usb_status: &'static EmbassyInterfaceStatus = USB_STATUS.init(EmbassyInterfaceStatus::new(
        B::USB_INTERFACE_ID,
        ConnectionState::Initializing,
    ));
    let usb_dev = UsbAutoDevice::new(UsbAutoDeviceInput {
        rx: usb_rx,
        tx: usb_tx,
        status: usb_status,
        host_present: || true,
    });

    let lora_lane = manifold_lanes
        .claim_interface(&LORA_MANIFOLD_LANE, lora.descriptor())
        .expect("LoRa lane is available");
    let ble_supervisor_lane = ble_identity.as_ref().map(|_| {
        manifold_lanes
            .claim_supervisor(&BLE_MANIFOLD_LANE, BLE_SUPERVISOR_ID, &OUTBOUND_WAKE)
            .expect("Bluetooth supervisor lane is available")
    });
    let usb_lane = manifold_lanes
        .claim_interface(&USB_MANIFOLD_LANE, usb_dev.descriptor())
        .expect("USB lane is available");

    let handle = PrnsNodeHandle::new(COMMANDS.sender(), &COMPLETION);
    let manifold_wiring = manifold_lanes.into_manifold_wiring(
        NOTIFY.receiver(),
        COMMANDS.receiver(),
        LIFECYCLE.receiver(),
        handle,
    );
    let host = EmbassyHost::new(seeded_entropy as fn(&mut [u8]));
    static NODE: StaticCell<Node> = StaticCell::new();
    let recipe = PrnsNodeRecipe {
        transport_identity: Some(transport_secret),
        pre_configured_destinations: hopspot::HopspotDestinationSet::new(
            destination_secret,
            B::ANNOUNCE_APP_DATA,
            B::NODE_ANNOUNCE_APP_DATA,
        )
        .into_preconfigured_destinations(),
        app_state: (),
        storage: crate::storage::Nrf52840Storage,
        request_endpoints: hopspot::node_pages::NodePageRoutes,
        interfaces: personal_rns::runtime::ManuallyAttached,
        persistence: crate::persistence::new(flash),
        on_event: ignore_events as for<'a> fn(PrnsEvent<'a>, &()),
    };
    let (node, persistence) =
        PrnsNode::init_static_with_persistence(&NODE, recipe, manifold_wiring, host);
    static PERSISTENCE: StaticCell<crate::persistence::Nrf52840Persistence> = StaticCell::new();
    let persistence = PERSISTENCE.init(persistence);
    spawner.spawn(manifold_task(node, persistence).expect("manifold task fits"));
    let lora_seam = lora_lane.into_seam(NOTIFY.sender(), seeded_entropy);

    let usb_seam = usb_lane.into_seam(NOTIFY.sender(), seeded_entropy);

    let backend = NrfBleBackend::new(&HUB);
    let bluetooth = ble_identity
        .zip(ble_supervisor_lane)
        .map(|(identity, lane)| {
            let supervisor = BluetoothAuto::new(
                backend,
                identity,
                Endpoint::Nrf52(Nrf52Host::Nrf52),
                LinkCapabilities {
                    l2cap: None,
                    link_mtu: BLE_HW_MTU as u16,
                },
                &BLE_SHARED,
            );
            let fleet: Fleet<Mtx, BLE_HW_MTU, NOTIFY_CAP, LIFECYCLE_CAP> =
                lane.into_fleet(NOTIFY.sender(), LIFECYCLE.sender());
            (supervisor, fleet)
        });

    let usb_fut = usb.run();

    let heartbeat = async {
        let mut n = 0u32;
        loop {
            Timer::after(Duration::from_secs(1)).await;
            n = n.wrapping_add(1);
            if n & 1 == 0 {
                led.set_low();
            } else {
                led.set_high();
            }
        }
    };

    let ui_handle = PrnsNodeHandle::new(COMMANDS.sender(), &COMPLETION);
    let render = async move {
        let mut probe = battery;
        let mut display = match display {
            Some(display) => display,
            None => core::future::pending().await,
        };
        let mut ui_state = hopspot::UiState::new(hopspot::UiConfiguration {
            storage_limits: <crate::storage::Nrf52840Storage as StorageLayout>::LIMITS,
            display_power_control: hopspot::DisplayPowerControl::Unavailable,
            access_point: hopspot::AccessPointState::Unsupported,
        });
        if let Some(notice) = identity_startup_notice {
            ui_state.show_notice(notice);
        }
        let mut working_lora_profile = DEFAULT_915_PROFILE;
        let mut refresh_urgency = hopspot::EinkRefreshUrgency::Immediate;
        let mut activity = hopspot::CardActivityTracker::<{ MEMBERS + 4 }>::new();
        let mut notice_until_ms =
            identity_startup_notice.map(|_| embassy_time::Instant::now().as_millis() + 5_000);
        let mut battery_gauge = hopspot::BatteryGauge::lipo();
        let mut persistence_notice_visible = false;
        loop {
            let vbat_mv = B::battery_millivolts(&mut probe).await;
            let battery = battery_gauge.update(vbat_mv, usb_vbus_present());

            let snapshots = build_snapshots(lora_status, usb_status);
            let mut cards = build_cards(&snapshots, lora_status.id(), usb_status.id());
            let now_ms = embassy_time::Instant::now().as_millis();
            let activity_secs = (now_ms / 1000).min(u64::from(u32::MAX)) as u32;
            activity.update(&mut cards, activity_secs);
            let content = hopspot::ScreenContent {
                cards: &cards,
                local_docs: None,
            };
            ui_state.sync(content);
            let state_not_saved = crate::persistence::state_not_saved();
            if state_not_saved {
                ui_state.show_notice(hopspot::UiNotice::StateNotSaved);
                notice_until_ms = None;
                persistence_notice_visible = true;
            } else if persistence_notice_visible {
                ui_state.clear_notice();
                persistence_notice_visible = false;
            }
            if notice_until_ms.is_some_and(|until| now_ms >= until) {
                ui_state.clear_notice();
                notice_until_ms = None;
                refresh_urgency = hopspot::EinkRefreshUrgency::Immediate;
            }

            let _ = display.clear(BinaryColor::Off);
            let mut interface_menu_details = hopspot::snapshots_to_interface_menu_details(
                ui_state.selected_card(content.cards),
                &snapshots,
            );
            if ui_state
                .selected_card(content.cards)
                .is_some_and(|card| card.id() == lora_status.id())
            {
                let spectrum = lora_spectrum.snapshot();
                interface_menu_details.push_lora_spectrum(hopspot::LoRaSpectrumMenuDetails {
                    channel_busy_per_mille: spectrum.channel_busy_per_mille,
                    noise_floor_dbm: spectrum.noise_floor_dbm,
                    cca_threshold_dbm: spectrum.cca_threshold_dbm,
                    deferrals: spectrum.deferrals,
                    false_preambles: spectrum.false_preambles,
                    contention_timeouts: spectrum.contention_timeouts,
                    duty_holds: spectrum.duty_holds,
                    duty_timeouts: spectrum.duty_timeouts,
                    radio_recoveries: spectrum.radio_recoveries,
                });
            }
            hopspot::render(
                &mut display,
                hopspot::RenderFrame {
                    content,
                    battery,
                    state: &ui_state,
                    interface_menu_details: &interface_menu_details,
                    animation_ms: B::ANIMATION_CLOCK.millis(now_ms),
                },
            );
            B::present(&mut display, now_ms, &refresh_urgency);

            match select3(
                input::EVENTS.receive(),
                INTERFACE_STORE.changed(),
                Timer::after(STATS_POLL),
            )
            .await
            {
                Either3::First(event) => {
                    refresh_urgency = hopspot::EinkRefreshUrgency::Immediate;
                    let action = ui_state.handle_input(event, content);
                    match action {
                        hopspot::UiAction::Sleep => {
                            ui_state.show_notice(hopspot::UiNotice::Sleeping);
                            notice_until_ms =
                                Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                            lora_status.disable();
                            usb_status.disable();
                            let status = BluetoothAutoStatus::new(&BLE_SHARED);
                            status.disable();
                        }
                        hopspot::UiAction::Wake => {
                            ui_state.show_notice(hopspot::UiNotice::Awake);
                            notice_until_ms =
                                Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                            lora_status.enable();
                            usb_status.enable();
                            let status = BluetoothAutoStatus::new(&BLE_SHARED);
                            status.enable();
                        }
                        hopspot::UiAction::Announce => {
                            ui_state.show_notice(hopspot::UiNotice::Announcing);
                            notice_until_ms =
                                Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                            let _ = ui_handle.issue(PrnsCommand::AnnounceNow(AnnounceNow {
                                destination: self_destination,
                                target: AnnounceTarget::AllInterfaces,
                                app_data: AnnounceAppData::Registered,
                            }));
                            let _ = ui_handle.issue(PrnsCommand::AnnounceNow(AnnounceNow {
                                destination: node_page_destination,
                                target: AnnounceTarget::AllInterfaces,
                                app_data: AnnounceAppData::Registered,
                            }));
                        }
                        hopspot::UiAction::ToggleSelectedInterface => {
                            if let Some(card) = ui_state.selected_card(content.cards) {
                                if card.id() == lora_status.id() {
                                    ui_state.show_notice(if lora_status.is_enabled() {
                                        hopspot::UiNotice::TurningOff
                                    } else {
                                        hopspot::UiNotice::TurningOn
                                    });
                                    notice_until_ms =
                                        Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                                    lora_status.toggle_enabled();
                                } else if card.id() == usb_status.id() {
                                    ui_state.show_notice(if usb_status.is_enabled() {
                                        hopspot::UiNotice::TurningOff
                                    } else {
                                        hopspot::UiNotice::TurningOn
                                    });
                                    notice_until_ms =
                                        Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                                    usb_status.toggle_enabled();
                                } else if card.id() == BLE_SUPERVISOR_ID {
                                    let status = BluetoothAutoStatus::new(&BLE_SHARED);
                                    ui_state.show_notice(if status.is_enabled() {
                                        hopspot::UiNotice::TurningOff
                                    } else {
                                        hopspot::UiNotice::TurningOn
                                    });
                                    notice_until_ms =
                                        Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                                    status.toggle_enabled();
                                }
                            }
                        }
                        hopspot::UiAction::OpenLoRaEditor => {
                            ui_state.open_lora_editor(working_lora_profile);
                        }
                        hopspot::UiAction::SetLoRaProfile(profile) => {
                            ui_state.show_notice(hopspot::UiNotice::Saved);
                            notice_until_ms =
                                Some(embassy_time::Instant::now().as_millis() + NOTICE_MS);
                            working_lora_profile = profile;
                            LORA_CONTROL.signal(profile);
                        }
                        hopspot::UiAction::OpenDocs => {}
                        hopspot::UiAction::SwapRadioMode => {}
                        hopspot::UiAction::OledOff => {}
                        hopspot::UiAction::None => {}
                    }
                }
                Either3::Second(()) => {
                    refresh_urgency = hopspot::EinkRefreshUrgency::Immediate;
                }
                Either3::Third(()) => {
                    refresh_urgency = hopspot::EinkRefreshUrgency::Telemetry;
                }
            }
        }
    };

    let io = join5(
        usb_fut,
        usb_dev.run(usb_seam),
        heartbeat,
        input::drive_button(button),
        B::drive_illumination(illumination),
    );
    let ble_plane = async move {
        match bluetooth {
            Some((supervisor, fleet)) => {
                join3(acceptor(sd, &HUB), scanner(sd, &HUB), supervisor.run(fleet)).await;
            }
            None => core::future::pending().await,
        }
    };
    let mesh = join(lora.run(lora_seam), render);
    join3(io, ble_plane, mesh).await;
    core::future::pending().await
}
