//! Controller-app roster replica. Bound to the Instance destination, not Operator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use personal_rns::crypto::{sha256_chunks, Ed25519Signature};
use personal_rns::identity::{
    IdentityHash, PrivateIdentityMaterial, PublicIdentityMaterial, Zeroizing,
    IDENTITY_PUBLIC_KEY_LEN, IDENTITY_SECRET_KEY_LEN,
};
use personal_rns::prelude::{
    Decline, RemoteControlRequestKind, RemoteControlRequestSet, RemoteControlTargetAccess,
    RemoteControlTargetIdentity, RequestContext, RequestEndpoint, RequestEndpointPolicy,
};
use personal_rns::remote_control::RemoteControlControllerAuthority;
use personal_rns::routing::announce::{derive_destination_hash, expand_name};
use personal_rns::wire::DestinationHash;

use crate::identity_clone::ControllerAppState;

pub const ROSTER_SYNC_APP_NAME: &str = "prns";
pub const ROSTER_SYNC_ASPECTS: &[&str] = &["controller", "roster-sync"];
pub const ROSTER_SYNC_REQUEST_ENDPOINT_ID: &str = "/prns/controller/roster-sync";

const HASH_LEN: usize = 16;
const SIGNATURE_LEN: usize = 64;
const REPLICA_DOMAIN: &[u8] = b"reticulum.controller.roster.replica.v2";
const PULL_DOMAIN: &[u8] = b"reticulum.controller.roster.pull.v1";
const CONTROLLER_USB_HOST_DESKTOP: [u8; 8] = [0xD0; 8];
const CONTROLLER_USB_HOST_ANDROID: [u8; 8] = [0xD1; 8];
const SIBLING_REMOVED_MARK: &str = "removed";
/// Local display name for a wifi peer whose LL is *this* install. Must never ride roster sync.
pub const THIS_CONTROLLER_PEER_ALIAS: &str = "This Controller";
/// `peer-alias-links` value for [`THIS_CONTROLLER_PEER_ALIAS`] rows (local-only).
pub const THIS_CONTROLLER_ALIAS_LINK: &str = "__this_controller__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RosterMessageKind {
    Pull = 1,
    Replica = 2,
    Error = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RosterErrorCode {
    NotSibling = 1,
    Malformed = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterLabelKind {
    TargetName,
    TargetAlias,
    ManagerAlias,
    PeerAlias,
    SiblingAlias,
    SiblingRemoved,
    PairingAdvertDismissed,
    TargetLooking,
    /// Instance hash → that sibling's wifi link-local (`fe80::…`). Only the signer may publish their own.
    SiblingWifiLl,
}

impl RosterLabelKind {
    const fn wire(self) -> u8 {
        match self {
            Self::TargetName => 1,
            Self::TargetAlias => 2,
            Self::ManagerAlias => 3,
            Self::PeerAlias => 4,
            Self::SiblingAlias => 5,
            Self::SiblingRemoved => 6,
            Self::PairingAdvertDismissed => 7,
            Self::TargetLooking => 8,
            Self::SiblingWifiLl => 9,
        }
    }

    fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::TargetName),
            2 => Some(Self::TargetAlias),
            3 => Some(Self::ManagerAlias),
            4 => Some(Self::PeerAlias),
            5 => Some(Self::SiblingAlias),
            6 => Some(Self::SiblingRemoved),
            7 => Some(Self::PairingAdvertDismissed),
            8 => Some(Self::TargetLooking),
            9 => Some(Self::SiblingWifiLl),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::TargetName => "target-name",
            Self::TargetAlias => "target-alias",
            Self::ManagerAlias => "manager-alias",
            Self::PeerAlias => "peer-alias",
            Self::SiblingAlias => "sibling-alias",
            Self::SiblingRemoved => "sibling-removed",
            Self::PairingAdvertDismissed => "pairing-advert",
            Self::TargetLooking => "target-looking",
            Self::SiblingWifiLl => "sibling-wifi-ll",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "target-name" => Some(Self::TargetName),
            "target-alias" => Some(Self::TargetAlias),
            "manager-alias" => Some(Self::ManagerAlias),
            "peer-alias" => Some(Self::PeerAlias),
            "sibling-alias" => Some(Self::SiblingAlias),
            "sibling-removed" => Some(Self::SiblingRemoved),
            "pairing-advert" => Some(Self::PairingAdvertDismissed),
            "target-looking" => Some(Self::TargetLooking),
            "sibling-wifi-ll" => Some(Self::SiblingWifiLl),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAttention {
    Free,
    HeldByThis,
    HeldBySibling,
}

#[must_use]
pub fn looking_instance<'a>(replica: &'a RosterReplica, target_id: &str) -> Option<&'a str> {
    let target_id = target_id.trim();
    replica.labels.iter().find_map(|label| {
        (label.kind == RosterLabelKind::TargetLooking && label.key.eq_ignore_ascii_case(target_id))
            .then(|| label.value.as_deref())
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[must_use]
pub fn attention_for_target(
    replica: &RosterReplica,
    target_id: &str,
    instance_hex: &str,
) -> TargetAttention {
    match looking_instance(replica, target_id) {
        None => TargetAttention::Free,
        Some(holder) if holder.eq_ignore_ascii_case(instance_hex.trim()) => {
            TargetAttention::HeldByThis
        }
        Some(_) => TargetAttention::HeldBySibling,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterLabel {
    pub kind: RosterLabelKind,
    pub key: String,
    pub value: Option<String>,
    pub clock: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterReplica {
    pub clock: u64,
    pub upserts: Vec<(IdentityHash, u64)>,
    pub tombstones: Vec<(IdentityHash, u64)>,
    pub siblings: Vec<PublicIdentityMaterial>,
    pub labels: Vec<RosterLabel>,
}

impl Default for RosterReplica {
    fn default() -> Self {
        Self {
            clock: 0,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RosterUpsert {
    pub access: RemoteControlTargetAccess,
    pub clock: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RosterDelta {
    pub clock: u64,
    pub signer: PublicIdentityMaterial,
    pub upserts: Vec<RosterUpsert>,
    pub tombstones: Vec<(IdentityHash, u64)>,
    pub siblings: Vec<PublicIdentityMaterial>,
    pub labels: Vec<RosterLabel>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MergePlan {
    pub clock: u64,
    pub upserts: Vec<RemoteControlTargetAccess>,
    pub forgets: Vec<IdentityHash>,
    pub labels: Vec<RosterLabel>,
}

#[derive(Default)]
pub struct RosterShared {
    pub replica: RosterReplica,
    pub pending: Option<RosterDelta>,
    pub cached_snapshot: Vec<u8>,
    pub instance_secret: Option<Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>>,
    pub heard_siblings: Vec<IdentityHash>,
    pub pull_due: Vec<IdentityHash>,
    pub applied_generation: u64,
}

impl RosterShared {
    pub fn note_heard_sibling(&mut self, hash: IdentityHash) {
        push_unique_hash(&mut self.heard_siblings, hash);
        push_unique_hash(&mut self.pull_due, hash);
    }

    pub fn take_pull_due(&mut self) -> Vec<IdentityHash> {
        std::mem::take(&mut self.pull_due)
    }

    #[must_use]
    pub fn heard_sibling(&self, hash: IdentityHash) -> bool {
        self.heard_siblings.contains(&hash)
    }
}

fn push_unique_hash(list: &mut Vec<IdentityHash>, hash: IdentityHash) {
    if !list.contains(&hash) {
        list.push(hash);
    }
}

fn instance_material(shared: &RosterShared) -> Option<PrivateIdentityMaterial> {
    shared
        .instance_secret
        .as_ref()
        .map(|secret| PrivateIdentityMaterial::from_bytes(**secret))
}

pub fn roster_sync_destination_hash(identity: IdentityHash) -> Option<DestinationHash> {
    let name = expand_name(ROSTER_SYNC_APP_NAME, ROSTER_SYNC_ASPECTS).ok()?;
    Some(derive_destination_hash(&identity, &name))
}

pub fn pin_sibling(replica: &mut RosterReplica, keys: PublicIdentityMaterial) {
    if replica
        .siblings
        .iter()
        .any(|sibling| sibling.identity_hash() == keys.identity_hash())
    {
        return;
    }
    replica.siblings.push(keys);
}

pub fn adopt_sibling(replica: &mut RosterReplica, keys: PublicIdentityMaterial) {
    let key = encode_hex(keys.identity_hash().as_bytes());
    note_local_label(replica, RosterLabelKind::SiblingRemoved, &key, None);
    pin_sibling(replica, keys);
}

pub fn forget_sibling_locally(replica: &mut RosterReplica, hash: IdentityHash) {
    replica
        .siblings
        .retain(|sibling| sibling.identity_hash() != hash);
    let key = encode_hex(hash.as_bytes());
    note_local_label(
        replica,
        RosterLabelKind::SiblingRemoved,
        &key,
        Some(SIBLING_REMOVED_MARK),
    );
    note_local_label(replica, RosterLabelKind::SiblingAlias, &key, None);
    note_local_label(replica, RosterLabelKind::SiblingWifiLl, &key, None);
}

fn sibling_is_removed(replica: &RosterReplica, hash: IdentityHash) -> bool {
    let key = encode_hex(hash.as_bytes());
    replica.labels.iter().any(|label| {
        label.kind == RosterLabelKind::SiblingRemoved
            && label.key == key
            && label
                .value
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    })
}

pub fn forget_target_locally(replica: &mut RosterReplica, target: IdentityHash) {
    replica.clock = replica.clock.saturating_add(1);
    remove_upsert(replica, target);
    set_tombstone(replica, target, replica.clock);
    let key = encode_hex(target.as_bytes());
    upsert_label(
        replica,
        RosterLabel {
            kind: RosterLabelKind::TargetName,
            key: key.clone(),
            value: None,
            clock: replica.clock,
        },
    );
    upsert_label(
        replica,
        RosterLabel {
            kind: RosterLabelKind::TargetAlias,
            key: key.clone(),
            value: None,
            clock: replica.clock,
        },
    );
    upsert_label(
        replica,
        RosterLabel {
            kind: RosterLabelKind::TargetLooking,
            key,
            value: None,
            clock: replica.clock,
        },
    );
}

#[must_use]
pub fn replica_forgets_target(replica: &RosterReplica, hash: IdentityHash) -> bool {
    let tombstone = tombstone_clock(replica, hash);
    tombstone > 0 && tombstone >= upsert_clock(replica, hash)
}

#[must_use]
pub fn replica_known_targets(replica: &RosterReplica) -> Vec<IdentityHash> {
    replica
        .upserts
        .iter()
        .filter(|(hash, _)| !replica_forgets_target(replica, *hash))
        .map(|(hash, _)| *hash)
        .collect()
}

pub fn note_local_upsert(replica: &mut RosterReplica, target: IdentityHash) {
    replica.clock = replica.clock.saturating_add(1);
    remove_tombstone(replica, target);
    record_upsert(replica, target, replica.clock);
}

pub fn note_local_label(
    replica: &mut RosterReplica,
    kind: RosterLabelKind,
    key: &str,
    value: Option<&str>,
) {
    let key = key.trim();
    if key.is_empty() {
        return;
    }
    if kind == RosterLabelKind::PeerAlias && !peer_alias_is_syncable(key) {
        return;
    }
    let next_value = value
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned);
    // Refuse to *publish* the local-only "This Controller" string. Clears (None) stay allowed
    // so a prior mistaken seed can be retracted to siblings.
    if kind == RosterLabelKind::PeerAlias && !peer_alias_value_is_syncable(next_value.as_deref()) {
        return;
    }
    if replica
        .labels
        .iter()
        .any(|label| label.kind == kind && label.key == key && label.value == next_value)
    {
        // Same fact already published — do not bump the clock or re-push siblings.
        return;
    }
    replica.clock = replica.clock.saturating_add(1);
    upsert_label(
        replica,
        RosterLabel {
            kind,
            key: key.to_owned(),
            value: next_value,
            clock: replica.clock,
        },
    );
}

pub fn import_seed_labels(
    replica: &mut RosterReplica,
    kind: RosterLabelKind,
    values: &std::collections::HashMap<String, String>,
) {
    let clock = replica.clock.max(1);
    for (key, value) in values {
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        if kind == RosterLabelKind::PeerAlias
            && (!peer_alias_is_syncable(key) || !peer_alias_value_is_syncable(Some(value)))
        {
            continue;
        }
        if label_clock(replica, kind, key) > 0 {
            continue;
        }
        upsert_label(
            replica,
            RosterLabel {
                kind,
                key: key.to_owned(),
                value: Some(value.to_owned()),
                clock,
            },
        );
    }
    replica.clock = replica.clock.max(clock);
}

#[must_use]
pub fn peer_alias_is_syncable(peer_id: &str) -> bool {
    match parse_hex::<8>(peer_id) {
        Ok(bytes) => bytes != CONTROLLER_USB_HOST_DESKTOP && bytes != CONTROLLER_USB_HOST_ANDROID,
        Err(()) => true,
    }
}

/// "This Controller" names the local AutoWifi LL view of *this* install — never a shared fact.
#[must_use]
pub fn peer_alias_value_is_syncable(value: Option<&str>) -> bool {
    !value
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .is_some_and(|name| name.eq_ignore_ascii_case(THIS_CONTROLLER_PEER_ALIAS))
}

/// Links owned by this-controller / sibling-controller auto-fill stay off the roster.
#[must_use]
pub fn peer_alias_link_is_local_only(
    link: &str,
    sibling_instance_hashes: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    let link = link.trim();
    if link.is_empty() {
        return false;
    }
    if link == THIS_CONTROLLER_ALIAS_LINK {
        return true;
    }
    sibling_instance_hashes
        .into_iter()
        .any(|hash| link.eq_ignore_ascii_case(hash.as_ref().trim()))
}

/// Retract any PeerAlias rows whose value is local-only so siblings stop showing them.
pub fn retract_unsyncable_peer_alias_values(replica: &mut RosterReplica) {
    let bad_keys = replica
        .labels
        .iter()
        .filter(|label| {
            label.kind == RosterLabelKind::PeerAlias
                && !peer_alias_value_is_syncable(label.value.as_deref())
        })
        .map(|label| label.key.clone())
        .collect::<Vec<_>>();
    for key in bad_keys {
        note_local_label(replica, RosterLabelKind::PeerAlias, &key, None);
    }
}

/// An install's name for itself is local display only ("This controller").
#[must_use]
pub fn sibling_alias_is_syncable(key: &str, instance_hex: &str) -> bool {
    let key = key.trim();
    let instance_hex = instance_hex.trim();
    !key.is_empty() && !instance_hex.is_empty() && !key.eq_ignore_ascii_case(instance_hex)
}

pub fn strip_local_sibling_alias(replica: &mut RosterReplica, instance_hex: &str) {
    replica.labels.retain(|label| {
        label.kind != RosterLabelKind::SiblingAlias
            || sibling_alias_is_syncable(&label.key, instance_hex)
    });
}

#[must_use]
pub fn next_sibling_alias(aliases: &HashMap<String, String>) -> String {
    let mut taken = aliases
        .values()
        .filter_map(|value| sibling_alias_number(value))
        .collect::<Vec<_>>();
    taken.sort_unstable();
    taken.dedup();
    let mut number = 1u32;
    for used in taken {
        if used == number {
            number = number.saturating_add(1);
        }
    }
    format!("Sibling {number}")
}

fn sibling_alias_number(value: &str) -> Option<u32> {
    let rest = value.trim().strip_prefix("Sibling ")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn upsert_clock(replica: &RosterReplica, hash: IdentityHash) -> u64 {
    replica
        .upserts
        .iter()
        .find(|(candidate, _)| *candidate == hash)
        .map(|(_, clock)| *clock)
        .unwrap_or(0)
}

fn record_upsert(replica: &mut RosterReplica, hash: IdentityHash, clock: u64) {
    if let Some((_, current)) = replica
        .upserts
        .iter_mut()
        .find(|(candidate, _)| *candidate == hash)
    {
        *current = (*current).max(clock);
        return;
    }
    replica.upserts.push((hash, clock));
}

fn remove_upsert(replica: &mut RosterReplica, hash: IdentityHash) {
    replica.upserts.retain(|(candidate, _)| *candidate != hash);
}

fn tombstone_clock(replica: &RosterReplica, hash: IdentityHash) -> u64 {
    replica
        .tombstones
        .iter()
        .find(|(candidate, _)| *candidate == hash)
        .map(|(_, clock)| *clock)
        .unwrap_or(0)
}

fn set_tombstone(replica: &mut RosterReplica, hash: IdentityHash, clock: u64) {
    if let Some((_, current)) = replica
        .tombstones
        .iter_mut()
        .find(|(candidate, _)| *candidate == hash)
    {
        *current = (*current).max(clock);
        return;
    }
    replica.tombstones.push((hash, clock));
}

fn remove_tombstone(replica: &mut RosterReplica, hash: IdentityHash) {
    replica
        .tombstones
        .retain(|(candidate, _)| *candidate != hash);
}

#[must_use]
pub fn merge_roster(local: &RosterReplica, delta: &RosterDelta) -> (RosterReplica, MergePlan) {
    let mut merged = local.clone();
    merged.clock = merged.clock.max(delta.clock);
    let mut upserts = Vec::new();
    let mut forgets = Vec::new();
    for &(hash, clock) in &delta.tombstones {
        if clock < upsert_clock(local, hash) || clock < tombstone_clock(local, hash) {
            continue;
        }
        remove_upsert(&mut merged, hash);
        set_tombstone(&mut merged, hash, clock);
    }
    for upsert in &delta.upserts {
        let hash = upsert.access.target().identity_hash();
        if upsert.clock < upsert_clock(local, hash) {
            continue;
        }
        let tombstone = tombstone_clock(&merged, hash);
        if tombstone >= upsert.clock {
            forgets.push(hash);
            continue;
        }
        remove_tombstone(&mut merged, hash);
        record_upsert(&mut merged, hash, upsert.clock);
        upserts.push(clone_access(&upsert.access));
    }
    for &(hash, clock) in &delta.tombstones {
        if clock < upsert_clock(local, hash) || clock < tombstone_clock(local, hash) {
            continue;
        }
        if !upserts
            .iter()
            .any(|access| access.target().identity_hash() == hash)
        {
            forgets.push(hash);
        }
    }
    forgets.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    forgets.dedup();
    let mut labels = Vec::new();
    for label in &delta.labels {
        if label.kind == RosterLabelKind::PeerAlias
            && (!peer_alias_is_syncable(&label.key)
                || !peer_alias_value_is_syncable(label.value.as_deref()))
        {
            continue;
        }
        if label.kind == RosterLabelKind::SiblingAlias {
            let signer_hex = encode_hex(delta.signer.identity_hash().as_bytes());
            if !sibling_alias_is_syncable(&label.key, &signer_hex) {
                continue;
            }
        }
        if label.kind == RosterLabelKind::SiblingWifiLl {
            let signer_hex = encode_hex(delta.signer.identity_hash().as_bytes());
            // Only the owning instance may publish its wifi LL.
            if !label.key.eq_ignore_ascii_case(&signer_hex) {
                continue;
            }
        }
        if label.clock < label_clock(&merged, label.kind, &label.key) {
            continue;
        }
        upsert_label(&mut merged, label.clone());
        labels.push(label.clone());
    }
    for sibling in &delta.siblings {
        if sibling_is_removed(&merged, sibling.identity_hash()) {
            continue;
        }
        pin_sibling(&mut merged, *sibling);
    }
    let removed = merged
        .siblings
        .iter()
        .filter(|sibling| sibling_is_removed(&merged, sibling.identity_hash()))
        .map(|sibling| sibling.identity_hash())
        .collect::<Vec<_>>();
    merged
        .siblings
        .retain(|sibling| !removed.contains(&sibling.identity_hash()));
    let plan = MergePlan {
        clock: merged.clock,
        upserts,
        forgets,
        labels,
    };
    (merged, plan)
}

fn label_clock(replica: &RosterReplica, kind: RosterLabelKind, key: &str) -> u64 {
    replica
        .labels
        .iter()
        .find(|label| label.kind == kind && label.key == key)
        .map(|label| label.clock)
        .unwrap_or(0)
}

fn upsert_label(replica: &mut RosterReplica, label: RosterLabel) {
    if let Some(existing) = replica
        .labels
        .iter_mut()
        .find(|candidate| candidate.kind == label.kind && candidate.key == label.key)
    {
        if existing.clock <= label.clock {
            *existing = label;
        }
        return;
    }
    replica.labels.push(label);
}

fn authority_for_requests(requests: &RemoteControlRequestSet) -> RemoteControlControllerAuthority {
    if requests
        .iter()
        .any(RemoteControlRequestKind::requires_administrator)
    {
        RemoteControlControllerAuthority::Administrator
    } else {
        RemoteControlControllerAuthority::Operator
    }
}

fn clone_access(access: &RemoteControlTargetAccess) -> RemoteControlTargetAccess {
    RemoteControlTargetAccess::new(
        RemoteControlTargetIdentity::new(*access.target().public_keys()),
        access.authority(),
        *access.permitted_requests(),
    )
    .expect("persisted access already has a request")
}

pub fn load_replica(path: &Path) -> RosterReplica {
    let Ok(text) = std::fs::read_to_string(path) else {
        return RosterReplica::default();
    };
    parse_replica(&text)
}

pub fn persist_replica(path: &Path, replica: &RosterReplica) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format_replica(replica));
}

pub fn replica_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("roster-replica")
}

fn format_replica(replica: &RosterReplica) -> String {
    let mut out = format!("clock {}\n", replica.clock);
    for sibling in &replica.siblings {
        out.push_str("sibling ");
        out.push_str(&encode_hex(sibling.as_bytes()));
        out.push('\n');
    }
    let mut upserts: Vec<_> = replica.upserts.iter().collect();
    upserts.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (hash, clock) in upserts {
        out.push_str("upsert ");
        out.push_str(&encode_hex(hash.as_bytes()));
        out.push(' ');
        out.push_str(&clock.to_string());
        out.push('\n');
    }
    let mut tombstones: Vec<_> = replica.tombstones.iter().collect();
    tombstones.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (hash, clock) in tombstones {
        out.push_str("tombstone ");
        out.push_str(&encode_hex(hash.as_bytes()));
        out.push(' ');
        out.push_str(&clock.to_string());
        out.push('\n');
    }
    let mut labels = replica.labels.clone();
    labels.sort_by(|left, right| {
        left.kind
            .token()
            .cmp(right.kind.token())
            .then_with(|| left.key.cmp(&right.key))
    });
    for label in labels {
        out.push_str("label ");
        out.push_str(label.kind.token());
        out.push(' ');
        out.push_str(&label.clock.to_string());
        out.push(' ');
        out.push_str(&label.key);
        if let Some(value) = &label.value {
            out.push('\t');
            out.push_str(value);
        }
        out.push('\n');
    }
    out
}

fn parse_replica(text: &str) -> RosterReplica {
    let mut replica = RosterReplica::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(clock) = line.strip_prefix("clock ") {
            if let Ok(value) = clock.parse() {
                replica.clock = value;
            }
        } else if let Some(hex) = line.strip_prefix("sibling ") {
            if let Ok(bytes) = parse_hex::<IDENTITY_PUBLIC_KEY_LEN>(hex) {
                if let Ok(keys) = PublicIdentityMaterial::from_slice(&bytes) {
                    pin_sibling(&mut replica, keys);
                }
            }
        } else if let Some(rest) = line.strip_prefix("tombstone ") {
            let mut parts = rest.split_whitespace();
            let Some(hash_hex) = parts.next() else {
                continue;
            };
            let Some(clock) = parts.next().and_then(|value| value.parse().ok()) else {
                continue;
            };
            if let Ok(bytes) = parse_hex::<HASH_LEN>(hash_hex) {
                set_tombstone(&mut replica, IdentityHash::new(bytes), clock);
            }
        } else if let Some(rest) = line.strip_prefix("upsert ") {
            let mut parts = rest.split_whitespace();
            let Some(hash_hex) = parts.next() else {
                continue;
            };
            let Some(clock) = parts.next().and_then(|value| value.parse().ok()) else {
                continue;
            };
            if let Ok(bytes) = parse_hex::<HASH_LEN>(hash_hex) {
                record_upsert(&mut replica, IdentityHash::new(bytes), clock);
            }
        } else if let Some(rest) = line.strip_prefix("label ") {
            let Some((kind_token, rest)) = rest.split_once(' ') else {
                continue;
            };
            let Some(kind) = RosterLabelKind::from_token(kind_token) else {
                continue;
            };
            let Some((clock_token, rest)) = rest.split_once(' ') else {
                continue;
            };
            let Ok(clock) = clock_token.parse() else {
                continue;
            };
            let (key, value) = match rest.split_once('\t') {
                Some((key, value)) => (
                    key.trim(),
                    Some(value.trim())
                        .filter(|name| !name.is_empty())
                        .map(ToOwned::to_owned),
                ),
                None => (rest.trim(), None),
            };
            if key.is_empty() {
                continue;
            }
            upsert_label(
                &mut replica,
                RosterLabel {
                    kind,
                    key: key.to_owned(),
                    value,
                    clock,
                },
            );
        }
    }
    replica
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn parse_hex<const N: usize>(input: &str) -> Result<[u8; N], ()> {
    let cleaned: String = input.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    if cleaned.len() != N * 2 {
        return Err(());
    }
    let mut out = [0u8; N];
    for (index, chunk) in cleaned.as_bytes().chunks_exact(2).enumerate() {
        let value = core::str::from_utf8(chunk).map_err(|_| ())?;
        out[index] = u8::from_str_radix(value, 16).map_err(|_| ())?;
    }
    Ok(out)
}

pub fn write_pull(signer: &PrivateIdentityMaterial, out: &mut [u8]) -> Option<usize> {
    let required = 1 + IDENTITY_PUBLIC_KEY_LEN + SIGNATURE_LEN;
    if out.len() < required {
        return None;
    }
    out[0] = RosterMessageKind::Pull as u8;
    out[1..1 + IDENTITY_PUBLIC_KEY_LEN].copy_from_slice(signer.public().as_bytes());
    let material = sha256_chunks(&[PULL_DOMAIN, signer.identity_hash().as_bytes()]);
    out[1 + IDENTITY_PUBLIC_KEY_LEN..required].copy_from_slice(&signer.sign(&material).0);
    Some(required)
}

pub fn write_replica_message(
    signer: &PrivateIdentityMaterial,
    replica: &RosterReplica,
    snapshot: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let body = encode_replica_body(replica, snapshot, signer.public())?;
    let required = 1 + 8 + IDENTITY_PUBLIC_KEY_LEN + SIGNATURE_LEN + body.len();
    if out.len() < required {
        return None;
    }
    out[0] = RosterMessageKind::Replica as u8;
    out[1..9].copy_from_slice(&replica.clock.to_le_bytes());
    out[9..9 + IDENTITY_PUBLIC_KEY_LEN].copy_from_slice(signer.public().as_bytes());
    let material = replica_material(replica.clock, signer.public(), &body);
    let signature = signer.sign(&material);
    let sig_at = 9 + IDENTITY_PUBLIC_KEY_LEN;
    out[sig_at..sig_at + SIGNATURE_LEN].copy_from_slice(&signature.0);
    out[sig_at + SIGNATURE_LEN..required].copy_from_slice(&body);
    Some(required)
}

pub fn replica_message(
    signer: &PrivateIdentityMaterial,
    replica: &RosterReplica,
    snapshot: &[u8],
) -> Option<Vec<u8>> {
    let body = encode_replica_body(replica, snapshot, signer.public())?;
    let required = 1 + 8 + IDENTITY_PUBLIC_KEY_LEN + SIGNATURE_LEN + body.len();
    let mut out = vec![0u8; required];
    write_replica_message(signer, replica, snapshot, &mut out)?;
    Some(out)
}

fn replica_material(clock: u64, signer: PublicIdentityMaterial, body: &[u8]) -> [u8; 32] {
    sha256_chunks(&[
        REPLICA_DOMAIN,
        &clock.to_le_bytes(),
        signer.as_bytes(),
        body,
    ])
}

fn encode_replica_body(
    replica: &RosterReplica,
    snapshot: &[u8],
    signer: PublicIdentityMaterial,
) -> Option<Vec<u8>> {
    let upserts = decode_snapshot_upserts(snapshot, replica)?;
    let sibling_count = u16::try_from(replica.siblings.len()).ok()?;
    let tombstone_count = u16::try_from(replica.tombstones.len()).ok()?;
    let upsert_count = u16::try_from(upserts.len()).ok()?;
    let mut body = Vec::new();
    body.extend_from_slice(&upsert_count.to_le_bytes());
    for upsert in &upserts {
        encode_upsert(&mut body, upsert)?;
    }
    body.extend_from_slice(&tombstone_count.to_le_bytes());
    let mut tombstones: Vec<_> = replica.tombstones.iter().collect();
    tombstones.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (hash, clock) in tombstones {
        body.extend_from_slice(hash.as_bytes());
        body.extend_from_slice(&clock.to_le_bytes());
    }
    body.extend_from_slice(&sibling_count.to_le_bytes());
    for sibling in &replica.siblings {
        body.extend_from_slice(sibling.as_bytes());
    }
    let signer_hex = encode_hex(signer.identity_hash().as_bytes());
    let labels: Vec<_> = replica
        .labels
        .iter()
        .filter(|label| {
            label.kind != RosterLabelKind::SiblingAlias
                || sibling_alias_is_syncable(&label.key, &signer_hex)
        })
        .cloned()
        .collect();
    encode_labels(&mut body, &labels)?;
    Some(body)
}

pub(crate) fn encode_labels(body: &mut Vec<u8>, labels: &[RosterLabel]) -> Option<()> {
    let count = u16::try_from(labels.len()).ok()?;
    body.extend_from_slice(&count.to_le_bytes());
    for label in labels {
        body.push(label.kind.wire());
        let key = label.key.as_bytes();
        let key_len = u8::try_from(key.len()).ok()?;
        body.push(key_len);
        body.extend_from_slice(key);
        body.extend_from_slice(&label.clock.to_le_bytes());
        match &label.value {
            Some(value) => {
                let bytes = value.as_bytes();
                let value_len = u16::try_from(bytes.len()).ok()?;
                body.extend_from_slice(&value_len.to_le_bytes());
                body.extend_from_slice(bytes);
            }
            None => body.extend_from_slice(&0u16.to_le_bytes()),
        }
    }
    Some(())
}

fn encode_upsert(body: &mut Vec<u8>, upsert: &RosterUpsert) -> Option<()> {
    body.extend_from_slice(&upsert.access.target().public_keys().public_key_bytes());
    body.extend_from_slice(&upsert.clock.to_le_bytes());
    let requests = upsert.access.permitted_requests();
    let count = u8::try_from(requests.len()).ok()?;
    body.push(count);
    for kind in requests.iter() {
        body.push(kind.wire_value());
    }
    Some(())
}

fn decode_snapshot_upserts(snapshot: &[u8], replica: &RosterReplica) -> Option<Vec<RosterUpsert>> {
    if snapshot.is_empty() {
        return Some(Vec::new());
    }
    let persisted =
        personal_rns::persistence::read_remote_control_target_accesses_snapshot(snapshot).ok()?;
    Some(
        persisted
            .map(|access| {
                let hash = access.target().identity_hash();
                let clock = match upsert_clock(replica, hash) {
                    0 => replica.clock.max(1),
                    clock => clock,
                };
                RosterUpsert { access, clock }
            })
            .collect(),
    )
}

fn parse_roster_message(bytes: &[u8]) -> Option<RosterInbound<'_>> {
    let (kind, rest) = bytes.split_first()?;
    match *kind {
        1 => parse_pull(rest),
        2 => parse_replica_message(rest),
        3 => {
            let (code, rest) = rest.split_first()?;
            if !rest.is_empty() {
                return None;
            }
            Some(RosterInbound::Error(*code))
        }
        _ => None,
    }
}

enum RosterInbound<'a> {
    Pull { signer: PublicIdentityMaterial },
    Replica { delta: RosterDelta, _body: &'a [u8] },
    Error(u8),
}

fn parse_pull(rest: &[u8]) -> Option<RosterInbound<'_>> {
    if rest.len() != IDENTITY_PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return None;
    }
    let signer = PublicIdentityMaterial::from_slice(&rest[..IDENTITY_PUBLIC_KEY_LEN]).ok()?;
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&rest[IDENTITY_PUBLIC_KEY_LEN..]);
    let material = sha256_chunks(&[PULL_DOMAIN, signer.identity_hash().as_bytes()]);
    signer
        .verify(&material, &Ed25519Signature(signature))
        .ok()?;
    Some(RosterInbound::Pull { signer })
}

fn parse_replica_message(rest: &[u8]) -> Option<RosterInbound<'_>> {
    if rest.len() < 8 + IDENTITY_PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return None;
    }
    let clock = u64::from_le_bytes(rest[..8].try_into().ok()?);
    let signer = PublicIdentityMaterial::from_slice(&rest[8..8 + IDENTITY_PUBLIC_KEY_LEN]).ok()?;
    let sig_at = 8 + IDENTITY_PUBLIC_KEY_LEN;
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&rest[sig_at..sig_at + SIGNATURE_LEN]);
    let body = &rest[sig_at + SIGNATURE_LEN..];
    let material = replica_material(clock, signer, body);
    signer
        .verify(&material, &Ed25519Signature(signature))
        .ok()?;
    let delta = decode_replica_body(clock, signer, body)?;
    Some(RosterInbound::Replica { delta, _body: body })
}

