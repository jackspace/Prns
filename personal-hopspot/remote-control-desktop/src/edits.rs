use std::collections::HashMap;

use personal_rns::interfaces::lora::{
    CodingRate, Frequency, LoraBandwidth, ModemPreset, Modulation, PreambleSymbols, RadioProfile,
    RegulatoryRegion, SpreadingFactor, SubGRegion, TxPower,
};
use personal_rns::interfaces::InterfaceMode;
use personal_rns::remote_control::parse_wifi_station_ssid;

use crate::backend::InterfaceEntry;

pub const LORA_TX_POWER_MIN_DBM: i8 = -9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceField {
    Mode,
    Group,
    LoRaTune,
    WifiStation,
    TcpTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoRaTuneControl {
    Region,
    Frequency,
    Preset,
    SpreadingFactor,
    Bandwidth,
    CodingRate,
    TxPower,
    Preamble,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceDraft {
    pub mode: InterfaceMode,
    pub group: String,
    pub lora: Option<RadioProfile>,
    pub wifi_ssid: String,
    pub wifi_password: String,
    pub tcp_target: String,
}

impl InterfaceDraft {
    #[must_use]
    pub fn captured_from(entry: &InterfaceEntry) -> Self {
        Self {
            mode: entry.mode,
            group: saved_group(entry),
            lora: saved_lora(entry),
            wifi_ssid: saved_wifi_ssid(entry),
            wifi_password: String::new(),
            tcp_target: saved_tcp_target(entry),
        }
    }

    #[must_use]
    pub fn changed_fields(&self, saved: &InterfaceEntry) -> Vec<InterfaceField> {
        let mut fields = Vec::new();
        if self.mode != saved.mode {
            fields.push(InterfaceField::Mode);
        }
        if can_edit_group(saved) && self.group != saved_group(saved) {
            fields.push(InterfaceField::Group);
        }
        if can_edit_lora(saved) && self.lora != saved_lora(saved) {
            fields.push(InterfaceField::LoRaTune);
        }
        if can_edit_wifi_station(saved)
            && (self.wifi_ssid != saved_wifi_ssid(saved) || !self.wifi_password.is_empty())
        {
            fields.push(InterfaceField::WifiStation);
        }
        if can_edit_tcp_target(saved) && self.tcp_target != saved_tcp_target(saved) {
            fields.push(InterfaceField::TcpTarget);
        }
        fields
    }

    #[must_use]
    pub fn lora_control_changed(&self, saved: &InterfaceEntry, control: LoRaTuneControl) -> bool {
        let (Some(draft), Some(saved)) = (self.lora, saved_lora(saved)) else {
            return false;
        };
        match control {
            LoRaTuneControl::Region => draft.region() != saved.region(),
            LoRaTuneControl::Frequency => draft.frequency() != saved.frequency(),
            LoRaTuneControl::Preset => {
                ModemPreset::matching(draft.modulation())
                    != ModemPreset::matching(saved.modulation())
            }
            LoRaTuneControl::SpreadingFactor => spreading_factor(draft) != spreading_factor(saved),
            LoRaTuneControl::Bandwidth => bandwidth(draft) != bandwidth(saved),
            LoRaTuneControl::CodingRate => coding_rate(draft) != coding_rate(saved),
            LoRaTuneControl::TxPower => draft.tx_power() != saved.tx_power(),
            LoRaTuneControl::Preamble => draft.preamble() != saved.preamble(),
        }
    }

    #[must_use]
    pub fn is_dirty(&self, saved: &InterfaceEntry) -> bool {
        !self.changed_fields(saved).is_empty()
    }

    #[must_use]
    pub fn field_changed(&self, saved: &InterfaceEntry, field: InterfaceField) -> bool {
        self.changed_fields(saved).contains(&field)
    }
}

#[must_use]
pub fn draft_or_saved(
    drafts: &HashMap<String, InterfaceDraft>,
    key: &str,
    saved: &InterfaceEntry,
) -> InterfaceDraft {
    drafts
        .get(key)
        .cloned()
        .unwrap_or_else(|| InterfaceDraft::captured_from(saved))
}

pub fn revert_drafts(drafts: &mut HashMap<String, InterfaceDraft>, keys: &[String]) {
    for key in keys {
        drafts.remove(key);
    }
}

pub fn apply_draft_to_entry(entry: &mut InterfaceEntry, draft: &InterfaceDraft) {
    entry.mode = draft.mode;
    if can_edit_group(entry) {
        entry.group = Some(draft.group.clone());
    }
    if let Some(profile) = draft.lora.filter(|_| can_edit_lora(entry)) {
        crate::backend::apply_lora_profile_to_entry(entry, profile);
    }
    if can_edit_wifi_station(entry)
        && (parse_wifi_station_ssid(entry.detail.as_deref().unwrap_or("")).is_some()
            || !draft.wifi_ssid.is_empty())
    {
        crate::backend::apply_wifi_station_to_entry(entry, &draft.wifi_ssid);
    }
    if can_edit_tcp_target(entry) {
        crate::backend::apply_tcp_target_to_entry(entry, &draft.tcp_target);
    }
}

pub fn put_draft(
    drafts: &mut HashMap<String, InterfaceDraft>,
    key: String,
    saved: &InterfaceEntry,
    draft: InterfaceDraft,
) {
    if draft.is_dirty(saved) {
        drafts.insert(key, draft);
    } else {
        drafts.remove(&key);
    }
}

#[must_use]
pub fn dirty_keys_matching(
    drafts: &HashMap<String, InterfaceDraft>,
    saved_by_key: &HashMap<String, InterfaceEntry>,
    prefix: Option<&str>,
) -> Vec<String> {
    let mut keys: Vec<String> = drafts
        .iter()
        .filter(|(key, draft)| {
            prefix.is_none_or(|prefix| key.starts_with(prefix))
                && saved_by_key
                    .get(*key)
                    .is_some_and(|saved| draft.is_dirty(saved))
        })
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();
    keys
}

#[must_use]
pub fn can_edit_group(entry: &InterfaceEntry) -> bool {
    matches!(entry.kind.as_str(), "auto-wifi" | "bluetooth-auto")
}

#[must_use]
pub fn can_edit_lora(entry: &InterfaceEntry) -> bool {
    matches!(entry.kind.as_str(), "lora" | "rnode") && saved_lora(entry).is_some()
}

#[must_use]
pub fn can_edit_wifi_station(entry: &InterfaceEntry) -> bool {
    entry.kind == "auto-wifi"
}

#[must_use]
pub fn can_edit_tcp_target(entry: &InterfaceEntry) -> bool {
    entry.kind == "tcp-client"
}

#[must_use]
pub fn saved_tcp_target(entry: &InterfaceEntry) -> String {
    let host = entry
        .extras
        .iter()
        .find(|fact| fact.label == "Host")
        .map(|fact| fact.value.as_str());
    let port = entry
        .extras
        .iter()
        .find(|fact| fact.label == "Port")
        .map(|fact| fact.value.as_str());
    if let (Some(host), Some(port)) = (host, port) {
        return format!("{host}:{port}");
    }
    entry
        .detail
        .as_deref()
        .into_iter()
        .flat_map(|detail| detail.split(" · "))
        .map(str::trim)
        .find(|part| part.contains(':') && !part.starts_with("IFAC "))
        .unwrap_or("")
        .to_string()
}

#[must_use]
pub fn saved_wifi_ssid(entry: &InterfaceEntry) -> String {
    entry
        .detail
        .as_deref()
        .and_then(parse_wifi_station_ssid)
        .unwrap_or("")
        .to_string()
}

#[must_use]
pub fn saved_lora(entry: &InterfaceEntry) -> Option<RadioProfile> {
    entry
        .detail
        .as_deref()
        .and_then(RadioProfile::parse_inventory_config)
}

/// Regions the operator can pick: every regulated band plus the custom one.
#[must_use]
pub fn region_choices() -> Vec<SubGRegion> {
    RegulatoryRegion::ALL
        .into_iter()
        .map(SubGRegion::Regulated)
        .chain(std::iter::once(SubGRegion::Custom))
        .collect()
}

#[must_use]
pub fn region_from_label(label: &str) -> Option<SubGRegion> {
    region_choices()
        .into_iter()
        .find(|region| region.label() == label)
}

#[must_use]
pub fn apply_lora_region(profile: RadioProfile, region: SubGRegion) -> RadioProfile {
    if region == profile.region() {
        return profile;
    }
    let defaults = region.manual_lora_defaults();
    RadioProfile::new(
        region,
        defaults.frequency(),
        defaults.modulation(),
        defaults.tx_power(),
        defaults.preamble(),
    )
    .unwrap_or(profile)
}

#[must_use]
pub fn apply_lora_preset(profile: RadioProfile, preset: ModemPreset) -> RadioProfile {
    apply_lora_modulation(profile, preset.modulation())
}

#[must_use]
pub fn apply_lora_spreading_factor(
    profile: RadioProfile,
    spreading_factor: SpreadingFactor,
) -> RadioProfile {
    let Modulation::Lora {
        bandwidth,
        coding_rate,
        ..
    } = profile.modulation();
    apply_lora_modulation(
        profile,
        Modulation::Lora {
            spreading_factor,
            bandwidth,
            coding_rate,
        },
    )
}

#[must_use]
pub fn apply_lora_bandwidth(profile: RadioProfile, bandwidth: LoraBandwidth) -> RadioProfile {
    let Modulation::Lora {
        spreading_factor,
        coding_rate,
        ..
    } = profile.modulation();
    apply_lora_modulation(
        profile,
        Modulation::Lora {
            spreading_factor,
            bandwidth,
            coding_rate,
        },
    )
}

#[must_use]
pub fn apply_lora_coding_rate(profile: RadioProfile, coding_rate: CodingRate) -> RadioProfile {
    let Modulation::Lora {
        spreading_factor,
        bandwidth,
        ..
    } = profile.modulation();
    apply_lora_modulation(
        profile,
        Modulation::Lora {
            spreading_factor,
            bandwidth,
            coding_rate,
        },
    )
}

/// A wider channel can no longer fit where the old one sat, so re-centre the
/// frequency for the new shape before giving up on the edit.
fn apply_lora_modulation(profile: RadioProfile, modulation: Modulation) -> RadioProfile {
    if let Ok(next) = profile.with_modulation(modulation) {
        return next;
    }
    let Ok(recentred) = RadioProfile::new(
        profile.region(),
        Frequency::new(clamp_center_hz(
            profile.frequency().hz(),
            profile.region(),
            modulation,
        )),
        modulation,
        profile.tx_power(),
        profile.preamble(),
    ) else {
        return profile;
    };
    recentred
}

#[must_use]
pub fn clamp_lora_frequency(profile: RadioProfile, hz: u32) -> RadioProfile {
    let hz = clamp_center_hz(hz, profile.region(), profile.modulation());
    profile
        .with_frequency(Frequency::new(hz))
        .unwrap_or(profile)
}

#[must_use]
pub fn clamp_lora_tx_power(profile: RadioProfile, dbm: i8) -> RadioProfile {
    let ceiling = profile.region().max_tx_power().dbm();
    profile
        .with_tx_power(TxPower::new(dbm.clamp(LORA_TX_POWER_MIN_DBM, ceiling)))
        .unwrap_or(profile)
}

#[must_use]
pub fn clamp_lora_preamble(profile: RadioProfile, count: u16) -> RadioProfile {
    profile
        .with_preamble(PreambleSymbols::new(count.max(1)))
        .unwrap_or(profile)
}

/// The region range bounds the whole occupied channel, not just its centre, so
/// half a bandwidth is reserved at each edge.
fn clamp_center_hz(hz: u32, region: SubGRegion, modulation: Modulation) -> u32 {
    let Modulation::Lora { bandwidth, .. } = modulation;
    let range = region.frequency_range();
    let lower_half = bandwidth.hz() / 2;
    let upper_half = bandwidth.hz() - lower_half;
    hz.clamp(
        range.minimum().hz().saturating_add(lower_half),
        range.maximum().hz().saturating_sub(upper_half),
    )
}

#[must_use]
pub fn format_lora_frequency_input(hz: u32) -> String {
    format!("{}.{:03}", hz / 1_000_000, (hz % 1_000_000) / 1_000)
}

#[must_use]
pub fn parse_lora_frequency_mhz(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (mhz, frac) = match text.split_once('.') {
        Some((whole, rest)) => (whole, rest),
        None => (text, ""),
    };
    if mhz.len() < 3 {
        return None;
    }
    let mhz: u32 = mhz.parse().ok()?;
    let khz = match frac.len() {
        0 => 0,
        1 => frac.parse::<u32>().ok()? * 100,
        2 => frac.parse::<u32>().ok()? * 10,
        _ => frac.get(..3)?.parse().ok()?,
    };
    Some(mhz.saturating_mul(1_000_000).saturating_add(khz * 1_000))
}

fn spreading_factor(profile: RadioProfile) -> SpreadingFactor {
    let Modulation::Lora {
        spreading_factor, ..
    } = profile.modulation();
    spreading_factor
}

fn bandwidth(profile: RadioProfile) -> LoraBandwidth {
    let Modulation::Lora { bandwidth, .. } = profile.modulation();
    bandwidth
}

fn coding_rate(profile: RadioProfile) -> CodingRate {
    let Modulation::Lora { coding_rate, .. } = profile.modulation();
    coding_rate
}

#[must_use]
pub fn saved_group(entry: &InterfaceEntry) -> String {
    entry
        .group
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("reticulum")
        .to_string()
}

#[cfg(test)]
mod tests {
    use personal_rns::interfaces::InterfaceMode;

    use crate::backend::{InterfaceEntry, InterfacePower};

    use super::{
        apply_draft_to_entry, apply_lora_preset, apply_lora_region, dirty_keys_matching,
        draft_or_saved, put_draft, revert_drafts, saved_group, saved_tcp_target, saved_wifi_ssid,
        InterfaceDraft, InterfaceField, LoRaTuneControl,
    };

    fn entry(mode: InterfaceMode) -> InterfaceEntry {
        InterfaceEntry {
            id: "aabbccdd".to_string(),
            name: "tcp-client aabbccdd".to_string(),
            kind: "tcp-client".to_string(),
            power: InterfacePower::On,
            mode,
            connection: "Connected".to_string(),
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
    fn a_captured_draft_is_clean_until_a_field_differs() {
        let saved = entry(InterfaceMode::Full);
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert!(!draft.is_dirty(&saved));
        assert_eq!(draft.changed_fields(&saved), []);
        draft.mode = InterfaceMode::Gateway;
        assert!(draft.is_dirty(&saved));
        assert_eq!(draft.changed_fields(&saved), [InterfaceField::Mode]);
        assert!(draft.field_changed(&saved, InterfaceField::Mode));
        draft.mode = saved.mode;
        assert!(!draft.is_dirty(&saved));
    }

    #[test]
    fn put_draft_keeps_only_dirty_edits() {
        let saved = entry(InterfaceMode::Full);
        let mut drafts = std::collections::HashMap::new();
        let key = "controller:iface".to_string();
        put_draft(
            &mut drafts,
            key.clone(),
            &saved,
            InterfaceDraft {
                mode: InterfaceMode::Roaming,
                group: saved_group(&saved),
                lora: None,
                wifi_ssid: saved_wifi_ssid(&saved),
                wifi_password: String::new(),
                tcp_target: saved_tcp_target(&saved),
            },
        );
        assert_eq!(
            draft_or_saved(&drafts, &key, &saved).mode,
            InterfaceMode::Roaming
        );
        put_draft(
            &mut drafts,
            key.clone(),
            &saved,
            InterfaceDraft {
                mode: InterfaceMode::Full,
                group: saved_group(&saved),
                lora: None,
                wifi_ssid: saved_wifi_ssid(&saved),
                wifi_password: String::new(),
                tcp_target: saved_tcp_target(&saved),
            },
        );
        assert!(drafts.is_empty());
        assert_eq!(
            draft_or_saved(&drafts, &key, &saved).mode,
            InterfaceMode::Full
        );
    }

    #[test]
    fn dirty_keys_matching_scopes_to_a_host_prefix() {
        let saved = entry(InterfaceMode::Full);
        let mut drafts = std::collections::HashMap::new();
        let mut saved_by_key = std::collections::HashMap::new();
        saved_by_key.insert("controller:a".to_string(), saved.clone());
        saved_by_key.insert("target:b".to_string(), saved.clone());
        put_draft(
            &mut drafts,
            "controller:a".to_string(),
            &saved,
            InterfaceDraft {
                mode: InterfaceMode::Boundary,
                group: saved_group(&saved),
                lora: None,
                wifi_ssid: saved_wifi_ssid(&saved),
                wifi_password: String::new(),
                tcp_target: saved_tcp_target(&saved),
            },
        );
        put_draft(
            &mut drafts,
            "target:b".to_string(),
            &saved,
            InterfaceDraft {
                mode: InterfaceMode::Gateway,
                group: saved_group(&saved),
                lora: None,
                wifi_ssid: saved_wifi_ssid(&saved),
                wifi_password: String::new(),
                tcp_target: saved_tcp_target(&saved),
            },
        );
        assert_eq!(
            dirty_keys_matching(&drafts, &saved_by_key, Some("controller:")),
            ["controller:a"]
        );
        assert_eq!(
            dirty_keys_matching(&drafts, &saved_by_key, None),
            ["controller:a", "target:b"]
        );
        revert_drafts(&mut drafts, &["controller:a".to_string()]);
        assert_eq!(
            dirty_keys_matching(&drafts, &saved_by_key, None),
            ["target:b"]
        );
    }

    #[test]
    fn applying_a_draft_writes_mode_and_lan_group() {
        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "auto-wifi".to_string();
        saved.group = Some("reticulum".to_string());
        let tcp_target = saved_tcp_target(&saved);
        apply_draft_to_entry(
            &mut saved,
            &InterfaceDraft {
                mode: InterfaceMode::Gateway,
                group: "field-mesh".to_string(),
                lora: None,
                wifi_ssid: "field-lab".to_string(),
                wifi_password: String::new(),
                tcp_target,
            },
        );
        assert_eq!(saved.mode, InterfaceMode::Gateway);
        assert_eq!(saved.group.as_deref(), Some("field-mesh"));
        assert_eq!(saved.detail.as_deref(), Some("W,field-lab"));
        let mut tcp = entry(InterfaceMode::Full);
        let tcp_target = saved_tcp_target(&tcp);
        apply_draft_to_entry(
            &mut tcp,
            &InterfaceDraft {
                mode: InterfaceMode::Roaming,
                group: "ignored".to_string(),
                lora: None,
                wifi_ssid: "ignored".to_string(),
                wifi_password: String::new(),
                tcp_target,
            },
        );
        assert_eq!(tcp.mode, InterfaceMode::Roaming);
        assert_eq!(tcp.group, None);
    }

    #[test]
    fn applying_a_draft_writes_mode_and_ble_group() {
        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "bluetooth-auto".to_string();
        saved.group = Some("reticulum".to_string());
        let tcp_target = saved_tcp_target(&saved);
        apply_draft_to_entry(
            &mut saved,
            &InterfaceDraft {
                mode: InterfaceMode::Roaming,
                group: "field-mesh".to_string(),
                lora: None,
                wifi_ssid: String::new(),
                wifi_password: String::new(),
                tcp_target,
            },
        );
        assert_eq!(saved.mode, InterfaceMode::Roaming);
        assert_eq!(saved.group.as_deref(), Some("field-mesh"));
    }

    #[test]
    fn lan_group_edits_are_dirty_until_they_match_the_saved_value() {
        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "auto-wifi".to_string();
        saved.group = Some("reticulum".to_string());
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert!(!draft.is_dirty(&saved));
        draft.group = "field-mesh".to_string();
        assert!(draft.field_changed(&saved, InterfaceField::Group));
        draft.group = "reticulum".to_string();
        assert!(!draft.is_dirty(&saved));
    }

    #[test]
    fn ble_group_edits_are_dirty_until_they_match_the_saved_value() {
        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "bluetooth-auto".to_string();
        saved.group = Some("reticulum".to_string());
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert!(!draft.is_dirty(&saved));
        draft.group = "field-mesh".to_string();
        assert!(draft.field_changed(&saved, InterfaceField::Group));
        draft.group = "reticulum".to_string();
        assert!(!draft.is_dirty(&saved));
    }

    #[test]
    fn lora_tune_edits_are_dirty_until_they_match_the_saved_profile() {
        use personal_rns::interfaces::lora::{
            ModemPreset, RegulatoryRegion, SubGRegion, DEFAULT_915_PROFILE,
        };

        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "lora".to_string();
        crate::backend::apply_lora_profile_to_entry(&mut saved, DEFAULT_915_PROFILE);
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert!(!draft.is_dirty(&saved));
        draft.lora = Some(apply_lora_region(
            DEFAULT_915_PROFILE,
            SubGRegion::Regulated(RegulatoryRegion::Eu868),
        ));
        assert!(draft.field_changed(&saved, InterfaceField::LoRaTune));
        assert!(draft.lora_control_changed(&saved, LoRaTuneControl::Region));
        assert!(draft.lora_control_changed(&saved, LoRaTuneControl::Frequency));
        draft.lora = Some(apply_lora_preset(
            DEFAULT_915_PROFILE,
            ModemPreset::LongSlow,
        ));
        assert!(draft.lora_control_changed(&saved, LoRaTuneControl::Preset));
        draft.lora = Some(DEFAULT_915_PROFILE);
        assert!(!draft.is_dirty(&saved));
    }

    #[test]
    fn tcp_target_edits_are_dirty_until_they_match_the_saved_value() {
        let mut saved = entry(InterfaceMode::Full);
        crate::backend::apply_tcp_target_to_entry(&mut saved, "127.0.0.1:4242");
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert_eq!(draft.tcp_target, "127.0.0.1:4242");
        assert!(!draft.is_dirty(&saved));
        draft.tcp_target = "gateway.example:4242".to_string();
        assert!(draft.field_changed(&saved, InterfaceField::TcpTarget));
        draft.tcp_target = saved_tcp_target(&saved);
        assert!(!draft.is_dirty(&saved));
    }

    #[test]
    fn applying_a_draft_writes_the_tcp_target() {
        let mut saved = entry(InterfaceMode::Full);
        crate::backend::apply_tcp_target_to_entry(&mut saved, "127.0.0.1:4242");
        let group = saved_group(&saved);
        let wifi_ssid = saved_wifi_ssid(&saved);
        apply_draft_to_entry(
            &mut saved,
            &InterfaceDraft {
                mode: InterfaceMode::Full,
                group,
                lora: None,
                wifi_ssid,
                wifi_password: String::new(),
                tcp_target: "gateway.example:4242".to_string(),
            },
        );
        assert_eq!(saved.detail.as_deref(), Some("gateway.example:4242"));
        assert!(saved
            .extras
            .iter()
            .any(|fact| fact.label == "Host" && fact.value == "gateway.example"));
        assert!(saved
            .extras
            .iter()
            .any(|fact| fact.label == "Port" && fact.value == "4242"));
    }

    #[test]
    fn lan_station_edits_are_dirty_when_ssid_or_password_changes() {
        let mut saved = entry(InterfaceMode::Full);
        saved.kind = "auto-wifi".to_string();
        crate::backend::apply_wifi_station_to_entry(&mut saved, "home");
        let mut draft = InterfaceDraft::captured_from(&saved);
        assert_eq!(draft.wifi_ssid, "home");
        assert!(!draft.is_dirty(&saved));
        draft.wifi_ssid = "field-lab".to_string();
        assert!(draft.field_changed(&saved, InterfaceField::WifiStation));
        draft.wifi_ssid = saved_wifi_ssid(&saved);
        draft.wifi_password = "secret".to_string();
        assert!(draft.field_changed(&saved, InterfaceField::WifiStation));
        draft.wifi_password.clear();
        assert!(!draft.is_dirty(&saved));
    }
}
