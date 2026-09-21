use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dioxus::prelude::spawn;
#[cfg(target_os = "android")]
use personal_rns::bluetooth_auto::BluetoothAuto;
use personal_rns::bluetooth_auto::BluetoothAutoStatus;
use personal_rns::engine::{
    AdmitRemoteControlControllerPairingResponseOutcome, ApproveRemoteControlControllerPairing,
    RejectRemoteControlControllerPairing, RouteSnapshot,
};
use personal_rns::identity::{IdentityHash, IDENTITY_PUBLIC_KEY_LEN};
use personal_rns::interfaces::bluetooth_auto::BleIdentity;
#[cfg(target_os = "android")]
use personal_rns::interfaces::bluetooth_auto::{
    AndroidHost, Endpoint, LinkCapabilities, BLE_HW_MTU,
};
use personal_rns::interfaces::lora::{ModemPreset, Modulation, RadioProfile};
use personal_rns::interfaces::{
    BluetoothIndication, ConnectionState, DiscoveryGroupSet, InterfaceId, InterfaceKind,
    InterfaceMode, InterfaceSnapshot, InterfaceStatus, LoRaIndication, Membership, PeerDetails,
    RadioIndication, RssiDbm, SignalQualityTenthsPercent, SnrQuarterDb, WifiIndication,
};
use personal_rns::load_or_create_ble_identity;
use personal_rns::manifold::tokio::TokioInterfaceStatus;
use personal_rns::node_introspection::{logical_interface_inventory, FrameAccountingCoverage};
use personal_rns::prelude::*;
use personal_rns::remote_control::{
    parse_controller_public_keys, parse_wifi_station_rssi_dbm, parse_wifi_station_ssid,
    ReceiveRemoteControlControllerPairingOfferOutcome, RemoteControlAuthorizeControllerOutcome,
    RemoteControlGroupOutcome, RemoteControlInterfaceCard, RemoteControlInterfaceConfigOutcome,
    RemoteControlInterfaceEntry, RemoteControlInterfaceGroup, RemoteControlInterfacePeer,
    RemoteControlInterfacePeerPage, RemoteControlInterfacePeersOutcome,
    RemoteControlInterfacePower, RemoteControlLoRaOutcome, RemoteControlLoRaProfile,
    RemoteControlModeOutcome, RemoteControlNetworkTransport, RemoteControlNetworkTransportOutcome,
    RemoteControlPairingEndpoint, RemoteControlPairingInvitationCode, RemoteControlPowerOutcome,
    RemoteControlRequestKind, RemoteControlRequestSet, RemoteControlRevokeControllerOutcome,
    RemoteControlSleepOutcome, RemoteControlTargetAccess, RemoteControlWifiStation,
    RemoteControlWifiStationOutcome, REMOTE_CONTROL_APPLICATION_ASPECTS,
    REMOTE_CONTROL_APPLICATION_NAME,
};
use personal_rns::routing::NextHop;
use personal_rns::runtime::RoutingControl;
use personal_rns::units::InstantMillis;
#[cfg(target_os = "android")]
use personal_rns::usb_auto::{UsbAutoCandidate, UsbAutoHost};
#[cfg(target_os = "macos")]
use personal_rns::wifi_auto::apple_service_discovery;
use personal_rns::wifi_auto::AutoWifiStatus;
#[cfg(target_os = "android")]
use personal_rns::wifi_auto::{
    native_service_discovery_with_host_lan, AutoWifiDevicePolicy,
};
#[cfg(not(target_os = "android"))]
use personal_rns::AutoBle;
#[cfg(target_os = "android")]
use prns_ffi::bluetooth_auto::android::AndroidBleBackend;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use tokio::sync::Notify;

use crate::identity_clone::{
    evaluate_clone_locality, identity_clone_destination_hash, parse_clone_siblings,
    parse_identity_clone_message, usb_supervisor_link_is_up, CloneLocality, ControllerAppState,
    DestCloneSession, IdentityClone, IdentityCloneAccept, IdentityCloneAnnounce,
    IdentityCloneHello, IdentityCloneInbound, IdentityClonePayload, IdentityCloneShared,
    IdentityCloneView, SourceCloneSession, IDENTITY_CLONE_APP_NAME, IDENTITY_CLONE_ASPECTS,
    IDENTITY_CLONE_NONCE_LEN, IDENTITY_CLONE_REQUEST_ENDPOINT_ID,
};
use crate::roster_sync::{
    adopt_sibling, attention_for_target, decode_labels, encode_labels, forget_sibling_locally,
    forget_target_locally, import_seed_labels, load_replica, looking_instance, merge_roster,
    next_sibling_alias, next_target_alias, note_local_label, note_local_upsert,
    parse_replica_reply, peer_alias_is_syncable, peer_alias_link_is_local_only,
    peer_alias_value_is_syncable, persist_replica, replica_forgets_target, replica_known_targets,
    replica_message, replica_path, retract_unsyncable_peer_alias_values,
    roster_sync_destination_hash, sibling_alias_is_syncable, strip_local_sibling_alias, write_pull,
    RosterDelta, RosterLabel, RosterLabelKind, RosterShared, RosterSync, TargetAttention,
    ROSTER_SYNC_APP_NAME, ROSTER_SYNC_ASPECTS, ROSTER_SYNC_REQUEST_ENDPOINT_ID,
    THIS_CONTROLLER_ALIAS_LINK, THIS_CONTROLLER_PEER_ALIAS,
};