fn decode_replica_body(
    clock: u64,
    signer: PublicIdentityMaterial,
    body: &[u8],
) -> Option<RosterDelta> {
    let mut at = 0;
    let upsert_count = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let mut upserts = Vec::with_capacity(upsert_count);
    for _ in 0..upsert_count {
        let keys =
            PublicIdentityMaterial::from_slice(body.get(at..at + IDENTITY_PUBLIC_KEY_LEN)?).ok()?;
        at += IDENTITY_PUBLIC_KEY_LEN;
        let upsert_clock = u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?);
        at += 8;
        let request_count = *body.get(at)? as usize;
        at += 1;
        let mut requests = RemoteControlRequestSet::empty();
        for _ in 0..request_count {
            let kind = request_kind_from_wire(*body.get(at)?)?;
            let _ = requests.insert(kind);
            at += 1;
        }
        let access = RemoteControlTargetAccess::new(
            RemoteControlTargetIdentity::new(keys.public_keys()),
            authority_for_requests(&requests),
            requests,
        )
        .ok()?;
        upserts.push(RosterUpsert {
            access,
            clock: upsert_clock,
        });
    }
    let tombstone_count = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let mut tombstones = Vec::with_capacity(tombstone_count);
    for _ in 0..tombstone_count {
        let mut hash = [0u8; HASH_LEN];
        hash.copy_from_slice(body.get(at..at + HASH_LEN)?);
        at += HASH_LEN;
        let tombstone_clock = u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?);
        at += 8;
        tombstones.push((IdentityHash::new(hash), tombstone_clock));
    }
    let sibling_count = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let mut siblings = Vec::with_capacity(sibling_count);
    for _ in 0..sibling_count {
        siblings.push(
            PublicIdentityMaterial::from_slice(body.get(at..at + IDENTITY_PUBLIC_KEY_LEN)?).ok()?,
        );
        at += IDENTITY_PUBLIC_KEY_LEN;
    }
    let labels = decode_labels(body.get(at..)?)?;
    Some(RosterDelta {
        clock,
        signer,
        upserts,
        tombstones,
        siblings,
        labels,
    })
}

pub(crate) fn decode_labels(body: &[u8]) -> Option<Vec<RosterLabel>> {
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut at = 0;
    let count = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let mut labels = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = RosterLabelKind::from_wire(*body.get(at)?);
        at += 1;
        let key_len = *body.get(at)? as usize;
        at += 1;
        let key = std::str::from_utf8(body.get(at..at + key_len)?)
            .ok()?
            .to_owned();
        at += key_len;
        let clock = u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?);
        at += 8;
        let value_len = u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize;
        at += 2;
        let value = if value_len == 0 {
            None
        } else {
            Some(
                std::str::from_utf8(body.get(at..at + value_len)?)
                    .ok()?
                    .to_owned(),
            )
        };
        at += value_len;
        let Some(kind) = kind else {
            continue;
        };
        labels.push(RosterLabel {
            kind,
            key,
            value,
            clock,
        });
    }
    if at != body.len() {
        return None;
    }
    Some(labels)
}

fn request_kind_from_wire(value: u8) -> Option<RemoteControlRequestKind> {
    RemoteControlRequestKind::ALL
        .into_iter()
        .find(|kind| kind.wire_value() == value)
}

fn is_sibling(replica: &RosterReplica, keys: &PublicIdentityMaterial) -> bool {
    replica
        .siblings
        .iter()
        .any(|sibling| sibling.identity_hash() == keys.identity_hash())
}