const IDENTITY_HASH_BYTES: usize = 16;
const INTERFACE_ID_BYTES: usize = 8;
#[cfg(not(target_os = "android"))]
const DEFAULT_DATA_DIRECTORY: &str = ".local/share/hopspot-remote-control";
const TARGET_NAMES_FILE: &str = "target-names";
const TARGET_ALIASES_FILE: &str = "target-aliases";
const PEER_ALIASES_FILE: &str = "peer-aliases";
const PEER_ALIAS_LINKS_FILE: &str = "peer-alias-links";
const TARGET_BLE_PREFIXES_FILE: &str = "target-ble-prefixes";
const TARGET_PEER_IDS_FILE: &str = "target-peer-ids";
const MANAGER_ALIASES_FILE: &str = "manager-aliases";
const SIBLING_ALIASES_FILE: &str = "sibling-aliases";
const SIBLING_WIFI_LL_FILE: &str = "sibling-wifi-ll";
const TCP_TARGET_FILE: &str = "tcp-target";
const CONTROLLER_IDENTITY_FILE: &str = "controller";
const INSTANCE_IDENTITY_FILE: &str = "instance";
const CONTROL_ANNOUNCE_POLL: Duration = Duration::from_millis(500);
/// Path request plus dest announce. USB/BLE/Wi-Fi are seconds; LoRa still fits in a minute.
const CONTROL_ANNOUNCE_WAIT: Duration = Duration::from_secs(60);
const CONNECT_AFTER_ANNOUNCE_ATTEMPTS: u8 = 2;
const CONNECT_AFTER_ANNOUNCE_GAP: Duration = Duration::from_secs(1);
const INVENTORY_RECOVERY_WAIT: Duration = Duration::from_secs(120);
const INVENTORY_RECOVERY_GAP: Duration = Duration::from_secs(2);
const ACTIVITY_CONTROLLER_SCOPE: &str = "controller";
const DEFAULT_TCP_TARGET: &str = "127.0.0.1:4242";
const ROSTER_ANNOUNCE_GAP: Duration = Duration::from_secs(30);
const TARGET_MONITOR_TTL: Duration = Duration::from_secs(10 * 60);
/// After Connect drops a route, prefer a direct BLE path when we already have that peer live.
const DIRECT_PATH_GRACE: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteControlAnnounceWait {
    UntilHeard,
    /// Wait until the control-destination announce is newer than the stored baseline.
    UntilRefreshed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerIdentity {
    pub operator_hash: String,
    pub allow_list_key: String,
    pub operator_secret_path: String,
    pub instance_hash: String,
    pub instance_secret_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetAccess {
    pub id: String,
    pub name: String,
    pub status: TargetStatus,
    pub path: Option<TargetPath>,
    pub build_version: Option<String>,
    pub battery: Option<String>,
    pub network_transport: Option<RemoteControlNetworkTransport>,
    pub monitor_remaining_secs: u32,
    /// Remaining pairing open-window seconds for awaiting advertisements.
    pub pairing_expires_in_secs: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetPath {
    pub hops: u8,
    pub via: String,
    pub interface: String,
    pub announced_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetStatus {
    Online,
    Sleeping,
    Offline,
    AwaitingPairing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathProbeReason {
    OperatorConnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceFact {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub power: InterfacePower,
    pub mode: InterfaceMode,
    pub connection: String,
    pub group: Option<String>,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_bps: Option<u32>,
    pub rx_bps: Option<u32>,
    pub links: u32,
    pub transported_links: u32,
    pub destinations: u32,
    pub rate_bytes_per_sec: u32,
    pub gravity: Option<i64>,
    pub ifac_bytes: Option<usize>,
    pub last_activity_secs: Option<u32>,
    pub detail: Option<String>,
    pub failure: Option<String>,
    pub extras: Vec<InterfaceFact>,
    pub shows_peers: bool,
    pub peers: Vec<InterfacePeer>,
    pub peers_error: Option<String>,
    pub arrived_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfacePeer {
    pub id: String,
    pub name: String,
    pub role: String,
    pub detail: String,
    pub endpoint: Option<String>,
    pub endpoint_label: Option<String>,
    pub local_endpoint: Option<String>,
    pub local_endpoint_label: Option<String>,
    pub alias: Option<String>,
    pub connection: String,
    pub health: Option<PeerHealth>,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub links: u32,
    pub destinations: u32,
    pub rate_bytes_per_sec: u32,
    pub last_activity_secs: Option<u32>,
    pub radio: RadioIndication,
    pub details: PeerDetails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerHealth {
    RadioOnlyIdle,
    RadioOnlyFrames,
    RadioOnlyAnnounced,
    RadioOnlyAnnouncedWithFrames,
    RnsLive,
}

impl PeerHealth {
    pub const fn label(self) -> &'static str {
        match self {
            Self::RadioOnlyIdle => "Radio only · no RNS yet",
            Self::RadioOnlyFrames => "Radio only · frames, no RNS link",
            Self::RadioOnlyAnnounced => "Radio only · announced, no RNS link",
            Self::RadioOnlyAnnouncedWithFrames => "Radio only · announced, frames, no RNS link",
            Self::RnsLive => "RNS live",
        }
    }

    pub const fn is_radio_only(self) -> bool {
        !matches!(self, Self::RnsLive)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfacePower {
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingState {
    Idle,
    /// Invitation accepted; establishing the pairing link / waiting for offer.
    Connecting,
    AwaitingConfirmation {
        digits: String,
    },
    /// Local Approve was pressed; waiting for the target to approve and return completion.
    WaitingForTarget,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("controller startup failed: {0}")]
    Startup(String),
    #[error(
        "that pairing announcement is missing or expired; open pairing on the target again while this app is running"
    )]
    NoPairingAdvertisement,
    #[error("invalid invitation code; enter the eight hexadecimal digits shown by the target")]
    InvalidInvitationCode,
    #[error(
        "the target never answered Begin. Pairing mints a new destination each time Pair remote opens (and after every flash), and a wrong invitation is dropped silently. Leave this app running, open Pair remote again, use the newest awaiting row (not an older Hopspot line), and type the eight digits on the screen now"
    )]
    PairingOfferTimeout,
    #[error("invalid target identity hash: {0}")]
    InvalidTargetId(String),
    #[error("invalid interface id: {0}")]
    InvalidInterfaceId(String),
    #[error("{operation} failed: {detail}")]
    Operation {
        operation: &'static str,
        detail: String,
    },
    #[error("no controller pairing confirmation is pending")]
    NoPendingPairing,
    #[error("{0}")]
    Clone(String),
}

#[derive(Clone)]
pub struct RemoteControlBackend {
    session: Result<Arc<ControllerSession>, BackendError>,
}

impl RemoteControlBackend {
    pub fn new() -> Self {
        Self {
            session: ControllerSession::start().map(|session| {
                let session = Arc::new(session);
                let handle = session.handle.clone();
                let remembered = session.clone();
                spawn(async move {
                    if let Some(transport) = handle.describe_network_transport().await {
                        remembered.set_local_network_transport(transport);
                    }
                });
                session
            }),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.session.is_ok()
    }

    pub fn controller_identity(&self) -> Result<ControllerIdentity, BackendError> {
        let session = self.session()?;
        let mut identity = session.controller_identity.clone();
        if let Ok(secret) = load_controller_secret(&session.identities_dir) {
            let material = personal_rns::identity::PrivateIdentityMaterial::from_bytes(*secret);
            identity.operator_hash = encode_hex(material.identity_hash().as_bytes());
            identity.allow_list_key = encode_hex(material.public().as_bytes());
        }
        Ok(identity)
    }

    pub fn identity_clone(&self) -> Result<IdentityCloneView, BackendError> {
        let session = self.session()?;
        let locality = match self.local_interfaces() {
            Ok(interfaces) => evaluate_clone_locality(&interfaces),
            Err(_) => CloneLocality::UsbOff,
        };
        let shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        let self_hash = session.controller_identity.instance_hash.clone();
        let siblings = session
            .roster
            .lock()
            .ok()
            .map(|roster| {
                let hashes = roster
                    .replica
                    .siblings
                    .iter()
                    .map(|keys| {
                        (
                            encode_hex(keys.identity_hash().as_bytes()),
                            roster.heard_sibling(keys.identity_hash()),
                        )
                    })
                    .collect::<Vec<_>>();
                drop(roster);
                ensure_missing_sibling_aliases(
                    session,
                    hashes
                        .iter()
                        .map(|(hash, _)| hash.as_str())
                        .filter(|hash| sibling_alias_is_syncable(hash, &self_hash)),
                );
                let aliases = session
                    .sibling_aliases
                    .lock()
                    .expect("sibling aliases mutex poisoned");
                let mut siblings = hashes
                    .into_iter()
                    .map(|(instance_hash, heard)| {
                        let is_self = instance_hash == self_hash;
                        crate::identity_clone::SiblingControllerView {
                            alias: aliases
                                .get(&instance_hash)
                                .cloned()
                                .unwrap_or_else(|| next_sibling_alias(&aliases)),
                            instance_hash,
                            heard,
                            is_self,
                        }
                    })
                    .collect::<Vec<_>>();
                siblings.sort_by(|left, right| left.instance_hash.cmp(&right.instance_hash));
                siblings
            })
            .unwrap_or_default();
        Ok(IdentityCloneView {
            locality,
            dest_waiting: shared.dest_waiting,
            dest_accepted: shared.dest.as_ref().is_some_and(|dest| dest.accepted),
            incoming_source: shared
                .dest
                .as_ref()
                .map(|dest| encode_hex(dest.announce.source().as_bytes())),
            incoming_digits: shared
                .dest
                .as_ref()
                .and_then(|dest| dest.transcript)
                .map(|transcript| transcript.confirmation_code().to_string()),
            outgoing_dest: shared.source.as_ref().and_then(|source| {
                source
                    .peer_keys
                    .map(|keys| encode_hex(keys.identity_hash().as_bytes()))
            }),
            outgoing_digits: shared
                .source
                .as_ref()
                .and_then(|source| source.transcript)
                .map(|transcript| transcript.confirmation_code().to_string()),
            source_accepted: shared.source.as_ref().is_some_and(|source| source.accepted),
            in_progress: shared.in_progress(),
            siblings,
            notice: shared.notice.clone(),
            error: shared.error.clone(),
        })
    }

    pub fn arm_clone_dest(&self) -> Result<(), BackendError> {
        let locality = evaluate_clone_locality(&self.local_interfaces()?);
        if !locality.allows_clone() {
            return Err(BackendError::Clone(locality.operator_message()));
        }
        let session = self.session()?;
        let mut shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        shared.dest_waiting = true;
        shared.dest = None;
        shared.error = None;
        shared.notice = Some(
            "Waiting to be adopted over USB. Approving replaces this app's Operator identity. The Instance id stays."
                .to_string(),
        );
        Ok(())
    }

    pub fn cancel_clone(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        let mut shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        *shared = IdentityCloneShared::default();
        Ok(())
    }

    pub async fn start_clone_source(&self) -> Result<(), BackendError> {
        let locality = evaluate_clone_locality(&self.local_interfaces()?);
        if !locality.allows_clone() {
            return Err(BackendError::Clone(locality.operator_message()));
        }
        let session = self.session()?;
        let instance = load_instance_secret(&session.identities_dir)?;
        let instance_material =
            personal_rns::identity::PrivateIdentityMaterial::from_bytes(*instance);
        let source_hash = instance_material.identity_hash();
        let destination = identity_clone_destination_hash(source_hash)
            .ok_or_else(|| BackendError::Clone("clone destination name is invalid".to_string()))?;
        let mut source_nonce = [0u8; IDENTITY_CLONE_NONCE_LEN];
        let mut entropy = OsRuntimeEntropy::try_new()
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        entropy.fill_random(&mut source_nonce);
        let announce = IdentityCloneAnnounce::new(instance_material.public(), source_nonce);
        let mut app_data = [0u8; 128];
        let app_data_len = announce
            .write(&mut app_data)
            .ok_or_else(|| BackendError::Clone("clone announce is too large".to_string()))?;
        let bytes = personal_rns::routing::announce::emit::AnnounceAppDataBytes::from_slice(
            &app_data[..app_data_len],
        )
        .map_err(|_| BackendError::Clone("clone announce app data is too large".to_string()))?;
        session
            .handle
            .announce_now(AnnounceNow {
                destination,
                target: AnnounceTarget::AllInterfaces,
                app_data: AnnounceAppData::Data(bytes),
            })
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        let mut shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        shared.source = Some(SourceCloneSession {
            source_hash,
            source_nonce,
            peer_keys: None,
            transcript: None,
            accepted: false,
            payload: None,
            last_announce: Instant::now(),
        });
        shared.notice = Some(
            "Adoption announce sent on USB. The other app must already have pressed I am up for adoption."
                .to_string(),
        );
        shared.error = None;
        Ok(())
    }

    pub async fn advance_clone(&self) -> Result<(), BackendError> {
        if let Err(error) = self.refresh_clone_source_announce().await {
            let Ok(session) = self.session() else {
                return Err(error);
            };
            if let Ok(mut shared) = session.clone.lock() {
                shared.error = Some(error.to_string());
            }
            return Err(error);
        }
        if let Err(error) = self.send_pending_clone_hello().await {
            let Ok(session) = self.session() else {
                return Err(error);
            };
            if let Ok(mut shared) = session.clone.lock() {
                shared.error = Some(error.to_string());
            }
            return Err(error);
        }
        Ok(())
    }

    async fn refresh_clone_source_announce(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        let (destination, source_nonce) = {
            let mut shared = session
                .clone
                .lock()
                .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
            let Some(source) = shared.source.as_mut() else {
                return Ok(());
            };
            if source.peer_keys.is_some() || source.last_announce.elapsed() < Duration::from_secs(3)
            {
                return Ok(());
            }
            source.last_announce = Instant::now();
            (
                identity_clone_destination_hash(source.source_hash).ok_or_else(|| {
                    BackendError::Clone("clone destination name is invalid".to_string())
                })?,
                source.source_nonce,
            )
        };
        let instance = load_instance_secret(&session.identities_dir)?;
        let instance_material =
            personal_rns::identity::PrivateIdentityMaterial::from_bytes(*instance);
        let announce = IdentityCloneAnnounce::new(instance_material.public(), source_nonce);
        let mut app_data = [0u8; 128];
        let app_data_len = announce
            .write(&mut app_data)
            .ok_or_else(|| BackendError::Clone("clone announce is too large".to_string()))?;
        let bytes = personal_rns::routing::announce::emit::AnnounceAppDataBytes::from_slice(
            &app_data[..app_data_len],
        )
        .map_err(|_| BackendError::Clone("clone announce app data is too large".to_string()))?;
        session
            .handle
            .announce_now(AnnounceNow {
                destination,
                target: AnnounceTarget::AllInterfaces,
                app_data: AnnounceAppData::Data(bytes),
            })
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        Ok(())
    }

    async fn send_pending_clone_hello(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        let should_hello = {
            let mut shared = session
                .clone
                .lock()
                .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
            if !shared.dest_waiting {
                return Ok(());
            }
            let Some(dest) = shared.dest.as_mut() else {
                return Ok(());
            };
            if dest.transcript.is_some() || dest.hello_in_flight {
                return Ok(());
            }
            dest.hello_in_flight = true;
            true
        };
        if !should_hello {
            return Ok(());
        }
        match self.send_clone_hello().await {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Ok(mut shared) = session.clone.lock() {
                    if let Some(dest) = shared.dest.as_mut() {
                        dest.hello_in_flight = false;
                    }
                }
                Err(error)
            }
        }
    }

    async fn send_clone_hello(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        let dest_secret = load_instance_secret(&session.identities_dir)?;
        let dest_material =
            personal_rns::identity::PrivateIdentityMaterial::from_bytes(*dest_secret);
        let (destination, announce) = {
            let shared = session
                .clone
                .lock()
                .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
            let dest = shared.dest.as_ref().ok_or_else(|| {
                BackendError::Clone("no sibling push has been heard on USB".to_string())
            })?;
            (dest.destination, dest.announce)
        };
        let mut dest_nonce = [0u8; IDENTITY_CLONE_NONCE_LEN];
        let mut entropy = OsRuntimeEntropy::try_new()
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        entropy.fill_random(&mut dest_nonce);
        let hello = IdentityCloneHello::sign(
            &dest_material,
            announce.source(),
            announce.source_nonce(),
            dest_nonce,
        );
        let mut hello_bytes = [0u8; 256];
        let hello_len = hello
            .write(&mut hello_bytes)
            .ok_or_else(|| BackendError::Clone("clone hello is too large".to_string()))?;
        let link_id = session
            .handle
            .establish_link(destination)
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        let (hello_reply, _) = session
            .handle
            .request(
                link_id,
                RequestEndpointId::of(IDENTITY_CLONE_REQUEST_ENDPOINT_ID),
                &hello_bytes[..hello_len],
            )
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        match parse_identity_clone_message(&hello_reply) {
            Some(IdentityCloneInbound::HelloAck) => {}
            other => {
                return Err(BackendError::Clone(format!(
                    "the other app rejected hello: {other:?}"
                )))
            }
        }
        let transcript = hello
            .verify(announce.source(), announce.source_nonce())
            .ok_or_else(|| BackendError::Clone("clone hello did not verify locally".to_string()))?;
        let mut shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        if let Some(dest) = shared.dest.as_mut() {
            dest.dest_nonce = Some(dest_nonce);
            dest.transcript = Some(transcript);
            dest.hello_in_flight = false;
        }
        shared.notice = Some(
            "USB sibling session is live. Confirm the six-digit adoption code, then Approve."
                .to_string(),
        );
        shared.error = None;
        Ok(())
    }

    pub async fn accept_clone_source(&self, alias: &str) -> Result<(), BackendError> {
        let locality = evaluate_clone_locality(&self.local_interfaces()?);
        if !locality.allows_clone() {
            return Err(BackendError::Clone(locality.operator_message()));
        }
        let session = self.session()?;
        let secret = load_controller_secret(&session.identities_dir)?;
        let accesses = session
            .handle
            .snapshot_remote_control_target_accesses()
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?
            .unwrap_or_default();
        let mut shared = session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
        let Some(source) = shared.source.as_mut() else {
            return Err(BackendError::Clone(
                "share Operator before accepting".to_string(),
            ));
        };
        if source.transcript.is_none() {
            return Err(BackendError::Clone(
                "the other app has not joined this sibling session yet".to_string(),
            ));
        }
        let mut siblings = {
            let roster = session
                .roster
                .lock()
                .map_err(|_| BackendError::Clone("roster lock was poisoned".to_string()))?;
            roster.replica.siblings.clone()
        };
        if let Some(dest_keys) = source.peer_keys {
            adopt_sibling(
                &mut session
                    .roster
                    .lock()
                    .map_err(|_| BackendError::Clone("roster lock was poisoned".to_string()))?
                    .replica,
                dest_keys,
            );
            remember_sibling_alias(
                session,
                &encode_hex(dest_keys.identity_hash().as_bytes()),
                alias,
            );
            if !siblings
                .iter()
                .any(|sibling| sibling.identity_hash() == dest_keys.identity_hash())
            {
                siblings.push(dest_keys);
            }
        }
        let (clock, labels) = {
            let roster = session
                .roster
                .lock()
                .map_err(|_| BackendError::Clone("roster lock was poisoned".to_string()))?;
            (
                roster.replica.clock,
                encode_clone_labels(
                    &roster.replica.labels,
                    &session.controller_identity.instance_hash,
                ),
            )
        };
        persist_session_replica(session);
        source.payload = Some(IdentityClonePayload {
            operator_secret: secret,
            accesses,
            clock,
            siblings,
            labels,
        });
        source.accepted = true;
        shared.notice = Some("You accepted. Waiting for the other app to Accept.".to_string());
        Ok(())
    }

    pub async fn accept_clone_dest(&self, alias: &str) -> Result<(), BackendError> {
        let locality = evaluate_clone_locality(&self.local_interfaces()?);
        if !locality.allows_clone() {
            return Err(BackendError::Clone(locality.operator_message()));
        }
        let session = self.session()?;
        let dest_secret = load_instance_secret(&session.identities_dir)?;
        let dest_material =
            personal_rns::identity::PrivateIdentityMaterial::from_bytes(*dest_secret);
        if session
            .clone
            .lock()
            .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?
            .dest
            .as_ref()
            .and_then(|dest| dest.transcript)
            .is_none()
        {
            self.send_clone_hello().await?;
        }
        let (destination, announce, transcript) = {
            let mut shared = session
                .clone
                .lock()
                .map_err(|_| BackendError::Clone("clone state lock was poisoned".to_string()))?;
            let dest = shared.dest.as_mut().ok_or_else(|| {
                BackendError::Clone("no sibling push has been heard on USB".to_string())
            })?;
            let transcript = dest.transcript.ok_or_else(|| {
                BackendError::Clone(
                    "the USB sibling session has not finished hello yet".to_string(),
                )
            })?;
            dest.accepted = true;
            (dest.destination, dest.announce, transcript)
        };
        let accept = IdentityCloneAccept::sign(&dest_material, transcript);
        let mut accept_bytes = [0u8; 256];
        let accept_len = accept
            .write(&mut accept_bytes)
            .ok_or_else(|| BackendError::Clone("clone accept is too large".to_string()))?;
        let link_id = session
            .handle
            .establish_link(destination)
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        for _ in 0..30 {
            let (reply, _) = session
                .handle
                .request(
                    link_id,
                    RequestEndpointId::of(IDENTITY_CLONE_REQUEST_ENDPOINT_ID),
                    &accept_bytes[..accept_len],
                )
                .await
                .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
            match parse_identity_clone_message(&reply) {
                Some(IdentityCloneInbound::WaitingForSource) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Some(IdentityCloneInbound::Payload {
                    secret,
                    accesses,
                    clock,
                    siblings,
                    labels,
                }) => {
                    let siblings = parse_clone_siblings(siblings).ok_or_else(|| {
                        BackendError::Clone("clone sibling list was malformed".to_string())
                    })?;
                    let clone_labels = decode_labels(labels).unwrap_or_default();
                    adopt_cloned_identity(
                        &session.identities_dir,
                        &session.persist_dir,
                        secret,
                        accesses,
                    )?;
                    let applied_labels = {
                        let mut roster = session.roster.lock().map_err(|_| {
                            BackendError::Clone("roster lock was poisoned".to_string())
                        })?;
                        roster.replica.clock = clock;
                        adopt_sibling(&mut roster.replica, announce.source_keys());
                        for sibling in &siblings {
                            if sibling.identity_hash() != dest_material.identity_hash() {
                                adopt_sibling(&mut roster.replica, *sibling);
                            }
                        }
                        roster.note_heard_sibling(announce.source());
                        if clone_labels.is_empty() {
                            Vec::new()
                        } else {
                            let (merged, plan) = merge_roster(
                                &roster.replica,
                                &RosterDelta {
                                    clock: roster.replica.clock,
                                    signer: announce.source_keys(),
                                    upserts: Vec::new(),
                                    tombstones: Vec::new(),
                                    siblings: Vec::new(),
                                    labels: clone_labels,
                                },
                            );
                            roster.replica = merged;
                            plan.labels
                        }
                    };
                    if !applied_labels.is_empty() {
                        apply_roster_labels(session, &applied_labels);
                    }
                    remember_sibling_alias(
                        session,
                        &encode_hex(announce.source().as_bytes()),
                        alias,
                    );
                    let extra = siblings
                        .iter()
                        .filter(|sibling| sibling.identity_hash() != dest_material.identity_hash())
                        .map(|sibling| encode_hex(sibling.identity_hash().as_bytes()))
                        .collect::<Vec<_>>();
                    ensure_missing_sibling_aliases(session, extra.iter().map(String::as_str));
                    persist_session_replica(session);
                    {
                        let mut shared = session.clone.lock().map_err(|_| {
                            BackendError::Clone("clone state lock was poisoned".to_string())
                        })?;
                        shared.notice = Some(
                            "Operator identity adopted. Instance is unchanged. Quit and reopen this app so the live node uses the new Operator."
                                .to_string(),
                        );
                        shared.dest_waiting = false;
                        shared.dest = None;
                        shared.error = None;
                    }
                    let _ = self
                        .exchange_roster_with_siblings(
                            RosterExchange::Pull,
                            Some(&[announce.source()]),
                            false,
                        )
                        .await;
                    self.apply_pending_roster().await;
                    persist_session_replica(session);
                    return Ok(());
                }
                other => {
                    return Err(BackendError::Clone(format!(
                        "the other app rejected accept: {other:?}"
                    )))
                }
            }
        }
        Err(BackendError::Clone(
            "the source did not Accept in time".to_string(),
        ))
    }

    pub fn stored_targets(&self) -> Vec<TargetAccess> {
        let Ok(session) = self.session() else {
            return Vec::new();
        };
        let persist = load_persisted_target_hashes(&session.persist_dir);
        let replica = session
            .roster
            .lock()
            .ok()
            .map(|roster| roster.replica.clone())
            .unwrap_or_default();
        let names = session
            .pairing
            .lock()
            .ok()
            .map(|pairing| pairing.target_names.clone())
            .unwrap_or_default();
        let mut items = managed_targets_from_disk(&persist, &replica, &names);
        for item in &mut items {
            item.build_version = session.cached_build_version(&item.id);
            item.battery = session.cached_battery(&item.id);
            item.network_transport = session.cached_network_transport(&item.id);
            item.monitor_remaining_secs = self.monitor_remaining_secs(&item.id);
            if session.target_attention(&item.id) == TargetAttention::HeldBySibling {
                item.status = TargetStatus::Offline;
            }
        }
        items
    }

    pub async fn targets(&self) -> Result<Vec<TargetAccess>, BackendError> {
        let session = self.session()?;
        let inventory = session
            .handle
            .remote_control_target_inventory()
            .await
            .map_err(|error| operation("read target inventory", error))?;
        let mut pairing = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned");
        if let Some((id, name)) = pairing.remember_unnamed_targets(inventory.targets()) {
            drop(pairing);
            self.record_local_label(RosterLabelKind::TargetName, &id, Some(&name));
            pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
        }
        let mut items = Vec::new();
        for target in inventory.targets() {
            let id = encode_hex(target.identity_hash().as_bytes());
            items.push(TargetAccess {
                name: pairing.paired_announce_name(&id).unwrap_or_default(),
                status: if session.target_attention(&id) == TargetAttention::HeldBySibling {
                    TargetStatus::Offline
                } else {
                    pairing.reachability(&id)
                },
                path: None,
                build_version: session.cached_build_version(&id),
                battery: session.cached_battery(&id),
                network_transport: session.cached_network_transport(&id),
                monitor_remaining_secs: 0,
                pairing_expires_in_secs: None,
                id,
            });
        }
        if items.is_empty() {
            drop(pairing);
            items = self.stored_targets();
            pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
        }
        let announcements = pairing.active_announcements();
        drop(pairing);
        for item in items.iter_mut() {
            item.monitor_remaining_secs = self.monitor_remaining_secs(&item.id);
            item.path = if self.path_probe_pending(&item.id) {
                None
            } else {
                self.target_path(&item.id).await
            };
        }
        for announcement in announcements {
            items.push(TargetAccess {
                name: announcement.name,
                status: TargetStatus::AwaitingPairing,
                path: announcement.path,
                build_version: None,
                battery: None,
                network_transport: None,
                monitor_remaining_secs: 0,
                pairing_expires_in_secs: Some(announcement.expires_in_secs),
                id: announcement.id,
            });
        }
        let paired_ids = items
            .iter()
            .filter(|item| item.status != TargetStatus::AwaitingPairing)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let assigned =
            ensure_missing_target_aliases(session, paired_ids.iter().map(String::as_str));
        for (id, alias) in &assigned {
            let _ = self.propagate_target_alias_to_peers(id, Some(alias), None);
        }
        Ok(items)
    }

    pub fn pairing_expires_in_secs(&self, announcement_id: &str) -> Option<u32> {
        let session = self.session().ok()?;
        let mut pairing = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned");
        pairing.announcement_expires_in_secs(announcement_id)
    }

    pub async fn dismiss_pairing_advertisement(
        &self,
        announcement_id: &str,
    ) -> Result<(), BackendError> {
        let announcement_id = announcement_id.trim();
        if announcement_id.is_empty() {
            return Err(BackendError::NoPairingAdvertisement);
        }
        let session = self.session()?;
        // Clear the awaiting card immediately. After Approve the Commit receipt can
        // sit for ~2 minutes; awaiting engine reject first made Dismiss look dead.
        let rejection = {
            let mut pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
            let rejection = pairing
                .pending
                .as_ref()
                .filter(|_| pairing.active_announcement_id.as_deref() == Some(announcement_id))
                .map(|pending| pending.rejection);
            if !pairing.dismiss_operator(announcement_id) {
                return Err(BackendError::NoPairingAdvertisement);
            }
            rejection
        };
        self.record_local_label(
            RosterLabelKind::PairingAdvertDismissed,
            announcement_id,
            Some("dismissed"),
        );
        if let Some(rejection) = rejection {
            let handle = session.handle.clone();
            spawn(async move {
                let _ = handle
                    .reject_remote_control_controller_pairing(rejection)
                    .await;
            });
        }
        Ok(())
    }

    pub async fn forget_target(&self, target_id: &str) -> Result<(), BackendError> {
        let target = IdentityHash::new(
            parse_hex::<IDENTITY_HASH_BYTES>(target_id)
                .map_err(|_| BackendError::InvalidTargetId(target_id.to_string()))?,
        );
        let session = self.session()?;
        let outcome = session
            .handle
            .forget_remote_control_target(target)
            .await
            .map_err(|error| operation("forget target", error))?;
        session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .forget_stored(target_id);
        session.forget_build_version(target_id);
        session.forget_battery(target_id);
        session.forget_network_transport(target_id);
        {
            let mut aliases = session
                .target_aliases
                .lock()
                .expect("target aliases mutex poisoned");
            aliases.remove(target_id);
            persist_target_names(&session.target_aliases_path, &aliases);
        }
        clear_target_peer_identity(session, target_id);
        match outcome {
            ForgetRemoteControlTargetOutcome::Forgotten { .. }
            | ForgetRemoteControlTargetOutcome::NotFound => {
                if let Ok(mut roster) = session.roster.lock() {
                    forget_target_locally(&mut roster.replica, target);
                }
                persist_session_replica(session);
                spawn({
                    let backend = self.clone();
                    async move {
                        let _ = backend.push_roster_to_siblings().await;
                    }
                });
                Ok(())
            }
        }
    }

    pub fn is_monitoring_target(&self, target_id: &str) -> bool {
        self.monitor_remaining_secs(target_id) > 0
    }

    pub fn monitor_remaining_secs(&self, target_id: &str) -> u32 {
        let Ok(session) = self.session() else {
            return 0;
        };
        let until = session
            .monitor_until
            .lock()
            .ok()
            .and_then(|until| until.get(target_id).copied());
        monitor_remaining_at(until, Instant::now(), session.target_attention(target_id))
    }

    pub fn advance_target_monitors(&self) {
        let Ok(session) = self.session() else {
            return;
        };
        let now = Instant::now();
        let live = {
            let Ok(until) = session.monitor_until.lock() else {
                return;
            };
            until
                .iter()
                .filter(|(_, deadline)| **deadline > now)
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>()
        };
        let mut released = Vec::new();
        if let Ok(mut until) = session.monitor_until.lock() {
            until.retain(|id, deadline| {
                let keep = *deadline > now;
                if !keep {
                    released.push(id.clone());
                }
                keep
            });
        }
        let stale = session
            .roster
            .lock()
            .ok()
            .map(|roster| {
                roster
                    .replica
                    .labels
                    .iter()
                    .filter(|label| {
                        label.kind == RosterLabelKind::TargetLooking
                            && looking_instance(&roster.replica, &label.key).is_some_and(|holder| {
                                holder.eq_ignore_ascii_case(
                                    &session.controller_identity.instance_hash,
                                )
                            })
                            && !live.contains(&label.key)
                    })
                    .map(|label| label.key.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        released.extend(stale);
        released.sort();
        released.dedup();
        if released.is_empty() {
            return;
        }
        if let Ok(mut roster) = session.roster.lock() {
            for id in &released {
                note_local_label(
                    &mut roster.replica,
                    RosterLabelKind::TargetLooking,
                    id,
                    None,
                );
            }
        }
        persist_session_replica(session);
        for id in &released {
            session.mark_reachable(id, TargetStatus::Offline);
        }
        let backend = self.clone();
        spawn(async move {
            let _ = backend.push_roster_to_siblings().await;
        });
    }

    fn begin_target_monitor(&self, target_id: &str) {
        let Ok(session) = self.session() else {
            return;
        };
        if let Ok(mut until) = session.monitor_until.lock() {
            until.insert(target_id.to_owned(), Instant::now() + TARGET_MONITOR_TTL);
        }
        self.claim_target_attention(target_id);
    }

    fn claim_target_attention(&self, target_id: &str) {
        let Ok(session) = self.session() else {
            return;
        };
        let instance = session.controller_identity.instance_hash.clone();
        self.record_local_label(RosterLabelKind::TargetLooking, target_id, Some(&instance));
    }

    pub async fn interfaces_after_announce(
        &self,
        target_id: &str,
        wait: RemoteControlAnnounceWait,
    ) -> Result<Vec<InterfaceEntry>, BackendError> {
        if !self.is_monitoring_target(target_id) {
            return Err(not_monitoring(target_id));
        }
        let inventory_deadline = Instant::now() + INVENTORY_RECOVERY_WAIT;
        loop {
            if !self.is_monitoring_target(target_id) {
                return Err(not_monitoring(target_id));
            }
            let baseline = match wait {
                RemoteControlAnnounceWait::UntilHeard => None,
                RemoteControlAnnounceWait::UntilRefreshed => self
                    .session()?
                    .pairing
                    .lock()
                    .expect("pairing state mutex poisoned")
                    .control_announce_baseline
                    .get(target_id)
                    .copied(),
            };
            let path = self.target_path(target_id).await;
            if should_wait_for_control_announce(wait, path.as_ref()) {
                let announce_deadline = Instant::now() + CONTROL_ANNOUNCE_WAIT;
                if let Err(error) = self
                    .wait_for_control_announce(target_id, wait, baseline, announce_deadline)
                    .await
                {
                    // After a dropped hop, prefer a refreshed announce before establish.
                    // If none arrives in time, still attempt inventory within the recovery window.
                    if !inventory_recovery_continues(Instant::now(), inventory_deadline) {
                        return Err(error);
                    }
                    eprintln!(
                        "connect to target {target_id}: announce wait ended ({error}); trying inventory"
                    );
                }
            } else {
                let _ = self.request_control_path(target_id).await;
            }
            match self.inventory_after_announce(target_id).await {
                Ok(entries) => return Ok(entries),
                Err(error) => {
                    eprintln!("connect to target {target_id} failed: {error}");
                    if !inventory_recovery_continues(Instant::now(), inventory_deadline) {
                        return Err(error);
                    }
                    if matches!(wait, RemoteControlAnnounceWait::UntilRefreshed) {
                        self.remember_control_announce_baseline(target_id).await;
                    }
                    tokio::time::sleep(INVENTORY_RECOVERY_GAP).await;
                }
            }
        }
    }

    pub async fn begin_pairing(
        &self,
        announcement_id: &str,
        invitation_code: &str,
    ) -> Result<PairingState, BackendError> {
        let session = self.session()?;
        let invitation_code = parse_invitation_code(invitation_code)?;
        let (endpoint, expires_at) = {
            let mut pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
            pairing.pending = None;
            pairing.pin_announcement(announcement_id);
            let heard = pairing
                .announcement(announcement_id)
                .ok_or(BackendError::NoPairingAdvertisement)?;
            pairing.pending_announce_name = pairing.active_announcement_name();
            heard
        };
        self.record_local_label(
            RosterLabelKind::PairingAdvertDismissed,
            announcement_id,
            Some("taken"),
        );
        let control_announce_baseline = self.snapshot_control_announces().await?;
        session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .control_announce_baseline = control_announce_baseline;
        let received = match session
            .handle
            .initiate_remote_control_controller_pairing(InitiateRemoteControlControllerPairing {
                endpoint,
                invitation_code,
                expires_at,
            })
            .await
        {
            Ok(received) => received,
            Err(error) => {
                return Err(begin_pairing_error(error));
            }
        };

        // Prefer the settled offer outcome (always available when initiate
        // returns Ok). The confirmation journal is a side channel that some
        // desktop executors can miss relative to this await.
        if let AdmitRemoteControlControllerPairingResponseOutcome::Offer(
            ReceiveRemoteControlControllerPairingOfferOutcome::ConfirmationRequired { attempt_id },
        ) = received.admission
        {
            let digits = attempt_id.confirmation_code().to_string();
            let pending = PendingPairing {
                digits: digits.clone(),
                approval: ApproveRemoteControlControllerPairing { attempt_id },
                rejection: RejectRemoteControlControllerPairing { attempt_id },
            };
            session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned")
                .set_pending(pending);
            return Ok(PairingState::AwaitingConfirmation { digits });
        }

        for _ in 0..100 {
            if let Some(digits) = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned")
                .pending
                .as_ref()
                .map(|pending| pending.digits.clone())
            {
                return Ok(PairingState::AwaitingConfirmation { digits });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(BackendError::Operation {
            operation: "begin controller pairing",
            detail: format!(
                "pairing offer returned without confirmation (admission={:?})",
                received.admission
            ),
        })
    }

    pub async fn approve_pairing(&self) -> Result<PairingState, BackendError> {
        let session = self.session()?;
        let approval = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .pending
            .as_ref()
            .map(|pending| pending.approval)
            .ok_or(BackendError::NoPendingPairing)?;
        session
            .handle
            .approve_remote_control_controller_pairing(approval)
            .await
            .map_err(|error| operation("approve controller pairing", error))?;
        let (pending_name, pending_target_id) = {
            let mut pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
            pairing.pending = None;
            let name = pairing
                .active_announcement_name()
                .or_else(|| pairing.pending_announce_name.clone());
            pairing.pending_announce_name = name.clone();
            (name, pairing.pending_target_id.clone())
        };
        let paired = self
            .wait_for_paired_inventory_target(pending_target_id.as_deref())
            .await;
        if let Some(target) = paired {
            let id = encode_hex(target.as_bytes());
            {
                let mut pairing = session
                    .pairing
                    .lock()
                    .expect("pairing state mutex poisoned");
                if let Some(name) = pending_name.as_deref() {
                    pairing.set_announce_name(&id, name);
                }
                pairing.finish_pairing_announcement();
            }
            if let Ok(mut roster) = session.roster.lock() {
                note_local_upsert(&mut roster.replica, target);
                if let Some(name) = pending_name.as_deref() {
                    note_local_label(
                        &mut roster.replica,
                        RosterLabelKind::TargetName,
                        &id,
                        Some(name),
                    );
                }
            }
            persist_session_replica(session);
            self.refresh_cached_snapshot().await;
        }
        spawn({
            let backend = self.clone();
            async move {
                let _ = backend.push_roster_to_siblings().await;
            }
        });
        Ok(PairingState::Approved)
    }

    async fn wait_for_paired_inventory_target(
        &self,
        pending_id: Option<&str>,
    ) -> Option<IdentityHash> {
        let session = self.session().ok()?;
        for _ in 0..20 {
            if let Ok(inventory) = session.handle.remote_control_target_inventory().await {
                let hashes: Vec<IdentityHash> = inventory
                    .targets()
                    .iter()
                    .map(|target| target.identity_hash())
                    .collect();
                if let Some(hash) = resolve_paired_target_hash(&hashes, pending_id) {
                    return Some(hash);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let inventory = session
            .handle
            .remote_control_target_inventory()
            .await
            .ok()?;
        let hashes: Vec<IdentityHash> = inventory
            .targets()
            .iter()
            .map(|target| target.identity_hash())
            .collect();
        resolve_paired_target_hash(&hashes, pending_id)
    }

    async fn refresh_cached_snapshot(&self) {
        let Ok(session) = self.session() else {
            return;
        };
        if let Ok(snapshot) = session
            .handle
            .snapshot_remote_control_target_accesses()
            .await
        {
            if let Ok(mut roster) = session.roster.lock() {
                roster.cached_snapshot = snapshot.unwrap_or_default();
            }
        }
    }

    pub async fn reject_pairing(&self) -> Result<PairingState, BackendError> {
        let session = self.session()?;
        let rejection = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .pending
            .as_ref()
            .map(|pending| pending.rejection)
            .ok_or(BackendError::NoPendingPairing)?;
        session
            .handle
            .reject_remote_control_controller_pairing(rejection)
            .await
            .map_err(|error| operation("reject controller pairing", error))?;
        let mut pairing = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned");
        pairing.pending = None;
        if let Some(id) = pairing.active_announcement_id.clone() {
            pairing.pinned.remove(&id);
        }
        Ok(PairingState::Rejected)
    }

    pub async fn enroll_flashed_target(
        &self,
        access: RemoteControlTargetAccess,
        announce_name: &str,
    ) -> Result<String, BackendError> {
        let session = self.session()?;
        let target = access.target().identity_hash();
        let id = encode_hex(target.as_bytes());
        session
            .handle
            .set_remote_control_target_access(access)
            .await
            .map_err(|error| operation("enroll flashed target", error))?;
        {
            let mut pairing = session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned");
            pairing.set_announce_name(&id, announce_name);
        }
        if let Ok(mut roster) = session.roster.lock() {
            note_local_upsert(&mut roster.replica, target);
            note_local_label(
                &mut roster.replica,
                RosterLabelKind::TargetName,
                &id,
                Some(announce_name),
            );
        }
        persist_session_replica(session);
        self.refresh_cached_snapshot().await;
        let assigned = ensure_missing_target_aliases(session, std::iter::once(id.as_str()));
        for (assigned_id, alias) in &assigned {
            let _ = self.propagate_target_alias_to_peers(assigned_id, Some(alias), None);
        }
        spawn({
            let backend = self.clone();
            async move {
                let _ = backend.push_roster_to_siblings().await;
            }
        });
        Ok(id)
    }

    pub fn target_announce_name(&self, target_id: &str) -> Option<String> {
        self.session().ok().and_then(|session| {
            session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned")
                .paired_announce_name(target_id)
        })
    }

    pub fn target_aliases(&self) -> HashMap<String, String> {
        self.session()
            .map(|session| {
                session
                    .target_aliases
                    .lock()
                    .expect("target aliases mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn set_target_alias(&self, target_id: &str, alias: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let id = target_id.trim();
        if id.is_empty() {
            return Ok(());
        }
        let name = alias.trim();
        let mut aliases = session
            .target_aliases
            .lock()
            .expect("target aliases mutex poisoned");
        let previous = aliases.get(id).cloned();
        if name.is_empty() {
            aliases.remove(id);
        } else {
            aliases.insert(id.to_owned(), name.to_owned());
        }
        persist_target_names(&session.target_aliases_path, &aliases);
        drop(aliases);
        self.record_local_label(
            RosterLabelKind::TargetAlias,
            id,
            (!name.is_empty()).then_some(name),
        );
        self.propagate_target_alias_to_peers(
            id,
            stored_alias(Some(name)).as_deref(),
            stored_alias(previous.as_deref()).as_deref(),
        )?;
        Ok(())
    }

    pub fn peer_aliases(&self) -> HashMap<String, String> {
        self.session()
            .map(|session| {
                session
                    .peer_aliases
                    .lock()
                    .expect("peer aliases mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn manager_aliases(&self) -> HashMap<String, String> {
        self.session()
            .map(|session| {
                session
                    .manager_aliases
                    .lock()
                    .expect("manager aliases mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn set_manager_alias(&self, hash: &str, alias: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let id = hash.trim();
        if id.is_empty() {
            return Ok(());
        }
        let name = alias.trim();
        let mut aliases = session
            .manager_aliases
            .lock()
            .expect("manager aliases mutex poisoned");
        if name.is_empty() {
            aliases.remove(id);
        } else {
            aliases.insert(id.to_owned(), name.to_owned());
        }
        persist_target_names(&session.manager_aliases_path, &aliases);
        self.record_local_label(
            RosterLabelKind::ManagerAlias,
            id,
            (!name.is_empty()).then_some(name),
        );
        Ok(())
    }

    pub fn set_peer_alias(&self, peer_id: &str, alias: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let id = peer_id.trim();
        if id.is_empty() {
            return Ok(());
        }
        let name = alias.trim();
        {
            let mut aliases = session
                .peer_aliases
                .lock()
                .expect("peer aliases mutex poisoned");
            if name.is_empty() {
                aliases.remove(id);
            } else {
                aliases.insert(id.to_owned(), name.to_owned());
            }
            persist_target_names(&session.peer_aliases_path, &aliases);
        }
        // Manual edits stop tracking the managed-node alias.
        remove_alias_map_key(
            &session.peer_alias_links,
            &session.peer_alias_links_path,
            id,
        );
        if name.is_empty() {
            // Clearing a manual alias restores the managed-node auto alias when known.
            // Auto aliases stay local; do not publish PeerAlias for the restore.
            if self.restore_auto_peer_alias(id)?.is_some() {
                return Ok(());
            }
        }
        if peer_alias_is_syncable(id) {
            if name.is_empty() || peer_alias_value_is_syncable(Some(name)) {
                self.record_local_label(
                    RosterLabelKind::PeerAlias,
                    id,
                    (!name.is_empty()).then_some(name),
                );
            } else {
                // Local-only display string (e.g. "This Controller") — retract any prior sync.
                self.record_local_label(RosterLabelKind::PeerAlias, id, None);
            }
        }
        Ok(())
    }

    fn restore_auto_peer_alias(&self, peer_id: &str) -> Result<Option<String>, BackendError> {
        let session = self.session()?;
        let Some(target_id) = match_peer_to_target(session, peer_id) else {
            return Ok(None);
        };
        let Some(alias) = stored_alias(
            session
                .target_aliases
                .lock()
                .expect("target aliases mutex poisoned")
                .get(&target_id)
                .map(String::as_str),
        ) else {
            return Ok(None);
        };
        auto_fill_peer_alias(Some(self), session, peer_id, &target_id, &alias)?;
        Ok(Some(alias))
    }

    pub fn set_sibling_alias(&self, instance_hash: &str, alias: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let id = instance_hash.trim();
        if id.is_empty()
            || !sibling_alias_is_syncable(id, &session.controller_identity.instance_hash)
        {
            return Ok(());
        }
        remember_sibling_alias(session, id, alias);
        let backend = self.clone();
        spawn(async move {
            let _ = backend.push_roster_to_siblings().await;
        });
        Ok(())
    }

    pub fn sibling_aliases(&self) -> HashMap<String, String> {
        self.session()
            .ok()
            .map(|session| {
                session
                    .sibling_aliases
                    .lock()
                    .expect("sibling aliases mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn forget_sibling(&self, instance_hash: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let id = instance_hash.trim();
        if id.is_empty()
            || !sibling_alias_is_syncable(id, &session.controller_identity.instance_hash)
        {
            return Ok(());
        }
        let hash = IdentityHash::new(parse_hex::<IDENTITY_HASH_BYTES>(id).map_err(|_| {
            BackendError::Operation {
                operation: "remove sibling",
                detail: "sibling Instance hash must be 32 hex characters".to_string(),
            }
        })?);
        {
            let mut roster = session.roster.lock().map_err(|_| BackendError::Operation {
                operation: "remove sibling",
                detail: "roster lock was poisoned".to_string(),
            })?;
            forget_sibling_locally(&mut roster.replica, hash);
            roster.applied_generation = roster.applied_generation.saturating_add(1);
        }
        remove_alias_map_key(&session.sibling_aliases, &session.sibling_aliases_path, id);
        remove_alias_map_key(&session.sibling_wifi_ll, &session.sibling_wifi_ll_path, id);
        refresh_sibling_linked_peer_aliases(session, id, None);
        persist_session_replica(session);
        let backend = self.clone();
        spawn(async move {
            let _ = backend.push_roster_to_siblings().await;
        });
        Ok(())
    }

    pub fn suggested_sibling_alias(&self) -> String {
        self.session()
            .ok()
            .map(|session| {
                next_sibling_alias(
                    &session
                        .sibling_aliases
                        .lock()
                        .expect("sibling aliases mutex poisoned"),
                )
            })
            .unwrap_or_else(|| "Sibling 1".to_string())
    }

    pub fn roster_apply_generation(&self) -> u64 {
        self.session()
            .ok()
            .and_then(|session| session.roster.lock().ok())
            .map(|roster| roster.applied_generation)
            .unwrap_or(0)
    }

    fn record_local_label(&self, kind: RosterLabelKind, key: &str, value: Option<&str>) {
        let Ok(session) = self.session() else {
            return;
        };
        if let Ok(mut roster) = session.roster.lock() {
            note_local_label(&mut roster.replica, kind, key, value);
        }
        persist_session_replica(session);
        let backend = self.clone();
        spawn(async move {
            let _ = backend.push_roster_to_siblings().await;
        });
    }

    pub fn local_interfaces(&self) -> Result<Vec<InterfaceEntry>, BackendError> {
        let mut items = self.local_interfaces_without_reconcile()?;
        self.reconcile_peer_aliases_from_entries(&items);
        let session = self.session()?;
        let aliases = session
            .peer_aliases
            .lock()
            .expect("peer aliases mutex poisoned")
            .clone();
        for item in &mut items {
            apply_peer_aliases(&mut item.peers, &aliases);
        }
        session.stamp_activity(ACTIVITY_CONTROLLER_SCOPE, &mut items);
        stamp_interface_arrivals(&mut items);
        Ok(items)
    }

    pub fn set_local_interface_power(
        &self,
        interface_id: &str,
        power: InterfacePower,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        self.session()?.local_power.set(id, power)
    }

    pub fn set_local_interface_mode(
        &self,
        interface_id: &str,
        mode: InterfaceMode,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        match self.session()?.handle.set_interface_mode(id, mode) {
            RemoteControlModeOutcome::Applied => Ok(()),
            RemoteControlModeOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set local interface mode",
                detail: "this controller does not know that interface".to_string(),
            }),
            RemoteControlModeOutcome::Failed => Err(BackendError::Operation {
                operation: "set local interface mode",
                detail: "the controller could not apply the requested mode".to_string(),
            }),
        }
    }

    pub fn set_local_interface_group(
        &self,
        interface_id: &str,
        group: &str,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let Some(group) = RemoteControlInterfaceGroup::parse(group.trim()) else {
            return Err(BackendError::Operation {
                operation: "set local interface group",
                detail: "group id must be 1 to 32 UTF-8 bytes".to_string(),
            });
        };
        match self.session()?.local_power.set_group(id, group) {
            RemoteControlGroupOutcome::Applied => Ok(()),
            RemoteControlGroupOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set local interface group",
                detail: "this controller does not know that interface".to_string(),
            }),
            RemoteControlGroupOutcome::Failed => Err(BackendError::Operation {
                operation: "set local interface group",
                detail: "the controller could not apply the requested group".to_string(),
            }),
        }
    }

    pub fn set_local_tcp_target(&self, target: &str) -> Result<(), BackendError> {
        let session = self.session()?;
        let canonical =
            parse_tcp_dial_target(target).map_err(|detail| BackendError::Operation {
                operation: "set controller TCP target",
                detail,
            })?;
        persist_tcp_target(&session.tcp_target_path, &canonical);
        *session
            .tcp_dial
            .lock()
            .expect("tcp client target mutex poisoned") = canonical;
        session.local_power.bounce_tokio_if_enabled(session.tcp_id)
    }

    pub async fn set_interface_power(
        &self,
        target_id: &str,
        interface_id: &str,
        power: InterfacePower,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .set_interface_power(
                id,
                match power {
                    InterfacePower::On => RemoteControlInterfacePower::On,
                    InterfacePower::Off => RemoteControlInterfacePower::Off,
                },
            )
            .await
            .map_err(|error| operation("set interface power", error))?;
        remote.close();
        match outcome {
            RemoteControlPowerOutcome::Applied
            | RemoteControlPowerOutcome::Unchanged
            | RemoteControlPowerOutcome::Scheduled => Ok(()),
            RemoteControlPowerOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set interface power",
                detail: "the target does not know that interface".to_string(),
            }),
            RemoteControlPowerOutcome::Failed => Err(BackendError::Operation {
                operation: "set interface power",
                detail: "the target could not apply the requested state".to_string(),
            }),
        }
    }

    pub async fn set_interface_mode(
        &self,
        target_id: &str,
        interface_id: &str,
        mode: InterfaceMode,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .set_interface_mode(id, mode)
            .await
            .map_err(|error| operation("set interface mode", error))?;
        remote.close();
        match outcome {
            RemoteControlModeOutcome::Applied => Ok(()),
            RemoteControlModeOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set interface mode",
                detail: "the target does not know that interface".to_string(),
            }),
            RemoteControlModeOutcome::Failed => Err(BackendError::Operation {
                operation: "set interface mode",
                detail: "the target could not apply the requested mode".to_string(),
            }),
        }
    }

    pub async fn set_interface_group(
        &self,
        target_id: &str,
        interface_id: &str,
        group: &str,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let Some(group) = RemoteControlInterfaceGroup::parse(group.trim()) else {
            return Err(BackendError::Operation {
                operation: "set interface group",
                detail: "group id must be 1 to 32 UTF-8 bytes".to_string(),
            });
        };
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .set_interface_group(id, group)
            .await
            .map_err(|error| operation("set interface group", error))?;
        remote.close();
        match outcome {
            RemoteControlGroupOutcome::Applied => Ok(()),
            RemoteControlGroupOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set interface group",
                detail: "the target does not know that interface".to_string(),
            }),
            RemoteControlGroupOutcome::Failed => Err(BackendError::Operation {
                operation: "set interface group",
                detail: "the target could not apply the requested group".to_string(),
            }),
        }
    }

    pub async fn set_interface_lora_profile(
        &self,
        target_id: &str,
        interface_id: &str,
        profile: RadioProfile,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let Some(profile) = RemoteControlLoRaProfile::from_profile(profile) else {
            return Err(BackendError::Operation {
                operation: "set interface LoRa profile",
                detail: "the requested radio settings are outside the selected region's limits"
                    .to_string(),
            });
        };
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .set_interface_lora_profile(id, profile)
            .await
            .map_err(lora_profile_exchange_error)?;
        remote.close();
        match outcome {
            RemoteControlLoRaOutcome::Applied => Ok(()),
            RemoteControlLoRaOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set interface LoRa profile",
                detail: "the target does not know that LoRa interface".to_string(),
            }),
            RemoteControlLoRaOutcome::Failed => Err(BackendError::Operation {
                operation: "set interface LoRa profile",
                detail: "the target could not apply the requested radio settings".to_string(),
            }),
        }
    }

    pub async fn set_interface_wifi_station(
        &self,
        target_id: &str,
        interface_id: &str,
        ssid: &str,
        password: &str,
    ) -> Result<(), BackendError> {
        let id = InterfaceId::new(
            parse_hex::<INTERFACE_ID_BYTES>(interface_id)
                .map_err(|_| BackendError::InvalidInterfaceId(interface_id.to_string()))?,
        );
        let station = RemoteControlWifiStation::parse(ssid.trim(), password).map_err(|_| {
            BackendError::Operation {
                operation: "set interface Wi-Fi station",
                detail: "SSID must be 1 to 32 UTF-8 bytes; password may be empty or up to 64 UTF-8 bytes".to_string(),
            }
        })?;
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .set_interface_wifi_station(id, station)
            .await
            .map_err(wifi_station_exchange_error)?;
        remote.close();
        match outcome {
            RemoteControlWifiStationOutcome::Applied => Ok(()),
            RemoteControlWifiStationOutcome::UnknownInterface => Err(BackendError::Operation {
                operation: "set interface Wi-Fi station",
                detail: "the target does not know that Auto Wi-Fi interface".to_string(),
            }),
            RemoteControlWifiStationOutcome::Failed => Err(BackendError::Operation {
                operation: "set interface Wi-Fi station",
                detail: "the target has no live station radio to join. Flash Wi-Fi once so the board starts a station stack, then you can change SSID and password here".to_string(),
            }),
        }
    }

    pub async fn inventory_controllers(
        &self,
        target_id: &str,
    ) -> Result<Vec<String>, BackendError> {
        let remote = self.connect_target(target_id).await?;
        eprintln!("inventory controllers request to {target_id}");
        let (inventory, _) = remote
            .inventory_controllers()
            .await
            .map_err(controller_whitelist_exchange_error)?;
        remote.close();
        Ok(inventory
            .hashes()
            .iter()
            .map(|hash| encode_hex(hash.as_bytes()))
            .collect())
    }

    pub async fn authorize_controller(
        &self,
        target_id: &str,
        allow_list_key: &str,
    ) -> Result<(), BackendError> {
        let controller = parse_allow_list_key(allow_list_key)?;
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .authorize_controller(controller, RemoteControlRequestSet::all())
            .await
            .map_err(controller_whitelist_exchange_error)?;
        remote.close();
        match outcome {
            RemoteControlAuthorizeControllerOutcome::Applied => Ok(()),
            RemoteControlAuthorizeControllerOutcome::CapacityExhausted => {
                Err(BackendError::Operation {
                    operation: "authorize controller",
                    detail: "the target allow-list is full".to_string(),
                })
            }
            RemoteControlAuthorizeControllerOutcome::Failed => Err(BackendError::Operation {
                operation: "authorize controller",
                detail: "the target could not add that controller".to_string(),
            }),
            RemoteControlAuthorizeControllerOutcome::Forbidden => Err(BackendError::Operation {
                operation: "authorize controller",
                detail: "this controller is not allowed to change the allow-list".to_string(),
            }),
            RemoteControlAuthorizeControllerOutcome::Busy => Err(BackendError::Operation {
                operation: "authorize controller",
                detail: "the target is busy applying another allow-list change".to_string(),
            }),
        }
    }

    pub async fn revoke_controller(&self, target_id: &str, hash: &str) -> Result<(), BackendError> {
        let hash = IdentityHash::new(parse_hex::<IDENTITY_HASH_BYTES>(hash).map_err(|_| {
            BackendError::Operation {
                operation: "revoke controller",
                detail: "controller hash must be 32 hex characters".to_string(),
            }
        })?);
        let remote = self.connect_target(target_id).await?;
        let (outcome, _) = remote
            .revoke_controller(hash)
            .await
            .map_err(controller_whitelist_exchange_error)?;
        remote.close();
        match outcome {
            RemoteControlRevokeControllerOutcome::Applied => Ok(()),
            RemoteControlRevokeControllerOutcome::NotFound => Err(BackendError::Operation {
                operation: "revoke controller",
                detail: "that controller is not on the allow-list".to_string(),
            }),
            RemoteControlRevokeControllerOutcome::Forbidden => Err(BackendError::Operation {
                operation: "revoke controller",
                detail: "this controller cannot remove itself from the allow-list".to_string(),
            }),
            RemoteControlRevokeControllerOutcome::Failed => Err(BackendError::Operation {
                operation: "revoke controller",
                detail: "the target could not remove that controller".to_string(),
            }),
            RemoteControlRevokeControllerOutcome::Busy => Err(BackendError::Operation {
                operation: "revoke controller",
                detail: "the target is busy applying another allow-list change".to_string(),
            }),
        }
    }

    #[allow(dead_code)]
    pub async fn refresh_build_version(
        &self,
        target_id: &str,
    ) -> Result<Option<String>, BackendError> {
        if !self.is_monitoring_target(target_id) {
            return Ok(self.session()?.cached_build_version(target_id));
        }
        if let Some(cached) = self.session()?.cached_build_version(target_id) {
            return Ok(Some(cached));
        }
        let remote = self.connect_target(target_id).await?;
        let result = remote.describe_build().await;
        remote.close();
        match result {
            Ok((version, _)) => {
                let text = version
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned);
                if let Some(text) = text.clone() {
                    self.session()?.remember_build_version(target_id, text);
                }
                Ok(text)
            }
            Err(_) => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub async fn refresh_battery(&self, target_id: &str) -> Result<Option<String>, BackendError> {
        if !self.is_monitoring_target(target_id) {
            return Ok(self.session()?.cached_battery(target_id));
        }
        if !self.session()?.battery_needs_refresh(target_id) {
            return Ok(self.session()?.cached_battery(target_id));
        }
        let remote = self.connect_target(target_id).await?;
        let result = remote.describe_power().await;
        remote.close();
        self.session()?.mark_battery_fetched(target_id);
        match result {
            Ok((snapshot, _)) => {
                // Keep a visible row even when the board reports UNKNOWN so we can tell
                // "RPC works" apart from "RPC never landed".
                let label = format_managed_node_battery(snapshot).or(Some("unknown".to_string()));
                eprintln!(
                    "describe_power {target_id}: applicable={} battery={:?} external={:?} label={label:?}",
                    snapshot.is_applicable(),
                    snapshot.battery().map(|percent| percent.get()),
                    snapshot.external_power(),
                );
                if let Some(text) = label.clone() {
                    self.session()?.remember_battery(target_id, text);
                }
                Ok(label)
            }
            Err(error) => {
                eprintln!("describe_power {target_id} failed: {error:?}");
                Ok(self.session()?.cached_battery(target_id))
            }
        }
    }

    pub async fn probe_target(
        &self,
        target_id: &str,
        reason: PathProbeReason,
    ) -> Result<TargetPath, BackendError> {
        match reason {
            PathProbeReason::OperatorConnect => self.begin_target_monitor(target_id),
        }
        // Hide announce/route in the UI until this probe learns a fresh path.
        self.set_path_probe_pending(target_id, true);
        let prefer_direct = self.has_live_direct_ble_peer(target_id);
        if let Some(path) = self.target_path(target_id).await {
            eprintln!(
                "connect drops hop to {target_id} on {} and waits for a dest announce",
                path.interface
            );
        }
        // Snapshot pre-drop announce so Connect inventory can UntilRefreshed.
        self.remember_control_announce_baseline(target_id).await;
        let outcome = async {
            self.forget_control_route(target_id).await?;
            self.request_control_path(target_id).await?;
            match self
                .wait_for_preferred_control_path(target_id, prefer_direct)
                .await
            {
                Some(path) => {
                    self.session()?
                        .mark_reachable(target_id, TargetStatus::Online);
                    Ok(path)
                }
                None => {
                    self.session()?
                        .mark_reachable(target_id, TargetStatus::Offline);
                    Err(BackendError::Operation {
                        operation: "connect to target",
                        detail: "the target did not answer a path request for its remote-control destination"
                            .to_string(),
                    })
                }
            }
        }
        .await;
        self.set_path_probe_pending(target_id, false);
        outcome
    }

    fn path_probe_pending(&self, target_id: &str) -> bool {
        self.session()
            .ok()
            .and_then(|session| session.path_probe_pending.lock().ok())
            .is_some_and(|pending| pending.contains(target_id))
    }

    fn set_path_probe_pending(&self, target_id: &str, pending: bool) {
        let Ok(session) = self.session() else {
            return;
        };
        let Ok(mut set) = session.path_probe_pending.lock() else {
            return;
        };
        if pending {
            set.insert(target_id.to_string());
        } else {
            set.remove(target_id);
        }
    }

    pub async fn announce(&self, target_id: &str) -> Result<(), BackendError> {
        let remote = self.connect_target(target_id).await?;
        remote
            .announce_self()
            .await
            .map_err(|error| operation("announce target", error))?;
        remote.close();
        self.session()?
            .mark_reachable(target_id, TargetStatus::Online);
        Ok(())
    }

    pub async fn sleep(&self, target_id: &str) -> Result<(), BackendError> {
        self.set_radio_sleep(target_id, RadioAction::Sleep).await
    }

    pub async fn wake(&self, target_id: &str) -> Result<(), BackendError> {
        self.set_radio_sleep(target_id, RadioAction::Wake).await
    }

    pub async fn reset(&self, target_id: &str) -> Result<(), BackendError> {
        let _ = target_id;
        Err(BackendError::Operation {
            operation: "reset target",
            detail: "device reset is not available on this trunk build yet".to_string(),
        })
    }

    pub fn local_build_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }

    pub fn local_network_transport(&self) -> RemoteControlNetworkTransport {
        self.session()
            .map(|session| session.local_network_transport())
            .unwrap_or(RemoteControlNetworkTransport::Enabled)
    }

    pub fn set_local_network_transport(
        &self,
        transport: RemoteControlNetworkTransport,
    ) -> Result<(), BackendError> {
        let session = self.session()?;
        match session.handle.set_network_transport(transport) {
            RemoteControlNetworkTransportOutcome::Applied => {
                session.set_local_network_transport(transport);
                Ok(())
            }
            RemoteControlNetworkTransportOutcome::Unidentified => Err(BackendError::Operation {
                operation: "set local network transport",
                detail: "this controller is not identified, so transport cannot be changed"
                    .to_string(),
            }),
            RemoteControlNetworkTransportOutcome::Failed => Err(BackendError::Operation {
                operation: "set local network transport",
                detail: "the controller could not apply the requested transport".to_string(),
            }),
        }
    }

    pub async fn describe_network_transport(
        &self,
        target_id: &str,
    ) -> Result<RemoteControlNetworkTransport, BackendError> {
        let remote = self.connect_target(target_id).await?;
        let result = remote.describe_network_transport().await;
        remote.close();
        let (transport, _) =
            result.map_err(|error| operation("describe network transport", error))?;
        self.session()?
            .remember_network_transport(target_id, transport);
        Ok(transport)
    }

    pub async fn set_network_transport(
        &self,
        target_id: &str,
        transport: RemoteControlNetworkTransport,
    ) -> Result<(), BackendError> {
        let remote = self.connect_target(target_id).await?;
        let result = remote.set_network_transport(transport).await;
        remote.close();
        let (outcome, _) = result.map_err(|error| operation("set network transport", error))?;
        match outcome {
            RemoteControlNetworkTransportOutcome::Applied => {
                self.session()?
                    .remember_network_transport(target_id, transport);
                Ok(())
            }
            RemoteControlNetworkTransportOutcome::Unidentified => Err(BackendError::Operation {
                operation: "set network transport",
                detail: "the target is not identified, so transport cannot be changed".to_string(),
            }),
            RemoteControlNetworkTransportOutcome::Failed => Err(BackendError::Operation {
                operation: "set network transport",
                detail: "the target could not apply the requested transport".to_string(),
            }),
        }
    }

    /// Announce this install's roster-sync destination on every started interface.
    pub async fn announce_this_controller(&self) -> Result<(), BackendError> {
        self.announce_roster_destination().await
    }

    fn session(&self) -> Result<&Arc<ControllerSession>, BackendError> {
        self.session.as_ref().map_err(Clone::clone)
    }

    async fn target_path(&self, target_id: &str) -> Option<TargetPath> {
        let session = self.session().ok()?;
        let target = IdentityHash::new(parse_hex::<IDENTITY_HASH_BYTES>(target_id).ok()?);
        let resolved = session
            .handle
            .resolve_remote_control_target(target)
            .await
            .ok()?;
        let route = session
            .handle
            .route(resolved.endpoint().destination_hash())
            .await?;
        let peer_aliases = session
            .peer_aliases
            .lock()
            .ok()
            .map(|aliases| aliases.clone())
            .unwrap_or_default();
        let target_aliases = session
            .target_aliases
            .lock()
            .ok()
            .map(|aliases| aliases.clone())
            .unwrap_or_default();
        self.note_route_peer_for_target(target_id, &route);
        Some(path_from_route(&route, &peer_aliases, &target_aliases))
    }

    fn has_live_direct_ble_peer(&self, target_id: &str) -> bool {
        let Ok(session) = self.session() else {
            return false;
        };
        let Ok(locals) = self.local_interfaces_without_reconcile() else {
            return false;
        };
        locals.iter().any(|entry| {
            entry.kind == "bluetooth-auto"
                && entry.peers.iter().any(|peer| {
                    matches!(peer.connection.as_str(), "Connected" | "Degraded")
                        && match_peer_to_target(session, &peer.id).as_deref() == Some(target_id)
                })
        })
    }

    async fn wait_for_preferred_control_path(
        &self,
        target_id: &str,
        prefer_direct: bool,
    ) -> Option<TargetPath> {
        let mut best = self.target_path(target_id).await;
        if !prefer_direct || path_is_direct_ble(best.as_ref()) {
            return best;
        }
        let deadline = Instant::now() + DIRECT_PATH_GRACE;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let Some(next) = self.target_path(target_id).await else {
                continue;
            };
            if path_is_better_than(&next, best.as_ref()) {
                best = Some(next);
            }
            if path_is_direct_ble(best.as_ref()) {
                break;
            }
        }
        best
    }

    fn note_route_peer_for_target(&self, target_id: &str, route: &RouteSnapshot) {
        if !matches!(route.via, NextHop::Direct) || route.hops > 1 {
            return;
        }
        let Some(kind) = route.interface.kind() else {
            return;
        };
        if !matches!(kind, InterfaceKind::BluetoothPeer | InterfaceKind::WifiPeer) {
            return;
        }
        let peer_id = encode_hex(route.interface.as_bytes());
        let Ok(session) = self.session() else {
            return;
        };
        remember_target_peer_id(session, target_id, &peer_id);
        if kind == InterfaceKind::BluetoothPeer {
            remember_target_ble_prefix(session, target_id, &appearance_prefix(route.interface));
        }
    }

    async fn snapshot_control_announces(
        &self,
    ) -> Result<HashMap<String, InstantMillis>, BackendError> {
        let session = self.session()?;
        let inventory = session
            .handle
            .remote_control_target_inventory()
            .await
            .map_err(|error| operation("read target inventory", error))?;
        let mut baselines = HashMap::new();
        for target in inventory.targets() {
            let id = encode_hex(target.identity_hash().as_bytes());
            if let Some(learned_at) = self.control_announce_learned_at(&id).await? {
                baselines.insert(id, learned_at);
            }
        }
        Ok(baselines)
    }

    async fn wait_for_control_announce(
        &self,
        target_id: &str,
        wait: RemoteControlAnnounceWait,
        baseline: Option<InstantMillis>,
        deadline: Instant,
    ) -> Result<(), BackendError> {
        let _ = self.request_control_path(target_id).await;
        loop {
            let heard = self.control_announce_learned_at(target_id).await?;
            if control_announce_satisfies(heard, wait, baseline) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BackendError::Operation {
                    operation: "wait for remote control announce",
                    detail: "the controller has not heard the target's remote-control destination"
                        .to_string(),
                });
            }
            tokio::time::sleep(CONTROL_ANNOUNCE_POLL).await;
        }
    }

    async fn control_destination(
        &self,
        target_id: &str,
    ) -> Result<Option<DestinationHash>, BackendError> {
        let session = self.session()?;
        let target = IdentityHash::new(
            parse_hex::<IDENTITY_HASH_BYTES>(target_id)
                .map_err(|_| BackendError::InvalidTargetId(target_id.to_string()))?,
        );
        let Ok(resolved) = session.handle.resolve_remote_control_target(target).await else {
            return Ok(None);
        };
        Ok(Some(resolved.endpoint().destination_hash()))
    }

    async fn remember_control_announce_baseline(&self, target_id: &str) {
        let heard = match self.control_announce_learned_at(target_id).await {
            Ok(heard) => heard,
            Err(_) => return,
        };
        let Ok(session) = self.session() else {
            return;
        };
        let mut pairing = session
            .pairing
            .lock()
            .expect("pairing state mutex poisoned");
        match heard {
            Some(learned_at) => {
                pairing
                    .control_announce_baseline
                    .insert(target_id.to_string(), learned_at);
            }
            None => {
                pairing.control_announce_baseline.remove(target_id);
            }
        }
    }

    async fn forget_control_route(&self, target_id: &str) -> Result<(), BackendError> {
        let destination = self.require_control_destination(target_id).await?;
        self.session()?
            .handle
            .drop_route(destination)
            .await
            .map(|_| ())
            .map_err(|error| operation("connect to target", error))
    }

    async fn request_control_path(&self, target_id: &str) -> Result<(), BackendError> {
        self.announce_operator().await?;
        let destination = self.require_control_destination(target_id).await?;
        eprintln!(
            "rnprobe: request_path for {target_id} dest={}",
            encode_hex(destination.as_bytes())
        );
        match self.session()?.handle.request_path(destination).await {
            Ok(found) => {
                eprintln!("rnprobe: path found for {target_id} hops={}", found.hops.0);
                Ok(())
            }
            Err(error) => {
                eprintln!("rnprobe: request_path failed for {target_id}: {error:?}");
                Err(operation("connect to target", error))
            }
        }
    }

    async fn require_control_destination(
        &self,
        target_id: &str,
    ) -> Result<DestinationHash, BackendError> {
        self.control_destination(target_id)
            .await?
            .ok_or_else(|| BackendError::Operation {
                operation: "connect to target",
                detail: "the controller has not stored that target yet".to_string(),
            })
    }

    async fn announce_operator(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        session
            .handle
            .announce_now(AnnounceNow {
                destination: session.operator_destination,
                target: AnnounceTarget::AllInterfaces,
                app_data: AnnounceAppData::Registered,
            })
            .await
            .map_err(|error| operation("announce controller", error))
    }

    async fn control_announce_learned_at(
        &self,
        target_id: &str,
    ) -> Result<Option<InstantMillis>, BackendError> {
        let session = self.session()?;
        let Some(destination) = self.control_destination(target_id).await? else {
            return Ok(None);
        };
        Ok(session
            .handle
            .route(destination)
            .await
            .map(|route| route.learned_at))
    }

    async fn inventory_after_announce(
        &self,
        target_id: &str,
    ) -> Result<Vec<InterfaceEntry>, BackendError> {
        let mut last_error = None;
        for attempt in 0..CONNECT_AFTER_ANNOUNCE_ATTEMPTS {
            match self.inventory_connected_target(target_id).await {
                Ok(entries) => return Ok(entries),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < CONNECT_AFTER_ANNOUNCE_ATTEMPTS {
                        tokio::time::sleep(CONNECT_AFTER_ANNOUNCE_GAP).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| operation("inventory target interfaces", "unknown error")))
    }

    async fn inventory_connected_target(
        &self,
        target_id: &str,
    ) -> Result<Vec<InterfaceEntry>, BackendError> {
        let remote = self.connect_target(target_id).await?;
        eprintln!("inventory interfaces request to {target_id}");
        let (inventory, _) = remote
            .inventory_interfaces()
            .await
            .map_err(|error| operation("inventory target interfaces", error))?;
        self.session()?
            .mark_reachable(target_id, TargetStatus::Online);
        let mut items = inventory
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| operator_interface_kind(entry.kind))
            .map(|(_, entry)| remote_interface_entry(entry, None))
            .collect::<Vec<_>>();
        for item in items.iter_mut() {
            fetch_interface_config(&remote, item).await;
        }
        if let Ok((version, _)) = remote.describe_build().await {
            if let Some(text) = version.as_str().filter(|text| !text.is_empty()) {
                self.session()?
                    .remember_build_version(target_id, text.to_owned());
            }
        }
        if let Ok((snapshot, _)) = remote.describe_power().await {
            self.session()?.mark_battery_fetched(target_id);
            let label = format_managed_node_battery(snapshot).or(Some("unknown".to_string()));
            eprintln!(
                "inventory describe_power {target_id}: applicable={} battery={:?} external={:?} label={label:?}",
                snapshot.is_applicable(),
                snapshot.battery().map(|percent| percent.get()),
                snapshot.external_power(),
            );
            if let Some(text) = label {
                self.session()?.remember_battery(target_id, text);
            }
        } else {
            eprintln!("inventory describe_power {target_id} failed");
        }
        match remote.describe_network_transport().await {
            Ok((transport, _)) => {
                self.session()?
                    .remember_network_transport(target_id, transport);
            }
            Err(error) => {
                eprintln!("inventory describe_network_transport {target_id} failed: {error:?}");
            }
        }
        for item in items.iter_mut() {
            if !item.shows_peers {
                continue;
            }
            let Ok(id) = parse_hex::<INTERFACE_ID_BYTES>(&item.id) else {
                continue;
            };
            match fetch_interface_peers(&remote, InterfaceId::new(id)).await {
                Ok(peers) => {
                    item.peers = peers;
                    item.peers_error = None;
                }
                Err(error) => {
                    eprintln!(
                        "inventory peers for {} on {target_id} failed: {error}",
                        item.kind
                    );
                    item.peers_error = Some(error);
                }
            }
        }
        remote.close();
        let ble_prefix = self.remote_bluetooth_auto_prefix(target_id).await;
        for item in items.iter_mut() {
            apply_bluetooth_auto_identity_title(item, ble_prefix.as_deref());
        }
        self.note_target_interface_identities(target_id, &items);
        let aliases = self
            .session()?
            .peer_aliases
            .lock()
            .expect("peer aliases mutex poisoned")
            .clone();
        for item in items.iter_mut() {
            apply_peer_aliases(&mut item.peers, &aliases);
        }
        self.reconcile_peer_aliases_from_entries(&items);
        // Re-apply after reconcile so newly filled aliases show immediately.
        let aliases = self
            .session()?
            .peer_aliases
            .lock()
            .expect("peer aliases mutex poisoned")
            .clone();
        for item in items.iter_mut() {
            apply_peer_aliases(&mut item.peers, &aliases);
        }
        self.session()?.stamp_activity(target_id, &mut items);
        stamp_interface_arrivals(&mut items);
        Ok(items)
    }

    pub fn refresh_local_interface(
        &self,
        interface_id: &str,
    ) -> Result<InterfaceEntry, BackendError> {
        self.local_interfaces()?
            .into_iter()
            .find(|item| item.id == interface_id)
            .ok_or_else(|| BackendError::InvalidInterfaceId(interface_id.to_string()))
    }

    pub async fn refresh_target_interface(
        &self,
        target_id: &str,
        interface_id: &str,
    ) -> Result<InterfaceEntry, BackendError> {
        let remote = self.connect_target(target_id).await?;
        eprintln!("inventory interface {interface_id} on {target_id}");
        let (inventory, _) = remote
            .inventory_interfaces()
            .await
            .map_err(|error| operation("inventory target interface", error))?;
        self.session()?
            .mark_reachable(target_id, TargetStatus::Online);
        let Some((_, entry)) = inventory.entries().iter().enumerate().find(|(_, entry)| {
            operator_interface_kind(entry.kind) && encode_hex(entry.id.as_bytes()) == interface_id
        }) else {
            remote.close();
            return Err(BackendError::InvalidInterfaceId(interface_id.to_string()));
        };
        let mut item = remote_interface_entry(entry, None);
        fetch_interface_config(&remote, &mut item).await;
        if item.shows_peers {
            let Ok(id) = parse_hex::<INTERFACE_ID_BYTES>(&item.id) else {
                remote.close();
                return Err(BackendError::InvalidInterfaceId(interface_id.to_string()));
            };
            match fetch_interface_peers(&remote, InterfaceId::new(id)).await {
                Ok(peers) => {
                    item.peers = peers;
                    item.peers_error = None;
                }
                Err(error) => {
                    eprintln!(
                        "inventory peers for {} on {target_id} failed: {error}",
                        item.kind
                    );
                    item.peers_error = Some(error);
                }
            }
        }
        remote.close();
        let ble_prefix = self.remote_bluetooth_auto_prefix(target_id).await;
        apply_bluetooth_auto_identity_title(&mut item, ble_prefix.as_deref());
        self.note_target_interface_identities(target_id, std::slice::from_ref(&item));
        self.reconcile_peer_aliases_from_entries(std::slice::from_ref(&item));
        let aliases = self
            .session()?
            .peer_aliases
            .lock()
            .expect("peer aliases mutex poisoned")
            .clone();
        apply_peer_aliases(&mut item.peers, &aliases);
        self.session()?
            .stamp_activity(target_id, std::slice::from_mut(&mut item));
        stamp_interface_arrival(&mut item);
        Ok(item)
    }

    fn note_target_interface_identities(&self, target_id: &str, items: &[InterfaceEntry]) {
        let Ok(session) = self.session() else {
            return;
        };
        for item in items {
            if item.kind == "bluetooth-auto" {
                if let Some(prefix) = bluetooth_auto_name_prefix(&item.name) {
                    remember_target_ble_prefix(session, target_id, &prefix);
                }
            }
            if item.kind == "auto-wifi" {
                remember_wifi_peers_from_addresses(session, target_id, &item.extras);
            }
            for peer in &item.peers {
                if let Some(matched) = match_peer_to_target(session, &peer.id) {
                    remember_target_peer_id(session, &matched, &peer.id);
                } else if let Some(endpoint) = peer.endpoint.as_deref() {
                    if let Some(matched) = match_wifi_ll_to_target(session, endpoint) {
                        remember_target_peer_id(session, &matched, &peer.id);
                    }
                }
            }
        }
    }

    fn reconcile_peer_aliases_from_entries(&self, items: &[InterfaceEntry]) {
        let Ok(session) = self.session() else {
            return;
        };
        self.ensure_local_sibling_wifi_ll_published(session);
        let controller_ll = session.local_power.wifi_link_local_keys();
        let sibling_aliases = session
            .sibling_aliases
            .lock()
            .expect("sibling aliases mutex poisoned")
            .clone();
        for item in items {
            for peer in &item.peers {
                if let Some(endpoint) = peer.endpoint.as_deref() {
                    if endpoint_matches_wifi_ll_keys(endpoint, &controller_ll) {
                        // Local-only label: do not publish to roster (sibling "this" would be wrong).
                        let _ = auto_fill_this_controller_peer_alias(session, &peer.id);
                    } else if let Some(sibling_hash) = match_wifi_ll_to_sibling(session, endpoint) {
                        if let Some(alias) =
                            stored_alias(sibling_aliases.get(&sibling_hash).map(String::as_str))
                        {
                            // Local-only: each install derives this from SiblingWifiLl + SiblingAlias.
                            let _ = auto_fill_sibling_controller_peer_alias(
                                session,
                                &peer.id,
                                &sibling_hash,
                                &alias,
                            );
                        }
                    } else if let Some(matched) = match_wifi_ll_to_target(session, endpoint) {
                        remember_target_peer_id(session, &matched, &peer.id);
                    }
                }
            }
        }
        let peer_ids = items
            .iter()
            .flat_map(|item| item.peers.iter().map(|peer| peer.id.as_str()))
            .collect::<Vec<_>>();
        let _ = self.reconcile_peer_aliases(&peer_ids);
    }

    fn ensure_local_sibling_wifi_ll_published(&self, session: &ControllerSession) {
        let instance = session.controller_identity.instance_hash.trim();
        if instance.is_empty() {
            return;
        }
        let Some(ll) = session.local_power.wifi_link_local_keys().into_iter().min() else {
            // No local LL yet — absence on the roster is valid; do not clear a prior fact.
            return;
        };
        let current = session.roster.lock().ok().and_then(|roster| {
            roster.replica.labels.iter().find_map(|label| {
                (label.kind == RosterLabelKind::SiblingWifiLl
                    && label.key.eq_ignore_ascii_case(instance))
                .then(|| label.value.clone())
                .flatten()
            })
        });
        if current.as_deref() == Some(ll.as_str()) {
            return;
        }
        self.record_local_label(RosterLabelKind::SiblingWifiLl, instance, Some(&ll));
    }

    fn reconcile_peer_aliases(&self, peer_ids: &[&str]) -> Result<(), BackendError> {
        let session = self.session()?;
        let target_aliases = session
            .target_aliases
            .lock()
            .expect("target aliases mutex poisoned")
            .clone();
        for peer_id in peer_ids {
            let Some(target_id) = match_peer_to_target(session, peer_id) else {
                continue;
            };
            let Some(alias) = stored_alias(target_aliases.get(&target_id).map(String::as_str))
            else {
                continue;
            };
            auto_fill_peer_alias(Some(self), session, peer_id, &target_id, &alias)?;
        }
        Ok(())
    }

    fn propagate_target_alias_to_peers(
        &self,
        target_id: &str,
        alias: Option<&str>,
        previous: Option<&str>,
    ) -> Result<(), BackendError> {
        let session = self.session()?;
        let mut extra = Vec::new();
        if let Ok(locals) = self.local_interfaces_without_reconcile() {
            for item in &locals {
                for peer in &item.peers {
                    if match_peer_to_target(session, &peer.id).as_deref() == Some(target_id) {
                        extra.push(peer.id.clone());
                    }
                }
            }
        }
        project_target_alias_to_peers(session, target_id, alias, previous, &extra, Some(self))
    }

    /// Local inventory without running peer-alias reconcile (avoids recursion).
    fn local_interfaces_without_reconcile(&self) -> Result<Vec<InterfaceEntry>, BackendError> {
        let session = self.session()?;
        let tcp_target = session
            .tcp_dial
            .lock()
            .expect("tcp client target mutex poisoned")
            .clone();
        let aliases = session
            .peer_aliases
            .lock()
            .expect("peer aliases mutex poisoned")
            .clone();
        let raw = session.handle.interface_inventory();
        let members = raw.clone();
        let mut items: Vec<InterfaceEntry> = logical_interface_inventory(raw)
            .into_iter()
            .filter(operator_local_interface)
            .map(|entry| {
                let mut peers: Vec<InterfacePeer> = members
                    .iter()
                    .filter_map(|member| match member.snapshot.membership {
                        Membership::FleetMember { supervisor_id }
                            if supervisor_id == entry.snapshot.id =>
                        {
                            Some(interface_peer(&member.snapshot, member.name.as_deref()))
                        }
                        Membership::Independent | Membership::FleetMember { .. } => None,
                    })
                    .collect();
                apply_peer_aliases(&mut peers, &aliases);
                local_interface_entry(
                    &entry.snapshot,
                    entry.name.as_deref(),
                    Some(session.ble_identity),
                    session.local_power.live_group(entry.snapshot.id).as_deref(),
                    entry.ifac.as_ref().map(|ifac| ifac.size.bytes()),
                    local_interface_config(
                        &entry.snapshot,
                        entry.ifac.as_ref().map(|ifac| ifac.size.bytes()),
                        Some(tcp_target.as_str()),
                    )
                    .as_deref(),
                    entry.snapshot.failure_reason,
                    local_frame_drops(&entry.frame_accounting),
                    peers,
                )
            })
            .collect();
        for item in &mut items {
            if item.kind == "bluetooth-auto" && generic_bluetooth_auto_title(&item.name) {
                item.name = bluetooth_auto_title(session.ble_identity);
            }
            if item.kind == "tcp-client" {
                item.name = "TCP client".to_string();
            }
            attach_usb_link_peer(item);
            if let Ok(id) = parse_hex::<INTERFACE_ID_BYTES>(&item.id) {
                let addresses = session
                    .local_power
                    .wifi_local_addresses(InterfaceId::new(id));
                attach_auto_wifi_local_addresses(item, &addresses);
                attach_peer_our_side_addresses(item, &addresses);
            }
        }
        Ok(items)
    }

    async fn remote_bluetooth_auto_prefix(&self, target_id: &str) -> Option<String> {
        let session = self.session().ok()?;
        let target = IdentityHash::new(parse_hex::<IDENTITY_HASH_BYTES>(target_id).ok()?);
        let resolved = session
            .handle
            .resolve_remote_control_target(target)
            .await
            .ok()?;
        let route = session
            .handle
            .route(resolved.endpoint().destination_hash())
            .await?;
        bluetooth_auto_prefix_from_direct_peer(route.hops, route.via, route.interface)
    }

    async fn connect_target(
        &self,
        target_id: &str,
    ) -> Result<RemoteControlTargetHandle<'_>, BackendError> {
        if !self.is_monitoring_target(target_id) {
            return Err(not_monitoring(target_id));
        }
        let target = IdentityHash::new(
            parse_hex::<IDENTITY_HASH_BYTES>(target_id)
                .map_err(|_| BackendError::InvalidTargetId(target_id.to_string()))?,
        );
        let path = self.target_path(target_id).await;
        let route = format_target_route(path.as_ref());
        let session = self.session()?;
        let dest_hex = match session.handle.resolve_remote_control_target(target).await {
            Ok(resolved) => encode_hex(resolved.endpoint().destination_hash().as_bytes()),
            Err(_) => "unresolved".to_string(),
        };
        let name = session
            .pairing
            .lock()
            .ok()
            .and_then(|pairing| pairing.paired_announce_name(target_id))
            .unwrap_or_else(|| target_label(target_id));
        eprintln!(
            "connect to target {target_id} ({name}) dest={dest_hex} via {route} — establishing link"
        );
        match session.handle.connect_remote_control_target(target).await {
            Ok(remote) => {
                eprintln!("connect to target {target_id} ({name}) dest={dest_hex} ok via {route}");
                Ok(remote)
            }
            Err(error) => {
                eprintln!(
                    "connect to target {target_id} ({name}) dest={dest_hex} failed via {route}: {error:?}"
                );
                session.mark_reachable(target_id, TargetStatus::Offline);
                Err(operation("connect to target", error))
            }
        }
    }

    async fn set_radio_sleep(
        &self,
        target_id: &str,
        action: RadioAction,
    ) -> Result<(), BackendError> {
        let remote = self.connect_target(target_id).await?;
        let result = match action {
            RadioAction::Sleep => remote.sleep_radios().await,
            RadioAction::Wake => remote.wake_radios().await,
        }
        .map_err(|error| operation(action.operation(), error))?;
        remote.close();
        match result.0 {
            RemoteControlSleepOutcome::Applied => {
                self.session()?.mark_reachable(
                    target_id,
                    match action {
                        RadioAction::Sleep => TargetStatus::Sleeping,
                        RadioAction::Wake => TargetStatus::Online,
                    },
                );
                Ok(())
            }
            RemoteControlSleepOutcome::Unavailable => Err(BackendError::Operation {
                operation: action.operation(),
                detail: "the target does not expose radio sleep control".to_string(),
            }),
            RemoteControlSleepOutcome::Failed => Err(BackendError::Operation {
                operation: action.operation(),
                detail: "the target could not apply the requested radio state".to_string(),
            }),
        }
    }

    pub async fn maintain_roster(&self) {
        let Ok(session) = self.session() else {
            return;
        };
        if let Ok(snapshot) = session
            .handle
            .snapshot_remote_control_target_accesses()
            .await
        {
            if let Ok(mut roster) = session.roster.lock() {
                roster.cached_snapshot = snapshot.unwrap_or_default();
            }
        }
        self.apply_pending_roster().await;
        let cloning = session
            .clone
            .lock()
            .ok()
            .is_some_and(|shared| shared.in_progress());
        if cloning {
            return;
        }
        let has_siblings = session
            .roster
            .lock()
            .ok()
            .is_some_and(|roster| !roster.replica.siblings.is_empty());
        if !has_siblings {
            return;
        }
        let due = session
            .last_roster_sync
            .lock()
            .ok()
            .is_none_or(|last| last.is_none_or(|at| at.elapsed() >= ROSTER_ANNOUNCE_GAP));
        if due {
            if let Ok(mut last) = session.last_roster_sync.lock() {
                *last = Some(Instant::now());
            }
            let _ = self.announce_roster_destination().await;
        }
        let pull_due = session
            .roster
            .lock()
            .ok()
            .map(|mut roster| roster.take_pull_due())
            .unwrap_or_default();
        if !pull_due.is_empty() {
            let _ = self
                .exchange_roster_with_siblings(RosterExchange::Pull, Some(&pull_due), true)
                .await;
            self.apply_pending_roster().await;
            return;
        }
        if !due {
            return;
        }
        let unheard = session
            .roster
            .lock()
            .ok()
            .map(|roster| {
                roster
                    .replica
                    .siblings
                    .iter()
                    .map(|sibling| sibling.identity_hash())
                    .filter(|hash| !roster.heard_sibling(*hash))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !unheard.is_empty() {
            let _ = self
                .exchange_roster_with_siblings(RosterExchange::Pull, Some(&unheard), false)
                .await;
            self.apply_pending_roster().await;
        }
    }

    async fn apply_pending_roster(&self) {
        let Ok(session) = self.session() else {
            return;
        };
        let pending = session
            .roster
            .lock()
            .ok()
            .and_then(|mut roster| roster.pending.take());
        let Some(delta) = pending else {
            return;
        };
        let plan = {
            let Ok(mut roster) = session.roster.lock() else {
                return;
            };
            let (merged, plan) = merge_roster(&roster.replica, &delta);
            roster.replica = merged;
            roster.applied_generation = roster.applied_generation.saturating_add(1);
            plan
        };
        persist_session_replica(session);
        for access in plan.upserts {
            let _ = session
                .handle
                .set_remote_control_target_access(access)
                .await;
        }
        for hash in plan.forgets {
            let id = encode_hex(hash.as_bytes());
            let _ = session.handle.forget_remote_control_target(hash).await;
            session
                .pairing
                .lock()
                .expect("pairing state mutex poisoned")
                .forget_stored(&id);
            session.forget_build_version(&id);
            session.forget_battery(&id);
            session.forget_network_transport(&id);
            remove_alias_map_key(&session.target_aliases, &session.target_aliases_path, &id);
        }
        apply_roster_labels(session, &plan.labels);
    }

    async fn push_roster_to_siblings(&self) -> Result<(), BackendError> {
        let cloning = self
            .session()?
            .clone
            .lock()
            .ok()
            .is_some_and(|shared| shared.in_progress());
        if cloning {
            return Ok(());
        }
        let _ = self.announce_roster_destination().await;
        self.exchange_roster_with_siblings(RosterExchange::Push, None, true)
            .await
    }

    async fn announce_roster_destination(&self) -> Result<(), BackendError> {
        let session = self.session()?;
        let instance_hash = IdentityHash::new(
            parse_hex::<IDENTITY_HASH_BYTES>(&session.controller_identity.instance_hash)
                .unwrap_or([0u8; IDENTITY_HASH_BYTES]),
        );
        let Some(destination) = roster_sync_destination_hash(instance_hash) else {
            return Ok(());
        };
        session
            .handle
            .announce_now(AnnounceNow {
                destination,
                target: AnnounceTarget::AllInterfaces,
                app_data: AnnounceAppData::Registered,
            })
            .await
            .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
        Ok(())
    }

    async fn exchange_roster_with_siblings(
        &self,
        exchange: RosterExchange,
        only: Option<&[IdentityHash]>,
        require_heard: bool,
    ) -> Result<(), BackendError> {
        let session = self.session()?;
        let (siblings, instance, replica, snapshot) = {
            let roster = session
                .roster
                .lock()
                .map_err(|_| BackendError::Clone("roster lock was poisoned".to_string()))?;
            let instance = roster
                .instance_secret
                .as_ref()
                .map(|secret| personal_rns::identity::PrivateIdentityMaterial::from_bytes(**secret))
                .ok_or_else(|| BackendError::Clone("instance secret is missing".to_string()))?;
            let siblings = roster
                .replica
                .siblings
                .iter()
                .copied()
                .filter(|sibling| !require_heard || roster.heard_sibling(sibling.identity_hash()))
                .filter(|sibling| {
                    only.is_none_or(|wanted| wanted.contains(&sibling.identity_hash()))
                })
                .collect::<Vec<_>>();
            (
                siblings,
                instance,
                roster.replica.clone(),
                roster.cached_snapshot.clone(),
            )
        };
        if siblings.is_empty() {
            return Ok(());
        }
        let request = match exchange {
            RosterExchange::Pull => {
                let mut request = vec![0u8; 1 + IDENTITY_PUBLIC_KEY_LEN + 64];
                let written = write_pull(&instance, &mut request).ok_or_else(|| {
                    BackendError::Clone("roster message is too large".to_string())
                })?;
                request.truncate(written);
                request
            }
            RosterExchange::Push => replica_message(&instance, &replica, &snapshot)
                .ok_or_else(|| BackendError::Clone("roster message is too large".to_string()))?,
        };
        for sibling in siblings {
            let Some(destination) = roster_sync_destination_hash(sibling.identity_hash()) else {
                continue;
            };
            let Ok(link_id) = session.handle.establish_link(destination).await else {
                continue;
            };
            let Ok((reply, _)) = session
                .handle
                .request(
                    link_id,
                    RequestEndpointId::of(ROSTER_SYNC_REQUEST_ENDPOINT_ID),
                    &request,
                )
                .await
            else {
                continue;
            };
            if let Some(delta) = parse_replica_reply(&reply) {
                if let Ok(mut roster) = session.roster.lock() {
                    roster.pending = Some(delta);
                }
            }
        }
        Ok(())
    }
}

enum RosterExchange {
    Pull,
    Push,
}

struct ControllerSession {
    handle: PrnsNodeHandle,
    pairing: Arc<Mutex<PairingEvents>>,
    local_power: Arc<LocalInterfacePower>,
    tcp_dial: Arc<Mutex<String>>,
    tcp_target_path: PathBuf,
    tcp_id: InterfaceId,
    controller_identity: ControllerIdentity,
    operator_destination: DestinationHash,
    activity: Mutex<HashMap<String, InterfaceActivityStamp>>,
    peer_aliases: Mutex<HashMap<String, String>>,
    peer_aliases_path: PathBuf,
    peer_alias_links: Mutex<HashMap<String, String>>,
    peer_alias_links_path: PathBuf,
    target_ble_prefixes: Mutex<HashMap<String, String>>,
    target_ble_prefixes_path: PathBuf,
    target_peer_ids: Mutex<HashMap<String, String>>,
    target_peer_ids_path: PathBuf,
    /// Normalized station LL (`fe80::…`, no zone) → managed target id.
    target_wifi_ll: Mutex<HashMap<String, String>>,
    manager_aliases: Mutex<HashMap<String, String>>,
    manager_aliases_path: PathBuf,
    target_aliases: Mutex<HashMap<String, String>>,
    target_aliases_path: PathBuf,
    sibling_aliases: Mutex<HashMap<String, String>>,
    sibling_aliases_path: PathBuf,
    /// Sibling instance hash → normalized wifi LL (`fe80::…`, no zone).
    sibling_wifi_ll: Mutex<HashMap<String, String>>,
    sibling_wifi_ll_path: PathBuf,
    build_versions: Mutex<HashMap<String, String>>,
    batteries: Mutex<HashMap<String, String>>,
    battery_fetched_at: Mutex<HashMap<String, Instant>>,
    network_transports: Mutex<HashMap<String, RemoteControlNetworkTransport>>,
    local_transport: Mutex<RemoteControlNetworkTransport>,
    clone: Arc<Mutex<IdentityCloneShared>>,
    roster: Arc<Mutex<RosterShared>>,
    ble_identity: BleIdentity,
    identities_dir: PathBuf,
    persist_dir: PathBuf,
    data_dir: PathBuf,
    last_roster_sync: Mutex<Option<Instant>>,
    monitor_until: Mutex<HashMap<String, Instant>>,
    /// Targets whose Connect probe has dropped the hop and is waiting for a
    /// fresh path — hide stale announce/route in the Managed Nodes header.
    path_probe_pending: Mutex<HashSet<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterfaceActivitySignature {
    connection: u8,
    tx_bytes: u64,
    rx_bytes: u64,
    links: u32,
    destinations: u32,
    rate_bytes_per_sec: u32,
    radio: RadioIndication,
    details: PeerDetails,
}

struct InterfaceActivityStamp {
    signature: InterfaceActivitySignature,
    last_at: Option<Instant>,
}

struct LocalInterfacePower {
    controls: Mutex<HashMap<InterfaceId, LocalPowerControl>>,
}

enum LocalPowerControl {
    Tokio(TokioInterfaceStatus),
    Wifi(AutoWifiStatus),
    Ble(BluetoothAutoStatus),
}

impl LocalInterfacePower {
    fn new() -> Self {
        Self {
            controls: Mutex::new(HashMap::new()),
        }
    }

    fn register(&self, control: LocalPowerControl) {
        let id = match &control {
            LocalPowerControl::Tokio(status) => status.id(),
            LocalPowerControl::Wifi(status) => status.id(),
            LocalPowerControl::Ble(status) => status.id(),
        };
        self.controls
            .lock()
            .expect("local interface power mutex poisoned")
            .insert(id, control);
    }

    fn set(&self, id: InterfaceId, power: InterfacePower) -> Result<(), BackendError> {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        let Some(control) = controls.get(&id) else {
            return Err(BackendError::Operation {
                operation: "set controller interface power",
                detail: "this controller does not own that interface".to_string(),
            });
        };
        match (control, power) {
            (LocalPowerControl::Tokio(status), InterfacePower::On) => status.enable(),
            (LocalPowerControl::Tokio(status), InterfacePower::Off) => status.disable(),
            (LocalPowerControl::Wifi(status), InterfacePower::On) => status.enable(),
            (LocalPowerControl::Wifi(status), InterfacePower::Off) => status.disable(),
            (LocalPowerControl::Ble(status), InterfacePower::On) => status.enable(),
            (LocalPowerControl::Ble(status), InterfacePower::Off) => status.disable(),
        }
        Ok(())
    }

    fn bounce_tokio_if_enabled(&self, id: InterfaceId) -> Result<(), BackendError> {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        let Some(control) = controls.get(&id) else {
            return Err(BackendError::Operation {
                operation: "set controller TCP target",
                detail: "this controller does not own that interface".to_string(),
            });
        };
        if let LocalPowerControl::Tokio(status) = control {
            if status.is_enabled() {
                status.disable();
                status.enable();
            }
        }
        Ok(())
    }

    fn wifi_local_addresses(&self, id: InterfaceId) -> Vec<(IpAddr, u32)> {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        match controls.get(&id) {
            Some(LocalPowerControl::Wifi(status)) => status.local_addresses(),
            Some(LocalPowerControl::Tokio(_) | LocalPowerControl::Ble(_)) | None => Vec::new(),
        }
    }

    fn wifi_link_local_keys(&self) -> HashSet<String> {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        let mut keys = HashSet::new();
        for control in controls.values() {
            let LocalPowerControl::Wifi(status) = control else {
                continue;
            };
            for (addr, _) in status.local_addresses() {
                if let Some(key) = normalize_link_local(addr) {
                    keys.insert(key);
                }
            }
        }
        keys
    }

    fn live_group(&self, id: InterfaceId) -> Option<String> {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        match controls.get(&id) {
            Some(LocalPowerControl::Wifi(status)) => status
                .discovery_groups()
                .iter()
                .next()
                .map(|group| group.as_str().to_string()),
            Some(LocalPowerControl::Ble(status)) => status
                .discovery_groups()
                .iter()
                .next()
                .map(|group| group.as_str().to_string()),
            Some(LocalPowerControl::Tokio(_)) | None => None,
        }
    }

    fn set_group(
        &self,
        id: InterfaceId,
        group: RemoteControlInterfaceGroup,
    ) -> RemoteControlGroupOutcome {
        let controls = self
            .controls
            .lock()
            .expect("local interface power mutex poisoned");
        let Some(control) = controls.get(&id) else {
            return RemoteControlGroupOutcome::UnknownInterface;
        };
        let Ok(groups) = DiscoveryGroupSet::singleton(group.into_discovery_group()) else {
            return RemoteControlGroupOutcome::Failed;
        };
        match control {
            LocalPowerControl::Wifi(status) => {
                if status.set_group_id(group.into_discovery_group().as_bytes()) {
                    RemoteControlGroupOutcome::Applied
                } else {
                    RemoteControlGroupOutcome::Failed
                }
            }
            LocalPowerControl::Ble(status) => {
                let status = status.clone();
                spawn(async move {
                    let _ = status.replace_discovery_groups(groups).await;
                });
                RemoteControlGroupOutcome::Applied
            }
            LocalPowerControl::Tokio(_) => RemoteControlGroupOutcome::Failed,
        }
    }
}

impl InterfaceActivitySignature {
    fn observed_active(self) -> bool {
        matches!(self.connection, 1 | 2) || self.links > 0 || self.rate_bytes_per_sec > 0
    }
}

impl ControllerSession {
    fn start() -> Result<Self, BackendError> {
        let data_dir = controller_data_dir();
        std::fs::create_dir_all(&data_dir)
            .map_err(|error| BackendError::Startup(error.to_string()))?;
        let tcp_target_path = data_dir.join(TCP_TARGET_FILE);
        let env_tcp = std::env::var("HOPSPOT_RC_TCP")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let (tcp_target, start_tcp) = resolve_controller_tcp_target(
            env_tcp.as_deref(),
            load_persisted_tcp_target(&tcp_target_path),
        )
        .map_err(BackendError::Startup)?;
        if env_tcp.is_some() {
            persist_tcp_target(&tcp_target_path, &tcp_target);
        }
        let auto_wifi = env_enabled("HOPSPOT_RC_AUTO_WIFI");
        let start_ble = !env_started_off("HOPSPOT_RC_BLE");
        #[cfg(any(
            target_os = "android",
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        ))]
        let start_usb = env_enabled("HOPSPOT_RC_USB");
        let local_power = Arc::new(LocalInterfacePower::new());
        let attach_power = local_power.clone();
        let attach_tcp = tcp_target.clone();
        let tcp_id = InterfaceId::from_channel_tag(InterfaceKind::TcpClient, attach_tcp.as_bytes());
        let tcp_dial_slot = Arc::new(Mutex::new(None));
        let attach_dial_slot = tcp_dial_slot.clone();
        let identities_dir = data_dir.join("identities");
        let ble_identity = load_or_create_ble_identity(&identities_dir.join("ble"))
            .map_err(|error| BackendError::Startup(error.to_string()))?;
        let (identity_secrets, _) = RemoteControlIdentityDirectory::new(&identities_dir)
            .load_or_generate()
            .map_err(|error| BackendError::Startup(error.to_string()))?
            .into_parts();
        let identities = identity_secrets.identities();
        let controller = identities.controller();
        let controller_secret = load_controller_secret(&identities_dir)?;
        let instance_secret = load_or_create_instance_secret(&identities_dir, &controller_secret)?;
        let instance_material =
            personal_rns::identity::PrivateIdentityMaterial::from_bytes(*instance_secret);
        let controller_identity = ControllerIdentity {
            operator_hash: encode_hex(controller.identity_hash().as_bytes()),
            allow_list_key: encode_hex(&controller.public_keys().public_key_bytes()),
            operator_secret_path: controller_identity_secret_path(&identities_dir)
                .display()
                .to_string(),
            instance_hash: encode_hex(instance_material.identity_hash().as_bytes()),
            instance_secret_path: instance_identity_secret_path(&identities_dir)
                .display()
                .to_string(),
        };
        let remote_control = RemoteControlService::new(
            identity_secrets,
            RemoteControlInitialControllerGrants::Nobody,
            RemoteControlSelfAnnouncement::Unavailable,
        );
        let pairing = Arc::new(Mutex::new(PairingEvents::load(
            data_dir.join(TARGET_NAMES_FILE),
        )));
        let clone = Arc::new(Mutex::new(IdentityCloneShared::default()));
        let clone_events = clone.clone();
        let persist_dir = data_dir.join("node");
        let peer_aliases_path = data_dir.join(PEER_ALIASES_FILE);
        let mut peer_aliases_map = load_target_names(&peer_aliases_path);
        let peer_alias_links_path = data_dir.join(PEER_ALIAS_LINKS_FILE);
        let peer_alias_links_map = load_target_names(&peer_alias_links_path);
        let target_ble_prefixes_path = data_dir.join(TARGET_BLE_PREFIXES_FILE);
        let target_ble_prefixes_map = load_target_names(&target_ble_prefixes_path);
        let target_peer_ids_path = data_dir.join(TARGET_PEER_IDS_FILE);
        let target_peer_ids_map = load_target_names(&target_peer_ids_path);
        let manager_aliases_path = data_dir.join(MANAGER_ALIASES_FILE);
        let manager_aliases_map = load_target_names(&manager_aliases_path);
        let target_aliases_path = data_dir.join(TARGET_ALIASES_FILE);
        let target_aliases_map = load_target_names(&target_aliases_path);
        let sibling_aliases_path = data_dir.join(SIBLING_ALIASES_FILE);
        let mut sibling_aliases_map = load_target_names(&sibling_aliases_path);
        let sibling_wifi_ll_path = data_dir.join(SIBLING_WIFI_LL_FILE);
        let mut sibling_wifi_ll_map = load_target_names(&sibling_wifi_ll_path);
        let target_names = pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .target_names
            .clone();
        let mut roster_state = RosterShared {
            replica: load_replica(&replica_path(&data_dir)),
            instance_secret: Some(personal_rns::identity::Zeroizing::new(*instance_secret)),
            ..RosterShared::default()
        };
        let instance_hex = encode_hex(instance_material.identity_hash().as_bytes());
        roster_state
            .replica
            .siblings
            .retain(|sibling| sibling.identity_hash() != instance_material.identity_hash());
        strip_local_sibling_alias(&mut roster_state.replica, &instance_hex);
        sibling_aliases_map.retain(|key, _| sibling_alias_is_syncable(key, &instance_hex));
        persist_target_names(&sibling_aliases_path, &sibling_aliases_map);
        sibling_wifi_ll_map.retain(|key, value| {
            sibling_alias_is_syncable(key, &instance_hex)
                && normalize_stored_wifi_ll(value).is_some()
        });
        for value in sibling_wifi_ll_map.values_mut() {
            if let Some(normalized) = normalize_stored_wifi_ll(value) {
                *value = normalized;
            }
        }
        persist_target_names(&sibling_wifi_ll_path, &sibling_wifi_ll_map);
        // Drop sibling-synced "This Controller" leftovers; keep local __this_controller__ rows.
        let before_scrub = peer_aliases_map.len();
        peer_aliases_map.retain(|peer_id, alias| {
            if peer_alias_value_is_syncable(Some(alias)) {
                return true;
            }
            peer_alias_links_map
                .get(peer_id)
                .is_some_and(|link| link == THIS_CONTROLLER_ALIAS_LINK)
        });
        if peer_aliases_map.len() != before_scrub {
            persist_target_names(&peer_aliases_path, &peer_aliases_map);
        }
        // Local-only auto peer aliases live in peer-aliases for UI, but must not seed the roster.
        let syncable_peer_aliases = peer_aliases_map
            .iter()
            .filter(|(peer_id, alias)| {
                peer_alias_is_syncable(peer_id)
                    && peer_alias_value_is_syncable(Some(alias))
                    && peer_alias_links_map
                        .get(peer_id.as_str())
                        .map(|link| {
                            !peer_alias_link_is_local_only(link, sibling_aliases_map.keys())
                        })
                        .unwrap_or(true)
            })
            .map(|(peer_id, alias)| (peer_id.clone(), alias.clone()))
            .collect::<HashMap<_, _>>();
        // Retract any prior mistaken "This Controller" PeerAlias before seeding other facts.
        retract_unsyncable_peer_alias_values(&mut roster_state.replica);
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::TargetName,
            &target_names,
        );
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::TargetAlias,
            &target_aliases_map,
        );
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::ManagerAlias,
            &manager_aliases_map,
        );
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::PeerAlias,
            &syncable_peer_aliases,
        );
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::SiblingAlias,
            &sibling_aliases_map,
        );
        import_seed_labels(
            &mut roster_state.replica,
            RosterLabelKind::SiblingWifiLl,
            &sibling_wifi_ll_map,
        );
        persist_replica(&replica_path(&data_dir), &roster_state.replica);
        let roster = Arc::new(Mutex::new(roster_state));
        let roster_events = roster.clone();
        let app_state = ControllerAppState {
            clone: clone.clone(),
            roster: roster.clone(),
        };
        let peer_aliases = Mutex::new(peer_aliases_map);
        let peer_alias_links = Mutex::new(peer_alias_links_map);
        let target_ble_prefixes = Mutex::new(target_ble_prefixes_map);
        let target_peer_ids = Mutex::new(target_peer_ids_map);
        let manager_aliases = Mutex::new(manager_aliases_map);
        let target_aliases = Mutex::new(target_aliases_map);
        let sibling_aliases = Mutex::new(sibling_aliases_map);
        let sibling_wifi_ll = Mutex::new(sibling_wifi_ll_map);
        let pairing_events = pairing.clone();
        let node = PrnsNode::new(PrnsNodeRecipe {
            transport_identity: None,
            remote_control,
            pre_configured_destinations: [
                PreConfiguredDestination::Single {
                    app_name: IDENTITY_CLONE_APP_NAME,
                    aspects: IDENTITY_CLONE_ASPECTS,
                    identity: personal_rns::identity::Zeroizing::new(*instance_secret),
                    announce_app_data: b"",
                    proof: ProofStrategy::ProveNone,
                    link_requests: LinkRequestPolicy::AcceptDirect,
                    ratchet: RatchetPolicy::NoRatchets,
                    resource_strategy: ResourceStrategy::AcceptNone,
                    maximum_request_bytes: Default::default(),
                    request_endpoints: ServeMyRequestEndpoints::Yes,
                },
                PreConfiguredDestination::Single {
                    app_name: ROSTER_SYNC_APP_NAME,
                    aspects: ROSTER_SYNC_ASPECTS,
                    identity: personal_rns::identity::Zeroizing::new(*instance_secret),
                    announce_app_data: b"",
                    proof: ProofStrategy::ProveNone,
                    link_requests: LinkRequestPolicy::AcceptAll,
                    ratchet: RatchetPolicy::NoRatchets,
                    resource_strategy: ResourceStrategy::AcceptNone,
                    maximum_request_bytes: Default::default(),
                    request_endpoints: ServeMyRequestEndpoints::Yes,
                },
                PreConfiguredDestination::Single {
                    app_name: REMOTE_CONTROL_APPLICATION_NAME,
                    aspects: REMOTE_CONTROL_APPLICATION_ASPECTS,
                    identity: controller_secret.clone(),
                    announce_app_data: b"",
                    proof: ProofStrategy::ProveAll,
                    link_requests: LinkRequestPolicy::AcceptNone,
                    ratchet: RatchetPolicy::NoRatchets,
                    resource_strategy: ResourceStrategy::AcceptNone,
                    maximum_request_bytes: Default::default(),
                    request_endpoints: ServeMyRequestEndpoints::No,
                },
            ],
            app_state,
            storage: GrowableHeap,
            request_endpoints: request_endpoints![IdentityClone, RosterSync],
            on_event: move |event, _state| match event {
                PrnsEvent::Diagnostic(Diagnostic::AnnounceHeard {
                    destination,
                    hops,
                    source_interface,
                    app_data,
                }) => {
                    if let Ok(mut shared) = roster_events.lock() {
                        for sibling in &shared.replica.siblings.clone() {
                            if Some(destination)
                                == roster_sync_destination_hash(sibling.identity_hash())
                            {
                                shared.note_heard_sibling(sibling.identity_hash());
                            }
                        }
                    }
                    if !clone_announce_is_usb_local(hops, source_interface) {
                        return;
                    }
                    let Some(announce) = IdentityCloneAnnounce::parse(app_data) else {
                        return;
                    };
                    let Some(expected) = identity_clone_destination_hash(announce.source()) else {
                        return;
                    };
                    if expected != destination {
                        return;
                    }
                    let Ok(mut shared) = clone_events.lock() else {
                        return;
                    };
                    if !shared.dest_waiting {
                        return;
                    }
                    shared.dest = Some(DestCloneSession {
                        announce,
                        destination,
                        dest_nonce: None,
                        transcript: None,
                        accepted: false,
                        hello_in_flight: false,
                    });
                    shared.error = None;
                    shared.notice = Some(
                        "Heard a USB sibling adoption. Confirm the six-digit adoption code, then Approve."
                            .to_string(),
                    );
                }
                PrnsEvent::Message(Message::RemoteControlPairingAvailable(observation)) => {
                    let id = encode_hex(observation.endpoint().destination_hash().as_bytes());
                    let label = String::from_utf8_lossy(observation.public_app_data().as_bytes())
                        .trim()
                        .to_owned();
                    let name = if label.is_empty() {
                        format!("Pairing {}", short_id(&id))
                    } else {
                        label
                    };
                    let remaining_ms = observation
                        .expires_at()
                        .0
                        .saturating_sub(observation.observed_at().0);
                    let mut pairing = pairing_events.lock().expect("pairing state mutex poisoned");
                    if !pairing.is_dismissed_session(&id, observation.expires_at()) {
                        pairing.announcements.insert(
                            id,
                            HeardAnnouncement {
                                endpoint: observation.endpoint(),
                                expires_at: observation.expires_at(),
                                wall_expires: Instant::now() + Duration::from_millis(remaining_ms),
                                name,
                                hops: observation.hops().0,
                                interface: observation.source_interface(),
                                observed_at: observation.observed_at(),
                            },
                        );
                    }
                }
                PrnsEvent::Message(
                    Message::RemoteControlControllerPairingConfirmationRequired(confirmation),
                ) => {
                    let target_id = encode_hex(
                        confirmation
                            .confirmation()
                            .target()
                            .identity_hash()
                            .as_bytes(),
                    );
                    let pending = PendingPairing {
                        digits: confirmation.confirmation().confirmation_code().to_string(),
                        approval: confirmation.approval(),
                        rejection: confirmation.rejection(),
                    };
                    let mut pairing = pairing_events.lock().expect("pairing state mutex poisoned");
                    pairing.pending_target_id = Some(target_id.clone());
                    pairing.remember_active_name(&target_id);
                    pairing.set_pending(pending);
                }
                PrnsEvent::Message(_) | PrnsEvent::Diagnostic(_) => {}
            },
            interfaces: move |handle: &PrnsNodeHandle| {
                let client = TcpClientInterface::new(attach_tcp);
                let tcp_status = client.status();
                *attach_dial_slot
                    .lock()
                    .expect("tcp client target slot mutex poisoned") = Some(client.target_handle());
                if !start_tcp {
                    tcp_status.disable();
                }
                attach_power.register(LocalPowerControl::Tokio(tcp_status));
                handle.attach(client);
                #[cfg(target_os = "macos")]
                let wifi = AutoWifi::default().with_host_discovery(apple_service_discovery());
                #[cfg(target_os = "android")]
                let wifi = {
                    let inventory = crate::android::lan_bridge().inventory();
                    AutoWifi::default().with_native_host_discovery(
                        native_service_discovery_with_host_lan(
                            AutoWifiDevicePolicy::default(),
                            inventory,
                        ),
                    )
                };
                #[cfg(not(any(target_os = "macos", target_os = "android")))]
                let wifi = AutoWifi::default();
                let wifi_status = wifi.status();
                if !auto_wifi {
                    wifi_status.disable();
                }
                #[cfg(target_os = "android")]
                crate::android::lan_bridge().attach_wifi_status(wifi_status.clone());
                attach_power.register(LocalPowerControl::Wifi(wifi_status.clone()));
                let attached = handle.supervise(wifi);
                let _ = attached;
                let _ = wifi_status.set_group_id(b"reticulum");

                #[cfg(not(target_os = "android"))]
                {
                    let attached_ble = handle.attach(AutoBle::new(ble_identity));
                    let ble_status = attached_ble.status();
                    if !start_ble {
                        ble_status.disable();
                    }
                    attach_power.register(LocalPowerControl::Ble(ble_status.clone()));
                    let apply_group = ble_status.clone();
                    spawn(async move {
                        let _ = apply_group
                            .replace_discovery_groups(DiscoveryGroupSet::reticulum())
                            .await;
                    });
                    let _ = attached_ble;
                }

                #[cfg(target_os = "android")]
                {
                    let platform = crate::android::platform();
                    platform.ble.set_local_identity(ble_identity);
                    let bluetooth = BluetoothAuto::<_, { AndroidBleBackend::MAX_PEERS }>::new(
                        AndroidBleBackend::new(platform.ble.clone()),
                        ble_identity,
                        Endpoint::Android(AndroidHost::Android),
                        LinkCapabilities {
                            l2cap: None,
                            link_mtu: BLE_HW_MTU as u16,
                        },
                    );
                    let ble_status = bluetooth.status();
                    if !start_ble {
                        ble_status.disable();
                    }
                    attach_power.register(LocalPowerControl::Ble(ble_status.clone()));
                    let apply_group = ble_status.clone();
                    spawn(async move {
                        let _ = apply_group
                            .replace_discovery_groups(DiscoveryGroupSet::reticulum())
                            .await;
                    });
                    let attached_ble = handle.supervise(bluetooth);
                    let _ = attached_ble;
                }

                #[cfg(target_os = "android")]
                {
                    let usb_bridge = crate::android::usb_bridge();
                    let scan = {
                        let bridge = usb_bridge.clone();
                        move || {
                            if bridge.is_connected() {
                                vec![UsbAutoCandidate::prns_specific(
                                    crate::android::android_usb_port(),
                                )]
                            } else {
                                Vec::new()
                            }
                        }
                    };
                    let open = {
                        let bridge = usb_bridge.clone();
                        move |_candidate: UsbAutoCandidate| {
                            let bridge = bridge.clone();
                            async move { Ok(bridge.open_stream()) }
                        }
                    };
                    let usb = UsbAutoHost::new(
                        InterfaceId::new([0xD1; 8]),
                        scan,
                        open,
                        usb_bridge.rescan(),
                    );
                    let usb_status = usb.status();
                    if !start_usb {
                        usb_status.disable();
                    }
                    attach_power.register(LocalPowerControl::Tokio(usb_status));
                    handle.add_interface(usb);
                }

                #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
                {
                    enable_controller_android_accessory_switch();
                    let rescan = Arc::new(Notify::new());
                    let usb = personal_rns::usb_auto::UsbAutoHost::new(
                        personal_rns::usb_auto::DEFAULT_USB_AUTO_ID,
                        personal_rns::usb_auto::scan_native_usb_auto_targets,
                        open_native_usb_auto_target_with_baud,
                        rescan,
                    );
                    let usb_status = usb.status();
                    if !start_usb {
                        usb_status.disable();
                    }
                    attach_power.register(LocalPowerControl::Tokio(usb_status));
                    handle.add_interface(usb);
                }
            },
            persistence: NodePersistence::custom_dir(data_dir.join("node"))
                .map_err(|error| BackendError::Startup(error.to_string()))?,
        });
        let handle = node.handle();
        let tcp_dial = tcp_dial_slot
            .lock()
            .expect("tcp client target slot mutex poisoned")
            .take()
            .ok_or_else(|| {
                BackendError::Startup("the controller TCP client was not attached".to_string())
            })?;
        // node.run() is !Send (local executor). Dioxus spawn polls it on the
        // desktop main thread, and mesh work there starves Start/Stop.
        // The default thread stack is too small for the LocalSet + mesh
        // futures on macOS debug builds (controller-node overflows at launch).
        let runtime = tokio::runtime::Handle::current();
        std::thread::Builder::new()
            .name("controller-node".into())
            .stack_size(crate::CONTROLLER_THREAD_STACK_BYTES)
            .spawn(move || {
                let local = tokio::task::LocalSet::new();
                runtime.block_on(local.run_until(async move {
                    if let Err(error) = node.run().await {
                        eprintln!("remote-control controller node stopped: {error}");
                    }
                }));
            })
            .map_err(|error| BackendError::Startup(error.to_string()))?;
        Ok(Self {
            handle,
            pairing,
            local_power,
            tcp_dial,
            tcp_target_path,
            tcp_id,
            controller_identity,
            operator_destination: controller.endpoint().destination_hash(),
            activity: Mutex::new(HashMap::new()),
            peer_aliases,
            peer_aliases_path,
            peer_alias_links,
            peer_alias_links_path,
            target_ble_prefixes,
            target_ble_prefixes_path,
            target_peer_ids,
            target_peer_ids_path,
            target_wifi_ll: Mutex::new(HashMap::new()),
            manager_aliases,
            manager_aliases_path,
            target_aliases,
            target_aliases_path,
            sibling_aliases,
            sibling_aliases_path,
            sibling_wifi_ll,
            sibling_wifi_ll_path,
            build_versions: Mutex::new(HashMap::new()),
            batteries: Mutex::new(HashMap::new()),
            battery_fetched_at: Mutex::new(HashMap::new()),
            network_transports: Mutex::new(HashMap::new()),
            local_transport: Mutex::new(RemoteControlNetworkTransport::Enabled),
            clone,
            roster,
            ble_identity,
            identities_dir,
            persist_dir,
            data_dir,
            last_roster_sync: Mutex::new(None),
            monitor_until: Mutex::new(HashMap::new()),
            path_probe_pending: Mutex::new(HashSet::new()),
        })
    }

    fn cached_build_version(&self, target_id: &str) -> Option<String> {
        self.build_versions
            .lock()
            .expect("build versions mutex poisoned")
            .get(target_id)
            .cloned()
    }

    fn remember_build_version(&self, target_id: &str, version: String) {
        self.build_versions
            .lock()
            .expect("build versions mutex poisoned")
            .insert(target_id.to_owned(), version);
    }

    fn forget_build_version(&self, target_id: &str) {
        self.build_versions
            .lock()
            .expect("build versions mutex poisoned")
            .remove(target_id);
    }

    fn cached_battery(&self, target_id: &str) -> Option<String> {
        self.batteries
            .lock()
            .expect("batteries mutex poisoned")
            .get(target_id)
            .cloned()
    }

    fn cached_network_transport(&self, target_id: &str) -> Option<RemoteControlNetworkTransport> {
        self.network_transports.lock().ok()?.get(target_id).copied()
    }

    fn remember_network_transport(
        &self,
        target_id: &str,
        transport: RemoteControlNetworkTransport,
    ) {
        if let Ok(mut transports) = self.network_transports.lock() {
            transports.insert(target_id.to_string(), transport);
        }
    }

    fn forget_network_transport(&self, target_id: &str) {
        if let Ok(mut transports) = self.network_transports.lock() {
            transports.remove(target_id);
        }
    }

    fn local_network_transport(&self) -> RemoteControlNetworkTransport {
        self.local_transport
            .lock()
            .map(|transport| *transport)
            .unwrap_or(RemoteControlNetworkTransport::Enabled)
    }

    fn set_local_network_transport(&self, transport: RemoteControlNetworkTransport) {
        if let Ok(mut current) = self.local_transport.lock() {
            *current = transport;
        }
    }

    fn remember_battery(&self, target_id: &str, label: String) {
        self.batteries
            .lock()
            .expect("batteries mutex poisoned")
            .insert(target_id.to_owned(), label);
    }

    fn forget_battery(&self, target_id: &str) {
        self.batteries
            .lock()
            .expect("batteries mutex poisoned")
            .remove(target_id);
        self.battery_fetched_at
            .lock()
            .expect("battery fetched-at mutex poisoned")
            .remove(target_id);
    }

    #[allow(dead_code)]
    fn battery_needs_refresh(&self, target_id: &str) -> bool {
        const BATTERY_REFRESH: Duration = Duration::from_secs(10);
        match self
            .battery_fetched_at
            .lock()
            .expect("battery fetched-at mutex poisoned")
            .get(target_id)
        {
            Some(at) => at.elapsed() >= BATTERY_REFRESH,
            None => true,
        }
    }

    fn mark_battery_fetched(&self, target_id: &str) {
        self.battery_fetched_at
            .lock()
            .expect("battery fetched-at mutex poisoned")
            .insert(target_id.to_owned(), Instant::now());
    }

    fn stamp_activity(&self, scope: &str, items: &mut [InterfaceEntry]) {
        let mut tracker = self
            .activity
            .lock()
            .expect("interface activity mutex poisoned");
        let now = Instant::now();
        for item in items {
            let key = format!("{scope}:{}", item.id);
            let signature = InterfaceActivitySignature {
                connection: activity_connection_code(&item.connection),
                tx_bytes: item.tx_bytes,
                rx_bytes: item.rx_bytes,
                links: item.links,
                destinations: item.destinations,
                rate_bytes_per_sec: item.rate_bytes_per_sec,
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            };
            let last_at = match tracker.get_mut(&key) {
                Some(stamp) => {
                    if stamp.signature != signature {
                        stamp.signature = signature;
                        stamp.last_at = Some(now);
                    }
                    stamp.last_at
                }
                None => {
                    let last_at = signature.observed_active().then_some(now);
                    tracker.insert(key, InterfaceActivityStamp { signature, last_at });
                    last_at
                }
            };
            item.last_activity_secs =
                last_at.map(|then| now.saturating_duration_since(then).as_secs() as u32);
            for peer in &mut item.peers {
                let peer_key = format!("{scope}:{}:{}", item.id, peer.id);
                let signature = InterfaceActivitySignature {
                    connection: activity_connection_code(&peer.connection),
                    tx_bytes: peer.tx_bytes,
                    rx_bytes: peer.rx_bytes,
                    links: peer.links,
                    destinations: peer.destinations,
                    rate_bytes_per_sec: peer.rate_bytes_per_sec,
                    radio: peer.radio,
                    details: peer.details,
                };
                let last_at = match tracker.get_mut(&peer_key) {
                    Some(stamp) => {
                        if stamp.signature != signature {
                            stamp.signature = signature;
                            stamp.last_at = Some(now);
                        }
                        stamp.last_at
                    }
                    None => {
                        let last_at = signature.observed_active().then_some(now);
                        tracker.insert(peer_key, InterfaceActivityStamp { signature, last_at });
                        last_at
                    }
                };
                peer.last_activity_secs =
                    last_at.map(|then| now.saturating_duration_since(then).as_secs() as u32);
            }
        }
    }

    fn target_attention(&self, target_id: &str) -> TargetAttention {
        let Ok(roster) = self.roster.lock() else {
            return TargetAttention::Free;
        };
        attention_for_target(
            &roster.replica,
            target_id,
            &self.controller_identity.instance_hash,
        )
    }

    fn mark_reachable(&self, target_id: &str, status: TargetStatus) {
        let status = if self.target_attention(target_id) == TargetAttention::HeldBySibling {
            TargetStatus::Offline
        } else {
            status
        };
        self.pairing
            .lock()
            .expect("pairing state mutex poisoned")
            .set_reachability(target_id, status);
    }
}

struct PairingEvents {
    announcements: HashMap<String, HeardAnnouncement>,
    /// Announcement ids kept visible while a pairing attempt is in progress.
    pinned: HashMap<String, HeardAnnouncement>,
    /// Per-device pairing dest hash → last dismissed open-window `expires_at`.
    /// Rebroadcasts of that same open stay hidden; a later Pair-again (`expires_at`
    /// greater than this) is admitted again.
    dismissed_sessions: HashMap<String, InstantMillis>,
    active_announcement_id: Option<String>,
    pending: Option<PendingPairing>,
    pending_target_id: Option<String>,
    target_names: HashMap<String, String>,
    reachability: HashMap<String, TargetStatus>,
    pending_announce_name: Option<String>,
    names_path: PathBuf,
    control_announce_baseline: HashMap<String, InstantMillis>,
}

impl PairingEvents {
    fn load(names_path: PathBuf) -> Self {
        let target_names = load_target_names(&names_path);
        Self {
            announcements: HashMap::new(),
            pinned: HashMap::new(),
            dismissed_sessions: HashMap::new(),
            active_announcement_id: None,
            pending: None,
            pending_target_id: None,
            target_names,
            reachability: HashMap::new(),
            pending_announce_name: None,
            names_path,
            control_announce_baseline: HashMap::new(),
        }
    }

    fn paired_announce_name(&self, id: &str) -> Option<String> {
        stored_alias(self.target_names.get(id).map(String::as_str))
    }

    fn set_announce_name(&mut self, target_id: &str, name: &str) -> bool {
        let id = target_id.trim();
        if id.is_empty() {
            return false;
        }
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let changed = self.target_names.get(id).map(String::as_str) != Some(name);
        self.target_names.insert(id.to_owned(), name.to_owned());
        persist_target_names(&self.names_path, &self.target_names);
        changed
    }

    fn active_announcement_name(&self) -> Option<String> {
        let id = self.active_announcement_id.as_ref()?;
        self.announcements
            .get(id)
            .or_else(|| self.pinned.get(id))
            .map(|announcement| announcement.name.clone())
    }

    fn remember_active_name(&mut self, target_id: &str) {
        if let Some(name) = self.active_announcement_name() {
            self.set_announce_name(target_id, &name);
            self.pending_announce_name = None;
        }
    }

    fn remember_unnamed_targets(
        &mut self,
        targets: &[AuthorizedRemoteControlTarget],
    ) -> Option<(String, String)> {
        let pending = self.pending_announce_name.clone();
        self.remember_unnamed_targets_with(targets, pending)
    }

    fn remember_unnamed_targets_with(
        &mut self,
        targets: &[AuthorizedRemoteControlTarget],
        name: Option<String>,
    ) -> Option<(String, String)> {
        let unnamed = targets
            .iter()
            .map(|target| encode_hex(target.identity_hash().as_bytes()))
            .filter(|id| !self.target_names.contains_key(id))
            .collect::<Vec<_>>();
        let name = name.filter(|name| !name.is_empty())?;
        if unnamed.len() == 1 {
            if let Some(id) = unnamed.into_iter().next() {
                if self.set_announce_name(&id, &name) {
                    self.pending_announce_name = None;
                    return Some((id, name));
                }
                self.pending_announce_name = None;
            }
        }
        None
    }

    fn reachability(&self, id: &str) -> TargetStatus {
        self.reachability
            .get(id)
            .copied()
            .unwrap_or(TargetStatus::Offline)
    }

    fn set_reachability(&mut self, id: &str, status: TargetStatus) {
        self.reachability.insert(id.to_owned(), status);
    }

    fn forget_stored(&mut self, target_id: &str) {
        self.target_names.remove(target_id);
        self.reachability.remove(target_id);
        persist_target_names(&self.names_path, &self.target_names);
    }

    fn clear_announce_name(&mut self, target_id: &str) {
        if self.target_names.remove(target_id).is_some() {
            persist_target_names(&self.names_path, &self.target_names);
        }
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        let keep = self
            .pending
            .is_some()
            .then(|| self.active_announcement_id.clone())
            .flatten();
        self.announcements.retain(|id, announcement| {
            announcement.wall_expires > now || keep.as_ref() == Some(id)
        });
        self.pinned.retain(|id, announcement| {
            announcement.wall_expires > now || keep.as_ref() == Some(id)
        });
    }

    /// True when this open-window (`expires_at`) was dismissed and has not been
    /// superseded by a later Pair-again on the same device.
    fn is_dismissed_session(&self, announcement_id: &str, expires_at: InstantMillis) -> bool {
        self.dismissed_sessions
            .get(announcement_id)
            .is_some_and(|dismissed| expires_at.0 <= dismissed.0)
    }

    fn remember_dismissed_session(&mut self, announcement_id: &str, expires_at: InstantMillis) {
        let expires_at = self
            .dismissed_sessions
            .get(announcement_id)
            .map(|previous| InstantMillis(previous.0.max(expires_at.0)))
            .unwrap_or(expires_at);
        self.dismissed_sessions
            .insert(announcement_id.to_owned(), expires_at);
    }

    fn pin_announcement(&mut self, announcement_id: &str) {
        if let Some(announcement) = self.announcements.get(announcement_id).cloned() {
            self.pinned.insert(announcement_id.to_owned(), announcement);
        }
    }

    #[allow(dead_code)]
    fn forget_announcement(&mut self, announcement_id: &str) {
        self.announcements.remove(announcement_id);
        self.pinned.remove(announcement_id);
        if self.active_announcement_id.as_deref() == Some(announcement_id) {
            self.active_announcement_id = None;
        }
    }

    /// Operator dismiss: drop a heard/pinned advert even mid-attempt, and clear
    /// local pending state when this row was the active pairing target.
    /// Suppresses rebroadcasts of this open-window only; a later Pair-again on
    /// the same device (newer `expires_at`) is shown again.
    fn dismiss_operator(&mut self, announcement_id: &str) -> bool {
        let id = announcement_id.trim();
        if id.is_empty() {
            return false;
        }
        if let Some(expires_at) = self
            .announcements
            .get(id)
            .or_else(|| self.pinned.get(id))
            .map(|announcement| announcement.expires_at)
        {
            self.remember_dismissed_session(id, expires_at);
        }
        if self.active_announcement_id.as_deref() == Some(id) {
            self.pending = None;
            self.active_announcement_id = None;
            self.pending_target_id = None;
            self.pending_announce_name = None;
        }
        self.announcements.remove(id);
        self.pinned.remove(id);
        true
    }

    fn announcement_expires_in_secs(&mut self, announcement_id: &str) -> Option<u32> {
        self.prune_expired();
        let announcement = self
            .announcements
            .get(announcement_id)
            .or_else(|| self.pinned.get(announcement_id))?;
        Some(wall_expires_in_secs(announcement.wall_expires))
    }

    fn finish_pairing_announcement(&mut self) {
        if let Some(id) = self.active_announcement_id.take() {
            self.announcements.remove(&id);
            self.pinned.remove(&id);
        }
        self.pending_target_id = None;
    }

    fn dismiss_heard_advert(&mut self, announcement_id: &str) {
        let id = announcement_id.trim();
        if id.is_empty() {
            return;
        }
        if self.active_announcement_id.as_deref() == Some(id) || self.pinned.contains_key(id) {
            return;
        }
        if let Some(expires_at) = self
            .announcements
            .get(id)
            .map(|announcement| announcement.expires_at)
        {
            self.remember_dismissed_session(id, expires_at);
        }
        self.announcements.remove(id);
    }

    fn set_pending(&mut self, pending: PendingPairing) {
        self.pending = Some(pending);
    }

    fn active_announcements(&mut self) -> Vec<AnnouncementSummary> {
        self.prune_expired();
        let mut items = HashMap::new();
        for (id, announcement) in self.announcements.iter().chain(self.pinned.iter()) {
            items.insert(
                id.clone(),
                (
                    announcement.observed_at,
                    AnnouncementSummary {
                        id: id.clone(),
                        name: announcement.name.clone(),
                        path: Some(path_from_announce(
                            announcement.hops,
                            announcement.interface,
                            announcement.observed_at,
                        )),
                        expires_in_secs: wall_expires_in_secs(announcement.wall_expires),
                    },
                ),
            );
        }
        let mut items: Vec<(InstantMillis, AnnouncementSummary)> = items.into_values().collect();
        items.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.id.cmp(&right.1.id)));
        items.into_iter().map(|(_, item)| item).collect()
    }

    fn announcement(
        &mut self,
        announcement_id: &str,
    ) -> Option<(RemoteControlPairingEndpoint, InstantMillis)> {
        self.prune_expired();
        let announcement = self
            .announcements
            .get(announcement_id)
            .or_else(|| self.pinned.get(announcement_id))?;
        self.active_announcement_id = Some(announcement_id.to_owned());
        Some((announcement.endpoint, announcement.expires_at))
    }
}

#[derive(Clone)]
struct HeardAnnouncement {
    endpoint: RemoteControlPairingEndpoint,
    expires_at: InstantMillis,
    wall_expires: Instant,
    name: String,
    hops: u8,
    interface: InterfaceId,
    observed_at: InstantMillis,
}

struct AnnouncementSummary {
    id: String,
    name: String,
    path: Option<TargetPath>,
    expires_in_secs: u32,
}

fn wall_expires_in_secs(wall_expires: Instant) -> u32 {
    wall_expires
        .checked_duration_since(Instant::now())
        .map(|remaining| u32::try_from(remaining.as_secs()).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

struct PendingPairing {
    digits: String,
    approval: ApproveRemoteControlControllerPairing,
    rejection: RejectRemoteControlControllerPairing,
}

#[derive(Clone, Copy)]
enum RadioAction {
    Sleep,
    Wake,
}

impl RadioAction {
    const fn operation(self) -> &'static str {
        match self {
            Self::Sleep => "sleep target radios",
            Self::Wake => "wake target radios",
        }
    }
}

fn load_controller_secret(
    identities_dir: &Path,
) -> Result<
    personal_rns::identity::Zeroizing<[u8; personal_rns::IDENTITY_SECRET_KEY_LEN]>,
    BackendError,
> {
    let vault = personal_rns::identity::vault::FileVault::new(identities_dir.to_path_buf());
    let label = personal_rns::identity::vault::IdentityLabel::new(CONTROLLER_IDENTITY_FILE)
        .map_err(|error| BackendError::Startup(format!("{error:?}")))?;
    personal_rns::identity::vault::IdentityVault::load(&vault, &label)
        .map_err(|error| BackendError::Startup(error.to_string()))?
        .ok_or_else(|| BackendError::Startup("controller identity is missing".to_string()))
}

fn adopt_cloned_identity(
    identities_dir: &Path,
    persist_dir: &Path,
    secret: &[u8; personal_rns::IDENTITY_SECRET_KEY_LEN],
    accesses: &[u8],
) -> Result<(), BackendError> {
    let mut vault = personal_rns::identity::vault::FileVault::new(identities_dir.to_path_buf());
    let label = personal_rns::identity::vault::IdentityLabel::new(CONTROLLER_IDENTITY_FILE)
        .map_err(|error| BackendError::Clone(format!("{error:?}")))?;
    let stored = personal_rns::identity::Zeroizing::new(*secret);
    personal_rns::identity::vault::IdentityVault::store(&mut vault, &label, &stored)
        .map_err(|error| BackendError::Clone(error.to_string()))?;
    if !accesses.is_empty() {
        let mut store = personal_rns::persistence::FileStore::new(persist_dir.to_path_buf());
        personal_rns::persistence::PersistedStore::store(
            &mut store,
            personal_rns::persistence::SnapshotRegion::RemoteControlTargetAccesses,
            accesses,
        )
        .map_err(|error| BackendError::Clone(error.to_string()))?;
    }
    Ok(())
}

fn controller_identity_secret_path(identities_dir: impl AsRef<Path>) -> PathBuf {
    identities_dir.as_ref().join(CONTROLLER_IDENTITY_FILE)
}

fn instance_identity_secret_path(identities_dir: impl AsRef<Path>) -> PathBuf {
    identities_dir.as_ref().join(INSTANCE_IDENTITY_FILE)
}

fn load_instance_secret(
    identities_dir: &Path,
) -> Result<
    personal_rns::identity::Zeroizing<[u8; personal_rns::IDENTITY_SECRET_KEY_LEN]>,
    BackendError,
> {
    personal_rns::load_or_create_identity_secret(&instance_identity_secret_path(identities_dir))
        .map_err(|error| BackendError::Startup(error.to_string()))
}

fn load_or_create_instance_secret(
    identities_dir: &Path,
    operator: &[u8; personal_rns::IDENTITY_SECRET_KEY_LEN],
) -> Result<
    personal_rns::identity::Zeroizing<[u8; personal_rns::IDENTITY_SECRET_KEY_LEN]>,
    BackendError,
> {
    let path = instance_identity_secret_path(identities_dir);
    let secret = personal_rns::load_or_create_identity_secret(&path)
        .map_err(|error| BackendError::Startup(error.to_string()))?;
    if *secret != *operator {
        return Ok(secret);
    }
    let fresh = personal_rns::try_generate_identity_secret()
        .map_err(|error| BackendError::Startup(error.to_string()))?;
    std::fs::write(&path, &*fresh).map_err(|error| BackendError::Startup(error.to_string()))?;
    Ok(fresh)
}

fn encode_clone_labels(labels: &[RosterLabel], instance_hex: &str) -> Vec<u8> {
    let labels: Vec<_> = labels
        .iter()
        .filter(|label| match label.kind {
            RosterLabelKind::SiblingAlias => sibling_alias_is_syncable(&label.key, instance_hex),
            RosterLabelKind::PeerAlias => {
                peer_alias_is_syncable(&label.key)
                    && peer_alias_value_is_syncable(label.value.as_deref())
            }
            _ => true,
        })
        .cloned()
        .collect();
    let mut out = Vec::new();
    if encode_labels(&mut out, &labels).is_none() {
        return Vec::new();
    }
    out
}

fn persist_session_replica(session: &ControllerSession) {
    if let Ok(mut roster) = session.roster.lock() {
        strip_local_sibling_alias(
            &mut roster.replica,
            &session.controller_identity.instance_hash,
        );
        persist_replica(&replica_path(&session.data_dir), &roster.replica);
    }
}

fn remember_sibling_alias(session: &ControllerSession, instance_hash: &str, alias: &str) {
    let id = instance_hash.trim();
    if id.is_empty() || !sibling_alias_is_syncable(id, &session.controller_identity.instance_hash) {
        return;
    }
    let name = {
        let aliases = session
            .sibling_aliases
            .lock()
            .expect("sibling aliases mutex poisoned");
        let trimmed = alias.trim();
        if trimmed.is_empty() {
            let mut considered = aliases.clone();
            considered.remove(id);
            next_sibling_alias(&considered)
        } else {
            trimmed.to_owned()
        }
    };
    {
        let mut aliases = session
            .sibling_aliases
            .lock()
            .expect("sibling aliases mutex poisoned");
        aliases.insert(id.to_owned(), name.clone());
        persist_target_names(&session.sibling_aliases_path, &aliases);
    }
    if let Ok(mut roster) = session.roster.lock() {
        note_local_label(
            &mut roster.replica,
            RosterLabelKind::SiblingAlias,
            id,
            Some(&name),
        );
    }
    persist_session_replica(session);
    refresh_sibling_linked_peer_aliases(session, id, Some(&name));
}

fn ensure_missing_sibling_aliases<'a>(
    session: &ControllerSession,
    hashes: impl IntoIterator<Item = &'a str>,
) {
    let mut aliases = session
        .sibling_aliases
        .lock()
        .expect("sibling aliases mutex poisoned");
    let mut changed = false;
    for hash in hashes {
        let hash = hash.trim();
        if hash.is_empty()
            || !sibling_alias_is_syncable(hash, &session.controller_identity.instance_hash)
        {
            continue;
        }
        if aliases
            .get(hash)
            .is_some_and(|name| !name.trim().is_empty())
        {
            continue;
        }
        let next = next_sibling_alias(&aliases);
        aliases.insert(hash.to_owned(), next);
        changed = true;
    }
    if !changed {
        return;
    }
    persist_target_names(&session.sibling_aliases_path, &aliases);
    drop(aliases);
    if let Ok(mut roster) = session.roster.lock() {
        let aliases = session
            .sibling_aliases
            .lock()
            .expect("sibling aliases mutex poisoned");
        import_seed_labels(&mut roster.replica, RosterLabelKind::SiblingAlias, &aliases);
    }
    persist_session_replica(session);
}

/// Assign `Alias <n>` to managed nodes that still lack a nickname.
fn ensure_missing_target_aliases<'a>(
    session: &ControllerSession,
    ids: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let mut aliases = session
        .target_aliases
        .lock()
        .expect("target aliases mutex poisoned");
    let mut assigned = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        if aliases.get(id).is_some_and(|name| !name.trim().is_empty()) {
            continue;
        }
        let next = next_target_alias(&aliases);
        aliases.insert(id.to_owned(), next.clone());
        assigned.push((id.to_owned(), next));
    }
    if assigned.is_empty() {
        return assigned;
    }
    persist_target_names(&session.target_aliases_path, &aliases);
    drop(aliases);
    if let Ok(mut roster) = session.roster.lock() {
        let aliases = session
            .target_aliases
            .lock()
            .expect("target aliases mutex poisoned");
        import_seed_labels(&mut roster.replica, RosterLabelKind::TargetAlias, &aliases);
    }
    persist_session_replica(session);
    assigned
}

fn remove_alias_map_key(aliases: &Mutex<HashMap<String, String>>, path: &PathBuf, key: &str) {
    let mut aliases = aliases.lock().expect("alias map mutex poisoned");
    if aliases.remove(key).is_some() {
        persist_target_names(path, &aliases);
    }
}

fn apply_map_label(
    aliases: &Mutex<HashMap<String, String>>,
    path: &PathBuf,
    key: &str,
    value: Option<&str>,
) {
    let mut aliases = aliases.lock().expect("alias map mutex poisoned");
    match value {
        Some(name) if !name.is_empty() => {
            aliases.insert(key.to_owned(), name.to_owned());
        }
        _ => {
            aliases.remove(key);
        }
    }
    persist_target_names(path, &aliases);
}

fn apply_roster_labels(session: &ControllerSession, labels: &[RosterLabel]) {
    for label in labels {
        match label.kind {
            RosterLabelKind::TargetName => {
                let mut pairing = session
                    .pairing
                    .lock()
                    .expect("pairing state mutex poisoned");
                match &label.value {
                    Some(name) if !name.is_empty() => {
                        pairing.set_announce_name(&label.key, name);
                    }
                    _ => {
                        pairing.clear_announce_name(&label.key);
                    }
                }
            }
            RosterLabelKind::TargetAlias => {
                let previous = session
                    .target_aliases
                    .lock()
                    .expect("target aliases mutex poisoned")
                    .get(&label.key)
                    .cloned();
                apply_map_label(
                    &session.target_aliases,
                    &session.target_aliases_path,
                    &label.key,
                    label.value.as_deref(),
                );
                // Local projection only — do not emit PeerAlias (siblings already
                // receive auto peer aliases from the originator; re-emitting storms).
                let _ = project_target_alias_to_peers(
                    session,
                    &label.key,
                    stored_alias(label.value.as_deref()).as_deref(),
                    stored_alias(previous.as_deref()).as_deref(),
                    &[],
                    None,
                );
            }
            RosterLabelKind::ManagerAlias => apply_map_label(
                &session.manager_aliases,
                &session.manager_aliases_path,
                &label.key,
                label.value.as_deref(),
            ),
            RosterLabelKind::PeerAlias => {
                if !peer_alias_is_syncable(&label.key)
                    || !peer_alias_value_is_syncable(label.value.as_deref())
                {
                    // Drop USB-host ids and the local-only "This Controller" string.
                } else if label
                    .value
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                    && session
                        .peer_alias_links
                        .lock()
                        .ok()
                        .and_then(|links| links.get(&label.key).cloned())
                        .is_some_and(|link| link == THIS_CONTROLLER_ALIAS_LINK)
                {
                    // Roster clear must not erase this install's local AutoWifi self-label.
                } else {
                    apply_map_label(
                        &session.peer_aliases,
                        &session.peer_aliases_path,
                        &label.key,
                        label.value.as_deref(),
                    );
                }
            }
            RosterLabelKind::SiblingAlias => {
                if sibling_alias_is_syncable(&label.key, &session.controller_identity.instance_hash)
                {
                    apply_map_label(
                        &session.sibling_aliases,
                        &session.sibling_aliases_path,
                        &label.key,
                        label.value.as_deref(),
                    );
                    refresh_sibling_linked_peer_aliases(
                        session,
                        &label.key,
                        stored_alias(label.value.as_deref()).as_deref(),
                    );
                }
            }
            RosterLabelKind::SiblingWifiLl => {
                if !label
                    .key
                    .eq_ignore_ascii_case(&session.controller_identity.instance_hash)
                {
                    remember_sibling_wifi_ll(session, &label.key, label.value.as_deref());
                    if let Some(alias) = stored_alias(
                        session
                            .sibling_aliases
                            .lock()
                            .expect("sibling aliases mutex poisoned")
                            .get(&label.key)
                            .map(String::as_str),
                    ) {
                        // Mapping arrived (or changed); peers matching this LL pick it up on
                        // the next inventory reconcile. Refresh already-linked peers now.
                        refresh_sibling_linked_peer_aliases(session, &label.key, Some(&alias));
                    }
                }
            }
            RosterLabelKind::SiblingRemoved => {
                if label
                    .value
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                {
                    if let Ok(bytes) = parse_hex::<IDENTITY_HASH_BYTES>(&label.key) {
                        if let Ok(mut roster) = session.roster.lock() {
                            roster.replica.siblings.retain(|sibling| {
                                sibling.identity_hash() != IdentityHash::new(bytes)
                            });
                        }
                    }
                    remove_alias_map_key(
                        &session.sibling_aliases,
                        &session.sibling_aliases_path,
                        &label.key,
                    );
                    remove_alias_map_key(
                        &session.sibling_wifi_ll,
                        &session.sibling_wifi_ll_path,
                        &label.key,
                    );
                    refresh_sibling_linked_peer_aliases(session, &label.key, None);
                }
            }
            RosterLabelKind::PairingAdvertDismissed => {
                if label
                    .value
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                {
                    session
                        .pairing
                        .lock()
                        .expect("pairing state mutex poisoned")
                        .dismiss_heard_advert(&label.key);
                }
            }
            RosterLabelKind::TargetLooking => {
                if session.target_attention(&label.key) == TargetAttention::HeldBySibling {
                    if let Ok(mut until) = session.monitor_until.lock() {
                        until.remove(&label.key);
                    }
                    session.mark_reachable(&label.key, TargetStatus::Offline);
                }
            }
        }
    }
}