pub struct RosterSync;

impl RequestEndpoint<ControllerAppState> for RosterSync {
    const ENDPOINT_ID: &'static str = ROSTER_SYNC_REQUEST_ENDPOINT_ID;
    const POLICY: RequestEndpointPolicy = RequestEndpointPolicy::AllowAll;

    async fn handle(
        mut context: RequestContext<'_, ControllerAppState>,
        _node: &impl personal_rns::runtime::PrnsNodeApi,
    ) -> Result<(), Decline> {
        let inbound = parse_roster_message(context.data);
        let Ok(mut shared) = context.state.roster.lock() else {
            return context.respond([
                RosterMessageKind::Error as u8,
                RosterErrorCode::Malformed as u8,
            ]);
        };
        let Some(instance) = instance_material(&shared) else {
            return context.respond([
                RosterMessageKind::Error as u8,
                RosterErrorCode::Malformed as u8,
            ]);
        };
        match inbound {
            Some(RosterInbound::Pull { signer }) => {
                if !is_sibling(&shared.replica, &signer) {
                    drop(shared);
                    return context.respond([
                        RosterMessageKind::Error as u8,
                        RosterErrorCode::NotSibling as u8,
                    ]);
                }
                let reply = replica_message(&instance, &shared.replica, &shared.cached_snapshot);
                drop(shared);
                let Some(reply) = reply else {
                    return context.respond([
                        RosterMessageKind::Error as u8,
                        RosterErrorCode::Malformed as u8,
                    ]);
                };
                context.respond(reply)
            }
            Some(RosterInbound::Replica { delta, .. }) => {
                if !is_sibling(&shared.replica, &delta.signer) {
                    drop(shared);
                    return context.respond([
                        RosterMessageKind::Error as u8,
                        RosterErrorCode::NotSibling as u8,
                    ]);
                }
                shared.pending = Some(delta);
                let reply = replica_message(&instance, &shared.replica, &shared.cached_snapshot);
                drop(shared);
                let Some(reply) = reply else {
                    return context.respond([
                        RosterMessageKind::Error as u8,
                        RosterErrorCode::Malformed as u8,
                    ]);
                };
                context.respond(reply)
            }
            Some(RosterInbound::Error(code)) => {
                drop(shared);
                context.respond([RosterMessageKind::Error as u8, code])
            }
            None => {
                drop(shared);
                context.respond([
                    RosterMessageKind::Error as u8,
                    RosterErrorCode::Malformed as u8,
                ])
            }
        }
    }
}

pub fn parse_replica_reply(bytes: &[u8]) -> Option<RosterDelta> {
    match parse_roster_message(bytes)? {
        RosterInbound::Replica { delta, .. } => Some(delta),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_rns::identity::{PrivateIdentityMaterial, IDENTITY_SECRET_KEY_LEN};
    use personal_rns::remote_control::RemoteControlRequestKind;

    fn secret(fill: u8) -> PrivateIdentityMaterial {
        PrivateIdentityMaterial::from_bytes([fill; IDENTITY_SECRET_KEY_LEN])
    }

    fn access(fill: u8) -> RemoteControlTargetAccess {
        RemoteControlTargetAccess::new(
            RemoteControlTargetIdentity::new(secret(fill).public().public_keys()),
            RemoteControlControllerAuthority::Operator,
            RemoteControlRequestSet::only(RemoteControlRequestKind::Describe),
        )
        .unwrap()
    }

    #[test]
    fn union_adds_the_missing_row() {
        let local = RosterReplica::default();
        let delta = RosterDelta {
            clock: 3,
            signer: secret(0x11).public(),
            upserts: vec![RosterUpsert {
                access: access(0x21),
                clock: 3,
            }],
            tombstones: Vec::new(),
            siblings: vec![secret(0x11).public()],
            labels: Vec::new(),
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(merged.clock, 3);
        assert_eq!(plan.upserts.len(), 1);
        assert!(plan.forgets.is_empty());
        assert_eq!(merged.siblings.len(), 1);
    }

    #[test]
    fn newer_tombstone_beats_an_older_upsert() {
        let local = RosterReplica::default();
        let hash = access(0x21).target().identity_hash();
        let delta = RosterDelta {
            clock: 5,
            signer: secret(0x11).public(),
            upserts: vec![RosterUpsert {
                access: access(0x21),
                clock: 2,
            }],
            tombstones: vec![(hash, 5)],
            siblings: Vec::new(),
            labels: Vec::new(),
        };
        let (_merged, plan) = merge_roster(&local, &delta);
        assert_eq!(plan.upserts.len(), 0);
        assert_eq!(plan.forgets, vec![hash]);
    }

    #[test]
    fn a_newer_local_pair_beats_a_stale_sibling_tombstone() {
        let hash = access(0x21).target().identity_hash();
        let mut local = RosterReplica::default();
        forget_target_locally(&mut local, hash);
        note_local_upsert(&mut local, hash);
        assert_eq!(upsert_clock(&local, hash), 2);
        let delta = RosterDelta {
            clock: 4,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: vec![(hash, 1)],
            siblings: Vec::new(),
            labels: Vec::new(),
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(upsert_clock(&merged, hash), 2);
        assert_eq!(tombstone_clock(&merged, hash), 0);
        assert!(plan.forgets.is_empty());
        assert!(plan.upserts.is_empty());
    }

    #[test]
    fn newer_upsert_clears_an_older_tombstone() {
        let hash = access(0x21).target().identity_hash();
        let mut local = RosterReplica::default();
        set_tombstone(&mut local, hash, 2);
        let delta = RosterDelta {
            clock: 6,
            signer: secret(0x11).public(),
            upserts: vec![RosterUpsert {
                access: access(0x21),
                clock: 6,
            }],
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: Vec::new(),
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(tombstone_clock(&merged, hash), 0);
        assert_eq!(plan.upserts.len(), 1);
        assert!(plan.forgets.is_empty());
    }

    #[test]
    fn replica_file_round_trips_clock_siblings_and_tombstones() {
        let hash = IdentityHash::new([0xab; 16]);
        let mut replica = RosterReplica {
            clock: 9,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: vec![secret(0x33).public()],
            labels: vec![RosterLabel {
                kind: RosterLabelKind::TargetAlias,
                key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                value: Some("kitchen".to_string()),
                clock: 4,
            }],
        };
        set_tombstone(&mut replica, hash, 4);
        record_upsert(&mut replica, IdentityHash::new([0xcd; 16]), 7);
        let parsed = parse_replica(&format_replica(&replica));
        assert_eq!(parsed.clock, 9);
        assert_eq!(parsed.siblings, replica.siblings);
        assert_eq!(parsed.labels, replica.labels);
        assert_eq!(tombstone_clock(&parsed, hash), 4);
        assert_eq!(upsert_clock(&parsed, IdentityHash::new([0xcd; 16])), 7);
    }

    #[test]
    fn pull_and_replica_messages_verify() {
        let signer = secret(0x44);
        let mut replica = RosterReplica::default();
        replica.clock = 1;
        pin_sibling(&mut replica, secret(0x55).public());
        let mut pull = [0u8; 256];
        let pull_len = write_pull(&signer, &mut pull).unwrap();
        match parse_roster_message(&pull[..pull_len]).unwrap() {
            RosterInbound::Pull { signer: parsed } => {
                assert_eq!(parsed, signer.public());
            }
            _ => panic!("pull"),
        }
        let mut message = vec![0u8; 512];
        let len = write_replica_message(&signer, &replica, &[], &mut message).unwrap();
        let delta = parse_replica_reply(&message[..len]).unwrap();
        assert_eq!(delta.clock, 1);
        assert_eq!(delta.siblings.len(), 1);
        assert!(delta.labels.is_empty());
    }

    #[test]
    fn newer_label_wins_and_clear_is_kept() {
        let mut local = RosterReplica::default();
        note_local_label(
            &mut local,
            RosterLabelKind::TargetAlias,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("old"),
        );
        let delta = RosterDelta {
            clock: 4,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::TargetAlias,
                key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                value: None,
                clock: 4,
            }],
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(plan.labels[0].value, None);
        assert_eq!(
            label_clock(
                &merged,
                RosterLabelKind::TargetAlias,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ),
            4
        );
    }

    #[test]
    fn note_local_label_skips_unchanged_values() {
        let mut local = RosterReplica::default();
        note_local_label(
            &mut local,
            RosterLabelKind::TargetAlias,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("hv4C"),
        );
        assert_eq!(local.clock, 1);
        note_local_label(
            &mut local,
            RosterLabelKind::TargetAlias,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("hv4C"),
        );
        assert_eq!(local.clock, 1);
        note_local_label(
            &mut local,
            RosterLabelKind::TargetAlias,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("renamed"),
        );
        assert_eq!(local.clock, 2);
    }

    #[test]
    fn older_label_does_not_overwrite() {
        let mut local = RosterReplica::default();
        note_local_label(
            &mut local,
            RosterLabelKind::TargetName,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("kitchen"),
        );
        let delta = RosterDelta {
            clock: 1,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::TargetName,
                key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                value: Some("porch".to_string()),
                clock: 0,
            }],
        };
        let (_merged, plan) = merge_roster(&local, &delta);
        assert!(plan.labels.is_empty());
    }

    #[test]
    fn replica_message_carries_labels() {
        let signer = secret(0x44);
        let mut replica = RosterReplica::default();
        note_local_label(
            &mut replica,
            RosterLabelKind::ManagerAlias,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            Some("dad"),
        );
        let message = replica_message(&signer, &replica, &[]).unwrap();
        let delta = parse_replica_reply(&message).unwrap();
        assert_eq!(
            delta.labels,
            vec![RosterLabel {
                kind: RosterLabelKind::ManagerAlias,
                key: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                value: Some("dad".to_string()),
                clock: 1,
            }]
        );
    }

    #[test]
    fn sibling_alias_for_the_signer_is_not_merged() {
        let signer = secret(0x11).public();
        let signer_hex = encode_hex(signer.identity_hash().as_bytes());
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let local = RosterReplica::default();
        let delta = RosterDelta {
            clock: 2,
            signer,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![
                RosterLabel {
                    kind: RosterLabelKind::SiblingAlias,
                    key: signer_hex,
                    value: Some("Sibling 2".to_string()),
                    clock: 2,
                },
                RosterLabel {
                    kind: RosterLabelKind::SiblingAlias,
                    key: other.to_string(),
                    value: Some("Laptop".to_string()),
                    clock: 2,
                },
            ],
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(plan.labels.len(), 1);
        assert_eq!(plan.labels[0].key, other);
        assert_eq!(merged.labels.len(), 1);
        assert_eq!(merged.labels[0].value.as_deref(), Some("Laptop"));
    }

    #[test]
    fn replica_message_omits_the_signer_sibling_alias() {
        let signer = secret(0x44);
        let self_hex = encode_hex(signer.identity_hash().as_bytes());
        let mut replica = RosterReplica::default();
        note_local_label(
            &mut replica,
            RosterLabelKind::SiblingAlias,
            &self_hex,
            Some("This controller"),
        );
        note_local_label(
            &mut replica,
            RosterLabelKind::SiblingAlias,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("Phone"),
        );
        let message = replica_message(&signer, &replica, &[]).unwrap();
        let delta = parse_replica_reply(&message).unwrap();
        assert_eq!(delta.labels.len(), 1);
        assert_eq!(delta.labels[0].key, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(delta.labels[0].value.as_deref(), Some("Phone"));
    }

    #[test]
    fn strip_local_sibling_alias_leaves_other_rows() {
        let mut replica = RosterReplica::default();
        replica.labels.push(RosterLabel {
            kind: RosterLabelKind::SiblingAlias,
            key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            value: Some("Self".to_string()),
            clock: 1,
        });
        replica.labels.push(RosterLabel {
            kind: RosterLabelKind::SiblingAlias,
            key: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            value: Some("Phone".to_string()),
            clock: 1,
        });
        strip_local_sibling_alias(&mut replica, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(replica.labels.len(), 1);
        assert_eq!(replica.labels[0].key, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn controller_usb_host_ids_are_not_syncable_peer_aliases() {
        assert!(!peer_alias_is_syncable("d0d0d0d0d0d0d0d0"));
        assert!(!peer_alias_is_syncable("d1d1d1d1d1d1d1d1"));
        assert!(peer_alias_is_syncable("aabbccddeeff0011"));
    }

    #[test]
    fn this_controller_peer_alias_value_is_not_syncable() {
        assert!(!peer_alias_value_is_syncable(Some("This Controller")));
        assert!(!peer_alias_value_is_syncable(Some("this controller")));
        assert!(peer_alias_value_is_syncable(Some("Hv4A")));
        assert!(peer_alias_value_is_syncable(None));
        assert!(peer_alias_link_is_local_only(
            THIS_CONTROLLER_ALIAS_LINK,
            std::iter::empty::<&str>()
        ));
        assert!(peer_alias_link_is_local_only(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
        ));
        assert!(!peer_alias_link_is_local_only(
            "7abab434e6cd6dc7dac44b01f93eec16",
            ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
        ));
    }

    #[test]
    fn seed_import_skips_usb_host_peer_aliases() {
        let mut replica = RosterReplica::default();
        let mut values = std::collections::HashMap::new();
        values.insert("d0d0d0d0d0d0d0d0".to_string(), "cable".to_string());
        values.insert("aabbccddeeff0011".to_string(), "tower".to_string());
        import_seed_labels(&mut replica, RosterLabelKind::PeerAlias, &values);
        assert_eq!(replica.labels.len(), 1);
        assert_eq!(replica.labels[0].key, "aabbccddeeff0011");
    }

    #[test]
    fn seed_import_skips_this_controller_peer_alias_value() {
        let mut replica = RosterReplica::default();
        let mut values = std::collections::HashMap::new();
        values.insert(
            "08530421d2659610".to_string(),
            THIS_CONTROLLER_PEER_ALIAS.to_string(),
        );
        values.insert("aabbccddeeff0011".to_string(), "tower".to_string());
        import_seed_labels(&mut replica, RosterLabelKind::PeerAlias, &values);
        assert_eq!(replica.labels.len(), 1);
        assert_eq!(replica.labels[0].key, "aabbccddeeff0011");
    }

    #[test]
    fn retract_clears_this_controller_peer_alias_from_replica() {
        let mut replica = RosterReplica {
            clock: 10,
            labels: vec![RosterLabel {
                kind: RosterLabelKind::PeerAlias,
                key: "08530421d2659610".to_string(),
                value: Some(THIS_CONTROLLER_PEER_ALIAS.to_string()),
                clock: 10,
            }],
            ..RosterReplica::default()
        };
        retract_unsyncable_peer_alias_values(&mut replica);
        let label = replica
            .labels
            .iter()
            .find(|label| label.key == "08530421d2659610")
            .expect("retract keeps a clear row");
        assert_eq!(label.value, None);
        assert!(label.clock > 10);
    }

    #[test]
    fn merge_drops_this_controller_peer_alias_values() {
        let local = RosterReplica::default();
        let delta = RosterDelta {
            clock: 1,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![
                RosterLabel {
                    kind: RosterLabelKind::PeerAlias,
                    key: "08530421d2659610".to_string(),
                    value: Some(THIS_CONTROLLER_PEER_ALIAS.to_string()),
                    clock: 1,
                },
                RosterLabel {
                    kind: RosterLabelKind::PeerAlias,
                    key: "aabbccddeeff0011".to_string(),
                    value: Some("tower".to_string()),
                    clock: 1,
                },
            ],
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert!(merged
            .labels
            .iter()
            .all(|label| label.key != "08530421d2659610"));
        assert_eq!(plan.labels.len(), 1);
        assert_eq!(plan.labels[0].key, "aabbccddeeff0011");
    }

    #[test]
    fn next_sibling_alias_fills_the_lowest_unused_number() {
        let mut aliases = HashMap::new();
        assert_eq!(next_sibling_alias(&aliases), "Sibling 1");
        aliases.insert("aa".to_string(), "Sibling 1".to_string());
        aliases.insert("bb".to_string(), "Kitchen".to_string());
        aliases.insert("cc".to_string(), "Sibling 3".to_string());
        assert_eq!(next_sibling_alias(&aliases), "Sibling 2");
    }

    #[test]
    fn replica_file_round_trips_sibling_alias() {
        let replica = RosterReplica {
            clock: 3,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::SiblingAlias,
                key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                value: Some("Kitchen".to_string()),
                clock: 2,
            }],
        };
        let parsed = parse_replica(&format_replica(&replica));
        assert_eq!(parsed.labels, replica.labels);
    }

    #[test]
    fn replica_file_round_trips_pairing_advert_dismiss() {
        let replica = RosterReplica {
            clock: 4,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::PairingAdvertDismissed,
                key: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                value: Some("taken".to_string()),
                clock: 4,
            }],
        };
        let parsed = parse_replica(&format_replica(&replica));
        assert_eq!(parsed.labels, replica.labels);
        assert_eq!(
            RosterLabelKind::from_wire(RosterLabelKind::PairingAdvertDismissed.wire()),
            Some(RosterLabelKind::PairingAdvertDismissed)
        );
    }

    #[test]
    fn forgotten_sibling_is_not_re_pinned_by_an_older_replica() {
        let sibling = secret(0x77).public();
        let mut local = RosterReplica::default();
        pin_sibling(&mut local, sibling);
        forget_sibling_locally(&mut local, sibling.identity_hash());
        assert!(local.siblings.is_empty());
        let delta = RosterDelta {
            clock: 1,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: vec![sibling],
            labels: Vec::new(),
        };
        let (merged, _) = merge_roster(&local, &delta);
        assert!(merged.siblings.is_empty());
    }

    #[test]
    fn adopt_clears_a_sibling_removal() {
        let sibling = secret(0x88).public();
        let mut replica = RosterReplica::default();
        pin_sibling(&mut replica, sibling);
        forget_sibling_locally(&mut replica, sibling.identity_hash());
        adopt_sibling(&mut replica, sibling);
        assert_eq!(replica.siblings, vec![sibling]);
        assert!(!sibling_is_removed(&replica, sibling.identity_hash()));
    }

    #[test]
    fn replica_known_targets_omit_tombstoned_hashes() {
        let hash = access(0x21).target().identity_hash();
        let mut replica = RosterReplica::default();
        note_local_upsert(&mut replica, hash);
        assert_eq!(replica_known_targets(&replica), vec![hash]);
        forget_target_locally(&mut replica, hash);
        assert!(replica_forgets_target(&replica, hash));
        assert!(replica_known_targets(&replica).is_empty());
    }

    #[test]
    fn target_looking_wire_and_file_round_trip() {
        let replica = RosterReplica {
            clock: 5,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::TargetLooking,
                key: "cccccccccccccccccccccccccccccccc".to_string(),
                value: Some("dddddddddddddddddddddddddddddddd".to_string()),
                clock: 5,
            }],
        };
        let parsed = parse_replica(&format_replica(&replica));
        assert_eq!(parsed.labels, replica.labels);
        assert_eq!(
            RosterLabelKind::from_wire(RosterLabelKind::TargetLooking.wire()),
            Some(RosterLabelKind::TargetLooking)
        );
        assert_eq!(
            RosterLabelKind::from_token(RosterLabelKind::TargetLooking.token()),
            Some(RosterLabelKind::TargetLooking)
        );
    }

    #[test]
    fn sibling_wifi_ll_wire_and_file_round_trip() {
        let replica = RosterReplica {
            clock: 3,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::SiblingWifiLl,
                key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                value: Some("fe80::494:446c:eb84:e48b".to_string()),
                clock: 3,
            }],
        };
        let parsed = parse_replica(&format_replica(&replica));
        assert_eq!(parsed.labels, replica.labels);
        assert_eq!(
            RosterLabelKind::from_wire(RosterLabelKind::SiblingWifiLl.wire()),
            Some(RosterLabelKind::SiblingWifiLl)
        );
        assert_eq!(
            RosterLabelKind::from_token(RosterLabelKind::SiblingWifiLl.token()),
            Some(RosterLabelKind::SiblingWifiLl)
        );
    }

    #[test]
    fn sibling_wifi_ll_must_be_the_signer() {
        let signer = secret(0x11).public();
        let signer_hex = encode_hex(signer.identity_hash().as_bytes());
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let local = RosterReplica::default();
        let delta = RosterDelta {
            clock: 2,
            signer,
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![
                RosterLabel {
                    kind: RosterLabelKind::SiblingWifiLl,
                    key: other.to_string(),
                    value: Some("fe80::1".to_string()),
                    clock: 2,
                },
                RosterLabel {
                    kind: RosterLabelKind::SiblingWifiLl,
                    key: signer_hex.clone(),
                    value: Some("fe80::494:446c:eb84:e48b".to_string()),
                    clock: 2,
                },
            ],
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(plan.labels.len(), 1);
        assert_eq!(plan.labels[0].key, signer_hex);
        assert_eq!(
            merged.labels[0].value.as_deref(),
            Some("fe80::494:446c:eb84:e48b")
        );
    }

    #[test]
    fn replica_message_includes_signer_sibling_wifi_ll() {
        let signer = secret(0x44);
        let self_hex = encode_hex(signer.identity_hash().as_bytes());
        let mut replica = RosterReplica::default();
        note_local_label(
            &mut replica,
            RosterLabelKind::SiblingWifiLl,
            &self_hex,
            Some("fe80::494:446c:eb84:e48b"),
        );
        let message = replica_message(&signer, &replica, &[]).unwrap();
        let delta = parse_replica_reply(&message).unwrap();
        assert_eq!(delta.labels.len(), 1);
        assert_eq!(delta.labels[0].kind, RosterLabelKind::SiblingWifiLl);
        assert_eq!(delta.labels[0].key, self_hex);
        assert_eq!(
            delta.labels[0].value.as_deref(),
            Some("fe80::494:446c:eb84:e48b")
        );
    }

    #[test]
    fn find_path_looking_label_is_exclusive_and_cleared_on_forget() {
        let target = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let this_instance = "11111111111111111111111111111111";
        let other_instance = "22222222222222222222222222222222";
        let mut replica = RosterReplica::default();
        assert_eq!(
            attention_for_target(&replica, target, this_instance),
            TargetAttention::Free
        );
        note_local_label(
            &mut replica,
            RosterLabelKind::TargetLooking,
            target,
            Some(other_instance),
        );
        assert_eq!(
            attention_for_target(&replica, target, this_instance),
            TargetAttention::HeldBySibling
        );
        note_local_label(
            &mut replica,
            RosterLabelKind::TargetLooking,
            target,
            Some(this_instance),
        );
        assert_eq!(
            attention_for_target(&replica, target, this_instance),
            TargetAttention::HeldByThis
        );
        let hash = IdentityHash::new(parse_hex::<HASH_LEN>(target).expect("target hash"));
        forget_target_locally(&mut replica, hash);
        assert_eq!(looking_instance(&replica, target), None);
        assert_eq!(
            attention_for_target(&replica, target, this_instance),
            TargetAttention::Free
        );
    }

    #[test]
    fn newer_looking_label_wins() {
        let target = "ffffffffffffffffffffffffffffffff";
        let mut local = RosterReplica::default();
        note_local_label(
            &mut local,
            RosterLabelKind::TargetLooking,
            target,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let delta = RosterDelta {
            clock: 9,
            signer: secret(0x11).public(),
            upserts: Vec::new(),
            tombstones: Vec::new(),
            siblings: Vec::new(),
            labels: vec![RosterLabel {
                kind: RosterLabelKind::TargetLooking,
                key: target.to_string(),
                value: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
                clock: 9,
            }],
        };
        let (merged, plan) = merge_roster(&local, &delta);
        assert_eq!(
            looking_instance(&merged, target),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(plan.labels.len(), 1);
    }
}