fn controller_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("HOPSPOT_RC_DATA_DIR") {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "android")]
    {
        return android_files_dir().unwrap_or_else(std::env::temp_dir);
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(DEFAULT_DATA_DIRECTORY)
    }
}

#[cfg(target_os = "android")]
fn android_files_dir() -> Option<PathBuf> {
    use jni::objects::{JObject, JString};
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let file = env
        .call_method(&context, "getFilesDir", "()Ljava/io/File;", &[])
        .ok()?
        .l()
        .ok()?;
    let path = env
        .call_method(file, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .ok()?
        .l()
        .ok()?;
    let path: String = env.get_string(&JString::from(path)).ok()?.into();
    Some(PathBuf::from(path).join("hopspot-remote-control"))
}

fn env_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn env_started_off(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(false)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn enable_controller_android_accessory_switch() {
    // Native scan only switches a Pixel into AOA when this is present.
    // Controller-to-Controller USB is that phone-as-device path.
    if std::env::var_os("PRNS_USB_AUTO_ANDROID_ACCESSORY").is_none() {
        std::env::set_var("PRNS_USB_AUTO_ANDROID_ACCESSORY", "1");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
async fn open_native_usb_auto_target_with_baud(
    candidate: personal_rns::usb_auto::UsbAutoCandidate,
) -> std::io::Result<personal_rns::usb_auto::NativeUsbAutoStream> {
    personal_rns::usb_auto::open_native_usb_auto_target(
        candidate,
        personal_rns::usb_auto::DEFAULT_USB_BAUD,
    )
    .await
}

fn parse_invitation_code(code: &str) -> Result<RemoteControlPairingInvitationCode, BackendError> {
    let normalized: String = code
        .chars()
        .filter(|character| *character != '-' && !character.is_whitespace())
        .collect();
    if normalized.len() != 8
        || !normalized
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(BackendError::InvalidInvitationCode);
    }
    u32::from_str_radix(&normalized, 16)
        .map(RemoteControlPairingInvitationCode::from_value)
        .map_err(|_| BackendError::InvalidInvitationCode)
}

fn begin_pairing_error(error: InitiateRemoteControlControllerPairingError) -> BackendError {
    match error {
        InitiateRemoteControlControllerPairingError::Request { failure, .. }
            if matches!(
                failure.cause,
                personal_rns::engine::RemoteControlControllerPairingRequestFailureCause::Request(
                    personal_rns::engine::SendRequestFailure::Timeout
                )
            ) =>
        {
            BackendError::PairingOfferTimeout
        }
        error => operation("begin controller pairing", error),
    }
}

fn parse_hex<const N: usize>(input: &str) -> Result<[u8; N], ()> {
    let input = input.trim();
    if input.len() != N.saturating_mul(2) {
        return Err(());
    }
    let mut output = [0u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        *byte = u8::from_str_radix(&input[start..start + 2], 16).map_err(|_| ())?;
    }
    Ok(output)
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn not_monitoring(target_id: &str) -> BackendError {
    BackendError::Operation {
        operation: "connect to target",
        detail: format!("this controller is not connected to {target_id}"),
    }
}

fn monitor_remaining_at(until: Option<Instant>, now: Instant, attention: TargetAttention) -> u32 {
    // Sibling hold only blocks *starting* a monitor. Once this controller has a local
    // Connect deadline, keep counting it down even if roster sync still shows another
    // instance as looking (common with Mac+Android Controllers on the same mesh).
    if attention == TargetAttention::HeldBySibling && until.is_none() {
        return 0;
    }
    until
        .filter(|deadline| *deadline > now)
        .map(|deadline| {
            u32::try_from(deadline.saturating_duration_since(now).as_secs()).unwrap_or(0)
        })
        .unwrap_or(0)
}

pub fn format_connect_label(remaining_secs: u32) -> String {
    format!(
        "Connect {:02}:{:02}",
        remaining_secs / 60,
        remaining_secs % 60
    )
}

pub fn format_pairing_open_label(remaining_secs: u32) -> String {
    format!("Open {:02}:{:02}", remaining_secs / 60, remaining_secs % 60)
}

fn load_persisted_target_hashes(persist_dir: &Path) -> Vec<IdentityHash> {
    let store = personal_rns::persistence::FileStore::new(persist_dir.to_path_buf());
    let Ok(Some(len)) = personal_rns::persistence::PersistedStore::stored_len(
        &store,
        personal_rns::persistence::SnapshotRegion::RemoteControlTargetAccesses,
    ) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; len];
    let Ok(Some(bytes)) = personal_rns::persistence::PersistedStore::load(
        &store,
        personal_rns::persistence::SnapshotRegion::RemoteControlTargetAccesses,
        &mut buf,
    ) else {
        return Vec::new();
    };
    let Ok(persisted) =
        personal_rns::persistence::read_remote_control_target_accesses_snapshot(bytes)
    else {
        return Vec::new();
    };
    persisted
        .map(|access| access.target().identity_hash())
        .collect()
}

fn managed_targets_from_disk(
    persist: &[IdentityHash],
    replica: &crate::roster_sync::RosterReplica,
    names: &HashMap<String, String>,
) -> Vec<TargetAccess> {
    let mut ids = HashSet::new();
    for hash in persist
        .iter()
        .copied()
        .chain(replica_known_targets(replica))
    {
        if replica_forgets_target(replica, hash) {
            continue;
        }
        ids.insert(encode_hex(hash.as_bytes()));
    }
    for key in names.keys() {
        let Ok(bytes) = parse_hex::<IDENTITY_HASH_BYTES>(key) else {
            continue;
        };
        let hash = IdentityHash::new(bytes);
        if replica_forgets_target(replica, hash) {
            continue;
        }
        ids.insert(encode_hex(&bytes));
    }
    let mut items: Vec<TargetAccess> = ids
        .into_iter()
        .map(|id| TargetAccess {
            name: names.get(&id).cloned().unwrap_or_default(),
            status: TargetStatus::Offline,
            path: None,
            build_version: None,
            battery: None,
            network_transport: None,
            monitor_remaining_secs: 0,
            pairing_expires_in_secs: None,
            id,
        })
        .collect();
    items.sort_by(|left, right| left.id.cmp(&right.id));
    items
}

fn resolve_paired_target_hash(
    hashes: &[IdentityHash],
    pending_id: Option<&str>,
) -> Option<IdentityHash> {
    if let Some(pending_id) = pending_id {
        let pending_id = pending_id.trim();
        if pending_id.is_empty() {
            return None;
        }
        return hashes
            .iter()
            .copied()
            .find(|hash| encode_hex(hash.as_bytes()).eq_ignore_ascii_case(pending_id));
    }
    (hashes.len() == 1).then_some(hashes[0])
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

pub fn target_label(id: &str) -> String {
    format!("T {}", short_id(id))
}

fn operator_interface_kind(kind: InterfaceKind) -> bool {
    kind.supervisor_kind().is_none()
}

fn operator_local_interface(
    entry: &personal_rns::node_introspection::InterfaceInventoryEntry,
) -> bool {
    operator_local_kind(&entry.snapshot).is_some()
}

/// This app's USB Auto host id is `[0xD0; 8]`, a cookie, not `kind ++ hash`.
/// Settings still reports it as `UsbAutoHost`.
///
/// Any independent supervisor whose first id byte is not a known
/// `InterfaceKind` gets that same stamp. Fleet members stay omitted; a
/// decodable member kind is dropped, not relabeled USB.
/// Ingress stores a direct USB announce as hop 1 (`header.hops + 1`).
/// Hop 0 is kept for any path that does not increment.
fn clone_announce_is_usb_local(hops: u8, source_interface: InterfaceId) -> bool {
    hops <= 1
        && matches!(
            source_interface.kind(),
            Some(InterfaceKind::UsbAutoHost | InterfaceKind::UsbAutoDevice) | None
        )
}

fn operator_local_kind(snapshot: &InterfaceSnapshot) -> Option<InterfaceKind> {
    if matches!(snapshot.membership, Membership::FleetMember { .. }) {
        return None;
    }
    match snapshot.id.kind() {
        Some(kind) if operator_interface_kind(kind) => Some(kind),
        None => Some(InterfaceKind::UsbAutoHost),
        Some(_) => None,
    }
}

fn operator_remote_kind(id: InterfaceId) -> Option<InterfaceKind> {
    match id.kind() {
        Some(kind) if operator_interface_kind(kind) => Some(kind),
        None => Some(InterfaceKind::UsbAutoDevice),
        Some(_) => None,
    }
}

/// USB Auto never publishes fleet-member peers. When the host handshake
/// is up, Settings still needs one row so clone and the accordion agree.
fn attach_peer_our_side_addresses(entry: &mut InterfaceEntry, addresses: &[(IpAddr, u32)]) {
    for peer in &mut entry.peers {
        attach_our_side_of_the_link(peer, addresses);
    }
}

fn attach_our_side_of_the_link(peer: &mut InterfacePeer, addresses: &[(IpAddr, u32)]) {
    let path = match peer.endpoint_label.as_deref() {
        Some("To") => PeerPath::TcpOutbound,
        Some("From") => PeerPath::TcpInbound,
        Some("Peer") => PeerPath::WifiUdp,
        Some(_) | None => return,
    };
    let Some(label) = path.our_endpoint_label() else {
        return;
    };
    let Some(endpoint) = peer.endpoint.as_deref() else {
        return;
    };
    let Some((peer_ip, peer_scope)) = parse_endpoint_ip(endpoint) else {
        return;
    };
    let Some((our_ip, our_ifindex)) = select_our_link_address(peer_ip, peer_scope, addresses)
    else {
        return;
    };
    peer.local_endpoint_label = Some(label.to_string());
    peer.local_endpoint = Some(format_our_link_address(
        our_ip,
        our_ifindex,
        path.our_listen_port(),
    ));
}

fn parse_endpoint_ip(endpoint: &str) -> Option<(IpAddr, Option<u32>)> {
    let endpoint = endpoint.trim();
    if let Ok(addr) = endpoint.parse::<SocketAddr>() {
        let scope = match addr {
            SocketAddr::V6(v6) if v6.scope_id() != 0 => Some(v6.scope_id()),
            SocketAddr::V4(_) | SocketAddr::V6(_) => None,
        };
        return Some((addr.ip(), scope));
    }
    if let Some((host, _)) = split_host_port(endpoint) {
        if let Some(parsed) = parse_ip_maybe_scoped(host) {
            return Some(parsed);
        }
    }
    parse_ip_maybe_scoped(endpoint)
}

fn parse_ip_maybe_scoped(value: &str) -> Option<(IpAddr, Option<u32>)> {
    let value = value.trim().trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some((ip, None));
    }
    let (host, scope) = value.rsplit_once('%')?;
    Some((host.parse().ok()?, Some(scope.parse().ok()?)))
}

fn select_our_link_address(
    peer: IpAddr,
    peer_scope: Option<u32>,
    locals: &[(IpAddr, u32)],
) -> Option<(IpAddr, u32)> {
    if peer.is_loopback() {
        return match peer {
            IpAddr::V4(_) => Some((IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
            IpAddr::V6(_) => Some((IpAddr::V6(Ipv6Addr::LOCALHOST), 0)),
        };
    }
    let candidates: Vec<(IpAddr, u32)> = locals
        .iter()
        .copied()
        .filter(|(local, _)| addresses_are_same_family_and_scope(peer, *local))
        .collect();
    if let Some(scope) = peer_scope {
        if let Some(match_by_scope) = candidates
            .iter()
            .copied()
            .find(|(_, ifindex)| *ifindex == scope)
        {
            return Some(match_by_scope);
        }
        if matches!(peer, IpAddr::V6(v6) if v6.is_unicast_link_local()) {
            return None;
        }
    }
    let fe80_iids: Vec<u64> = locals
        .iter()
        .filter_map(|(addr, _)| match addr {
            IpAddr::V6(v6) if v6.is_unicast_link_local() => Some(ipv6_interface_id(*v6)),
            IpAddr::V4(_) | IpAddr::V6(_) => None,
        })
        .collect();
    candidates
        .into_iter()
        .max_by_key(|(local, _)| score_our_link_address(peer, *local, &fe80_iids))
}

fn addresses_are_same_family_and_scope(peer: IpAddr, local: IpAddr) -> bool {
    match (peer, local) {
        (IpAddr::V4(_), IpAddr::V4(_)) => true,
        (IpAddr::V6(peer), IpAddr::V6(local)) => {
            peer.is_unicast_link_local() == local.is_unicast_link_local()
        }
        (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => false,
    }
}

fn score_our_link_address(peer: IpAddr, local: IpAddr, fe80_iids: &[u64]) -> (u8, u8) {
    match (peer, local) {
        (IpAddr::V4(peer), IpAddr::V4(local)) => {
            let same_slash24 = u32::from(peer) & 0xffff_ff00 == u32::from(local) & 0xffff_ff00;
            (u8::from(same_slash24), 0)
        }
        (IpAddr::V6(peer), IpAddr::V6(local)) => {
            let same_slash64 = u128::from(peer) >> 64 == u128::from(local) >> 64;
            let stable = fe80_iids.contains(&ipv6_interface_id(local));
            (u8::from(same_slash64), u8::from(stable))
        }
        (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => (0, 0),
    }
}

fn ipv6_interface_id(addr: Ipv6Addr) -> u64 {
    u128::from(addr) as u64
}

fn format_our_link_address(addr: IpAddr, ifindex: u32, listen_port: Option<u16>) -> String {
    match (addr, listen_port) {
        (IpAddr::V4(v4), Some(port)) => format!("{v4}:{port}"),
        (IpAddr::V4(v4), None) => v4.to_string(),
        (IpAddr::V6(v6), Some(port)) if v6.is_unicast_link_local() && ifindex != 0 => {
            format!("[{v6}%{ifindex}]:{port}")
        }
        (IpAddr::V6(v6), Some(port)) => format!("[{v6}]:{port}"),
        (IpAddr::V6(v6), None) if v6.is_unicast_link_local() && ifindex != 0 => {
            format!("{v6}%{ifindex}")
        }
        (IpAddr::V6(v6), None) => v6.to_string(),
    }
}

fn attach_auto_wifi_local_addresses(entry: &mut InterfaceEntry, addresses: &[(IpAddr, u32)]) {
    if entry.kind != "auto-wifi" || addresses.is_empty() {
        return;
    }
    entry.extras.extend(auto_wifi_address_facts(addresses));
}

fn auto_wifi_address_facts(addresses: &[(IpAddr, u32)]) -> Vec<InterfaceFact> {
    addresses
        .iter()
        .map(|(addr, ifindex)| match addr {
            IpAddr::V4(v4) => interface_fact("IPv4", v4.to_string()),
            IpAddr::V6(v6) if v6.is_unicast_link_local() && *ifindex != 0 => {
                interface_fact("IPv6", format!("{v6}%{ifindex}"))
            }
            IpAddr::V6(v6) => interface_fact("IPv6", v6.to_string()),
        })
        .collect()
}

fn attach_usb_link_peer(entry: &mut InterfaceEntry) {
    if !entry.peers.is_empty() || !usb_supervisor_link_is_up(entry) {
        return;
    }
    entry.shows_peers = true;
    entry.peers.push(InterfacePeer {
        id: format!("{}:link", entry.id),
        name: "USB link".to_string(),
        role: PeerPath::Usb.role().to_string(),
        detail: PeerPath::Usb.detail().to_string(),
        endpoint: None,
        endpoint_label: None,
        local_endpoint: None,
        local_endpoint_label: None,
        alias: None,
        connection: entry.connection.clone(),
        health: None,
        tx_bytes: entry.tx_bytes,
        rx_bytes: entry.rx_bytes,
        links: entry.links,
        destinations: entry.destinations,
        rate_bytes_per_sec: entry.rate_bytes_per_sec,
        last_activity_secs: entry.last_activity_secs,
        radio: RadioIndication::NotRadio,
        details: PeerDetails::NotApplicable,
    });
}

fn local_interface_entry(
    snapshot: &InterfaceSnapshot,
    name: Option<&str>,
    ble_identity: Option<BleIdentity>,
    group: Option<&str>,
    ifac_bytes: Option<usize>,
    detail: Option<&str>,
    failure: Option<&str>,
    drops: Option<(u32, u32)>,
    peers: Vec<InterfacePeer>,
) -> InterfaceEntry {
    let id = encode_hex(snapshot.id.as_bytes());
    let kind = operator_local_kind(snapshot);
    let kind_name = kind
        .map(InterfaceKind::name)
        .unwrap_or("unknown")
        .to_string();
    let label = ble_identity
        .filter(|_| kind == Some(InterfaceKind::BluetoothAuto))
        .map(bluetooth_auto_title)
        .or_else(|| {
            name.map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| format!("{} {}", kind_name, short_id(&id)));
    let mut extras = hopspot_extra_facts(detail, kind, drops);
    extend_radio_facts(&mut extras, snapshot.radio);
    ensure_auto_wifi_rssi_fact(&mut extras, kind);
    InterfaceEntry {
        name: label,
        id,
        kind: kind_name,
        power: interface_power_from_connection(snapshot.connection),
        mode: snapshot.mode,
        connection: connection_label(kind, snapshot.connection),
        group: group
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        tx_bytes: snapshot.tx_bytes,
        rx_bytes: snapshot.rx_bytes,
        tx_bps: snapshot.transfer_rates.map(|rates| rates.tx_bps),
        rx_bps: snapshot.transfer_rates.map(|rates| rates.rx_bps),
        links: snapshot.links,
        transported_links: snapshot.transported_links,
        destinations: snapshot.destinations,
        rate_bytes_per_sec: snapshot
            .transfer_rates
            .map(|rates| rates.rx_bps.saturating_add(rates.tx_bps) / 8)
            .unwrap_or(0),
        gravity: (snapshot.gravity.get() != 0).then_some(snapshot.gravity.get()),
        ifac_bytes,
        last_activity_secs: None,
        detail: detail
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        failure: failure
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        extras,
        shows_peers: kind.is_some_and(|kind| kind.member_kind().is_some()) || !peers.is_empty(),
        peers,
        peers_error: None,
        arrived_at: Some(format_wall_clock_now()),
    }
}

async fn fetch_interface_config(
    remote: &RemoteControlTargetHandle<'_>,
    entry: &mut InterfaceEntry,
) {
    let Ok(id) = parse_hex::<INTERFACE_ID_BYTES>(&entry.id) else {
        return;
    };
    match remote
        .inventory_interface_config(InterfaceId::new(id))
        .await
    {
        Ok((RemoteControlInterfaceConfigOutcome::Card(card), _)) => {
            apply_remote_card(entry, &card);
        }
        Ok((RemoteControlInterfaceConfigOutcome::UnknownInterface, _)) | Err(_) => {}
    }
}

fn apply_remote_card(entry: &mut InterfaceEntry, card: &RemoteControlInterfaceCard) {
    let card_name = card.name.as_str().trim();
    if !card_name.is_empty() && !generic_bluetooth_auto_title(card_name) {
        entry.name = card_name.to_owned();
    }
    entry.group = Some(card.group.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    entry.detail = Some(card.config.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    entry.failure = Some(card.failure.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    entry.destinations = card.destinations;
    entry.transported_links = card.transported_links;
    let kind = parse_hex::<INTERFACE_ID_BYTES>(&entry.id)
        .ok()
        .and_then(|bytes| operator_remote_kind(InterfaceId::new(bytes)));
    entry.extras = hopspot_extra_facts(entry.detail.as_deref(), kind, None);
    ensure_auto_wifi_rssi_fact(&mut entry.extras, kind);
    if let Some(ipv6) = take_ipv6_group(&mut entry.group) {
        entry.extras.push(interface_fact("IPv6", ipv6));
    }
}

async fn fetch_interface_peers(
    remote: &RemoteControlTargetHandle<'_>,
    id: InterfaceId,
) -> Result<Vec<InterfacePeer>, String> {
    let mut peers = Vec::new();
    let mut page_cursor = personal_rns::remote_control::RemoteControlPeerPage::First;
    loop {
        let page = fetch_interface_peer_page(remote, id, page_cursor).await?;
        if page.peers.is_empty() {
            break;
        }
        for peer in &page.peers {
            peers.push(interface_peer_from_wire(peer));
        }
        page_cursor = match page.continuation() {
            personal_rns::remote_control::RemoteControlPeerContinuation::Complete => break,
            personal_rns::remote_control::RemoteControlPeerContinuation::More(cursor) => {
                personal_rns::remote_control::RemoteControlPeerPage::After(cursor)
            }
        };
    }
    Ok(peers)
}

async fn fetch_interface_peer_page(
    remote: &RemoteControlTargetHandle<'_>,
    id: InterfaceId,
    page: personal_rns::remote_control::RemoteControlPeerPage,
) -> Result<RemoteControlInterfacePeerPage, String> {
    let mut last_error = None;
    for _ in 0..2 {
        match remote.inventory_interface_peers(id, page).await {
            Ok((RemoteControlInterfacePeersOutcome::Page(page), _)) => return Ok(page),
            Ok((RemoteControlInterfacePeersOutcome::UnknownInterface, _)) => {
                return Err("the target does not know that interface".to_string());
            }
            Err(error) => last_error = Some(format!("{error:?}")),
        }
    }
    Err(last_error.unwrap_or_else(|| "peer page request failed".to_string()))
}

fn interface_peer_from_wire(peer: &RemoteControlInterfacePeer) -> InterfacePeer {
    described_interface_peer(
        peer.id,
        peer.connection,
        None,
        peer.tx_bytes,
        peer.rx_bytes,
        peer.links,
        peer.destinations,
        peer.rate_bytes_per_sec,
        None,
        peer.radio,
        peer.details,
    )
}

fn remote_interface_entry(
    entry: &RemoteControlInterfaceEntry,
    card: Option<&RemoteControlInterfaceCard>,
) -> InterfaceEntry {
    let id = encode_hex(entry.id.as_bytes());
    let kind = entry.kind.name().to_string();
    let card_name = card
        .map(|card| card.name.as_str().trim())
        .filter(|name| !name.is_empty());
    let name = card_name
        .filter(|name| !generic_bluetooth_auto_title(name))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{} {}", kind, short_id(&id)));
    let mut group = card
        .map(|card| card.group.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let detail = card
        .map(|card| card.config.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let failure = card
        .map(|card| card.failure.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let peers: Vec<InterfacePeer> = Vec::new();
    let mut extras = hopspot_extra_facts(detail.as_deref(), Some(entry.kind), None);
    ensure_auto_wifi_rssi_fact(&mut extras, Some(entry.kind));
    if let Some(ipv6) = take_ipv6_group(&mut group) {
        extras.push(interface_fact("IPv6", ipv6));
    }
    InterfaceEntry {
        name,
        id,
        kind,
        power: if entry.enabled {
            InterfacePower::On
        } else {
            InterfacePower::Off
        },
        mode: entry.mode,
        connection: connection_label(Some(entry.kind), entry.connection),
        group,
        tx_bytes: entry.tx_bytes,
        rx_bytes: entry.rx_bytes,
        tx_bps: None,
        rx_bps: None,
        links: entry.links,
        transported_links: card.map(|card| card.transported_links).unwrap_or(0),
        destinations: card.map(|card| card.destinations).unwrap_or(0),
        rate_bytes_per_sec: entry.rate_bytes_per_sec,
        gravity: None,
        ifac_bytes: None,
        last_activity_secs: None,
        detail,
        failure,
        extras,
        shows_peers: entry.kind.member_kind().is_some() || !peers.is_empty(),
        peers,
        peers_error: None,
        arrived_at: Some(format_wall_clock_now()),
    }
}

fn local_interface_config(
    snapshot: &InterfaceSnapshot,
    ifac_bytes: Option<usize>,
    tcp_target: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(size) = ifac_bytes {
        parts.push(format!("IFAC {size}"));
    }
    match operator_local_kind(snapshot) {
        Some(InterfaceKind::TcpClient) => {
            if let Some(target) = tcp_target.map(str::trim).filter(|value| !value.is_empty()) {
                parts.push(target.to_string());
            } else {
                parts.push("TCP dial".to_string());
            }
        }
        Some(InterfaceKind::AutoWifi) => parts.push("Auto discovery".to_string()),
        Some(InterfaceKind::LocalServer) => parts.push("Shared instance".to_string()),
        Some(InterfaceKind::TcpServer) => parts.push("TCP listen".to_string()),
        Some(InterfaceKind::BluetoothAuto) => parts.push("BLE supervisor".to_string()),
        Some(InterfaceKind::UsbAutoHost | InterfaceKind::UsbAutoDevice) => {
            parts.push("USB".to_string())
        }
        Some(InterfaceKind::LoRa | InterfaceKind::Rnode) => parts.push("LoRa".to_string()),
        Some(
            InterfaceKind::Loopback
            | InterfaceKind::Udp
            | InterfaceKind::Serial
            | InterfaceKind::WifiPeer
            | InterfaceKind::LocalClient
            | InterfaceKind::TcpServerPeer
            | InterfaceKind::BluetoothPeer
            | InterfaceKind::Kiss
            | InterfaceKind::Ax25Kiss
            | InterfaceKind::Pipe
            | InterfaceKind::BackboneServer
            | InterfaceKind::BackboneServerPeer
            | InterfaceKind::BackboneClient
            | InterfaceKind::EspNow
            | InterfaceKind::WebSocketClient
            | InterfaceKind::WebSocketServer
            | InterfaceKind::WebSocketServerPeer
            | InterfaceKind::WifiDirect
            | InterfaceKind::WifiDirectPeer
            | InterfaceKind::WifiAware
            | InterfaceKind::WifiAwarePeer
            | InterfaceKind::I2p
            | InterfaceKind::I2pPeer
            | InterfaceKind::Weave
            | InterfaceKind::WeavePeer,
        )
        | None => {}
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn interface_peer(snapshot: &InterfaceSnapshot, endpoint: Option<&str>) -> InterfacePeer {
    described_interface_peer(
        snapshot.id,
        snapshot.connection,
        endpoint,
        snapshot.tx_bytes,
        snapshot.rx_bytes,
        snapshot.links,
        snapshot.destinations,
        snapshot
            .transfer_rates
            .map(|rates| rates.rx_bps.saturating_add(rates.tx_bps) / 8)
            .unwrap_or(0),
        None,
        snapshot.radio,
        snapshot.details,
    )
}

fn described_interface_peer(
    id: InterfaceId,
    connection: ConnectionState,
    endpoint: Option<&str>,
    tx_bytes: u64,
    rx_bytes: u64,
    links: u32,
    destinations: u32,
    rate_bytes_per_sec: u32,
    last_activity_secs: Option<u32>,
    radio: RadioIndication,
    details: PeerDetails,
) -> InterfacePeer {
    let path = PeerPath::from_kind(id.kind());
    let endpoint = match path {
        PeerPath::Bluetooth => None,
        PeerPath::WifiUdp
        | PeerPath::TcpOutbound
        | PeerPath::TcpInbound
        | PeerPath::Usb
        | PeerPath::Other => endpoint
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    };
    InterfacePeer {
        id: encode_hex(id.as_bytes()),
        name: path.title(id, endpoint.as_deref()),
        role: path.role().to_string(),
        detail: path.detail().to_string(),
        endpoint_label: endpoint.as_ref().map(|_| path.endpoint_label().to_string()),
        endpoint,
        local_endpoint: None,
        local_endpoint_label: None,
        alias: None,
        connection: connection_label(id.kind(), connection),
        health: peer_health(
            id.kind(),
            connection,
            links,
            destinations,
            tx_bytes,
            rx_bytes,
            rate_bytes_per_sec,
        ),
        tx_bytes,
        rx_bytes,
        links,
        destinations,
        rate_bytes_per_sec,
        last_activity_secs,
        radio,
        details,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerPath {
    WifiUdp,
    TcpOutbound,
    TcpInbound,
    Bluetooth,
    Usb,
    Other,
}

impl PeerPath {
    const fn from_kind(kind: Option<InterfaceKind>) -> Self {
        match kind {
            Some(InterfaceKind::WifiPeer) => Self::WifiUdp,
            Some(InterfaceKind::TcpClient) => Self::TcpOutbound,
            Some(InterfaceKind::TcpServerPeer) => Self::TcpInbound,
            Some(InterfaceKind::BluetoothPeer) => Self::Bluetooth,
            Some(InterfaceKind::UsbAutoHost | InterfaceKind::UsbAutoDevice) => Self::Usb,
            Some(_) | None => Self::Other,
        }
    }

    const fn role(self) -> &'static str {
        match self {
            Self::WifiUdp => "UDP",
            Self::TcpOutbound => "TCP out",
            Self::TcpInbound => "TCP in",
            Self::Bluetooth => "BLE",
            Self::Usb => "USB",
            Self::Other => "Peer",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::WifiUdp => {
                "IPv6 fe80 UDP on the LAN: Auto Wi-Fi peering tokens and Reticulum frames. Status is UDP peering. Health is whether an RNS link is up. Not TCP; a TCP row to the same node is another peer."
            }
            Self::TcpOutbound => {
                "This device dialed them. Stock Auto Wi-Fi also dials the Wi-Fi gateway on port 42699; that row is not a mesh node if it never connects."
            }
            Self::TcpInbound => "They dialed this device's TCP rendezvous.",
            Self::Bluetooth => {
                "Bluetooth Auto member. Status is the radio session. Health is whether an RNS link is up. Details is the BLE data plane (GATT floor vs CoC)."
            }
            Self::Usb => "USB Auto host link.",
            Self::Other => "Interface member attached under this supervisor.",
        }
    }

    const fn endpoint_label(self) -> &'static str {
        match self {
            Self::TcpOutbound => "To",
            Self::TcpInbound => "From",
            Self::WifiUdp => "Peer",
            Self::Bluetooth | Self::Usb | Self::Other => "Address",
        }
    }

    const fn our_endpoint_label(self) -> Option<&'static str> {
        match self {
            Self::TcpOutbound => Some("From (us)"),
            Self::TcpInbound => Some("To (us)"),
            Self::WifiUdp => Some("Local (us)"),
            Self::Bluetooth | Self::Usb | Self::Other => None,
        }
    }

    const fn our_listen_port(self) -> Option<u16> {
        match self {
            Self::TcpInbound => Some(personal_rns::interfaces::wifi_auto::TCP_RENDEZVOUS_PORT),
            Self::TcpOutbound | Self::WifiUdp | Self::Bluetooth | Self::Usb | Self::Other => None,
        }
    }

    fn title(self, id: InterfaceId, endpoint: Option<&str>) -> String {
        match (self, endpoint) {
            (Self::Bluetooth, _) | (_, None) => {
                format!("{} · {}", self.role(), peer_label(id))
            }
            (_, Some(endpoint)) => format!("{} · {}", self.role(), endpoint),
        }
    }
}

pub fn auto_wifi_peer_list_note(kind: &str) -> Option<&'static str> {
    (kind == "auto-wifi").then_some(
        "Each row is one path, not one device. UDP and TCP both ways are separate. Status is UDP peering. Health is the RNS plane. A TCP-out row that stays Retrying is usually the Wi-Fi gateway.",
    )
}

pub fn bluetooth_auto_peer_list_note(kind: &str) -> Option<&'static str> {
    (kind == "bluetooth-auto").then_some(
        "Status is the radio session. Health is the RNS plane. Details is GATT vs CoC. Radio only means the peer looks Connected while remote control will not work.",
    )
}

fn peer_health(
    kind: Option<InterfaceKind>,
    connection: ConnectionState,
    links: u32,
    destinations: u32,
    tx_bytes: u64,
    rx_bytes: u64,
    rate_bytes_per_sec: u32,
) -> Option<PeerHealth> {
    if !matches!(
        kind,
        Some(InterfaceKind::BluetoothPeer | InterfaceKind::WifiPeer)
    ) {
        return None;
    }
    if connection != ConnectionState::Connected {
        return None;
    }
    if links > 0 {
        return Some(PeerHealth::RnsLive);
    }
    let has_frames = tx_bytes > 0 || rx_bytes > 0 || rate_bytes_per_sec > 0;
    match (destinations > 0, has_frames) {
        (true, true) => Some(PeerHealth::RadioOnlyAnnouncedWithFrames),
        (true, false) => Some(PeerHealth::RadioOnlyAnnounced),
        (false, true) => Some(PeerHealth::RadioOnlyFrames),
        (false, false) => Some(PeerHealth::RadioOnlyIdle),
    }
}

/// Short display label from the first two hash bytes of an `InterfaceId`.
/// The full id is `kind ++ SHA-256(channel_tag)[:7]`. BLE tags the persisted
/// BleIdentity; that id (and this prefix) come back the same after a reboot
/// when the peer identity is unchanged. Aliases key on the full hex id.
fn peer_label(id: InterfaceId) -> String {
    format!("P {}", appearance_prefix(id))
}

fn appearance_prefix(id: InterfaceId) -> String {
    let bytes = id.as_bytes();
    match (bytes.get(1), bytes.get(2)) {
        (Some(first), Some(second)) => format!("{first:02x}{second:02x}"),
        (Some(_) | None, Some(_) | None) => "----".to_string(),
    }
}

fn bluetooth_auto_title(identity: BleIdentity) -> String {
    let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, identity.as_bytes());
    format!("bluetooth-auto {}", appearance_prefix(id))
}

fn generic_bluetooth_auto_title(name: &str) -> bool {
    matches!(name, "" | "bluetooth-auto" | "BLE")
        || name
            .strip_prefix("bluetooth-auto ")
            .is_some_and(|_| !bluetooth_auto_title_has_identity_suffix(name))
}

fn bluetooth_auto_title_has_identity_suffix(name: &str) -> bool {
    let Some(prefix) = name.strip_prefix("bluetooth-auto ") else {
        return false;
    };
    prefix.len() == 4 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn apply_bluetooth_auto_identity_title(entry: &mut InterfaceEntry, prefix: Option<&str>) {
    if entry.kind != "bluetooth-auto" || bluetooth_auto_title_has_identity_suffix(&entry.name) {
        return;
    }
    if let Some(prefix) = prefix.filter(|prefix| prefix.len() == 4) {
        entry.name = format!("bluetooth-auto {prefix}");
    }
}

/// The peer-facing `XXXX` for a managed node is that node's BLE identity.
/// A direct Bluetooth-peer route is keyed on that identity. A first-hop
/// supervisor, a relay, or some other local peer is a different radio.
fn bluetooth_auto_prefix_from_direct_peer(
    hops: u8,
    via: NextHop,
    interface: InterfaceId,
) -> Option<String> {
    if !matches!(via, NextHop::Direct) || hops > 1 {
        return None;
    }
    if interface.kind() != Some(InterfaceKind::BluetoothPeer) {
        return None;
    }
    Some(appearance_prefix(interface))
}

fn apply_peer_aliases(peers: &mut [InterfacePeer], aliases: &HashMap<String, String>) {
    for peer in peers {
        peer.alias = aliases
            .get(&peer.id)
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned);
    }
}

fn bluetooth_auto_name_prefix(name: &str) -> Option<String> {
    let prefix = name.strip_prefix("bluetooth-auto ")?.trim();
    (prefix.len() == 4 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| prefix.to_ascii_lowercase())
}

fn peer_interface_kind(peer_id: &str) -> Option<InterfaceKind> {
    parse_hex::<INTERFACE_ID_BYTES>(peer_id)
        .ok()
        .and_then(|bytes| InterfaceId::new(bytes).kind())
}

fn interface_appearance_prefix(peer_id: &str) -> Option<String> {
    let bytes = parse_hex::<INTERFACE_ID_BYTES>(peer_id).ok()?;
    Some(appearance_prefix(InterfaceId::new(bytes)).to_ascii_lowercase())
}

fn wifi_peer_id_from_link_local(address: IpAddr) -> Option<String> {
    let IpAddr::V6(v6) = address else {
        return None;
    };
    if !v6.is_unicast_link_local() {
        return None;
    }
    Some(encode_hex(
        InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, &v6.octets()).as_bytes(),
    ))
}

fn normalize_link_local(address: IpAddr) -> Option<String> {
    let IpAddr::V6(v6) = address else {
        return None;
    };
    if !v6.is_unicast_link_local() {
        return None;
    }
    Some(v6.to_string().to_ascii_lowercase())
}

fn normalize_stored_wifi_ll(value: &str) -> Option<String> {
    let addr = parse_ipv6_fact(value)?;
    normalize_link_local(addr)
}

fn endpoint_matches_wifi_ll_keys(endpoint: &str, keys: &HashSet<String>) -> bool {
    let Some(addr) = parse_ipv6_fact(endpoint) else {
        return false;
    };
    let Some(key) = normalize_link_local(addr) else {
        return false;
    };
    keys.contains(&key)
}

fn parse_ipv6_fact(value: &str) -> Option<IpAddr> {
    let host = value.split('%').next()?.trim();
    host.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

fn take_ipv6_group(group: &mut Option<String>) -> Option<String> {
    let value = group.as_deref()?.trim();
    if parse_ipv6_fact(value).is_none() {
        return None;
    }
    group.take()
}

fn remember_target_wifi_ll(session: &ControllerSession, target_id: &str, address: IpAddr) {
    let Some(key) = normalize_link_local(address) else {
        return;
    };
    let target_id = target_id.trim();
    if target_id.is_empty() {
        return;
    }
    let mut known = session
        .target_wifi_ll
        .lock()
        .expect("target wifi ll mutex poisoned");
    if known.get(&key).is_some_and(|linked| linked == target_id) {
        return;
    }
    known.insert(key, target_id.to_owned());
}

fn match_wifi_ll_to_target(session: &ControllerSession, endpoint: &str) -> Option<String> {
    let addr = parse_ipv6_fact(endpoint)?;
    let key = normalize_link_local(addr)?;
    session
        .target_wifi_ll
        .lock()
        .expect("target wifi ll mutex poisoned")
        .get(&key)
        .cloned()
}

fn remember_sibling_wifi_ll(session: &ControllerSession, instance_hash: &str, value: Option<&str>) {
    let id = instance_hash.trim();
    if id.is_empty() || !sibling_alias_is_syncable(id, &session.controller_identity.instance_hash) {
        return;
    }
    let mut known = session
        .sibling_wifi_ll
        .lock()
        .expect("sibling wifi ll mutex poisoned");
    match value.and_then(normalize_stored_wifi_ll) {
        Some(ll) => {
            if known.get(id).is_some_and(|stored| stored == &ll) {
                return;
            }
            known.insert(id.to_owned(), ll);
        }
        None => {
            if known.remove(id).is_none() {
                return;
            }
        }
    }
    persist_target_names(&session.sibling_wifi_ll_path, &known);
}

fn match_wifi_ll_to_sibling(session: &ControllerSession, endpoint: &str) -> Option<String> {
    let addr = parse_ipv6_fact(endpoint)?;
    let key = normalize_link_local(addr)?;
    session
        .sibling_wifi_ll
        .lock()
        .expect("sibling wifi ll mutex poisoned")
        .iter()
        .find_map(|(instance, ll)| (ll == &key).then(|| instance.clone()))
}

fn remember_wifi_peers_from_addresses(
    session: &ControllerSession,
    target_id: &str,
    extras: &[InterfaceFact],
) {
    for fact in extras {
        if fact.label != "IPv6" {
            continue;
        }
        let Some(addr) = parse_ipv6_fact(&fact.value) else {
            continue;
        };
        remember_target_wifi_ll(session, target_id, addr);
        let Some(peer_id) = wifi_peer_id_from_link_local(addr) else {
            continue;
        };
        remember_target_peer_id(session, target_id, &peer_id);
    }
}

fn remember_target_ble_prefix(session: &ControllerSession, target_id: &str, prefix: &str) {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.len() != 4 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return;
    }
    let mut prefixes = session
        .target_ble_prefixes
        .lock()
        .expect("target ble prefixes mutex poisoned");
    if prefixes
        .get(target_id)
        .is_some_and(|known| known == &prefix)
    {
        return;
    }
    prefixes.insert(target_id.to_owned(), prefix);
    persist_target_names(&session.target_ble_prefixes_path, &prefixes);
}

fn remember_target_peer_id(session: &ControllerSession, target_id: &str, peer_id: &str) {
    let peer_id = peer_id.trim();
    let target_id = target_id.trim();
    if peer_id.is_empty() || target_id.is_empty() || !peer_alias_is_syncable(peer_id) {
        return;
    }
    let mut known = session
        .target_peer_ids
        .lock()
        .expect("target peer ids mutex poisoned");
    if known.get(peer_id).is_some_and(|linked| linked == target_id) {
        return;
    }
    known.insert(peer_id.to_owned(), target_id.to_owned());
    persist_target_names(&session.target_peer_ids_path, &known);
}

fn match_peer_to_target(session: &ControllerSession, peer_id: &str) -> Option<String> {
    let peer_id = peer_id.trim();
    if peer_id.is_empty() {
        return None;
    }
    {
        let known = session
            .target_peer_ids
            .lock()
            .expect("target peer ids mutex poisoned");
        if let Some(target_id) = known.get(peer_id) {
            return Some(target_id.clone());
        }
    }
    let kind = peer_interface_kind(peer_id)?;
    if kind != InterfaceKind::BluetoothPeer {
        return None;
    }
    let prefix = interface_appearance_prefix(peer_id)?;
    let prefixes = session
        .target_ble_prefixes
        .lock()
        .expect("target ble prefixes mutex poisoned");
    prefixes
        .iter()
        .find(|(_, known)| known.as_str() == prefix)
        .map(|(target_id, _)| target_id.clone())
}

fn auto_fill_peer_alias(
    backend: Option<&RemoteControlBackend>,
    session: &ControllerSession,
    peer_id: &str,
    target_id: &str,
    alias: &str,
) -> Result<(), BackendError> {
    let alias = alias.trim();
    if alias.is_empty() || peer_id.is_empty() {
        return Ok(());
    }
    remember_target_peer_id(session, target_id, peer_id);
    let mut peers = session
        .peer_aliases
        .lock()
        .expect("peer aliases mutex poisoned");
    let mut links = session
        .peer_alias_links
        .lock()
        .expect("peer alias links mutex poisoned");
    let linked_here = links.get(peer_id).is_some_and(|linked| linked == target_id);
    let current = stored_alias(peers.get(peer_id).map(String::as_str));
    if current.as_deref() == Some(alias) {
        // Same string as the managed-node alias: keep/claim the auto link locally.
        if !linked_here {
            links.insert(peer_id.to_owned(), target_id.to_owned());
            persist_target_names(&session.peer_alias_links_path, &links);
        }
        drop(peers);
        drop(links);
        // Publish once if the roster does not already have this fact (no-op when unchanged).
        if let Some(backend) = backend {
            if peer_alias_is_syncable(peer_id) && peer_alias_value_is_syncable(Some(alias)) {
                backend.record_local_label(RosterLabelKind::PeerAlias, peer_id, Some(alias));
            }
        }
        return Ok(());
    }
    if current.is_some() && !linked_here {
        // Operator-owned (or sibling-synced manual) peer alias; do not overwrite.
        return Ok(());
    }
    peers.insert(peer_id.to_owned(), alias.to_owned());
    persist_target_names(&session.peer_aliases_path, &peers);
    links.insert(peer_id.to_owned(), target_id.to_owned());
    persist_target_names(&session.peer_alias_links_path, &links);
    drop(peers);
    drop(links);
    // Publish so siblings can name this peer before they ever connect to the node.
    // Remote TargetAlias apply passes backend=None so we do not echo PeerAlias back.
    if let Some(backend) = backend {
        if peer_alias_is_syncable(peer_id) && peer_alias_value_is_syncable(Some(alias)) {
            backend.record_local_label(RosterLabelKind::PeerAlias, peer_id, Some(alias));
        }
    }
    Ok(())
}

/// Name a wifi peer whose LL is this Controller's AutoWifi address. Local-only (no roster).
fn auto_fill_this_controller_peer_alias(
    session: &ControllerSession,
    peer_id: &str,
) -> Result<(), BackendError> {
    let peer_id = peer_id.trim();
    if peer_id.is_empty() {
        return Ok(());
    }
    let mut peers = session
        .peer_aliases
        .lock()
        .expect("peer aliases mutex poisoned");
    let mut links = session
        .peer_alias_links
        .lock()
        .expect("peer alias links mutex poisoned");
    let linked_here = links
        .get(peer_id)
        .is_some_and(|linked| linked == THIS_CONTROLLER_ALIAS_LINK);
    let current = stored_alias(peers.get(peer_id).map(String::as_str));
    if current.as_deref() == Some(THIS_CONTROLLER_PEER_ALIAS) {
        if !linked_here {
            links.insert(peer_id.to_owned(), THIS_CONTROLLER_ALIAS_LINK.to_owned());
            persist_target_names(&session.peer_alias_links_path, &links);
        }
        return Ok(());
    }
    if current.is_some() && !linked_here {
        return Ok(());
    }
    peers.insert(peer_id.to_owned(), THIS_CONTROLLER_PEER_ALIAS.to_owned());
    persist_target_names(&session.peer_aliases_path, &peers);
    links.insert(peer_id.to_owned(), THIS_CONTROLLER_ALIAS_LINK.to_owned());
    persist_target_names(&session.peer_alias_links_path, &links);
    Ok(())
}

/// Name a wifi peer whose LL is a sibling Controller. Local-only (no PeerAlias roster row).
fn auto_fill_sibling_controller_peer_alias(
    session: &ControllerSession,
    peer_id: &str,
    sibling_hash: &str,
    alias: &str,
) -> Result<(), BackendError> {
    let peer_id = peer_id.trim();
    let sibling_hash = sibling_hash.trim();
    let alias = alias.trim();
    if peer_id.is_empty() || sibling_hash.is_empty() || alias.is_empty() {
        return Ok(());
    }
    if !sibling_alias_is_syncable(sibling_hash, &session.controller_identity.instance_hash) {
        return Ok(());
    }
    let mut peers = session
        .peer_aliases
        .lock()
        .expect("peer aliases mutex poisoned");
    let mut links = session
        .peer_alias_links
        .lock()
        .expect("peer alias links mutex poisoned");
    let linked_here = links
        .get(peer_id)
        .is_some_and(|linked| linked == sibling_hash);
    let current = stored_alias(peers.get(peer_id).map(String::as_str));
    if current.as_deref() == Some(alias) {
        if !linked_here {
            links.insert(peer_id.to_owned(), sibling_hash.to_owned());
            persist_target_names(&session.peer_alias_links_path, &links);
        }
        return Ok(());
    }
    if current.is_some() && !linked_here {
        return Ok(());
    }
    peers.insert(peer_id.to_owned(), alias.to_owned());
    persist_target_names(&session.peer_aliases_path, &peers);
    links.insert(peer_id.to_owned(), sibling_hash.to_owned());
    persist_target_names(&session.peer_alias_links_path, &links);
    Ok(())
}

fn refresh_sibling_linked_peer_aliases(
    session: &ControllerSession,
    sibling_hash: &str,
    alias: Option<&str>,
) {
    let sibling_hash = sibling_hash.trim();
    if sibling_hash.is_empty() {
        return;
    }
    let linked = {
        let links = session
            .peer_alias_links
            .lock()
            .expect("peer alias links mutex poisoned");
        links
            .iter()
            .filter(|(_, linked)| linked.as_str() == sibling_hash)
            .map(|(peer_id, _)| peer_id.clone())
            .collect::<Vec<_>>()
    };
    match stored_alias(alias) {
        Some(alias) => {
            for peer_id in linked {
                let _ = auto_fill_sibling_controller_peer_alias(
                    session,
                    &peer_id,
                    sibling_hash,
                    &alias,
                );
            }
        }
        None => {
            let mut peers = session
                .peer_aliases
                .lock()
                .expect("peer aliases mutex poisoned");
            let mut links = session
                .peer_alias_links
                .lock()
                .expect("peer alias links mutex poisoned");
            let mut changed_peers = false;
            for peer_id in linked {
                links.remove(&peer_id);
                if peers.remove(&peer_id).is_some() {
                    changed_peers = true;
                }
            }
            persist_target_names(&session.peer_alias_links_path, &links);
            if changed_peers {
                persist_target_names(&session.peer_aliases_path, &peers);
            }
        }
    }
}

fn clear_linked_peer_aliases(
    backend: Option<&RemoteControlBackend>,
    session: &ControllerSession,
    target_id: &str,
    previous: Option<&str>,
) -> Result<(), BackendError> {
    let linked = {
        let links = session
            .peer_alias_links
            .lock()
            .expect("peer alias links mutex poisoned");
        links
            .iter()
            .filter(|(_, linked)| linked.as_str() == target_id)
            .map(|(peer_id, _)| peer_id.clone())
            .collect::<Vec<_>>()
    };
    let mut peers = session
        .peer_aliases
        .lock()
        .expect("peer aliases mutex poisoned");
    let mut links = session
        .peer_alias_links
        .lock()
        .expect("peer alias links mutex poisoned");
    let mut changed_peers = false;
    let mut cleared = Vec::new();
    for peer_id in linked {
        links.remove(&peer_id);
        let should_clear = match (previous, peers.get(&peer_id).map(String::as_str)) {
            (Some(prior), Some(current)) => current.trim() == prior,
            (_, Some(_)) => true,
            _ => false,
        };
        if should_clear {
            peers.remove(&peer_id);
            changed_peers = true;
            cleared.push(peer_id);
        }
    }
    persist_target_names(&session.peer_alias_links_path, &links);
    if changed_peers {
        persist_target_names(&session.peer_aliases_path, &peers);
    }
    drop(peers);
    drop(links);
    if let Some(backend) = backend {
        for peer_id in cleared {
            if peer_alias_is_syncable(&peer_id) {
                backend.record_local_label(RosterLabelKind::PeerAlias, &peer_id, None);
            }
        }
    }
    Ok(())
}

fn project_target_alias_to_peers(
    session: &ControllerSession,
    target_id: &str,
    alias: Option<&str>,
    previous: Option<&str>,
    extra_peer_ids: &[String],
    emit_roster: Option<&RemoteControlBackend>,
) -> Result<(), BackendError> {
    let linked_peers = {
        let links = session
            .peer_alias_links
            .lock()
            .expect("peer alias links mutex poisoned");
        links
            .iter()
            .filter(|(_, linked)| linked.as_str() == target_id)
            .map(|(peer_id, _)| peer_id.clone())
            .collect::<Vec<_>>()
    };
    let known_peers = {
        let known = session
            .target_peer_ids
            .lock()
            .expect("target peer ids mutex poisoned");
        known
            .iter()
            .filter(|(_, linked)| linked.as_str() == target_id)
            .map(|(peer_id, _)| peer_id.clone())
            .collect::<Vec<_>>()
    };
    let mut peer_ids = linked_peers;
    for peer_id in known_peers
        .into_iter()
        .chain(extra_peer_ids.iter().cloned())
    {
        if !peer_ids.iter().any(|seen| seen == &peer_id) {
            peer_ids.push(peer_id);
        }
    }
    match alias {
        Some(alias) => {
            for peer_id in &peer_ids {
                auto_fill_peer_alias(emit_roster, session, peer_id, target_id, alias)?;
            }
        }
        None => {
            clear_linked_peer_aliases(emit_roster, session, target_id, previous)?;
        }
    }
    Ok(())
}

fn clear_target_peer_identity(session: &ControllerSession, target_id: &str) {
    remove_alias_map_key(
        &session.target_ble_prefixes,
        &session.target_ble_prefixes_path,
        target_id,
    );
    {
        let mut known = session
            .target_peer_ids
            .lock()
            .expect("target peer ids mutex poisoned");
        let before = known.len();
        known.retain(|_, linked| linked != target_id);
        if known.len() != before {
            persist_target_names(&session.target_peer_ids_path, &known);
        }
    }
    // Local forget only; sibling clears arrive via TargetAlias/PeerAlias roster labels.
    let _ = clear_linked_peer_aliases(None, session, target_id, None);
}

pub fn interface_mode_label(mode: InterfaceMode) -> &'static str {
    mode.label()
}

fn connection_label(kind: Option<InterfaceKind>, connection: ConnectionState) -> String {
    match connection {
        ConnectionState::Initializing => "Initializing".to_string(),
        ConnectionState::Connected => "Connected".to_string(),
        ConnectionState::Degraded => "Degraded".to_string(),
        ConnectionState::Reconnecting => "Retrying".to_string(),
        ConnectionState::Failed => "Failed".to_string(),
        ConnectionState::Disconnected => match kind {
            Some(
                InterfaceKind::AutoWifi
                | InterfaceKind::WifiDirect
                | InterfaceKind::WifiAware
                | InterfaceKind::UsbAutoHost
                | InterfaceKind::UsbAutoDevice,
            ) => "Waiting".to_string(),
            Some(InterfaceKind::BluetoothAuto) => "No Peers".to_string(),
            Some(_) | None => "Disconnected".to_string(),
        },
        ConnectionState::Disabled => "Off".to_string(),
        ConnectionState::Unknown => "Unknown".to_string(),
    }
}

pub(crate) fn apply_tcp_target_to_entry(entry: &mut InterfaceEntry, target: &str) {
    let mut parts = Vec::new();
    if let Some(size) = entry.ifac_bytes {
        parts.push(format!("IFAC {size}"));
    }
    parts.push(target.to_string());
    let detail = parts.join(" · ");
    let kind = InterfaceKind::ALL
        .into_iter()
        .find(|kind| kind.name() == entry.kind);
    entry.detail = Some(detail.clone());
    entry.extras = hopspot_extra_facts(Some(&detail), kind, None);
}

pub(crate) fn apply_wifi_station_to_entry(entry: &mut InterfaceEntry, ssid: &str) {
    let detail = personal_rns::remote_control::wifi_station_inventory_config(ssid)
        .map(|config| config.to_string())
        .unwrap_or_else(|_| format!("W,{ssid}"));
    let kind = InterfaceKind::ALL
        .into_iter()
        .find(|kind| kind.name() == entry.kind);
    entry.detail = Some(detail.clone());
    entry.extras = hopspot_extra_facts(Some(&detail), kind, None);
}

pub(crate) fn apply_lora_profile_to_entry(entry: &mut InterfaceEntry, profile: RadioProfile) {
    let detail = profile.inventory_config().to_string();
    let kind = InterfaceKind::ALL
        .into_iter()
        .find(|kind| kind.name() == entry.kind);
    entry.detail = Some(detail.clone());
    entry.extras = hopspot_extra_facts(Some(&detail), kind, None);
}

fn hopspot_extra_facts(
    detail: Option<&str>,
    kind: Option<InterfaceKind>,
    drops: Option<(u32, u32)>,
) -> Vec<InterfaceFact> {
    let mut extras = Vec::new();
    if let Some((egress, ingress)) = drops {
        if egress > 0 {
            extras.push(interface_fact("Egress drops", egress.to_string()));
        }
        if ingress > 0 {
            extras.push(interface_fact("RX drops", ingress.to_string()));
        }
    }
    if let Some(detail) = detail.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(profile) = RadioProfile::parse_inventory_config(detail) {
            extras.push(interface_fact("Config", "LoRa".to_string()));
            extras.extend(lora_tune_facts(&profile));
        } else if let Some(ssid) = parse_wifi_station_ssid(detail) {
            extras.push(interface_fact(
                "SSID",
                if ssid.is_empty() {
                    "not set".to_string()
                } else {
                    ssid.to_string()
                },
            ));
            if let Some(rssi) = parse_wifi_station_rssi_dbm(detail) {
                extras.push(rssi_fact(RssiDbm::new(rssi)));
            } else if kind == Some(InterfaceKind::AutoWifi) {
                extras.push(rssi_unavailable_fact());
            }
        } else {
            for part in detail.split(" · ") {
                let part = part.trim();
                if let Some(size) = part.strip_prefix("IFAC ") {
                    extras.push(interface_fact("IFAC", size.to_string()));
                } else if let Some((host, port)) = split_host_port(part) {
                    extras.push(interface_fact("Host", host.to_string()));
                    extras.push(interface_fact("Port", port.to_string()));
                } else if part == "LoRa" {
                    extras.push(interface_fact("Config", part.to_string()));
                } else if !part.is_empty() {
                    extras.push(interface_fact("Role", part.to_string()));
                }
            }
        }
    } else if matches!(kind, Some(InterfaceKind::LoRa | InterfaceKind::Rnode)) {
        extras.push(interface_fact("Config", "LoRa".to_string()));
    } else if let Some(role) = kind.and_then(interface_role_label) {
        extras.push(interface_fact("Role", role.to_string()));
    }
    extras
}

fn lora_tune_facts(profile: &RadioProfile) -> Vec<InterfaceFact> {
    let Modulation::Lora {
        spreading_factor,
        bandwidth,
        coding_rate,
    } = profile.modulation();
    let preset = ModemPreset::matching(profile.modulation())
        .map(ModemPreset::label)
        .unwrap_or("Custom");
    vec![
        interface_fact("Region", profile.region().label().to_string()),
        interface_fact(
            "Frequency",
            format_lora_frequency_mhz(profile.frequency().hz()),
        ),
        interface_fact("Preset", preset.to_string()),
        interface_fact("SF", (spreading_factor as u8).to_string()),
        interface_fact("Bandwidth", format!("{} kHz", bandwidth.hz() / 1_000)),
        interface_fact("CR", format!("4/{}", coding_rate.denominator())),
        interface_fact("TX power", format!("{} dBm", profile.tx_power().dbm())),
        interface_fact("Preamble", profile.preamble().count().to_string()),
    ]
}

fn format_lora_frequency_mhz(hz: u32) -> String {
    format!("{}.{:03} MHz", hz / 1_000_000, (hz % 1_000_000) / 1_000)
}

fn interface_fact(label: &str, value: String) -> InterfaceFact {
    InterfaceFact {
        label: label.to_string(),
        value,
    }
}

fn split_host_port(value: &str) -> Option<(&str, &str)> {
    let (host, port) = value.rsplit_once(':')?;
    if host.is_empty() || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((host, port))
}

fn interface_role_label(kind: InterfaceKind) -> Option<&'static str> {
    match kind {
        InterfaceKind::AutoWifi => Some("Auto discovery"),
        InterfaceKind::LocalServer => Some("Shared instance"),
        InterfaceKind::TcpServer => Some("TCP listen"),
        InterfaceKind::TcpClient => Some("TCP dial"),
        InterfaceKind::WebSocketServer => Some("WebSocket listen"),
        InterfaceKind::WebSocketClient => Some("WebSocket dial"),
        InterfaceKind::BackboneServer => Some("Backbone listen"),
        InterfaceKind::BackboneClient => Some("Backbone dial"),
        InterfaceKind::BluetoothAuto => Some("BLE supervisor"),
        InterfaceKind::UsbAutoHost | InterfaceKind::UsbAutoDevice => Some("USB"),
        InterfaceKind::LoRa | InterfaceKind::Rnode => Some("LoRa"),
        InterfaceKind::EspNow => Some("ESP-NOW"),
        _ => None,
    }
}

fn local_frame_drops(coverage: &FrameAccountingCoverage) -> Option<(u32, u32)> {
    let accounting = coverage.complete()?;
    let ingress = accounting
        .malformed
        .saturating_add(accounting.undecodable)
        .min(u64::from(u32::MAX)) as u32;
    let egress = 0;
    (ingress > 0).then_some((egress, ingress))
}

fn activity_connection_code(connection: &str) -> u8 {
    match connection {
        "Connected" => 1,
        "Degraded" => 2,
        "Initializing" => 3,
        "Retrying" => 4,
        "Failed" => 5,
        "Waiting" | "No Peers" | "Disconnected" => 6,
        "Off" => 7,
        _ => 0,
    }
}

const RSSI_LABEL: &str = "RSSI";
const SNR_LABEL: &str = "SNR";
const QUALITY_LABEL: &str = "Quality";
const RSSI_UNAVAILABLE: &str = "not available";
const RSSI_PENDING: &str = "pending";

pub fn radio_facts(radio: RadioIndication) -> Vec<InterfaceFact> {
    match radio {
        RadioIndication::NotRadio => Vec::new(),
        RadioIndication::Bluetooth(BluetoothIndication::Pending)
        | RadioIndication::Wifi(WifiIndication::Pending)
        | RadioIndication::LoRa(LoRaIndication::Pending) => vec![InterfaceFact {
            label: RSSI_LABEL.to_string(),
            value: RSSI_PENDING.to_string(),
        }],
        RadioIndication::Wifi(WifiIndication::Unavailable) => vec![InterfaceFact {
            label: RSSI_LABEL.to_string(),
            value: RSSI_UNAVAILABLE.to_string(),
        }],
        RadioIndication::Bluetooth(BluetoothIndication::Rssi(rssi))
        | RadioIndication::Wifi(WifiIndication::Rssi(rssi)) => vec![rssi_fact(rssi)],
        RadioIndication::LoRa(LoRaIndication::Sample { rssi, snr, quality }) => {
            let mut facts = vec![rssi_fact(rssi)];
            if let Some(snr) = snr {
                facts.push(InterfaceFact {
                    label: SNR_LABEL.to_string(),
                    value: format_snr(snr),
                });
            }
            if let Some(quality) = quality {
                facts.push(InterfaceFact {
                    label: QUALITY_LABEL.to_string(),
                    value: format_quality(quality),
                });
            }
            facts
        }
    }
}

fn rssi_unavailable_fact() -> InterfaceFact {
    InterfaceFact {
        label: RSSI_LABEL.to_string(),
        value: RSSI_UNAVAILABLE.to_string(),
    }
}

fn rssi_fact(rssi: RssiDbm) -> InterfaceFact {
    InterfaceFact {
        label: RSSI_LABEL.to_string(),
        value: format!("{} dBm", rssi.get()),
    }
}

fn extend_radio_facts(extras: &mut Vec<InterfaceFact>, radio: RadioIndication) {
    if extras.iter().any(|fact| fact.label == RSSI_LABEL) {
        return;
    }
    extras.extend(radio_facts(radio));
}

fn ensure_auto_wifi_rssi_fact(extras: &mut Vec<InterfaceFact>, kind: Option<InterfaceKind>) {
    if kind != Some(InterfaceKind::AutoWifi) {
        return;
    }
    if extras.iter().any(|fact| fact.label == RSSI_LABEL) {
        return;
    }
    extras.push(rssi_unavailable_fact());
}

fn format_snr(snr: SnrQuarterDb) -> String {
    let tenths = i32::from(snr.quarters()).saturating_mul(10) / 4;
    let magnitude = tenths.unsigned_abs();
    let sign = if tenths < 0 { "-" } else { "" };
    format!("{sign}{}.{} dB", magnitude / 10, magnitude % 10)
}

fn format_quality(quality: SignalQualityTenthsPercent) -> String {
    let tenths = quality.tenths_percent();
    format!("{}.{}%", tenths / 10, tenths % 10)
}

pub fn format_activity_age(age_secs: Option<u32>) -> String {
    match age_secs {
        None => "—".to_string(),
        Some(0) => "now".to_string(),
        Some(seconds) if seconds < 60 => format!("{seconds}s"),
        Some(seconds) if seconds < 3600 => format!("{}m", seconds / 60),
        Some(seconds) => format!("{}h", seconds / 3600),
    }
}

fn interface_power_from_connection(connection: ConnectionState) -> InterfacePower {
    match connection {
        ConnectionState::Disabled => InterfacePower::Off,
        ConnectionState::Connected
        | ConnectionState::Degraded
        | ConnectionState::Initializing
        | ConnectionState::Reconnecting
        | ConnectionState::Failed
        | ConnectionState::Disconnected
        | ConnectionState::Unknown => InterfacePower::On,
    }
}

fn stored_alias(stored: Option<&str>) -> Option<String> {
    stored
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn load_target_names(path: &PathBuf) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    parse_target_names(&text)
}

fn persist_target_names(path: &PathBuf, names: &HashMap<String, String>) {
    let _ = std::fs::write(path, render_target_names(names));
}

fn format_tcp_endpoint(endpoint: &prns_flash_manifest::TcpClientEndpoint) -> String {
    match &endpoint.host {
        prns_flash_manifest::TcpClientHost::Ipv4(address) => {
            format!("{address}:{}", endpoint.port)
        }
        prns_flash_manifest::TcpClientHost::Hostname(host) => {
            format!("{host}:{}", endpoint.port)
        }
    }
}

fn parse_tcp_dial_target(value: &str) -> Result<String, String> {
    let endpoint =
        prns_flash_manifest::TcpClientEndpoint::parse(value).map_err(|error| error.to_string())?;
    Ok(format_tcp_endpoint(&endpoint))
}

fn load_persisted_tcp_target(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_tcp_dial_target(text.trim()).ok()
}

fn persist_tcp_target(path: &Path, target: &str) {
    let _ = std::fs::write(path, format!("{target}\n"));
}

fn resolve_controller_tcp_target(
    env_target: Option<&str>,
    stored_target: Option<String>,
) -> Result<(String, bool), String> {
    if let Some(value) = env_target.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok((parse_tcp_dial_target(value)?, true));
    }
    if let Some(stored) = stored_target.filter(|value| !value.is_empty()) {
        return Ok((stored, false));
    }
    Ok((DEFAULT_TCP_TARGET.to_string(), false))
}

fn parse_target_names(text: &str) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for line in text.lines() {
        let Some((id, name)) = line.split_once('\t') else {
            continue;
        };
        let id = id.trim();
        let name = name.trim();
        if id.is_empty() || name.is_empty() {
            continue;
        }
        names.insert(id.to_owned(), name.to_owned());
    }
    names
}

fn render_target_names(names: &HashMap<String, String>) -> String {
    let mut lines = names
        .iter()
        .map(|(id, name)| format!("{id}\t{name}"))
        .collect::<Vec<_>>();
    lines.sort();
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

fn parse_allow_list_key(
    input: &str,
) -> Result<personal_rns::remote_control::RemoteControlControllerIdentity, BackendError> {
    let input = input.trim();
    if parse_hex::<IDENTITY_HASH_BYTES>(input).is_ok() {
        return Err(BackendError::Operation {
            operation: "authorize controller",
            detail: "paste the allow-list key from that controller's Settings, not the 32-character identity hash".to_string(),
        });
    }
    let bytes =
        parse_hex::<IDENTITY_PUBLIC_KEY_LEN>(input).map_err(|_| BackendError::Operation {
            operation: "authorize controller",
            detail: "allow-list key must be 128 hex characters from that controller's Settings"
                .to_string(),
        })?;
    parse_controller_public_keys(&bytes).ok_or(BackendError::Operation {
        operation: "authorize controller",
        detail: "that allow-list key is not a valid controller public key".to_string(),
    })
}

fn controller_whitelist_exchange_error(error: RemoteControlTargetOperationError) -> BackendError {
    match error {
        RemoteControlTargetOperationError::Exchange(RemoteControlError::Remote(
            RemoteControlProtocolError::UnknownRequestKind { found },
        )) if found == RemoteControlRequestKind::InventoryControllers.wire_value()
            || found == RemoteControlRequestKind::AuthorizeController.wire_value()
            || found == RemoteControlRequestKind::RevokeController.wire_value() =>
        {
            BackendError::Operation {
                operation: "node management whitelist",
                detail: "the target firmware does not recognize controller allow-list edits yet. Flash the current Hopspot build onto that board; an existing pair already has the grant".to_string(),
            }
        }
        error => operation("node management whitelist", error),
    }
}

fn wifi_station_exchange_error(error: RemoteControlTargetOperationError) -> BackendError {
    match error {
        RemoteControlTargetOperationError::Exchange(RemoteControlError::Remote(
            RemoteControlProtocolError::UnknownRequestKind { found },
        )) if found == RemoteControlRequestKind::SetInterfaceWifiStation.wire_value() => {
            BackendError::Operation {
                operation: "set interface Wi-Fi station",
                detail: "the target firmware does not recognize Wi-Fi station edits yet. Flash the current Hopspot build onto that board; an existing pair already has the grant".to_string(),
            }
        }
        error => operation("set interface Wi-Fi station", error),
    }
}

fn lora_profile_exchange_error(error: RemoteControlTargetOperationError) -> BackendError {
    match error {
        RemoteControlTargetOperationError::Exchange(RemoteControlError::Remote(
            RemoteControlProtocolError::UnknownRequestKind { found },
        )) if found == RemoteControlRequestKind::SetInterfaceLoRaProfile.wire_value() => {
            BackendError::Operation {
                operation: "set interface LoRa profile",
                detail: "the target firmware does not recognize LoRa tune yet. Flash the current Hopspot build onto that board; an existing pair already has the grant".to_string(),
            }
        }
        error => operation("set interface LoRa profile", error),
    }
}

fn operation(operation: &'static str, error: impl std::fmt::Debug) -> BackendError {
    BackendError::Operation {
        operation,
        detail: format!("{error:?}"),
    }
}

fn inventory_recovery_continues(now: Instant, deadline: Instant) -> bool {
    now < deadline
}

fn should_wait_for_control_announce(
    wait: RemoteControlAnnounceWait,
    path: Option<&TargetPath>,
) -> bool {
    match wait {
        RemoteControlAnnounceWait::UntilHeard => path.is_none(),
        RemoteControlAnnounceWait::UntilRefreshed => true,
    }
}

#[cfg(test)]
fn local_interface_is_live(connection: &str) -> bool {
    matches!(connection, "Connected" | "Degraded")
}

#[cfg(test)]
fn interface_is_usb_auto(kind: &str) -> bool {
    matches!(kind, "usb-auto-host" | "usb-auto-device")
}

#[cfg(test)]
fn hop_interface_is_live(interface: &str, locals: &[InterfaceEntry]) -> bool {
    locals.iter().any(|entry| {
        local_interface_is_live(&entry.connection) && hop_kind_matches_local(interface, &entry.kind)
    })
}

#[cfg(test)]
fn hop_kind_matches_local(hop: &str, local: &str) -> bool {
    hop == local
        || (interface_is_usb_auto(hop) && interface_is_usb_auto(local))
        || matches!(
            (hop, local),
            ("bluetooth-peer", "bluetooth-auto") | ("wifi-peer", "auto-wifi")
        )
}

/// Drop a stored hop only when that hop's interface is down. Routing
/// picks USB vs BLE; this app does not pin a transport.
#[cfg(test)]
fn should_forget_control_route(path: Option<&TargetPath>, locals: &[InterfaceEntry]) -> bool {
    match path {
        Some(path) => !hop_interface_is_live(&path.interface, locals),
        None => false,
    }
}

fn control_announce_satisfies(
    heard: Option<InstantMillis>,
    wait: RemoteControlAnnounceWait,
    baseline: Option<InstantMillis>,
) -> bool {
    let Some(learned_at) = heard else {
        return false;
    };
    match wait {
        RemoteControlAnnounceWait::UntilHeard => true,
        RemoteControlAnnounceWait::UntilRefreshed => match baseline {
            None => true,
            Some(prior) => learned_at != prior,
        },
    }
}

fn path_from_route(
    route: &RouteSnapshot,
    peer_aliases: &HashMap<String, String>,
    target_aliases: &HashMap<String, String>,
) -> TargetPath {
    TargetPath {
        hops: route.hops,
        via: format_next_hop(route.via, route.interface, peer_aliases, target_aliases),
        interface: format_interface(route.interface),
        announced_at: format_announce_millis(route.learned_at.0),
    }
}

fn path_is_direct_ble(path: Option<&TargetPath>) -> bool {
    path.is_some_and(|path| {
        path.hops <= 1 && path.via == "direct" && path.interface == "bluetooth-peer"
    })
}

fn path_is_better_than(candidate: &TargetPath, current: Option<&TargetPath>) -> bool {
    let Some(current) = current else {
        return true;
    };
    if path_is_direct_ble(Some(candidate)) && !path_is_direct_ble(Some(current)) {
        return true;
    }
    candidate.hops < current.hops
}

fn path_from_announce(hops: u8, interface: InterfaceId, observed_at: InstantMillis) -> TargetPath {
    TargetPath {
        hops,
        via: if hops == 0 {
            "direct".to_string()
        } else {
            "relayed".to_string()
        },
        interface: format_interface(interface),
        announced_at: format_announce_millis(observed_at.0),
    }
}

fn format_next_hop(
    via: NextHop,
    interface: InterfaceId,
    peer_aliases: &HashMap<String, String>,
    target_aliases: &HashMap<String, String>,
) -> String {
    match via {
        NextHop::Direct => "direct".to_string(),
        NextHop::Via(transport) => format!(
            "via {}",
            via_hop_label(transport, interface, peer_aliases, target_aliases)
        ),
    }
}

/// Prefer an operator alias for the first-hop peer or the via identity; otherwise
/// show the durable `P XXXX` peer label when the hop is a fleet member interface.
fn via_hop_label(
    transport: personal_rns::TransportId,
    interface: InterfaceId,
    peer_aliases: &HashMap<String, String>,
    target_aliases: &HashMap<String, String>,
) -> String {
    let interface_hex = encode_hex(interface.as_bytes());
    let transport_hex = encode_hex(transport.as_bytes());
    if let Some(alias) = stored_alias(peer_aliases.get(&interface_hex).map(String::as_str)) {
        return alias;
    }
    if let Some(alias) = stored_alias(target_aliases.get(&transport_hex).map(String::as_str)) {
        return alias;
    }
    if interface
        .kind()
        .is_some_and(|kind| kind.supervisor_kind().is_some())
    {
        return peer_label(interface);
    }
    short_id(&transport_hex).to_string()
}

fn format_interface(interface: InterfaceId) -> String {
    route_interface_kind(interface).name().to_string()
}

/// USB Auto host ids are a `[0xD0; 8]` cookie, not `kind ++ hash`, so
/// `InterfaceId::kind` is `None`. Settings already stamps those as
/// `UsbAutoHost`; route lines use the same override.
fn route_interface_kind(interface: InterfaceId) -> InterfaceKind {
    match interface.kind() {
        Some(kind) => kind,
        None => InterfaceKind::UsbAutoHost,
    }
}

fn format_hop_count(hops: u8) -> String {
    if hops == 1 {
        "1 hop".to_string()
    } else {
        format!("{hops} hops")
    }
}

#[allow(dead_code)]
pub fn format_target_announce(path: Option<&TargetPath>) -> String {
    match path {
        Some(path) => format!("Last announce {}", path.announced_at),
        None => "Last announce not heard yet".to_string(),
    }
}

pub fn format_target_route(path: Option<&TargetPath>) -> String {
    match path {
        Some(path) => format!(
            "{} {} on {}",
            format_hop_count(path.hops),
            path.via,
            path.interface
        ),
        None => "No path heard yet".to_string(),
    }
}

/// Managed Nodes battery row: state of charge and external power are independent.
/// Show both when known (e.g. `73% · USB`, `73% · charging`); omit only when fully unknown.
#[must_use]
pub fn format_managed_node_battery(
    snapshot: prns_core::capabilities::power::PowerSnapshot,
) -> Option<String> {
    use prns_core::capabilities::power::{ChargingState, ExternalPowerState};

    match (snapshot.battery(), snapshot.external_power()) {
        (
            Some(percent),
            ExternalPowerState::Present {
                charging: ChargingState::Charging,
            },
        ) => Some(format!("{}% · charging", percent.get())),
        (Some(percent), ExternalPowerState::Present { .. }) => {
            Some(format!("{}% · USB", percent.get()))
        }
        (Some(percent), _) => Some(format!("{}%", percent.get())),
        (
            None,
            ExternalPowerState::Present {
                charging: ChargingState::Charging,
            },
        ) => Some("USB · charging".to_string()),
        (None, ExternalPowerState::Present { .. }) => Some("USB".to_string()),
        _ => None,
    }
}

fn stamp_interface_arrival(entry: &mut InterfaceEntry) {
    entry.arrived_at = Some(format_wall_clock_now());
}

fn stamp_interface_arrivals(items: &mut [InterfaceEntry]) {
    let at = format_wall_clock_now();
    for item in items {
        item.arrived_at = Some(at.clone());
    }
}

pub fn format_wall_clock_now() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format_announce_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

fn format_announce_millis(millis: u64) -> String {
    let unix_secs = i64::try_from(millis / 1_000).unwrap_or(0);
    match local_civil(unix_secs) {
        Some((year, month, day, hour, minute, second)) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
        }
        None => format_utc_millis(millis),
    }
}

#[cfg(unix)]
fn local_civil(unix_secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let time = libc::time_t::try_from(unix_secs).ok()?;
    let ptr = unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some((
        tm.tm_year.checked_add(1900)?,
        u32::try_from(tm.tm_mon.checked_add(1)?).ok()?,
        u32::try_from(tm.tm_mday).ok()?,
        u32::try_from(tm.tm_hour).ok()?,
        u32::try_from(tm.tm_min).ok()?,
        u32::try_from(tm.tm_sec).ok()?,
    ))
}

#[cfg(not(unix))]
fn local_civil(_unix_secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    None
}

fn format_utc_millis(millis: u64) -> String {
    let (year, month, day, hour, minute, second) = utc_date_time(millis / 1_000);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

fn utc_date_time(seconds: u64) -> (i32, u32, u32, u32, u32, u32) {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    let days = seconds.div_euclid(86_400);
    let time_of_day = u32::try_from(seconds.rem_euclid(86_400)).unwrap_or(0);
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = u32::try_from(shifted - era * 146_097).unwrap_or(0);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = i32::try_from(year_of_era).unwrap_or(0) + i32::try_from(era).unwrap_or(0) * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_period = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_period + 2) / 5 + 1;
    let month = if month_period < 10 {
        month_period + 3
    } else {
        month_period - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::{
        appearance_prefix, apply_bluetooth_auto_identity_title, apply_peer_aliases,
        apply_remote_card, attach_auto_wifi_local_addresses, attach_our_side_of_the_link,
        attach_usb_link_peer, auto_wifi_peer_list_note, bluetooth_auto_name_prefix,
        bluetooth_auto_peer_list_note, bluetooth_auto_prefix_from_direct_peer,
        bluetooth_auto_title, clone_announce_is_usb_local, control_announce_satisfies,
        controller_identity_secret_path, encode_hex, endpoint_matches_wifi_ll_keys,
        format_activity_age, format_announce_millis, format_connect_label, format_hop_count,
        format_interface, format_managed_node_battery, format_next_hop, format_pairing_open_label,
        format_target_announce, format_target_route, format_utc_millis,
        generic_bluetooth_auto_title, instance_identity_secret_path, interface_peer,
        interface_peer_from_wire, interface_power_from_connection, inventory_recovery_continues,
        load_persisted_tcp_target, local_interface_config, local_interface_entry,
        managed_targets_from_disk, monitor_remaining_at, normalize_stored_wifi_ll,
        operator_interface_kind, operator_local_kind, parse_invitation_code, parse_target_names,
        parse_tcp_dial_target, path_is_better_than, path_is_direct_ble, peer_label,
        persist_tcp_target, radio_facts, remote_interface_entry, render_target_names,
        resolve_controller_tcp_target, resolve_paired_target_hash, route_interface_kind,
        should_forget_control_route, should_wait_for_control_announce, stored_alias, target_label,
        BackendError, InterfaceEntry, InterfaceFact, InterfacePower, PeerHealth,
        RemoteControlAnnounceWait, TargetPath, TargetStatus, CONTROLLER_IDENTITY_FILE,
        DEFAULT_TCP_TARGET, INSTANCE_IDENTITY_FILE, MANAGER_ALIASES_FILE, TARGET_MONITOR_TTL,
        THIS_CONTROLLER_PEER_ALIAS,
    };
    use personal_rns::identity::IdentityHash;
    use personal_rns::interfaces::bluetooth_auto::BleIdentity;
    use personal_rns::interfaces::{
        ConnectionState, InterfaceGravity, InterfaceId, InterfaceKind, InterfaceMode,
        InterfaceSnapshot, Membership, PacketPhyStats, PeerDetails, RadioIndication, RssiDbm,
        SignalQualityTenthsPercent, SnrQuarterDb, WifiIndication,
    };
    use personal_rns::remote_control::{
        RemoteControlInterfaceCard, RemoteControlInterfaceEntry, RemoteControlInterfacePeer,
    };
    use personal_rns::routing::NextHop;
    use personal_rns::units::InstantMillis;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn env_tcp_target_wins_is_canonical_and_starts_on() {
        assert_eq!(
            resolve_controller_tcp_target(
                Some("https://Node.Example.:5252/path"),
                Some("127.0.0.1:4242".to_string()),
            )
            .expect("valid env target"),
            ("node.example:5252".to_string(), true)
        );
    }

    #[test]
    fn stored_tcp_target_is_used_when_env_is_absent() {
        assert_eq!(
            resolve_controller_tcp_target(None, Some("192.0.2.10:4242".to_string()))
                .expect("stored target"),
            ("192.0.2.10:4242".to_string(), false)
        );
    }

    #[test]
    fn connect_label_starts_at_zero_and_resets_to_ten_minutes() {
        assert_eq!(format_connect_label(0), "Connect 00:00");
        assert_eq!(
            format_connect_label(u32::try_from(TARGET_MONITOR_TTL.as_secs()).unwrap()),
            "Connect 10:00"
        );
    }

    #[test]
    fn pairing_open_label_formats_minutes_and_seconds() {
        assert_eq!(format_pairing_open_label(0), "Open 00:00");
        assert_eq!(format_pairing_open_label(185), "Open 03:05");
    }

    #[test]
    fn managed_node_battery_labels_cover_percent_charging_and_usb() {
        use prns_core::capabilities::power::{
            BatteryPercent, ChargingState, ExternalPowerState, PowerSnapshot,
        };

        assert_eq!(format_managed_node_battery(PowerSnapshot::UNKNOWN), None);
        assert_eq!(
            format_managed_node_battery(PowerSnapshot::new(
                Some(BatteryPercent::saturating(73)),
                ExternalPowerState::Absent,
            )),
            Some("73%".to_string())
        );
        assert_eq!(
            format_managed_node_battery(PowerSnapshot::new(
                Some(BatteryPercent::saturating(73)),
                ExternalPowerState::Present {
                    charging: ChargingState::Unknown,
                },
            )),
            Some("73% · USB".to_string())
        );
        assert_eq!(
            format_managed_node_battery(PowerSnapshot::new(
                Some(BatteryPercent::saturating(73)),
                ExternalPowerState::Present {
                    charging: ChargingState::Charging,
                },
            )),
            Some("73% · charging".to_string())
        );
        assert_eq!(
            format_managed_node_battery(PowerSnapshot::new(
                None,
                ExternalPowerState::Present {
                    charging: ChargingState::Unknown,
                },
            )),
            Some("USB".to_string())
        );
        assert_eq!(
            format_managed_node_battery(PowerSnapshot::new(
                None,
                ExternalPowerState::Present {
                    charging: ChargingState::Charging,
                },
            )),
            Some("USB · charging".to_string())
        );
    }

    #[test]
    fn monitor_remaining_is_zero_when_idle_expired_or_a_sibling_holds_without_local_connect() {
        let now = Instant::now();
        assert_eq!(
            monitor_remaining_at(None, now, crate::roster_sync::TargetAttention::Free),
            0
        );
        assert_eq!(
            monitor_remaining_at(
                None,
                now,
                crate::roster_sync::TargetAttention::HeldBySibling,
            ),
            0
        );
        assert_eq!(
            monitor_remaining_at(
                Some(now),
                now + Duration::from_secs(1),
                crate::roster_sync::TargetAttention::HeldByThis,
            ),
            0
        );
        assert_eq!(
            monitor_remaining_at(
                Some(now + TARGET_MONITOR_TTL),
                now,
                crate::roster_sync::TargetAttention::HeldBySibling,
            ),
            u32::try_from(TARGET_MONITOR_TTL.as_secs()).unwrap()
        );
        let remaining = monitor_remaining_at(
            Some(now + Duration::from_secs(90)),
            now,
            crate::roster_sync::TargetAttention::HeldByThis,
        );
        assert!((89..=90).contains(&remaining));
    }

    #[test]
    fn managed_targets_from_disk_union_persist_replica_and_names() {
        let persist = IdentityHash::new([0x11; 16]);
        let replica_only = IdentityHash::new([0x22; 16]);
        let named = IdentityHash::new([0x33; 16]);
        let forgotten = IdentityHash::new([0x44; 16]);
        let mut replica = crate::roster_sync::RosterReplica::default();
        crate::roster_sync::note_local_upsert(&mut replica, replica_only);
        crate::roster_sync::note_local_upsert(&mut replica, forgotten);
        crate::roster_sync::forget_target_locally(&mut replica, forgotten);
        let persist_hex = encode_hex(persist.as_bytes());
        let replica_hex = encode_hex(replica_only.as_bytes());
        let named_hex = encode_hex(named.as_bytes());
        let forgotten_hex = encode_hex(forgotten.as_bytes());
        let names = HashMap::from([
            (persist_hex.clone(), "Kitchen".to_string()),
            (named_hex.clone(), "Named only".to_string()),
            (forgotten_hex, "Gone".to_string()),
        ]);
        let items = managed_targets_from_disk(&[persist, forgotten], &replica, &names);
        let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
        assert_eq!(ids, vec![persist_hex.clone(), replica_hex, named_hex]);
        assert_eq!(
            items
                .iter()
                .find(|item| item.id == persist_hex)
                .map(|item| item.name.as_str()),
            Some("Kitchen")
        );
        assert!(items
            .iter()
            .all(|item| item.status == TargetStatus::Offline));
    }

    #[test]
    fn default_tcp_target_when_nothing_is_configured() {
        assert_eq!(
            resolve_controller_tcp_target(None, None).expect("default target"),
            (DEFAULT_TCP_TARGET.to_string(), false)
        );
    }

    #[test]
    fn invalid_env_tcp_target_is_rejected() {
        assert!(resolve_controller_tcp_target(Some("[2001:db8::1]:4242"), None).is_err());
        assert!(resolve_controller_tcp_target(Some("0.0.0.0:4242"), None).is_err());
    }

    #[test]
    fn host_only_tcp_target_gets_the_default_port() {
        assert_eq!(
            parse_tcp_dial_target("192.0.2.10").expect("valid host"),
            "192.0.2.10:4242"
        );
    }

    #[test]
    fn persist_and_load_tcp_target_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "hopspot-rc-tcp-target-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        persist_tcp_target(&path, "gateway.example:4242");
        assert_eq!(
            load_persisted_tcp_target(&path).as_deref(),
            Some("gateway.example:4242")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn target_label_is_the_short_dest_prefix_and_alias_is_optional() {
        assert_eq!(
            target_label("b5ae6f59f1febf48dd3623181fb7a9f1"),
            "T b5ae6f59"
        );
        assert_eq!(stored_alias(Some("prnsd")).as_deref(), Some("prnsd"));
        assert_eq!(stored_alias(None), None);
        assert_eq!(stored_alias(Some("   ")), None);
    }

    #[test]
    fn resolve_paired_target_hash_prefers_the_pending_identity() {
        let wanted = IdentityHash::new([0x11; 16]);
        let other = IdentityHash::new([0x22; 16]);
        let listed = [other, wanted];
        assert_eq!(
            resolve_paired_target_hash(&listed, Some(&encode_hex(wanted.as_bytes()))),
            Some(wanted)
        );
        assert_eq!(resolve_paired_target_hash(&listed, None), None);
        assert_eq!(resolve_paired_target_hash(&[wanted], None), Some(wanted));
        assert_eq!(
            resolve_paired_target_hash(
                &listed,
                Some(&encode_hex(IdentityHash::new([0x33; 16]).as_bytes()))
            ),
            None
        );
    }

    #[test]
    fn peer_label_uses_the_first_two_hash_bytes_and_is_stable_for_the_same_tag() {
        let first = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"stable-identity");
        let again = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"stable-identity");
        assert_eq!(first, again);
        assert_eq!(peer_label(first), peer_label(again));
        assert!(peer_label(first).starts_with("P "));
        assert_eq!(peer_label(first).len(), 6);
        let identity = BleIdentity::new(*b"stable-identity!");
        let seen = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, identity.as_bytes());
        assert_eq!(
            bluetooth_auto_title(identity),
            format!("bluetooth-auto {}", &peer_label(seen)[2..])
        );
    }

    #[test]
    fn auto_wifi_card_lists_each_local_address_and_keeps_the_id_title() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"reticulum");
        let mut entry = local_interface_entry(
            &InterfaceSnapshot {
                id,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::Independent,
                radio: personal_rns::interfaces::RadioIndication::for_kind(id.kind()),
                details: personal_rns::interfaces::PeerDetails::NotApplicable,
            },
            None,
            None,
            Some("reticulum"),
            None,
            local_interface_config(
                &InterfaceSnapshot {
                    id,
                    mode: InterfaceMode::Full,
                    gravity: InterfaceGravity::ZERO,
                    connection: ConnectionState::Connected,
                    failure_reason: None,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    transfer_rates: None,
                    destinations: 0,
                    links: 0,
                    transported_links: 0,
                    membership: Membership::Independent,
                    radio: personal_rns::interfaces::RadioIndication::for_kind(id.kind()),
                    details: personal_rns::interfaces::PeerDetails::NotApplicable,
                },
                None,
                None,
            )
            .as_deref(),
            None,
            None,
            Vec::new(),
        );
        let title = entry.name.clone();
        assert!(title.starts_with("auto-wifi "));
        attach_auto_wifi_local_addresses(
            &mut entry,
            &[
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 18)), 47),
                (
                    IpAddr::V6(Ipv6Addr::new(
                        0xfe80, 0, 0, 0, 0x282f, 0x4eff, 0xfe59, 0x3321,
                    )),
                    47,
                ),
            ],
        );
        assert_eq!(entry.name, title);
        assert!(!entry.name.contains("192.168.1.18"));
        assert!(entry
            .extras
            .iter()
            .any(|fact| { fact.label == "IPv4" && fact.value == "192.168.1.18" }));
        assert!(entry
            .extras
            .iter()
            .any(|fact| { fact.label == "IPv6" && fact.value == "fe80::282f:4eff:fe59:3321%47" }));
    }

    #[test]
    fn auto_wifi_peer_cards_name_the_path_instead_of_a_bare_hash() {
        let ll = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);
        let udp = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id: InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, &ll.octets()),
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio: RadioIndication::NotRadio,
            details: PeerDetails::NotApplicable,
        });
        let bare = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id: InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, b"fe80::1"),
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio: RadioIndication::NotRadio,
            details: PeerDetails::NotApplicable,
        });
        let tcp_out = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id: InterfaceId::from_channel_tag(InterfaceKind::TcpClient, b"192.168.1.18:42699"),
            connection: ConnectionState::Reconnecting,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio: RadioIndication::NotRadio,
            details: PeerDetails::NotApplicable,
        });
        let tcp_in = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id: InterfaceId::from_channel_tag(InterfaceKind::TcpServerPeer, b"192.168.1.36:54040"),
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio: RadioIndication::NotRadio,
            details: PeerDetails::NotApplicable,
        });
        assert_eq!(udp.role, "UDP");
        assert!(udp.endpoint.is_none());
        assert!(udp.name.starts_with("UDP · P "));
        assert!(udp.detail.contains("TCP row"));
        assert_eq!(udp.health, Some(PeerHealth::RadioOnlyIdle));
        assert!(bare.endpoint.is_none());
        assert!(bare.name.starts_with("UDP · P "));
        assert_eq!(tcp_out.role, "TCP out");
        assert!(tcp_out.name.starts_with("TCP out · P "));
        assert!(tcp_out.detail.contains("42699"));
        assert_eq!(tcp_out.health, None);
        assert_eq!(tcp_in.role, "TCP in");
        assert!(tcp_in.name.starts_with("TCP in · P "));
        assert_eq!(tcp_in.health, None);
        assert_eq!(
            auto_wifi_peer_list_note("auto-wifi"),
            Some(
                "Each row is one path, not one device. UDP and TCP both ways are separate. Status is UDP peering. Health is the RNS plane. A TCP-out row that stays Retrying is usually the Wi-Fi gateway."
            )
        );
        assert_eq!(auto_wifi_peer_list_note("bluetooth-auto"), None);
    }

    #[test]
    fn local_auto_wifi_peer_title_uses_the_socket_address() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::TcpClient, b"192.168.1.1:42699");
        let inbound =
            InterfaceId::from_channel_tag(InterfaceKind::TcpServerPeer, b"192.168.1.36:54716");
        let udp = InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, b"fe80::1%14");
        let mut outbound = interface_peer(
            &InterfaceSnapshot {
                id,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Reconnecting,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan"),
                },
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            },
            Some("192.168.1.1:42699"),
        );
        let mut accepted = interface_peer(
            &InterfaceSnapshot {
                id: inbound,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan"),
                },
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            },
            Some("192.168.1.36:54716"),
        );
        let mut scoped = interface_peer(
            &InterfaceSnapshot {
                id: udp,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan"),
                },
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            },
            Some("fe80::aea7:4ff:fee1:4b3c%14"),
        );
        assert_eq!(outbound.name, "TCP out · 192.168.1.1:42699");
        assert_eq!(outbound.endpoint.as_deref(), Some("192.168.1.1:42699"));
        assert_eq!(outbound.endpoint_label.as_deref(), Some("To"));
        assert_eq!(accepted.name, "TCP in · 192.168.1.36:54716");
        assert_eq!(accepted.endpoint_label.as_deref(), Some("From"));
        assert_eq!(scoped.name, "UDP · fe80::aea7:4ff:fee1:4b3c%14");
        assert_eq!(scoped.endpoint_label.as_deref(), Some("Peer"));
        assert!(scoped.detail.contains("fe80"));
        let locals = [
            (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 18)), 14),
            (
                IpAddr::V6(Ipv6Addr::new(
                    0xfe80, 0, 0, 0, 0x282f, 0x4eff, 0xfe59, 0x3321,
                )),
                14,
            ),
        ];
        attach_our_side_of_the_link(&mut outbound, &locals);
        attach_our_side_of_the_link(&mut accepted, &locals);
        attach_our_side_of_the_link(&mut scoped, &locals);
        assert_eq!(outbound.local_endpoint_label.as_deref(), Some("From (us)"));
        assert_eq!(outbound.local_endpoint.as_deref(), Some("192.168.1.18"));
        assert_eq!(accepted.local_endpoint_label.as_deref(), Some("To (us)"));
        assert_eq!(
            accepted.local_endpoint.as_deref(),
            Some("192.168.1.18:42699")
        );
        assert_eq!(scoped.local_endpoint_label.as_deref(), Some("Local (us)"));
        assert_eq!(
            scoped.local_endpoint.as_deref(),
            Some("fe80::282f:4eff:fe59:3321%14")
        );
    }

    #[test]
    fn our_side_of_a_loopback_dial_is_localhost() {
        let mut loopback = interface_peer(
            &InterfaceSnapshot {
                id: InterfaceId::from_channel_tag(InterfaceKind::TcpClient, b"127.0.0.1:42699"),
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Reconnecting,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan"),
                },
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            },
            Some("127.0.0.1:42699"),
        );
        attach_our_side_of_the_link(&mut loopback, &[]);
        assert_eq!(loopback.local_endpoint_label.as_deref(), Some("From (us)"));
        assert_eq!(loopback.local_endpoint.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn udp_local_us_stays_on_the_same_ifindex() {
        let mut scoped = interface_peer(
            &InterfaceSnapshot {
                id: InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, b"fe80::1%14"),
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan"),
                },
                radio: RadioIndication::NotRadio,
                details: PeerDetails::NotApplicable,
            },
            Some("fe80::aea7:4ff:fee1:4b3c%14"),
        );
        attach_our_side_of_the_link(
            &mut scoped,
            &[(
                IpAddr::V6(Ipv6Addr::new(
                    0xfe80, 0, 0, 0, 0x282f, 0x4eff, 0xfe59, 0x3321,
                )),
                47,
            )],
        );
        assert_eq!(scoped.local_endpoint, None);
    }

    #[test]
    fn peer_aliases_attach_to_the_full_interface_id() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"kitchen");
        let mut peer = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio: RadioIndication::for_kind(Some(InterfaceKind::BluetoothPeer)),
            details: PeerDetails::NotApplicable,
        });
        let mut aliases = HashMap::new();
        aliases.insert(encode_hex(id.as_bytes()), "Kitchen HV4".to_string());
        apply_peer_aliases(std::slice::from_mut(&mut peer), &aliases);
        assert_eq!(peer.alias.as_deref(), Some("Kitchen HV4"));
        apply_peer_aliases(std::slice::from_mut(&mut peer), &HashMap::new());
        assert_eq!(peer.alias, None);
    }

    #[test]
    fn target_name_file_round_trips() {
        let parsed = parse_target_names("b5ae6f59f1febf48dd3623181fb7a9f1\tprnsd\n");
        assert_eq!(
            parsed
                .get("b5ae6f59f1febf48dd3623181fb7a9f1")
                .map(String::as_str),
            Some("prnsd")
        );
        assert_eq!(
            render_target_names(&parsed),
            "b5ae6f59f1febf48dd3623181fb7a9f1\tprnsd\n"
        );
    }

    #[test]
    fn fleet_peer_kinds_are_not_operator_interfaces() {
        assert!(!operator_interface_kind(InterfaceKind::BluetoothPeer));
        assert!(!operator_interface_kind(InterfaceKind::WifiPeer));
        assert!(operator_interface_kind(InterfaceKind::BluetoothAuto));
        assert!(operator_interface_kind(InterfaceKind::TcpClient));
        assert!(operator_interface_kind(InterfaceKind::LocalServer));
        assert!(operator_interface_kind(InterfaceKind::AutoWifi));
    }

    #[test]
    fn hand_rolled_local_usb_ids_leave_as_usb_auto_host() {
        let id = InterfaceId::new([0xD0; 8]);
        let snapshot = InterfaceSnapshot {
            id,
            mode: InterfaceMode::Full,
            gravity: InterfaceGravity::ZERO,
            connection: ConnectionState::Disconnected,
            failure_reason: None,
            rx_bytes: 0,
            tx_bytes: 0,
            transfer_rates: None,
            destinations: 0,
            links: 0,
            transported_links: 0,
            membership: Membership::Independent,
            radio: RadioIndication::for_kind(None),
            details: PeerDetails::NotApplicable,
        };
        assert_eq!(
            operator_local_kind(&snapshot),
            Some(InterfaceKind::UsbAutoHost)
        );
        let entry = local_interface_entry(
            &snapshot,
            None,
            None,
            None,
            None,
            local_interface_config(&snapshot, None, None).as_deref(),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(entry.kind, "usb-auto-host");
        assert_eq!(entry.detail.as_deref(), Some("USB"));
        assert_eq!(entry.connection, "Waiting");
        let mut waiting = entry.clone();
        attach_usb_link_peer(&mut waiting);
        assert!(waiting.peers.is_empty());
        let mut connected = entry;
        connected.connection = "Connected".to_string();
        attach_usb_link_peer(&mut connected);
        assert_eq!(connected.peers.len(), 1);
        assert_eq!(connected.peers[0].name, "USB link");
    }

    #[test]
    fn a_direct_usb_announce_is_local_at_hop_one() {
        let usb = InterfaceId::new([0xD0; 8]);
        assert!(clone_announce_is_usb_local(0, usb));
        assert!(clone_announce_is_usb_local(1, usb));
        assert!(!clone_announce_is_usb_local(2, usb));
        assert!(!clone_announce_is_usb_local(
            1,
            InterfaceId::from_channel_tag(InterfaceKind::BluetoothAuto, b"ble")
        ));
    }

    #[test]
    fn utc_millis_format_known_instants() {
        assert_eq!(format_utc_millis(0), "1970-01-01 00:00:00");
        assert_eq!(format_utc_millis(1_704_067_200_000), "2024-01-01 00:00:00");
        assert_eq!(format_utc_millis(1_704_067_261_000), "2024-01-01 00:01:01");
        let local = format_announce_millis(1_704_067_200_000);
        assert_eq!(local.len(), "2024-01-01 00:00:00".len());
        assert!(!local.contains("UTC") && !local.contains('+'), "{local}");
    }

    #[test]
    fn format_interface_stamps_undecodable_ids_as_usb_auto_host() {
        let usb = InterfaceId::new([0xD0; 8]);
        assert_eq!(usb.kind(), None);
        assert_eq!(route_interface_kind(usb), InterfaceKind::UsbAutoHost);
        assert_eq!(format_interface(usb), "usb-auto-host");
        assert_eq!(
            format_interface(InterfaceId::from_channel_tag(InterfaceKind::LoRa, b"lab")),
            "lora"
        );
    }

    #[test]
    fn disabled_connection_is_powered_off() {
        assert_eq!(
            interface_power_from_connection(ConnectionState::Disabled),
            InterfacePower::Off
        );
        assert_eq!(
            interface_power_from_connection(ConnectionState::Connected),
            InterfacePower::On
        );
    }

    #[test]
    fn local_interface_entry_uses_kind_name_and_tcp_detail() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::TcpClient, b"tcp");
        let entry = local_interface_entry(
            &InterfaceSnapshot {
                id,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::Independent,
                radio: personal_rns::interfaces::RadioIndication::for_kind(id.kind()),
                details: personal_rns::interfaces::PeerDetails::NotApplicable,
            },
            None,
            None,
            Some("home"),
            None,
            Some("127.0.0.1:4242"),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(entry.kind, "tcp-client");
        assert_eq!(entry.detail.as_deref(), Some("127.0.0.1:4242"));
        assert_eq!(entry.group.as_deref(), Some("home"));
        assert_eq!(entry.power, InterfacePower::On);
        assert_eq!(entry.mode, InterfaceMode::Full);
        assert_eq!(entry.connection, "Connected");
        assert_eq!(
            entry.extras,
            vec![
                InterfaceFact {
                    label: "Host".to_string(),
                    value: "127.0.0.1".to_string(),
                },
                InterfaceFact {
                    label: "Port".to_string(),
                    value: "4242".to_string(),
                },
            ]
        );
        assert!(!entry.shows_peers);
    }

    #[test]
    fn local_ble_auto_title_uses_the_peer_facing_identity_prefix() {
        let identity = BleIdentity::new(*b"stable-identity!");
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothAuto, b"bluetooth-auto");
        let entry = local_interface_entry(
            &InterfaceSnapshot {
                id,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::Independent,
                radio: personal_rns::interfaces::RadioIndication::for_kind(id.kind()),
                details: personal_rns::interfaces::PeerDetails::NotApplicable,
            },
            None,
            Some(identity),
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        );
        assert_eq!(entry.name, bluetooth_auto_title(identity));
        assert_eq!(
            entry.name,
            format!(
                "bluetooth-auto {}",
                &peer_label(InterfaceId::from_channel_tag(
                    InterfaceKind::BluetoothPeer,
                    identity.as_bytes()
                ))[2..]
            )
        );
    }

    #[test]
    fn local_ble_peer_title_uses_the_identity_prefix_not_the_runtime_name() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"stable-identity");
        let peer = interface_peer(
            &InterfaceSnapshot {
                id,
                mode: InterfaceMode::Full,
                gravity: InterfaceGravity::ZERO,
                connection: ConnectionState::Connected,
                failure_reason: None,
                rx_bytes: 0,
                tx_bytes: 0,
                transfer_rates: None,
                destinations: 0,
                links: 0,
                transported_links: 0,
                membership: Membership::FleetMember {
                    supervisor_id: InterfaceId::from_channel_tag(
                        InterfaceKind::BluetoothAuto,
                        b"bluetooth-auto",
                    ),
                },
                radio: RadioIndication::for_kind(Some(InterfaceKind::BluetoothPeer)),
                details: PeerDetails::NotApplicable,
            },
            Some("7a1b2c3d… @ AA:BB:CC:DD:EE:FF"),
        );
        assert_eq!(peer.name, format!("BLE · {}", peer_label(id)));
        assert_eq!(peer.endpoint, None);
        assert_eq!(peer.endpoint_label, None);
        assert_eq!(peer.health, Some(PeerHealth::RadioOnlyIdle));
        assert!(peer.detail.contains("Health"));
    }

    #[test]
    fn bluetooth_peer_health_separates_radio_membership_from_rns() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"peer");
        let radio = RadioIndication::for_kind(Some(InterfaceKind::BluetoothPeer));
        let announced = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 3,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let busy = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 32_000,
            rx_bytes: 26_000,
            links: 0,
            destinations: 1,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let framed = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 180,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let live = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 180,
            rx_bytes: 40,
            links: 1,
            destinations: 3,
            rate_bytes_per_sec: 12,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let retrying = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Reconnecting,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        assert_eq!(announced.health, Some(PeerHealth::RadioOnlyAnnounced));
        assert_eq!(busy.health, Some(PeerHealth::RadioOnlyAnnouncedWithFrames));
        assert_eq!(framed.health, Some(PeerHealth::RadioOnlyFrames));
        assert_eq!(live.health, Some(PeerHealth::RnsLive));
        assert_eq!(retrying.health, None);
        assert_eq!(
            bluetooth_auto_peer_list_note("bluetooth-auto"),
            Some(
                "Status is the radio session. Health is the RNS plane. Details is GATT vs CoC. Radio only means the peer looks Connected while remote control will not work.",
            )
        );
        assert_eq!(bluetooth_auto_peer_list_note("auto-wifi"), None);
    }

    #[test]
    fn wifi_peer_health_separates_udp_peering_from_rns() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::WifiPeer, b"fe80::1");
        let radio = RadioIndication::for_kind(Some(InterfaceKind::WifiPeer));
        let idle = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let framed = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 64,
            rx_bytes: 0,
            links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let announced = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 0,
            rx_bytes: 0,
            links: 0,
            destinations: 2,
            rate_bytes_per_sec: 0,
            radio,
            details: PeerDetails::NotApplicable,
        });
        let live = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id,
            connection: ConnectionState::Connected,
            tx_bytes: 200,
            rx_bytes: 80,
            links: 1,
            destinations: 2,
            rate_bytes_per_sec: 8,
            radio,
            details: PeerDetails::NotApplicable,
        });
        assert_eq!(idle.health, Some(PeerHealth::RadioOnlyIdle));
        assert_eq!(framed.health, Some(PeerHealth::RadioOnlyFrames));
        assert_eq!(announced.health, Some(PeerHealth::RadioOnlyAnnounced));
        assert_eq!(live.health, Some(PeerHealth::RnsLive));
        assert!(idle.detail.contains("Health"));
        assert_eq!(
            auto_wifi_peer_list_note("auto-wifi"),
            Some(
                "Each row is one path, not one device. UDP and TCP both ways are separate. Status is UDP peering. Health is the RNS plane. A TCP-out row that stays Retrying is usually the Wi-Fi gateway.",
            )
        );
    }

    #[test]
    fn remote_ble_auto_title_ignores_generic_card_names_and_takes_the_identity_prefix() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothAuto, b"ble");
        let mut card = RemoteControlInterfaceCard::empty();
        let _ = card.set_name("bluetooth-auto");
        let mut entry = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::BluetoothAuto,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 0,
                rx_bytes: 0,
                links: 0,
                rate_bytes_per_sec: 0,
            },
            Some(&card),
        );
        assert!(generic_bluetooth_auto_title("bluetooth-auto"));
        assert!(generic_bluetooth_auto_title("BLE"));
        assert!(!generic_bluetooth_auto_title("bluetooth-auto 7a1b"));
        assert_ne!(entry.name, "bluetooth-auto");
        apply_remote_card(&mut entry, &card);
        assert_ne!(entry.name, "bluetooth-auto");
        apply_bluetooth_auto_identity_title(&mut entry, Some("7a1b"));
        assert_eq!(entry.name, "bluetooth-auto 7a1b");
        let _ = card.set_name("bluetooth-auto 7a1b");
        apply_remote_card(&mut entry, &card);
        apply_bluetooth_auto_identity_title(&mut entry, Some("ffff"));
        assert_eq!(entry.name, "bluetooth-auto 7a1b");
        apply_bluetooth_auto_identity_title(&mut entry, None);
        assert_eq!(entry.name, "bluetooth-auto 7a1b");
    }

    #[test]
    fn remote_auto_wifi_card_group_becomes_an_ipv6_fact() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"lan");
        let mut card = RemoteControlInterfaceCard::empty();
        let _ = card.set_name("auto-wifi");
        let _ = card.set_config("W,field-lab");
        let _ = card.set_group("fe80::aea7:4ff:fee1:4b3c");
        let mut entry = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::AutoWifi,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 0,
                rx_bytes: 0,
                links: 0,
                rate_bytes_per_sec: 0,
            },
            Some(&card),
        );
        apply_remote_card(&mut entry, &card);
        assert!(entry.group.is_none());
        assert!(entry
            .extras
            .iter()
            .any(|fact| fact.label == "IPv6" && fact.value == "fe80::aea7:4ff:fee1:4b3c"));
        assert!(entry
            .extras
            .iter()
            .any(|fact| fact.label == "SSID" && fact.value == "field-lab"));
    }

    #[test]
    fn peer_endpoint_matching_controller_ll_ignores_scope() {
        let mut keys = HashSet::new();
        keys.insert("fe80::494:446c:eb84:e48b".to_string());
        assert!(endpoint_matches_wifi_ll_keys(
            "fe80::494:446c:eb84:e48b",
            &keys
        ));
        assert!(endpoint_matches_wifi_ll_keys(
            "fe80::494:446c:eb84:e48b%14",
            &keys
        ));
        assert!(endpoint_matches_wifi_ll_keys(
            "FE80::494:446C:EB84:E48B%en0",
            &keys
        ));
        assert!(!endpoint_matches_wifi_ll_keys(
            "fe80::aea7:4ff:fee1:4b3c",
            &keys
        ));
        assert_eq!(THIS_CONTROLLER_PEER_ALIAS, "This Controller");
    }

    #[test]
    fn normalize_stored_wifi_ll_rejects_non_link_local() {
        assert_eq!(
            normalize_stored_wifi_ll("fe80::494:446c:eb84:e48b%en0").as_deref(),
            Some("fe80::494:446c:eb84:e48b")
        );
        assert_eq!(
            normalize_stored_wifi_ll("FE80::494:446C:EB84:E48B").as_deref(),
            Some("fe80::494:446c:eb84:e48b")
        );
        assert_eq!(normalize_stored_wifi_ll("2001:db8::1"), None);
        assert_eq!(normalize_stored_wifi_ll("not-an-ip"), None);
    }

    #[test]
    fn managed_ble_auto_prefix_is_only_that_nodes_direct_peer_route() {
        let hv4 = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"hv4-identity");
        let mt2 = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"mt2-identity");
        let supervisor =
            InterfaceId::from_channel_tag(InterfaceKind::BluetoothAuto, b"bluetooth-auto");
        assert_eq!(
            bluetooth_auto_prefix_from_direct_peer(1, NextHop::Direct, hv4).as_deref(),
            Some(appearance_prefix(hv4).as_str())
        );
        assert_eq!(
            bluetooth_auto_prefix_from_direct_peer(1, NextHop::Direct, mt2).as_deref(),
            Some(appearance_prefix(mt2).as_str())
        );
        assert_ne!(appearance_prefix(hv4), appearance_prefix(mt2));
        assert_eq!(
            bluetooth_auto_prefix_from_direct_peer(1, NextHop::Direct, supervisor),
            None
        );
        assert_eq!(
            bluetooth_auto_prefix_from_direct_peer(2, NextHop::Direct, hv4),
            None
        );
    }

    #[test]
    fn remote_interface_entry_uses_card_group_peers_and_config() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::BluetoothAuto, b"ble");
        let mut card = RemoteControlInterfaceCard::empty();
        let _ = card.set_name("bluetooth-auto 7a1b");
        let _ = card.set_group("home");
        let _ = card.set_config("BLE supervisor");
        let _ = card.set_failure("radio timeout");
        card.destinations = 4;
        let entry = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::BluetoothAuto,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 8,
                rx_bytes: 16,
                links: 2,
                rate_bytes_per_sec: 32,
            },
            Some(&card),
        );
        assert_eq!(entry.name, "bluetooth-auto 7a1b");
        assert_eq!(entry.group.as_deref(), Some("home"));
        assert_eq!(entry.detail.as_deref(), Some("BLE supervisor"));
        assert_eq!(entry.failure.as_deref(), Some("radio timeout"));
        assert_eq!(entry.destinations, 4);
        assert_eq!(
            entry.extras,
            vec![InterfaceFact {
                label: "Role".to_string(),
                value: "BLE supervisor".to_string(),
            }]
        );
        assert!(entry.shows_peers);
        assert!(
            entry.peers.is_empty(),
            "peers are fetched on a separate page"
        );
        let peer = interface_peer_from_wire(&RemoteControlInterfacePeer {
            id: InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"peer"),
            connection: ConnectionState::Connected,
            tx_bytes: 4,
            rx_bytes: 6,
            links: 1,
            destinations: 2,
            rate_bytes_per_sec: 8,
            radio: RadioIndication::from_bluetooth_rssi(Some(-62)),
            details: PeerDetails::NotApplicable,
        });
        assert_eq!(peer.role, "BLE");
        assert!(peer.name.starts_with("BLE · P "));
        assert_eq!(
            radio_facts(peer.radio),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "-62 dBm".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::for_kind(Some(
                InterfaceKind::BluetoothPeer
            ))),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "pending".to_string(),
            }]
        );
    }

    #[test]
    fn remote_auto_wifi_card_always_lists_rssi() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::AutoWifi, b"wifi");
        let mut card = RemoteControlInterfaceCard::empty();
        let _ = card.set_name("auto-wifi");
        let _ = card.set_config("W,field-lab");
        let without_sample = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::AutoWifi,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 0,
                rx_bytes: 0,
                links: 0,
                rate_bytes_per_sec: 0,
            },
            Some(&card),
        );
        assert!(without_sample
            .extras
            .iter()
            .any(|fact| { fact.label == "RSSI" && fact.value == "not available" }));

        let _ = card.set_config("W,field-lab|R-67").unwrap();
        let with_sample = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::AutoWifi,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 0,
                rx_bytes: 0,
                links: 0,
                rate_bytes_per_sec: 0,
            },
            Some(&card),
        );
        assert!(with_sample
            .extras
            .iter()
            .any(|fact| { fact.label == "RSSI" && fact.value == "-67 dBm" }));
    }

    #[test]
    fn remote_lora_entry_lists_tune_facts_under_config() {
        let id = InterfaceId::from_channel_tag(InterfaceKind::LoRa, b"lora");
        let config = personal_rns::interfaces::lora::DEFAULT_915_PROFILE.inventory_config();
        let mut card = RemoteControlInterfaceCard::empty();
        let _ = card.set_name("LoRa");
        let _ = card.set_config(config.as_str());
        let entry = remote_interface_entry(
            &RemoteControlInterfaceEntry {
                id,
                kind: InterfaceKind::LoRa,
                mode: InterfaceMode::Full,
                connection: ConnectionState::Connected,
                enabled: true,
                tx_bytes: 0,
                rx_bytes: 0,
                links: 0,
                rate_bytes_per_sec: 0,
            },
            Some(&card),
        );
        assert_eq!(
            entry.extras,
            super::hopspot_extra_facts(Some(config.as_str()), Some(InterfaceKind::LoRa), None)
        );
    }

    #[test]
    fn target_path_lines_split_announce_from_route() {
        let path = TargetPath {
            hops: 2,
            via: "via a1b2c3d4".to_string(),
            interface: "auto-wifi".to_string(),
            announced_at: "2026-09-05 18:59:12 UTC".to_string(),
        };
        assert_eq!(format_hop_count(0), "0 hops");
        assert_eq!(format_hop_count(1), "1 hop");
        assert_eq!(
            format_target_announce(Some(&path)),
            "Last announce 2026-09-05 18:59:12 UTC"
        );
        assert_eq!(
            format_target_route(Some(&path)),
            "2 hops via a1b2c3d4 on auto-wifi"
        );
        assert_eq!(format_target_announce(None), "Last announce not heard yet");
        assert_eq!(format_target_route(None), "No path heard yet");
    }

    #[test]
    fn connect_prefers_direct_bluetooth_over_relayed_paths() {
        let direct = TargetPath {
            hops: 1,
            via: "direct".to_string(),
            interface: "bluetooth-peer".to_string(),
            announced_at: String::new(),
        };
        let relayed = TargetPath {
            hops: 2,
            via: "via HV4A-peer".to_string(),
            interface: "bluetooth-peer".to_string(),
            announced_at: String::new(),
        };
        assert!(path_is_direct_ble(Some(&direct)));
        assert!(!path_is_direct_ble(Some(&relayed)));
        assert!(path_is_better_than(&direct, Some(&relayed)));
        assert!(!path_is_better_than(&relayed, Some(&direct)));
    }

    #[test]
    fn via_hop_prefers_alias_then_peer_label() {
        let peer = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"hv4a-ble");
        let transport = personal_rns::TransportId::new([0x28; 16]);
        let peer_id = encode_hex(peer.as_bytes());
        let transport_id = encode_hex(transport.as_bytes());

        assert_eq!(
            format_next_hop(
                NextHop::Via(transport),
                peer,
                &HashMap::new(),
                &HashMap::new()
            ),
            format!("via {}", peer_label(peer))
        );

        let mut peer_aliases = HashMap::new();
        peer_aliases.insert(peer_id, "HV4A-peer".to_string());
        assert_eq!(
            format_next_hop(
                NextHop::Via(transport),
                peer,
                &peer_aliases,
                &HashMap::new()
            ),
            "via HV4A-peer"
        );

        let mut target_aliases = HashMap::new();
        target_aliases.insert(transport_id, "HV4A".to_string());
        assert_eq!(
            format_next_hop(
                NextHop::Via(transport),
                peer,
                &HashMap::new(),
                &target_aliases
            ),
            "via HV4A"
        );
        assert_eq!(
            format_next_hop(NextHop::Direct, peer, &HashMap::new(), &HashMap::new()),
            "direct"
        );
    }

    #[test]
    fn bluetooth_auto_name_prefix_matches_peer_appearance() {
        let peer = InterfaceId::from_channel_tag(InterfaceKind::BluetoothPeer, b"hv4c-ble-id");
        let prefix = appearance_prefix(peer);
        assert_eq!(
            bluetooth_auto_name_prefix(&format!("bluetooth-auto {prefix}")).as_deref(),
            Some(prefix.as_str())
        );
        assert_eq!(bluetooth_auto_name_prefix("bluetooth-auto"), None);
        assert_eq!(bluetooth_auto_name_prefix("lora"), None);
        let wifi = InterfaceId::from_channel_tag(
            InterfaceKind::WifiPeer,
            &Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1).octets(),
        );
        assert_eq!(wifi.kind(), Some(InterfaceKind::WifiPeer));
    }

    #[test]
    fn inventory_waits_for_the_control_destination_announce() {
        let first = InstantMillis(1_000);
        let newer = InstantMillis(2_000);
        assert!(!control_announce_satisfies(
            None,
            RemoteControlAnnounceWait::UntilHeard,
            None
        ));
        assert!(control_announce_satisfies(
            Some(first),
            RemoteControlAnnounceWait::UntilHeard,
            Some(first)
        ));
        assert!(!control_announce_satisfies(
            Some(first),
            RemoteControlAnnounceWait::UntilRefreshed,
            Some(first)
        ));
        assert!(control_announce_satisfies(
            Some(newer),
            RemoteControlAnnounceWait::UntilRefreshed,
            Some(first)
        ));
        assert!(control_announce_satisfies(
            Some(first),
            RemoteControlAnnounceWait::UntilRefreshed,
            None
        ));
        let now = Instant::now();
        assert!(inventory_recovery_continues(
            now,
            now + Duration::from_secs(1)
        ));
        assert!(!inventory_recovery_continues(now, now));
        assert_eq!(format_activity_age(None), "—");
        assert_eq!(format_activity_age(Some(0)), "now");
        assert_eq!(format_activity_age(Some(12)), "12s");
        assert_eq!(format_activity_age(Some(120)), "2m");
        let usb_path = TargetPath {
            hops: 1,
            via: "direct".to_string(),
            interface: "usb-auto-host".to_string(),
            announced_at: "now".to_string(),
        };
        let ble_path = TargetPath {
            hops: 1,
            via: "direct".to_string(),
            interface: "bluetooth-peer".to_string(),
            announced_at: "now".to_string(),
        };
        assert!(!should_wait_for_control_announce(
            RemoteControlAnnounceWait::UntilHeard,
            Some(&usb_path)
        ));
        assert!(should_wait_for_control_announce(
            RemoteControlAnnounceWait::UntilHeard,
            None
        ));
        assert!(should_wait_for_control_announce(
            RemoteControlAnnounceWait::UntilRefreshed,
            Some(&usb_path)
        ));
        let usb_up = [test_local_interface("usb-auto-host", "Connected")];
        let usb_down = [test_local_interface("usb-auto-host", "Waiting")];
        let ble_up = [test_local_interface("bluetooth-auto", "Connected")];
        assert!(!should_forget_control_route(Some(&usb_path), &usb_up));
        assert!(should_forget_control_route(Some(&usb_path), &usb_down));
        assert!(
            should_forget_control_route(Some(&ble_path), &usb_up),
            "a BLE hop whose radio is down is stale"
        );
        assert!(!should_forget_control_route(Some(&ble_path), &ble_up));
        assert!(!should_forget_control_route(None, &usb_up));
    }

    fn test_local_interface(kind: &str, connection: &str) -> InterfaceEntry {
        InterfaceEntry {
            id: "00".repeat(8),
            name: kind.to_string(),
            kind: kind.to_string(),
            power: InterfacePower::On,
            mode: InterfaceMode::Full,
            connection: connection.to_string(),
            group: None,
            tx_bytes: 0,
            rx_bytes: 0,
            tx_bps: None,
            rx_bps: None,
            links: 0,
            transported_links: 0,
            destinations: 0,
            rate_bytes_per_sec: 0,
            gravity: None,
            ifac_bytes: None,
            last_activity_secs: None,
            detail: None,
            failure: None,
            extras: Vec::new(),
            shows_peers: false,
            peers: Vec::new(),
            peers_error: None,
            arrived_at: None,
        }
    }

    #[test]
    fn radio_facts_name_the_measurement_the_medium_actually_has() {
        assert_eq!(radio_facts(RadioIndication::NotRadio), Vec::new());
        assert_eq!(
            radio_facts(RadioIndication::Wifi(WifiIndication::Unavailable)),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "not available".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::Wifi(WifiIndication::Pending)),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "pending".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::Wifi(WifiIndication::Rssi(RssiDbm::new(
                -51
            )))),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "-51 dBm".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::from_bluetooth_rssi(None)),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "pending".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::from_bluetooth_rssi(Some(-62))),
            vec![InterfaceFact {
                label: "RSSI".to_string(),
                value: "-62 dBm".to_string(),
            }]
        );
        assert_eq!(
            radio_facts(RadioIndication::from_lora_phy(PacketPhyStats {
                rssi: Some(RssiDbm::new(-103)),
                snr: Some(SnrQuarterDb::new(-11)),
                quality: SignalQualityTenthsPercent::new(812),
            })),
            vec![
                InterfaceFact {
                    label: "RSSI".to_string(),
                    value: "-103 dBm".to_string(),
                },
                InterfaceFact {
                    label: "SNR".to_string(),
                    value: "-2.7 dB".to_string(),
                },
                InterfaceFact {
                    label: "Quality".to_string(),
                    value: "81.2%".to_string(),
                },
            ]
        );
    }

    #[test]
    fn controller_identity_secret_lives_under_the_identities_vault() {
        assert_eq!(
            controller_identity_secret_path(Path::new("/tmp/hopspot-remote-control/identities")),
            Path::new("/tmp/hopspot-remote-control/identities").join(CONTROLLER_IDENTITY_FILE)
        );
        assert_eq!(
            instance_identity_secret_path(Path::new("/tmp/hopspot-remote-control/identities")),
            Path::new("/tmp/hopspot-remote-control/identities").join(INSTANCE_IDENTITY_FILE)
        );
        assert_eq!(CONTROLLER_IDENTITY_FILE, "controller");
        assert_eq!(INSTANCE_IDENTITY_FILE, "instance");
    }

    #[test]
    fn manager_aliases_use_the_controller_hash_file() {
        assert_eq!(MANAGER_ALIASES_FILE, "manager-aliases");
        let parsed = parse_target_names("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tlaptop\n");
        assert_eq!(
            parsed
                .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .map(String::as_str),
            Some("laptop")
        );
    }

    #[test]
    fn invitation_codes_are_case_insensitive_eight_digit_hex() {
        let upper = parse_invitation_code("ABCD1234").expect("uppercase hex is valid");
        let lower = parse_invitation_code("abcd1234").expect("lowercase hex is valid");
        let dashed = parse_invitation_code("AbCd-1234").expect("dashes are ignored");
        assert_eq!(upper, lower);
        assert_eq!(upper, dashed);
        assert_eq!(upper.value(), 0xABCD_1234);
        assert_eq!(
            parse_invitation_code("ABCD123"),
            Err(BackendError::InvalidInvitationCode)
        );
        assert_eq!(
            parse_invitation_code("ABCD12345"),
            Err(BackendError::InvalidInvitationCode)
        );
        assert_eq!(
            parse_invitation_code("ABCD12GH"),
            Err(BackendError::InvalidInvitationCode)
        );
    }

    #[test]
    fn dismiss_operator_suppresses_same_open_not_later_pair_again() {
        let mut pairing = super::PairingEvents::load(
            std::env::temp_dir().join("prns-controller-dismiss-pairing-session-test-names"),
        );
        let id = "aabbccddeeff00112233445566778899";
        let first_open = InstantMillis(1_000);
        let second_open = InstantMillis(2_000);
        pairing.remember_dismissed_session(id, first_open);
        assert!(pairing.is_dismissed_session(id, first_open));
        assert!(pairing.is_dismissed_session(id, InstantMillis(999)));
        assert!(!pairing.is_dismissed_session(id, second_open));
        assert!(!pairing.is_dismissed_session("other-device", first_open));
        assert!(pairing.dismiss_operator(id));
        assert!(!pairing.dismiss_operator(""));
    }
}
