use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;

use crate::backend::{
    auto_wifi_peer_list_note, bluetooth_auto_peer_list_note, format_activity_age,
    format_connect_label, format_pairing_open_label, format_target_route, format_wall_clock_now,
    interface_mode_label, radio_facts, target_label, BackendError, ControllerIdentity,
    InterfaceEntry, InterfacePower, PairingState, PathProbeReason, RemoteControlAnnounceWait,
    RemoteControlBackend, TargetAccess, TargetStatus,
};
use crate::edits::{
    apply_draft_to_entry, apply_lora_preset, apply_lora_region, can_edit_group, can_edit_lora,
    can_edit_tcp_target, can_edit_wifi_station, clamp_lora_frequency, clamp_lora_preamble,
    clamp_lora_tx_power, dirty_keys_matching, draft_or_saved, format_lora_frequency_input,
    parse_lora_frequency_mhz, put_draft, revert_drafts, saved_group, saved_tcp_target,
    set_lora_modulation, InterfaceDraft, InterfaceField, LoRaTuneControl, LORA_TX_POWER_MIN_DBM,
};
#[cfg(not(target_os = "android"))]
use crate::flash::{
    EnrolledFlashTarget, FlashDraft, FlashProgress, FlashRunOutcome, FlashStage, FlashStageState,
};
use crate::identity_clone::{IdentityCloneView, SiblingControllerView};

#[cfg(target_os = "android")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct FlashProgress;

#[cfg(target_os = "android")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FlashDraft;
use personal_rns::interfaces::lora::{
    CodingRate, LoraBandwidth, ModemPreset, Modulation, RadioProfile, RegulatoryRegion as Region,
    SpreadingFactor, SubGRegion,
};
use personal_rns::interfaces::InterfaceMode;
use personal_rns::remote_control::RemoteControlNetworkTransport;

const CONTROLLER_SCOPE: &str = "controller";
const ACTIVITY_LOG_LIMIT: usize = 80;

const STYLES: &str = r#"
:root { font-family: Inter, system-ui, sans-serif; color: #17221b; background: #edf2ed; }
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; }
button, input, select { font: inherit; }
button { cursor: pointer; }
.shell {
  height: 100vh;
  height: 100dvh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.app-bar {
  flex: 0 0 auto;
  z-index: 10;
  display: grid;
  grid-template-columns: 1fr auto 1fr;
  align-items: center;
  gap: 12px;
  padding: 12px 18px;
  background: #183d2b;
  color: #f5faf6;
  box-shadow: 0 1px 0 rgba(0, 0, 0, .18);
}
.app-bar-nav { display: flex; align-items: center; gap: 4px; justify-self: start; }
.app-bar h1 { margin: 0; font-size: 20px; font-weight: 700; text-align: center; letter-spacing: .01em; }
.app-bar-spacer { justify-self: end; }
.app-bar button {
  border: 0;
  background: transparent;
  color: #b7cfc0;
  width: 42px;
  height: 42px;
  border-radius: 10px;
  padding: 8px;
  display: grid;
  place-items: center;
}
.app-bar button:hover { color: white; background: #2d6046; }
.app-bar button.active {
  color: #143326;
  background: #eef7f1;
  box-shadow: 0 0 0 2px rgba(238, 247, 241, .35), inset 0 0 0 1px #9fcdb3;
}
.app-bar button.active:hover { color: #143326; background: #e4f2e9; }
.app-bar svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
.app-bar button.active svg { stroke-width: 2.35; }
.content {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
  padding: 28px 38px 38px;
  max-width: 1050px;
  width: 100%;
  margin: 0 auto;
}
@media (max-width: 720px) {
  .content { padding: 16px 14px 28px; }
  .app-bar { padding: 10px 12px; gap: 8px; }
  .app-bar h1 { font-size: 17px; }
}
h1 { margin: 5px 0 8px; font-size: 31px; }
.lead { color: #5a6a60; margin: 0; }
.section-intro { margin: 8px 0 26px; }
.section-intro .info-note { margin: 10px 0 0; }
.grid { display: grid; gap: 16px; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); }
.card { background: white; border: 1px solid #d7e0d9; border-radius: 10px; padding: 20px; box-shadow: 0 2px 8px rgba(24, 61, 43, .05); }
.flash-board { cursor: pointer; text-align: left; width: 100%; border: 1px solid #d7e0d9; background: white; border-radius: 10px; padding: 16px 18px; }
.flash-board.selected, .flash-board:hover { border-color: #2d6046; background: #f4f8f5; }
.flash-board.probable { border-color: #2d6046; box-shadow: 0 0 0 2px rgba(45, 96, 70, .22); }
.flash-board h3 { margin: 0 0 4px; font-size: 17px; }
.flash-board .meta { color: #5a6a60; font-size: 13px; margin: 0; }
.flash-board .detected { margin: 8px 0 0; font-size: 12px; font-weight: 600; color: #183d2b; }
.accordion-item.probable { border-color: #2d6046; box-shadow: 0 0 0 2px rgba(45, 96, 70, .22); }
.flash-actions { display: flex; flex-wrap: wrap; gap: 9px; align-items: center; margin-top: 4px; }
.flash-stages { list-style: none; display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px; margin: 10px 0 0; padding: 0; }
.flash-stages[data-count="3"] { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.flash-stage { position: relative; display: grid; justify-items: center; gap: 8px; text-align: center; }
.flash-stage::before { content: ""; position: absolute; top: 11px; right: calc(50% + 16px); left: calc(-50% + 16px); height: 2px; background: #d7e0d9; }
.flash-stage:first-child::before { content: none; }
.flash-stage.done::before, .flash-stage.current::before { background: #2d6046; }
.flash-stage.failed::before { background: #8a2f2f; }
.flash-stage .dot { width: 22px; height: 22px; border-radius: 50%; border: 2px solid #d7e0d9; background: white; }
.flash-stage.current .dot { border-color: #2d6046; background: #2d6046; box-shadow: 0 0 0 4px rgba(45, 96, 70, .18); }
.flash-stage.done .dot { border-color: #2d6046; background: #2d6046; }
.flash-stage.failed .dot { border-color: #8a2f2f; background: #8a2f2f; }
.flash-stage.skipped .dot { border-style: dashed; }
.flash-stage .label { font-size: 12px; font-weight: 600; color: #5a6a60; }
.flash-stage.current .label, .flash-stage.done .label { color: #183d2b; }
.flash-stage.failed .label { color: #8a2f2f; }
.flash-stage-detail { margin: 10px 0 0; white-space: pre-wrap; max-height: 14em; overflow: auto; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; }
.flash-manage { margin-top: 12px; }
.card h2, .card h3 { margin-top: 0; }
.row { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
.heading-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.heading-row h1, .heading-row h2 { margin: 0; }
.heading-actions { display: flex; align-items: center; }
.info { flex: 0 0 auto; width: 32px; height: 32px; border: 0; background: transparent; color: #5a6a60; border-radius: 50%; padding: 0; font: italic 700 16px/1 Georgia, "Times New Roman", serif; }
.info:hover, .info.open { background: #eef5f0; color: #183d2b; }
.info-note { margin: 10px 0 14px; }
.twisty-bar { display: flex; align-items: stretch; }
.twisty-bar .twisty-row { flex: 1; min-width: 0; }
.twisty-bar .interface-power { align-self: center; flex: 0 0 auto; min-width: 4.5rem; margin-right: 2px; }
.target-head { display: flex; align-items: flex-start; gap: 8px; padding: 14px 16px; }
.target-head .twisty-toggle { border: 0; background: transparent; padding: 2px 0 0; width: 24px; height: 28px; flex: 0 0 auto; display: grid; place-items: center; }
.target-main { flex: 1; min-width: 0; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 6px 12px; align-items: start; }
.target-copy { min-width: 0; display: grid; gap: 6px; }
.target-main > .target-section,
.target-main > .whitelist-rule { grid-column: 1 / -1; min-width: 0; }
.target-section { display: grid; gap: 8px; min-width: 0; }
.target-section-title { margin: 0; font-size: 12px; font-weight: 700; color: #17221b; }
.target-title { display: flex; flex-direction: column; align-items: stretch; gap: 2px; min-width: 0; }
.target-title .name { font-weight: 700; flex: 0 0 auto; }
.target-title .build { font-size: 12px; font-weight: 500; color: #64736a; flex: 0 0 auto; }
.target-title .peer-alias { flex: 1; min-width: 72px; max-width: none; height: 30px; margin: 0; border: 0; border-radius: 0; padding: 0; background: transparent; font: inherit; font-weight: 700; color: inherit; }
.target-title .peer-alias:focus { outline: none; box-shadow: 0 1px 0 #2d6046; }
.target-subtitle { font-size: 12px; font-weight: 500; color: #64736a; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.target-aside { display: flex; flex-direction: column; align-items: stretch; gap: 6px; min-width: 7.75rem; }
.target-aside-status { display: flex; align-items: center; justify-content: flex-end; gap: 2px; }
.target-controller-actions { display: flex; flex-wrap: wrap; gap: 6px; align-items: stretch; }
.target-controller-actions .button { flex: 0 0 auto; white-space: nowrap; }
.target-controller-actions .note { flex: 1 1 100%; margin: 0; font-size: 12px; }
.target-controller-actions .open-window {
  align-self: center;
  font-variant-numeric: tabular-nums;
  font-size: 13px;
  color: #2d6a9f;
  white-space: nowrap;
}
.target-copy .target-controller-actions { flex-wrap: wrap; gap: 9px; }
.target-head .refresh { align-self: center; }
.refresh { flex: 0 0 auto; align-self: center; border: 0; background: transparent; color: #183d2b; padding: 8px 10px; line-height: 1; font-size: 18px; }
.refresh:hover { background: #eef5f0; border-radius: 6px; }
.peer-head .refresh { padding: 2px 4px; font-size: 16px; }
.stack { display: grid; gap: 12px; }
.status { color: #64736a; font-size: 13px; }
.status.online { color: #167346; }
.status.sleeping { color: #956b16; }
.status.awaiting { color: #2d6a9f; }
.button { appearance: none; -webkit-appearance: none; border: 1px solid #b7c8bc; border-radius: 7px; background: white; color: #183d2b; padding: 7px 10px; font-family: inherit; font-size: 13px; font-weight: 400; line-height: normal; }
.button:hover { background: #f3f7f4; }
.button.primary { border-color: #246844; background: #246844; color: white; }
.button.danger { border-color: #a3493f; color: #8b3029; }
.button:disabled { opacity: .5; cursor: default; }
.button:disabled:hover { background: inherit; }
.button.primary:disabled:hover { background: #246844; }
.button.busy { display: inline-flex; align-items: center; justify-content: center; gap: 8px; min-width: 92px; }
.button.busy:disabled { opacity: 1; cursor: progress; }
.spinner { width: 14px; height: 14px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: spin .6s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.actions { display: flex; flex-wrap: wrap; gap: 9px; }
.interface-toolbar { overflow: hidden; }
.toolbar-track { display: flex; width: 200%; transition: transform .28s ease; }
.interface-toolbar.editing .toolbar-track { transform: translateX(-50%); }
.toolbar-pane { width: 50%; flex: 0 0 50%; min-width: 0; display: flex; align-items: center; gap: 9px; }
.toolbar-pane .button { min-width: 92px; }
.deck { overflow: hidden; }
.deck-track { display: flex; width: 200%; align-items: flex-start; transition: transform .28s ease; }
.deck-pane { width: 50%; flex: 0 0 50%; min-width: 0; }
.deck:not(.editing) .deck-pane:last-child,
.deck.editing .deck-pane:first-child { height: 0; min-height: 0; overflow: hidden; visibility: hidden; pointer-events: none; }
.deck.editing .deck-track { transform: translateX(-50%); }
.edit-card { display: grid; gap: 12px; }
.edit-card h3 { margin: 0; font-size: 15px; }
.flash-image-pick { display: grid; gap: 10px; margin: 0 0 20px; }
.flash-image-pick label { display: grid; gap: 8px; margin: 0; }
.flash-image-pick select { margin-top: 0; }
.flash-image-pick .flash-actions { margin: 0; }
.flash-image-pick .flash-actions .button { min-width: 92px; }
.flash-image-pick .flash-build-status { margin: 0; white-space: pre-wrap; max-height: 14em; overflow: auto; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; }
.facts div.changed { background: #fff8ea; box-shadow: inset 3px 0 0 #c48a1a; border-radius: 4px; padding: 2px 6px; }
.facts div.changed dt { color: #956b16; }
.unsaved-overlay { position: fixed; inset: 0; background: rgba(23, 34, 27, .35); display: grid; place-items: center; z-index: 20; padding: 24px; }
.unsaved-dialog { background: white; border: 1px solid #d7e0d9; border-radius: 10px; padding: 22px; max-width: 440px; width: 100%; box-shadow: 0 8px 24px rgba(24, 61, 43, .16); }
.unsaved-dialog p { margin: 0 0 16px; }
label { color: #425348; font-size: 14px; font-weight: 600; }
input { width: 100%; margin-top: 6px; border: 1px solid #bfcac2; border-radius: 7px; padding: 10px 11px; background: #fbfdfb; }
select { width: 100%; margin-top: 6px; border: 1px solid #bfcac2; border-radius: 7px; padding: 10px 11px; background: #fbfdfb; }
.flash-check { display: flex; align-items: center; gap: 8px; font-weight: 600; }
.flash-check input { width: auto; margin: 0; padding: 0; }
.digits { font-size: 30px; font-weight: 700; letter-spacing: .22em; }
.toggle { min-width: 58px; }
.note { color: #66766c; font-size: 14px; }
.error { color: #9b3129; }
.activity-log { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; max-height: 280px; overflow: auto; }
.activity-log li { display: grid; grid-template-columns: 72px 1fr; gap: 10px; align-items: start; font-size: 13px; }
.activity-log time { color: #52705c; font-variant-numeric: tabular-nums; }
.activity-log p { margin: 0; }
.accordion { display: grid; gap: 10px; }
.accordion-item { background: white; border: 1px solid #d7e0d9; border-radius: 8px; box-shadow: 0 2px 8px rgba(24, 61, 43, .05); }
.accordion-item.awaiting { border-style: dashed; }
.accordion-item.open { border-color: #27734c; box-shadow: 0 0 0 2px #d8ebdf; }
.twisty-row { width: 100%; border: 0; background: transparent; padding: 14px 16px; text-align: left; display: flex; align-items: center; gap: 8px; }
.twisty { width: 16px; height: 16px; flex: 0 0 auto; position: relative; color: #5a6a60; }
.twisty::before { content: ""; position: absolute; top: 50%; left: 50%; width: 6px; height: 6px; border-right: 1.5px solid currentColor; border-bottom: 1.5px solid currentColor; transform: translate(-70%, -50%) rotate(-45deg); }
.twisty.open::before { transform: translate(-50%, -65%) rotate(45deg); }
.twisty-copy { min-width: 0; flex: 1; display: grid; gap: 2px; }
.twisty-title { font-weight: 700; }
.twisty-as-of { color: #66766c; font-size: 12px; font-variant-numeric: tabular-nums; }
.twisty-path { color: #52705c; font-size: 12px; }
.twisty-address { color: #64736a; font-size: 13px; word-break: break-all; }
.allow-list-key { color: #64736a; font-size: 12px; word-break: break-all; }
.identity-block { margin: 18px 0 8px; }
.card > .heading-row { margin-bottom: 8px; }
.card > .heading-row h2 { margin: 0; }
.identity-block h2, .identity-block h3, .identity-block h4 { margin: 0 0 8px; }
.identity-kicker { font-size: 12px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; color: #52705c; margin: 0 0 4px; }
.identity-block .note { margin: 0 0 8px; }
.identity-block .interface-toolbar { margin: 4px 0 12px; }
.identity-block .actions { margin-top: 12px; }
.sibling-list { display: grid; gap: 8px; margin: 0 0 12px; }
.sibling-row { display: grid; gap: 2px; padding: 10px 12px; border: 1px solid #e3ebe5; border-radius: 7px; background: #fbfdfb; }
.sibling-row .twisty-title { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.sibling-row .sibling-alias { font-weight: 700; min-width: 0; }
.sibling-row .peer-alias { flex: 1; min-width: 80px; max-width: 220px; }
.sibling-row .whitelist-remove { flex: 0 0 auto; }
.hash-row { display: flex; align-items: center; gap: 8px; }
.hash-row .twisty-address { flex: 1; min-width: 0; }
.hash-row .status { margin-left: 0; }
.whitelist-table { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 6px 10px; align-items: center; width: 100%; min-width: 0; }
.whitelist-table.can-remove { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto; }
.whitelist-head { color: #52705c; font-size: 12px; font-weight: 600; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.whitelist-hash { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; border: 0; background: transparent; padding: 0; text-align: left; color: #17221b; font: inherit; font-variant-numeric: tabular-nums; }
.whitelist-hash:hover { text-decoration: underline; }
.whitelist-alias { min-width: 0; width: 100%; height: 30px; margin: 0; border: 1px solid #bfcac2; border-radius: 7px; padding: 0 8px; background: #fbfdfb; }
.whitelist-alias-text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #17221b; }
.whitelist-alias-text.placeholder { color: #64736a; }
.whitelist-remove { min-width: 92px; }
.hash-popup { max-width: 520px; }
.hash-popup .twisty-address { word-break: break-all; }
.whitelist-rule { border: 0; border-top: 1px solid #d7e0d9; margin: 4px 0; }
.accordion-body { padding: 0 16px 16px 40px; display: grid; gap: 12px; }
.interface-list-head { display: flex; align-items: center; gap: 12px; }
.interface-list-head .actions { margin: 0; }
.interface-list-head .interface-as-of { margin-left: auto; }
.interface-as-of { margin: 0; text-align: right; color: #66766c; font-size: 12px; font-variant-numeric: tabular-nums; }
.interface-list { display: grid; gap: 8px; }
.interface-item { border: 1px solid #e3ebe5; border-radius: 7px; background: #fbfdfb; }
.interface-item.open { border-color: #b7c8bc; }
.interface-item .twisty-row { padding: 11px 12px; }
.interface-item .accordion-body { padding: 0 12px 12px 38px; }
.facts { display: grid; gap: 4px; margin: 0; min-width: 0; }
.facts div { display: grid; grid-template-columns: minmax(0, 6.75rem) minmax(0, 1fr); gap: 8px 10px; align-items: start; font-size: 13px; min-width: 0; }
.facts dt { color: #52705c; font-weight: 600; min-width: 0; overflow-wrap: anywhere; }
.facts dd { margin: 0; color: #17221b; min-width: 0; overflow-wrap: anywhere; word-break: break-word; }
.facts dd.wrap { word-break: normal; overflow-wrap: anywhere; }
.facts dd.with-unit { display: flex; align-items: center; gap: 8px; min-width: 0; }
.facts dd.with-unit input { flex: 1 1 auto; width: auto; min-width: 0; }
.facts .unit { flex: 0 0 3.25em; color: #52705c; white-space: nowrap; }
.facts input,
.facts select { width: 100%; height: 30px; margin: 0; border: 1px solid #bfcac2; border-radius: 7px; padding: 0 8px; background: #fbfdfb; }
.facts .failure dd { color: #9b3129; }
.facts .health-warn dd { color: #8a5a12; }
.facts .health-ok dd { color: #1f6b3a; }
.peer-list { display: grid; gap: 10px; margin: 8px 0 0; padding: 0; list-style: none; min-width: 0; }
.peer-card { border: 1px solid #e3ebe5; border-radius: 6px; padding: 10px; background: #fff; min-width: 0; }
.peer-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 8px; font-size: 13px; }
.peer-title { display: flex; flex-direction: column; align-items: stretch; gap: 2px; min-width: 0; flex: 1; }
.peer-title .peer-alias { flex: 1; min-width: 72px; max-width: none; height: 30px; margin: 0; border: 0; border-radius: 0; padding: 0; background: transparent; font: inherit; font-weight: 700; color: inherit; }
.peer-title .peer-alias:focus { outline: none; box-shadow: 0 1px 0 #2d6046; }
.peer-subtitle { font-size: 12px; font-weight: 500; color: #64736a; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.peer-alias { flex: 1; min-width: 72px; height: 28px; margin: 0; border: 1px solid #bfcac2; border-radius: 7px; padding: 0 8px; background: #fbfdfb; }
@media (max-width: 720px) {
  .app-bar { padding: 10px 12px; }
  .content { padding: 20px 18px 24px; }
  .accordion-body,
  .interface-item .accordion-body { padding-left: 16px; }
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForgetPrompt {
    Idle,
    Confirming,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InterfaceHost {
    Controller,
    Target(String),
}

impl InterfaceHost {
    fn scope_id(&self) -> &str {
        match self {
            Self::Controller => CONTROLLER_SCOPE,
            Self::Target(id) => id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Nodes,
    Flash,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivityLogEntry {
    at: String,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoadedInterfaces {
    Idle,
    Loading,
    Ready {
        entries: Vec<InterfaceEntry>,
        arrived_at: String,
    },
    Failed(String),
}

impl LoadedInterfaces {
    fn ready(entries: Vec<InterfaceEntry>) -> Self {
        Self::Ready {
            entries,
            arrived_at: format_wall_clock_now(),
        }
    }

    fn entries(&self) -> Option<&[InterfaceEntry]> {
        match self {
            Self::Ready { entries, .. } => Some(entries),
            Self::Idle | Self::Loading | Self::Failed(_) => None,
        }
    }

    fn entries_mut(&mut self) -> Option<&mut Vec<InterfaceEntry>> {
        match self {
            Self::Ready { entries, .. } => Some(entries),
            Self::Idle | Self::Loading | Self::Failed(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoadedWhitelist {
    Loading,
    Ready(Vec<String>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhitelistTableKind {
    Status,
    Configure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnsavedPrompt {
    keys: Vec<String>,
    after: UnsavedAfter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UnsavedAfter {
    CollapseTarget { target_id: String },
    ChangeScreen { screen: Screen },
    ToggleInterface { key: String },
    CloseEditor { key: String },
}

#[allow(non_snake_case)]
pub fn App() -> Element {
    let screen = use_signal(|| Screen::Settings);
    let backend = use_signal(RemoteControlBackend::new);
    let mut selected_target = use_signal(String::new);
    let mut targets = use_signal({
        let items = backend().stored_targets();
        move || items
    });
    let mut interfaces_by_target = use_signal(HashMap::<String, LoadedInterfaces>::new);
    let mut expanded_targets = use_signal(HashSet::<String>::new);
    let expanded_interfaces = use_signal(HashSet::<String>::new);
    let drafts = use_signal(HashMap::<String, InterfaceDraft>::new);
    let mut editing = use_signal(HashSet::<String>::new);
    let saving = use_signal(HashSet::<String>::new);
    let save_notices = use_signal(HashMap::<String, String>::new);
    let focused_interface = use_signal(|| None::<String>);
    let mut unsaved = use_signal(|| None::<UnsavedPrompt>);
    let mut invitation_code = use_signal(String::new);
    let mut pairing = use_signal(|| PairingState::Idle);
    let mut pairing_error = use_signal(String::new);
    let mut pairing_target = use_signal(String::new);
    let activity_log = use_signal(Vec::<ActivityLogEntry>::new);
    let mut peer_aliases = use_signal(|| backend().peer_aliases());
    let mut manager_aliases = use_signal(|| backend().manager_aliases());
    let mut target_aliases = use_signal(|| backend().target_aliases());
    let mut sibling_aliases = use_signal(|| backend().sibling_aliases());
    let focused_sibling_alias = use_signal(|| None::<String>);
    let mut roster_gen = use_signal(|| 0u64);
    let interfaces_info = use_signal(|| false);
    let settings_info = use_signal(|| false);
    let nodes_info = use_signal(|| false);
    let flash_info = use_signal(|| false);
    let selected_flash_board = use_signal(|| None::<String>);
    let flash_configuring = use_signal(|| false);
    let flash_forms = use_signal(HashMap::<String, FlashDraft>::new);
    let flash_status = use_signal(String::new);
    let flashing = use_signal(|| false);
    let flash_progress = use_signal(|| None::<FlashProgress>);
    let mut clone_view = use_signal(|| backend().identity_clone().ok());

    use_effect(move || {
        let backend = backend();
        spawn(async move {
            if !backend.is_connected() {
                push_activity(activity_log, "Controller node failed to start.");
                return;
            }
            loop {
                backend.advance_target_monitors();
                // Local inventory is synchronous and must not wait on sibling
                // roster/clone link attempts (those can stall several seconds).
                match backend.local_interfaces() {
                    Ok(items) => {
                        interfaces_by_target
                            .write()
                            .insert(CONTROLLER_SCOPE.to_string(), LoadedInterfaces::ready(items));
                    }
                    Err(error) => {
                        interfaces_by_target.write().insert(
                            CONTROLLER_SCOPE.to_string(),
                            LoadedInterfaces::Failed(error.to_string()),
                        );
                    }
                }
                let _ = backend.advance_clone().await;
                if let Ok(view) = backend.identity_clone() {
                    if clone_view() != Some(view.clone()) {
                        clone_view.set(Some(view));
                    }
                    adopt_missing_aliases_preserving(
                        sibling_aliases,
                        backend.sibling_aliases(),
                        focused_sibling_alias(),
                    );
                }
                backend.maintain_roster().await;
                let gen = backend.roster_apply_generation();
                if gen != roster_gen() {
                    roster_gen.set(gen);
                    target_aliases.set(backend.target_aliases());
                    peer_aliases.set(backend.peer_aliases());
                    manager_aliases.set(backend.manager_aliases());
                    let next_siblings = aliases_preserving(
                        backend.sibling_aliases(),
                        &sibling_aliases(),
                        focused_sibling_alias().as_deref(),
                    );
                    if next_siblings != sibling_aliases() {
                        sibling_aliases.set(next_siblings);
                    }
                }
                match backend.targets().await {
                    Ok(items) => {
                        let pairing_busy = pairing_in_progress(&pairing());
                        let selected = selected_target();
                        if !pairing_busy
                            && (selected.is_empty()
                                || !items.iter().any(|item| item.id == selected))
                        {
                            if let Some(first) = items.first() {
                                selected_target.set(first.id.clone());
                            } else {
                                selected_target.set(String::new());
                            }
                        }
                        let live_ids: HashSet<String> =
                            items.iter().map(|item| item.id.clone()).collect();
                        interfaces_by_target
                            .write()
                            .retain(|id, _| id == CONTROLLER_SCOPE || live_ids.contains(id));
                        expanded_targets.write().retain(|id| live_ids.contains(id));
                        // Build / battery come from Connect or explicit refresh only.
                        adopt_missing_aliases(target_aliases, backend.target_aliases());
                        adopt_missing_aliases(peer_aliases, backend.peer_aliases());
                        targets.set(items);
                    }
                    Err(_) => {}
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    });

    rsx! {
        style { {STYLES} }
        div { class: "shell",
            header { class: "app-bar",
                div { class: "app-bar-nav",
                    button {
                        class: if screen() == Screen::Settings { "active" } else { "" },
                        title: "Settings",
                        aria_label: "Settings",
                        onclick: move |_| {
                            request_screen(
                                Screen::Settings,
                                screen,
                                drafts,
                                interfaces_by_target,
                                unsaved,
                            );
                        },
                        svg {
                            view_box: "0 0 24 24",
                            circle { cx: "12", cy: "12", r: "3" }
                            path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" }
                        }
                    }
                    button {
                        class: if screen() == Screen::Nodes { "active" } else { "" },
                        title: "Managed Nodes",
                        aria_label: "Managed Nodes",
                        onclick: move |_| {
                            request_screen(
                                Screen::Nodes,
                                screen,
                                drafts,
                                interfaces_by_target,
                                unsaved,
                            );
                        },
                        svg {
                            view_box: "0 0 24 24",
                            circle { cx: "12", cy: "5", r: "2.2" }
                            circle { cx: "6", cy: "18", r: "2.2" }
                            circle { cx: "18", cy: "18", r: "2.2" }
                            path { d: "M12 7.2v3.4M10.3 12.2 7.4 16.2M13.7 12.2l2.9 4" }
                        }
                    }
                    if cfg!(not(target_os = "android")) {
                        button {
                            class: if screen() == Screen::Flash { "active" } else { "" },
                            title: "Flash",
                            aria_label: "Flash",
                            onclick: move |_| {
                                request_screen(
                                    Screen::Flash,
                                    screen,
                                    drafts,
                                    interfaces_by_target,
                                    unsaved,
                                );
                            },
                            svg {
                                view_box: "0 0 24 24",
                                path { d: "M13 2 4 14h7l-1 8 10-14h-7l0-6z" }
                            }
                        }
                    }
                }
                h1 { "PRNS Controller" }
                div { class: "app-bar-spacer", aria_hidden: "true" }
            }
            main { class: "content",
                match screen() {
                    Screen::Flash => rsx! {
                        FlashSection {
                            backend,
                            screen,
                            drafts,
                            interfaces_by_target,
                            unsaved,
                            targets,
                            selected_target,
                            expanded_targets,
                            selected_flash_board,
                            flash_configuring,
                            flash_forms,
                            flash_status,
                            flashing,
                            flash_progress,
                            flash_info,
                        }
                    },
                    Screen::Settings => rsx! {
                        div { class: "heading-row",
                            h1 { "Settings" }
                            InfoHint {
                                label: "About Settings".to_string(),
                                open: settings_info,
                            }
                        }
                        div { class: "section-intro",
                            p { class: "lead", "This app runs its own Personal Reticulum (PRNS) node. Use the interfaces below to reach targets directly (1 hop) for pairing, or generally (any number of hops) to manage them." }
                            if settings_info() {
                                p { class: "note info-note", "to come" }
                            }
                        }
                        section { class: "card",
                            ControllerConfigurationPanel {
                                backend,
                                activity_log,
                            }
                        }
                        section { class: "card",
                            div { class: "heading-row",
                                h2 { "Interfaces" }
                                div { class: "heading-actions",
                                    InfoHint {
                                        label: "About these interfaces".to_string(),
                                        open: interfaces_info,
                                    }
                                    RefreshButton {
                                        label: "Refresh interfaces".to_string(),
                                        on_refresh: move |_| {
                                            load_controller_interfaces(
                                                backend(),
                                                interfaces_by_target,
                                            );
                                        },
                                    }
                                }
                            }
                            if interfaces_info() {
                                p { class: "note info-note", "These are this app's local transports (Auto Wi-Fi, TCP client, BLE, and USB). They are a separate stack from a prnsd running on this machine unless you Start the TCP client at that daemon. Start/Stop brings each transport up and down. Configure on the TCP client card sets this app's outbound host:port (IPv4 or DNS; default port 4242) and keeps it in the app data directory; that is not the Flash form's board TCP client. Configure also matches a target card, including BLE group. Only BLE Auto starts on unless HOPSPOT_RC_BLE=0. USB, Auto Wi-Fi, and TCP start off unless HOPSPOT_RC_USB=1, HOPSPOT_RC_AUTO_WIFI=1, or HOPSPOT_RC_TCP=host:port. HOPSPOT_RC_TCP overrides the saved target at boot and starts that client on. Do not run this app's BLE and a local prnsd Bluetooth Auto at the same time." }
                            }
                            InterfaceAccordion {
                                host: InterfaceHost::Controller,
                                loaded: interfaces_by_target()
                                    .get(CONTROLLER_SCOPE)
                                    .cloned()
                                    .unwrap_or(LoadedInterfaces::Loading),
                                expanded_interfaces,
                                drafts,
                                editing,
                                saving,
                                save_notices,
                                focused_interface,
                                unsaved,
                                expanded_targets,
                                screen,
                                interfaces_by_target,
                                backend,
                                activity_log,
                                peer_aliases,
                                pairing,
                            }
                        }
                        section { class: "card",
                            ControllerIdentityCard {
                                identity: backend().controller_identity(),
                                clone: clone_view(),
                                backend,
                                sibling_aliases,
                                focused_sibling_alias,
                                activity_log,
                            }
                        }
                        section { class: "card",
                            h2 { "Activity log" }
                            p { class: "note", "Configuration changes made through this app, newest first." }
                            if activity_log().is_empty() {
                                p { class: "note", "No configuration changes yet." }
                            } else {
                                ul { class: "activity-log",
                                    for (index, entry) in activity_log().into_iter().enumerate() {
                                        li {
                                            key: "{index}-{entry.at}",
                                            time { "{entry.at}" }
                                            p { "{entry.message}" }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    Screen::Nodes => rsx! {
                        div { class: "heading-row",
                            h1 { "Managed Nodes" }
                            div { class: "heading-actions",
                                InfoHint {
                                    label: "About Managed Nodes".to_string(),
                                    open: nodes_info,
                                }
                                RefreshButton {
                                    label: "Refresh target list".to_string(),
                                    on_refresh: move |_| {
                                        refresh_target_list(backend(), targets);
                                    },
                                }
                            }
                        }
                        div { class: "section-intro",
                            p { class: "lead", "Expand a node to configure it, or expand an awaiting pairing announcement to complete the pairing." }
                            if nodes_info() {
                                p { class: "note info-note",
                                    "Manual pairing: start this Controller first so it is already listening on a shared transport (BLE is on by default; enable USB, Auto Wi-Fi, or TCP in Settings if needed). Then open pairing on the target — each open mints a new invitation, so older codes will not work. On an OLED Hopspot use Global menu → Pair remote and note the eight hex digits. On MeshTower long-press the button; the LED blinks the invitation (0 is a long flash; 1–F are that many short flashes). For prnsd run pairing open. Expand the newest Awaiting pairing row here, enter that code, and Continue. Compare the six confirmation digits (not the invitation), then Approve on both sides in either order within about two minutes — OLED Approve on screen; MeshTower short-press approves and long-press rejects while the LED double-pulses; prnsd uses pairing approve. After pairing, expand the target and press Connect when you want inventory. If this app was not running when the target opened pairing, reopen pairing on the target. Use the newest awaiting row after a failed Continue; a wrong invitation fails silently. Enrolled Flash from this Controller already writes the grant, so those boards skip the invitation."
                                }
                            }
                        }
                        if targets().is_empty() {
                            p { class: "note", "No paired targets or pairing announcements yet." }
                        }
                        div { class: "accordion",
                            for target in targets() {
                                {
                                    let expanded = expanded_targets().contains(&target.id);
                                    let awaiting = target.status == TargetStatus::AwaitingPairing;
                                    let show_pairing = awaiting
                                        && (pairing_target().is_empty()
                                            || pairing_target() == target.id
                                            || !pairing_in_progress(&pairing()));
                                    rsx! {
                                        section {
                                            key: "{target.id}",
                                            class: accordion_item_class(awaiting, expanded),
                                            div { class: "target-head",
                                            button {
                                                class: "twisty-toggle",
                                                aria_expanded: if expanded { "true" } else { "false" },
                                                aria_label: format!(
                                                    "{} {}",
                                                    if expanded { "Collapse" } else { "Expand" },
                                                    target_display_name(&target)
                                                ),
                                                onclick: {
                                                    let target_id = target.id.clone();
                                                    let awaiting = awaiting;
                                                    move |_| {
                                                        let opening = !expanded_targets().contains(&target_id);
                                                        if !opening {
                                                            let keys = dirty_keys_matching(
                                                                &drafts(),
                                                                &saved_interfaces_by_key(
                                                                    &interfaces_by_target(),
                                                                ),
                                                                Some(&format!("{target_id}:")),
                                                            );
                                                            if !keys.is_empty() {
                                                                unsaved.set(Some(UnsavedPrompt {
                                                                    keys,
                                                                    after: UnsavedAfter::CollapseTarget {
                                                                        target_id: target_id.clone(),
                                                                    },
                                                                }));
                                                                return;
                                                            }
                                                        }
                                                        toggle_id(&mut expanded_targets, &target_id);
                                                        if !expanded_targets().contains(&target_id) {
                                                            let prefix = format!("{target_id}:");
                                                            editing.write().retain(|key| !key.starts_with(&prefix));
                                                        }
                                                        selected_target.set(target_id.clone());
                                                        if awaiting {
                                                            if opening
                                                                && !pairing_in_progress(&pairing())
                                                            {
                                                                pairing_target.set(target_id.clone());
                                                                invitation_code.set(String::new());
                                                                pairing.set(PairingState::Idle);
                                                                pairing_error.set(String::new());
                                                            }
                                                            return;
                                                        }
                                                    }
                                                },
                                                span { class: if expanded { "twisty open" } else { "twisty" } }
                                            }
                                            div { class: "target-main",
                                                div { class: "target-copy",
                                                    div { class: "target-title",
                                                        if awaiting {
                                                            span { class: "name", "{target_display_name(&target)}" }
                                                        } else {
                                                            input {
                                                                class: "peer-alias",
                                                                r#type: "text",
                                                                placeholder: "Alias",
                                                                value: "{target_aliases().get(&target.id).cloned().unwrap_or_default()}",
                                                                aria_label: format!("Alias for {}", target_display_name(&target)),
                                                                oninput: {
                                                                    let target_id = target.id.clone();
                                                                    move |event| {
                                                                        let value = event.value();
                                                                        if value.is_empty() {
                                                                            target_aliases.write().remove(&target_id);
                                                                        } else {
                                                                            target_aliases.write().insert(target_id.clone(), value);
                                                                        }
                                                                    }
                                                                },
                                                                onblur: {
                                                                    let target_id = target.id.clone();
                                                                    move |_| {
                                                                        persist_target_alias(
                                                                            target_id.clone(),
                                                                            backend(),
                                                                            target_aliases,
                                                                            peer_aliases,
                                                                            activity_log,
                                                                        );
                                                                    }
                                                                },
                                                                onkeydown: {
                                                                    let target_id = target.id.clone();
                                                                    move |event| {
                                                                        if event.key() == Key::Enter {
                                                                            persist_target_alias(
                                                                                target_id.clone(),
                                                                                backend(),
                                                                                target_aliases,
                                                                                peer_aliases,
                                                                                activity_log,
                                                                            );
                                                                        }
                                                                    }
                                                                },
                                                            }
                                                        }
                                                        if !awaiting {
                                                            span { class: "target-subtitle", "{target_display_name(&target)}" }
                                                        }
                                                    }
                                                    if awaiting {
                                                        AwaitingPairingActions {
                                                            target_id: target.id.clone(),
                                                            pairing_expires_in_secs: target
                                                                .pairing_expires_in_secs
                                                                .unwrap_or(0),
                                                            backend,
                                                            activity_log,
                                                            targets,
                                                            selected_target,
                                                            expanded_targets,
                                                            pairing,
                                                            pairing_error,
                                                            pairing_target,
                                                            invitation_code,
                                                        }
                                                    } else {
                                                        ControllerTargetActions {
                                                            target_id: target.id.clone(),
                                                            monitor_remaining_secs: target.monitor_remaining_secs,
                                                            backend,
                                                            activity_log,
                                                            target_aliases,
                                                            targets,
                                                            selected_target,
                                                            expanded_targets,
                                                            interfaces_by_target,
                                                            pairing,
                                                        }
                                                    }
                                                }
                                                div { class: "target-aside",
                                                    div { class: "target-aside-status",
                                                        span {
                                                            class: status_class(&target.status),
                                                            {status_label(&target.status)}
                                                        }
                                                        RefreshButton {
                                                            label: if awaiting {
                                                                format!("Refresh {}", target_display_name(&target))
                                                            } else {
                                                                format!(
                                                                    "Refresh interfaces on {}",
                                                                    target_aliases()
                                                                        .get(&target.id)
                                                                        .cloned()
                                                                        .filter(|name| !name.trim().is_empty())
                                                                        .unwrap_or_else(|| target_display_name(&target))
                                                                )
                                                            },
                                                            on_refresh: {
                                                                let target_id = target.id.clone();
                                                                let awaiting = awaiting;
                                                                move |_| {
                                                                    if awaiting {
                                                                        refresh_target_list(
                                                                            backend(),
                                                                            targets,
                                                                        );
                                                                        return;
                                                                    }
                                                                    refresh_host_interfaces(
                                                                        InterfaceHost::Target(target_id.clone()),
                                                                        backend(),
                                                                        interfaces_by_target,
                                                                        pairing(),
                                                                    );
                                                                }
                                                            },
                                                        }
                                                    }
                                                }
                                                if !awaiting {
                                                    hr { class: "whitelist-rule" }
                                                    ManagedTargetConfiguration {
                                                        target: target.clone(),
                                                        backend,
                                                        activity_log,
                                                        targets,
                                                        target_aliases,
                                                    }
                                                }
                                            }
                                            }
                                            if expanded {
                                                div { class: "accordion-body",
                                                    if awaiting {
                                                        if show_pairing {
                                                            PairingPanel {
                                                                target_id: target.id.clone(),
                                                                invitation_code,
                                                                pairing,
                                                                pairing_error,
                                                                pairing_target,
                                                                activity_log,
                                                                target_aliases,
                                                                targets,
                                                                selected_target,
                                                                expanded_targets,
                                                                interfaces_by_target,
                                                                backend,
                                                            }
                                                        } else {
                                                            p { class: "note", "A pairing attempt is already in progress on another target." }
                                                        }
                                                    } else {
                                                        {
                                                            let loaded = interfaces_by_target()
                                                                .get(&target.id)
                                                                .cloned()
                                                                .unwrap_or(LoadedInterfaces::Idle);
                                                            let arrived_at = match &loaded {
                                                                LoadedInterfaces::Ready {
                                                                    arrived_at,
                                                                    ..
                                                                } => Some(arrived_at.clone()),
                                                                LoadedInterfaces::Idle
                                                                | LoadedInterfaces::Loading
                                                                | LoadedInterfaces::Failed(_) => {
                                                                    None
                                                                }
                                                            };
                                                            rsx! {
                                                                hr { class: "whitelist-rule" }
                                                                div { class: "target-section",
                                                                    h3 { class: "target-section-title", "Actions" }
                                                                    TargetControls {
                                                                        target_id: target.id.clone(),
                                                                        status: target.status,
                                                                        backend,
                                                                        activity_log,
                                                                        targets,
                                                                    }
                                                                }
                                                                hr { class: "whitelist-rule" }
                                                                div { class: "target-section",
                                                                    div { class: "interface-list-head",
                                                                        h3 { class: "target-section-title", "Interfaces" }
                                                                        if let Some(arrived_at) = arrived_at {
                                                                            p { class: "interface-as-of", "as of {arrived_at}" }
                                                                        }
                                                                    }
                                                                    InterfaceAccordion {
                                                                        host: InterfaceHost::Target(target.id.clone()),
                                                                        loaded,
                                                                        expanded_interfaces,
                                                                        drafts,
                                                                        editing,
                                                                        saving,
                                                                        save_notices,
                                                                        focused_interface,
                                                                        unsaved,
                                                                        expanded_targets,
                                                                        screen,
                                                                        interfaces_by_target,
                                                                        backend,
                                                                        activity_log,
                                                                        peer_aliases,
                                                                        pairing,
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        hr { class: "whitelist-rule" }
                                                        div { class: "target-section",
                                                            h3 { class: "target-section-title", "Controller Id" }
                                                            NodeManagementWhitelist {
                                                                target_id: target.id.clone(),
                                                                backend,
                                                                activity_log,
                                                                manager_aliases,
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
            if let Some(prompt) = unsaved() {
                UnsavedChangesDialog {
                    prompt,
                    drafts,
                    editing,
                    saving,
                    save_notices,
                    focused_interface,
                    unsaved,
                    expanded_interfaces,
                    expanded_targets,
                    interfaces_by_target,
                    screen,
                    backend,
                    activity_log,
                }
            }
        }
    }
}

#[component]
fn ManagedTargetConfiguration(
    target: TargetAccess,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    mut targets: Signal<Vec<TargetAccess>>,
    target_aliases: Signal<HashMap<String, String>>,
) -> Element {
    let target_id = target.id.clone();
    let configure_title = target_aliases()
        .get(&target.id)
        .map(|alias| alias.trim())
        .filter(|alias| !alias.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| target_display_name(&target));
    let mut configuring = use_signal(|| false);
    let mut transport_draft = use_signal(|| RemoteControlNetworkTransport::Disabled);
    let mut connect_remaining = use_signal(|| target.monitor_remaining_secs);
    let mut saving = use_signal(|| false);
    use_effect({
        let remaining = target.monitor_remaining_secs;
        move || {
            connect_remaining.set(remaining);
        }
    });
    let tick_stop = use_hook(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let tick_cancel = tick_stop.clone();
    use_drop(move || {
        tick_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    use_hook({
        let stop = tick_stop.clone();
        let target_id = target_id.clone();
        move || {
            let stop = stop.clone();
            let target_id = target_id.clone();
            spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let remaining = backend().monitor_remaining_secs(&target_id);
                    if connect_remaining() != remaining {
                        connect_remaining.set(remaining);
                    }
                    if remaining == 0 && configuring() {
                        configuring.set(false);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
    });

    let baseline_transport = targets()
        .iter()
        .find(|item| item.id == target_id)
        .and_then(|item| item.network_transport)
        .or(target.network_transport);
    let dirty = baseline_transport.is_some_and(|transport| transport != transport_draft());

    rsx! {
        div { class: "target-section",
            h3 { class: "target-section-title", "Configuration" }
            if connect_remaining() > 0 {
                div { class: if configuring() { "interface-toolbar editing" } else { "interface-toolbar" },
                    div { class: "toolbar-track",
                        div { class: "toolbar-pane",
                            button {
                                class: "button",
                                r#type: "button",
                                onclick: {
                                    let target_id = target_id.clone();
                                    move |_| {
                                        if let Some(transport) = targets()
                                            .iter()
                                            .find(|item| item.id == target_id)
                                            .and_then(|item| item.network_transport)
                                        {
                                            transport_draft.set(transport);
                                        }
                                        configuring.set(true);
                                        let target_id = target_id.clone();
                                        let backend = backend();
                                        spawn(async move {
                                            if let Ok(transport) =
                                                backend.describe_network_transport(&target_id).await
                                            {
                                                targets.write().iter_mut().filter(|item| item.id == target_id).for_each(
                                                    |item| item.network_transport = Some(transport),
                                                );
                                                transport_draft.set(transport);
                                            }
                                        });
                                    }
                                },
                                "Configure"
                            }
                        }
                        div { class: "toolbar-pane",
                            button {
                                class: "button",
                                disabled: saving(),
                                r#type: "button",
                                onclick: {
                                    let target_id = target_id.clone();
                                    let fallback = target.network_transport;
                                    move |_| {
                                        let baseline = targets()
                                            .iter()
                                            .find(|item| item.id == target_id)
                                            .and_then(|item| item.network_transport)
                                            .or(fallback);
                                        if baseline.is_some_and(|transport| transport != transport_draft()) {
                                            if let Some(transport) = baseline {
                                                transport_draft.set(transport);
                                            }
                                        }
                                        configuring.set(false);
                                    }
                                },
                                if dirty {
                                    "Cancel"
                                } else {
                                    "Back"
                                }
                            }
                            button {
                                class: if saving() { "button busy" } else { "button" },
                                disabled: !dirty || saving(),
                                r#type: "button",
                                aria_busy: if saving() { "true" } else { "false" },
                                onclick: {
                                    let target_id = target_id.clone();
                                    let fallback = target.network_transport;
                                    move |_| {
                                        let baseline = targets()
                                            .iter()
                                            .find(|item| item.id == target_id)
                                            .and_then(|item| item.network_transport)
                                            .or(fallback);
                                        let dirty_now = baseline
                                            .is_some_and(|transport| transport != transport_draft());
                                        if saving() || !dirty_now {
                                            return;
                                        }
                                        let target_id = target_id.clone();
                                        let transport = transport_draft();
                                        let backend = backend();
                                        saving.set(true);
                                        spawn(async move {
                                            match backend.set_network_transport(&target_id, transport).await {
                                                Ok(()) => {
                                                    targets.write().iter_mut().filter(|item| item.id == target_id).for_each(
                                                        |item| item.network_transport = Some(transport),
                                                    );
                                                    configuring.set(false);
                                                    push_activity(
                                                        activity_log,
                                                        format!(
                                                            "Set transport on {target_id} to {}.",
                                                            network_transport_label(transport)
                                                        ),
                                                    );
                                                }
                                                Err(error) => push_activity(
                                                    activity_log,
                                                    format!("Configure transport failed: {error}"),
                                                ),
                                            }
                                            saving.set(false);
                                        });
                                    }
                                },
                                if saving() {
                                    span { class: "spinner", aria_hidden: "true" }
                                    "Saving"
                                } else {
                                    "Save"
                                }
                            }
                        }
                    }
                }
            }
            div { class: if configuring() { "deck editing" } else { "deck" },
                div { class: "deck-track",
                    div { class: "deck-pane",
                        dl { class: "facts",
                            if let Some(version) = target.build_version.as_deref().filter(|text| !text.is_empty()) {
                                div { dt { "PRNS" } dd { "{version}" } }
                            }
                            if let Some(battery) = target.battery.as_deref().filter(|text| !text.is_empty()) {
                                div { dt { "Battery" } dd { "{battery}" } }
                            }
                            div { dt { "Address" } dd { "{target.id}" } }
                            div {
                                dt { "Path found at" }
                                dd {
                                    if let Some(path) = target.path.as_ref() {
                                        "{path.announced_at}"
                                    } else {
                                        "Not heard yet"
                                    }
                                }
                            }
                            div {
                                dt { "Route" }
                                dd { {format_target_route(target.path.as_ref())} }
                            }
                            div {
                                dt { "Transport" }
                                dd { "{network_transport_status(target.network_transport)}" }
                            }
                        }
                    }
                    div { class: "deck-pane",
                        div { class: "edit-card",
                            h3 { "Configure {configure_title}" }
                            dl { class: "facts",
                                div {
                                    class: if dirty { "changed" } else { "" },
                                    dt { "Transport" }
                                    dd {
                                        select {
                                            value: "{transport_draft().wire_value()}",
                                            disabled: saving(),
                                            onchange: move |event| {
                                                let Some(transport) = event
                                                    .value()
                                                    .parse::<u8>()
                                                    .ok()
                                                    .and_then(RemoteControlNetworkTransport::from_wire)
                                                else {
                                                    return;
                                                };
                                                transport_draft.set(transport);
                                            },
                                            for option in RemoteControlNetworkTransport::ALL {
                                                option {
                                                    value: "{option.wire_value()}",
                                                    selected: transport_draft() == option,
                                                    "{network_transport_label(option)}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AwaitingPairingActions(
    target_id: String,
    pairing_expires_in_secs: u32,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    mut targets: Signal<Vec<TargetAccess>>,
    mut selected_target: Signal<String>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut pairing: Signal<PairingState>,
    mut pairing_error: Signal<String>,
    mut pairing_target: Signal<String>,
    mut invitation_code: Signal<String>,
) -> Element {
    let mut open_remaining = use_signal(|| pairing_expires_in_secs);
    use_effect(move || {
        open_remaining.set(pairing_expires_in_secs);
    });
    let tick_stop = use_hook(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let tick_cancel = tick_stop.clone();
    use_drop(move || {
        tick_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    use_hook({
        let stop = tick_stop.clone();
        let target_id = target_id.clone();
        move || {
            let stop = stop.clone();
            let target_id = target_id.clone();
            spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let remaining = backend().pairing_expires_in_secs(&target_id).unwrap_or(0);
                    if open_remaining() != remaining {
                        open_remaining.set(remaining);
                        targets
                            .write()
                            .iter_mut()
                            .filter(|item| item.id == target_id)
                            .for_each(|item| item.pairing_expires_in_secs = Some(remaining));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
    });
    rsx! {
        div { class: "target-controller-actions",
            span { class: "open-window", "{format_pairing_open_label(open_remaining())}" }
            button {
                class: "button danger",
                onclick: {
                    let target = target_id.clone();
                    move |_| {
                        let target = target.clone();
                        let backend = backend();
                        spawn(async move {
                            match backend.dismiss_pairing_advertisement(&target).await {
                                Ok(()) => {
                                    // Always drop local pairing UI for this row —
                                    // including WaitingForTarget after Approve.
                                    if pairing_target() == target
                                        || matches!(
                                            pairing(),
                                            PairingState::WaitingForTarget
                                                | PairingState::AwaitingConfirmation { .. }
                                                | PairingState::Connecting
                                        )
                                    {
                                        pairing.set(PairingState::Idle);
                                        pairing_error.set(String::new());
                                        pairing_target.set(String::new());
                                        invitation_code.set(String::new());
                                    }
                                    expanded_targets.write().remove(&target);
                                    targets.write().retain(|item| item.id != target);
                                    if selected_target() == target {
                                        selected_target.set(
                                            targets()
                                                .iter()
                                                .map(|item| item.id.clone())
                                                .next()
                                                .unwrap_or_default(),
                                        );
                                    }
                                    push_activity(
                                        activity_log,
                                        format!("Dismissed pairing advertisement {target}."),
                                    );
                                }
                                Err(error) => push_activity(
                                    activity_log,
                                    format!("Dismiss failed: {error}"),
                                ),
                            }
                        });
                    }
                },
                "Dismiss"
            }
        }
    }
}

#[component]
fn ControllerTargetActions(
    target_id: String,
    monitor_remaining_secs: u32,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    mut target_aliases: Signal<HashMap<String, String>>,
    mut targets: Signal<Vec<TargetAccess>>,
    mut selected_target: Signal<String>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    pairing: Signal<PairingState>,
) -> Element {
    let mut forget_prompt = use_signal(|| ForgetPrompt::Idle);
    let mut connect_remaining = use_signal(|| monitor_remaining_secs);
    use_effect(move || {
        connect_remaining.set(monitor_remaining_secs);
    });
    // Keep the Connect label ticking from the session deadline even when the parent
    // targets refresh is stalled on path work (common on Android with many BLE peers).
    let tick_stop = use_hook(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let tick_cancel = tick_stop.clone();
    use_drop(move || {
        tick_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    use_hook({
        let stop = tick_stop.clone();
        let target_id = target_id.clone();
        move || {
            let stop = stop.clone();
            let target_id = target_id.clone();
            spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let remaining = backend().monitor_remaining_secs(&target_id);
                    if connect_remaining() != remaining {
                        connect_remaining.set(remaining);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
    });
    rsx! {
        div { class: "target-controller-actions",
        button {
            class: "button",
            onclick: {
                let target = target_id.clone();
                move |_| {
                    let target = target.clone();
                    let backend = backend();
                    // Clear announce/route immediately so the header does not keep
                    // showing the pre-Connect hop while the probe drops and waits.
                    targets.write().iter_mut().filter(|item| item.id == target).for_each(
                        |item| item.path = None,
                    );
                    spawn(async move {
                        match backend.probe_target(&target, PathProbeReason::OperatorConnect).await {
                            Ok(path) => {
                                targets.write().iter_mut().filter(|item| item.id == target).for_each(
                                    |item| item.path = Some(path.clone()),
                                );
                            }
                            Err(error) => {
                                targets.write().iter_mut().filter(|item| item.id == target).for_each(
                                    |item| item.path = None,
                                );
                                push_activity(
                                    activity_log,
                                    format!("Connect dropped the hop; waiting for a dest announce. {error}"),
                                );
                            }
                        }
                        connect_remaining.set(backend.monitor_remaining_secs(&target));
                        if !pairing_in_progress(&pairing()) {
                            // Inventory (build / transport / power / peers) runs only here
                            // and on explicit refresh — not on the roster UI tick.
                            load_interfaces_for_target(
                                backend,
                                target,
                                interfaces_by_target,
                                true,
                                RemoteControlAnnounceWait::UntilRefreshed,
                            );
                        }
                    });
                }
            },
            "{format_connect_label(connect_remaining())}"
        }
        if forget_prompt() == ForgetPrompt::Confirming {
            button {
                class: "button danger",
                onclick: {
                    let target = target_id.clone();
                    move |_| {
                        let target = target.clone();
                        let backend = backend();
                        forget_prompt.set(ForgetPrompt::Idle);
                        spawn(async move {
                            match backend.forget_target(&target).await {
                                Ok(()) => {
                                    interfaces_by_target.write().remove(&target);
                                    expanded_targets.write().remove(&target);
                                    target_aliases.write().remove(&target);
                                    targets.write().retain(|item| item.id != target);
                                    if selected_target() == target {
                                        selected_target.set(
                                            targets()
                                                .iter()
                                                .find(|item| {
                                                    item.status != TargetStatus::AwaitingPairing
                                                })
                                                .map(|item| item.id.clone())
                                                .unwrap_or_default(),
                                        );
                                    }
                                    push_activity(
                                        activity_log,
                                        format!("Forgot stored access for {target}."),
                                    );
                                }
                                Err(error) => push_activity(
                                    activity_log,
                                    format!("Forget failed: {error}"),
                                ),
                            }
                        });
                    }
                },
                "Confirm forget"
            }
            button {
                class: "button",
                onclick: move |_| forget_prompt.set(ForgetPrompt::Idle),
                "Cancel"
            }
            p { class: "note", "Removes stored pairing and the local alias. Pair again to return this node to the list." }
        } else {
            button {
                class: "button danger",
                onclick: move |_| forget_prompt.set(ForgetPrompt::Confirming),
                "Forget"
            }
        }
        }
    }
}

#[component]
fn TargetControls(
    target_id: String,
    status: TargetStatus,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    mut targets: Signal<Vec<TargetAccess>>,
) -> Element {
    rsx! {
        div { class: "actions",
            button {
                class: "button",
                onclick: {
                    let target = target_id.clone();
                    move |_| {
                        let target = target.clone();
                        let backend = backend();
                        spawn(async move {
                            let _ = backend.announce(&target).await;
                        });
                    }
                },
                "Announce"
            }
            button {
                class: if status == TargetStatus::Sleeping { "button primary" } else { "button" },
                onclick: {
                    let target = target_id.clone();
                    let sleeping = status == TargetStatus::Sleeping;
                    move |_| {
                        let target = target.clone();
                        let backend = backend();
                        spawn(async move {
                            let result = if sleeping {
                                backend.wake(&target).await
                            } else {
                                backend.sleep(&target).await
                            };
                            match result {
                                Ok(()) => {
                                    let next = if sleeping {
                                        TargetStatus::Online
                                    } else {
                                        TargetStatus::Sleeping
                                    };
                                    targets.write().iter_mut().filter(|item| item.id == target).for_each(
                                        |item| item.status = next,
                                    );
                                    push_activity(
                                        activity_log,
                                        if sleeping {
                                            format!("Wake requested for {target}.")
                                        } else {
                                            format!("Sleep requested for {target}.")
                                        },
                                    );
                                }
                                Err(error) => push_activity(
                                    activity_log,
                                    if sleeping {
                                        format!("Wake failed: {error}")
                                    } else {
                                        format!("Sleep failed: {error}")
                                    },
                                ),
                            }
                        });
                    }
                },
                if status == TargetStatus::Sleeping {
                    "Wake"
                } else {
                    "Sleep"
                }
            }
            button {
                class: "button danger",
                onclick: {
                    let target = target_id.clone();
                    move |_| {
                        let target = target.clone();
                        let backend = backend();
                        spawn(async move {
                            match backend.reset(&target).await {
                                Ok(()) => push_activity(
                                    activity_log,
                                    format!("Reset requested for {target}."),
                                ),
                                Err(error) => push_activity(
                                    activity_log,
                                    format!("Reset failed: {error}"),
                                ),
                            }
                        });
                    }
                },
                "Reset"
            }
        }
    }
}

#[component]
fn NodeManagementWhitelist(
    target_id: String,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    manager_aliases: Signal<HashMap<String, String>>,
) -> Element {
    let loaded = use_signal(|| LoadedWhitelist::Loading);
    let mut expanded = use_signal(|| false);
    let mut configuring = use_signal(|| false);
    let add_key = use_signal(String::new);
    let busy = use_signal(|| false);
    let mut notice = use_signal(String::new);
    let mut hash_popup = use_signal(|| None::<String>);
    let self_hash = backend()
        .controller_identity()
        .ok()
        .map(|identity| identity.operator_hash);

    use_effect({
        let target_id = target_id.clone();
        move || {
            reload_whitelist(target_id.clone(), backend(), loaded);
        }
    });

    let count_label = match loaded() {
        LoadedWhitelist::Loading => "Loading".to_string(),
        LoadedWhitelist::Ready(hashes) => match hashes.len() {
            1 => "1 controller".to_string(),
            n => format!("{n} controllers"),
        },
        LoadedWhitelist::Failed(_) => "Unavailable".to_string(),
    };

    rsx! {
        section {
            class: if expanded() { "interface-item open" } else { "interface-item" },
            div { class: "twisty-bar",
                button {
                    class: "twisty-row",
                    aria_expanded: if expanded() { "true" } else { "false" },
                    onclick: move |_| {
                        let next = !expanded();
                        expanded.set(next);
                        if !next {
                            configuring.set(false);
                        }
                    },
                    span { class: if expanded() { "twisty open" } else { "twisty" }, aria_hidden: "true" }
                    div { class: "twisty-copy",
                        span { class: "twisty-title", "Node Management Whitelist" }
                        span { class: "twisty-address", "Controllers that may manage this node" }
                    }
                    span { class: "status", "{count_label}" }
                }
                RefreshButton {
                    label: "Refresh node management whitelist".to_string(),
                    on_refresh: {
                        let target_id = target_id.clone();
                        move |_| {
                            notice.set(String::new());
                            reload_whitelist(target_id.clone(), backend(), loaded);
                        }
                    },
                }
            }
            if expanded() {
                div { class: "accordion-body",
                    div { class: if configuring() { "interface-toolbar editing" } else { "interface-toolbar" },
                        div { class: "toolbar-track",
                            div { class: "toolbar-pane",
                                button {
                                    class: "button",
                                    r#type: "button",
                                    onclick: move |_| configuring.set(true),
                                    "Configure"
                                }
                            }
                            div { class: "toolbar-pane",
                                button {
                                    class: "button",
                                    r#type: "button",
                                    disabled: busy(),
                                    onclick: move |_| configuring.set(false),
                                    "Back"
                                }
                            }
                        }
                    }
                    div { class: if configuring() { "deck editing" } else { "deck" },
                        div { class: "deck-track",
                            div { class: "deck-pane",
                                {whitelist_status_pane(
                                    loaded(),
                                    self_hash.clone(),
                                    target_id.clone(),
                                    manager_aliases,
                                    hash_popup,
                                    busy,
                                    notice,
                                    backend,
                                    activity_log,
                                    loaded,
                                )}
                            }
                            div { class: "deck-pane",
                                {whitelist_configure_pane(
                                    target_id.clone(),
                                    loaded,
                                    self_hash.clone(),
                                    add_key,
                                    busy,
                                    notice,
                                    hash_popup,
                                    manager_aliases,
                                    backend,
                                    activity_log,
                                )}
                            }
                        }
                    }
                }
            }
            if let Some(hash) = hash_popup() {
                div {
                    class: "unsaved-overlay",
                    onclick: move |_| hash_popup.set(None),
                    div {
                        class: "unsaved-dialog hash-popup",
                        onclick: |event| event.stop_propagation(),
                        h3 { "Manager Address Hash" }
                        p { class: "twisty-address", "{hash}" }
                        if self_hash.as_deref() == Some(hash.as_str()) {
                            p { class: "status online", "This controller" }
                        }
                        div { class: "actions",
                            button {
                                class: "button",
                                r#type: "button",
                                onclick: move |_| hash_popup.set(None),
                                "Close"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn whitelist_status_pane(
    loaded_view: LoadedWhitelist,
    self_hash: Option<String>,
    target_id: String,
    manager_aliases: Signal<HashMap<String, String>>,
    hash_popup: Signal<Option<String>>,
    busy: Signal<bool>,
    notice: Signal<String>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    loaded: Signal<LoadedWhitelist>,
) -> Element {
    match loaded_view {
        LoadedWhitelist::Loading => rsx! {
            p { class: "note", "Loading who can manage this node…" }
        },
        LoadedWhitelist::Failed(message) => rsx! {
            p { class: "error", "{message}" }
        },
        LoadedWhitelist::Ready(hashes) if hashes.is_empty() => rsx! {
            p { class: "note", "No controllers are on this node's allow-list." }
        },
        LoadedWhitelist::Ready(hashes) => whitelist_manager_table(
            hashes,
            self_hash,
            target_id,
            manager_aliases,
            hash_popup,
            busy,
            notice,
            backend,
            activity_log,
            loaded,
            WhitelistTableKind::Status,
        ),
    }
}

fn whitelist_configure_pane(
    target_id: String,
    loaded: Signal<LoadedWhitelist>,
    self_hash: Option<String>,
    mut add_key: Signal<String>,
    mut busy: Signal<bool>,
    mut notice: Signal<String>,
    hash_popup: Signal<Option<String>>,
    manager_aliases: Signal<HashMap<String, String>>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    rsx! {
        div { class: "edit-card",
            h3 { "Allow-list" }
            p { class: "note", "Add a controller with the 128-character allow-list key from that app's Settings. The 32-character identity hash is what the list shows after it is added." }
            match loaded() {
                LoadedWhitelist::Loading => rsx! {
                    p { class: "note", "Loading allow-list…" }
                },
                LoadedWhitelist::Failed(message) => rsx! {
                    p { class: "error", "{message}" }
                },
                LoadedWhitelist::Ready(hashes) if hashes.is_empty() => rsx! {
                    p { class: "note", "No controllers are on this node's allow-list." }
                },
                LoadedWhitelist::Ready(hashes) => whitelist_manager_table(
                    hashes,
                    self_hash.clone(),
                    target_id.clone(),
                    manager_aliases,
                    hash_popup,
                    busy,
                    notice,
                    backend,
                    activity_log,
                    loaded,
                    WhitelistTableKind::Configure,
                ),
            }
            label { "Allow-list key"
                input {
                    r#type: "text",
                    value: "{add_key}",
                    maxlength: "128",
                    placeholder: "128 hex characters from that controller's Settings",
                    disabled: busy(),
                    oninput: move |event| {
                        add_key.set(event.value());
                        notice.set(String::new());
                    },
                }
            }
            div { class: "actions",
                button {
                    class: "button",
                    r#type: "button",
                    disabled: busy() || add_key().trim().is_empty(),
                    onclick: {
                        let target_id = target_id.clone();
                        move |_| {
                            if busy() {
                                return;
                            }
                            let key = add_key().trim().to_string();
                            if key.is_empty() {
                                return;
                            }
                            busy.set(true);
                            notice.set(String::new());
                            let backend = backend();
                            let target_id = target_id.clone();
                            spawn(async move {
                                let result = backend.authorize_controller(&target_id, &key).await;
                                busy.set(false);
                                match result {
                                    Ok(()) => {
                                        add_key.set(String::new());
                                        push_activity(
                                            activity_log,
                                            "Added a controller to the node management whitelist.",
                                        );
                                        reload_whitelist(
                                            target_id,
                                            backend,
                                            loaded,
                                        );
                                    }
                                    Err(error) => {
                                        notice.set(error.to_string());
                                    }
                                }
                            });
                        }
                    },
                    if busy() {
                        span { class: "spinner", aria_hidden: "true" }
                        "Saving"
                    } else {
                        "Add"
                    }
                }
            }
            if !notice().is_empty() {
                p { class: "error", "{notice}" }
            }
        }
    }
}

fn whitelist_manager_table(
    hashes: Vec<String>,
    self_hash: Option<String>,
    target_id: String,
    mut manager_aliases: Signal<HashMap<String, String>>,
    mut hash_popup: Signal<Option<String>>,
    mut busy: Signal<bool>,
    mut notice: Signal<String>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    loaded: Signal<LoadedWhitelist>,
    kind: WhitelistTableKind,
) -> Element {
    let configure = kind == WhitelistTableKind::Configure;
    rsx! {
        div { class: if configure { "whitelist-table can-remove" } else { "whitelist-table" },
            span { class: "whitelist-head", "Manager Address Hash" }
            span { class: "whitelist-head", "Alias" }
            if configure {
                span { class: "whitelist-head", aria_hidden: "true" }
            }
            for hash in hashes {
                {
                    let self_entry = self_hash.as_deref() == Some(hash.as_str());
                    let alias = manager_aliases().get(&hash).cloned().unwrap_or_default();
                    let hash_for_alias = hash.clone();
                    let hash_for_remove = hash.clone();
                    let alias_placeholder = if self_entry { "This controller" } else { "Alias" };
                    let alias_display = if alias.is_empty() {
                        alias_placeholder.to_string()
                    } else {
                        alias.clone()
                    };
                    rsx! {
                        button {
                            class: "whitelist-hash",
                            r#type: "button",
                            title: "{hash}",
                            aria_label: "Show full manager address hash {hash}",
                            onclick: {
                                let hash = hash.clone();
                                move |_| hash_popup.set(Some(hash.clone()))
                            },
                            "{hash}"
                        }
                        if configure {
                            input {
                                class: "whitelist-alias",
                                r#type: "text",
                                placeholder: "{alias_placeholder}",
                                value: "{alias}",
                                aria_label: "Alias for {hash}",
                                oninput: {
                                    let hash = hash_for_alias.clone();
                                    move |event| {
                                        let value = event.value();
                                        if value.is_empty() {
                                            manager_aliases.write().remove(&hash);
                                        } else {
                                            manager_aliases.write().insert(hash.clone(), value);
                                        }
                                    }
                                },
                                onblur: {
                                    let hash = hash_for_alias.clone();
                                    move |_| {
                                        persist_manager_alias(
                                            hash.clone(),
                                            backend(),
                                            manager_aliases,
                                            activity_log,
                                        );
                                    }
                                },
                                onkeydown: {
                                    let hash = hash_for_alias;
                                    move |event| {
                                        if event.key() == Key::Enter {
                                            persist_manager_alias(
                                                hash.clone(),
                                                backend(),
                                                manager_aliases,
                                                activity_log,
                                            );
                                        }
                                    }
                                },
                            }
                        } else {
                            span {
                                class: if alias.is_empty() { "whitelist-alias-text placeholder" } else { "whitelist-alias-text" },
                                "{alias_display}"
                            }
                        }
                        if configure {
                            button {
                                class: "button danger whitelist-remove",
                                r#type: "button",
                                disabled: busy() || self_entry,
                                onclick: {
                                    let target_id = target_id.clone();
                                    let hash = hash_for_remove;
                                    move |_| {
                                        if busy() {
                                            return;
                                        }
                                        busy.set(true);
                                        notice.set(String::new());
                                        let backend = backend();
                                        let target_id = target_id.clone();
                                        let hash = hash.clone();
                                        spawn(async move {
                                            let result = backend
                                                .revoke_controller(&target_id, &hash)
                                                .await;
                                            busy.set(false);
                                            match result {
                                                Ok(()) => {
                                                    push_activity(
                                                        activity_log,
                                                        format!(
                                                            "Removed controller {hash} from the node management whitelist."
                                                        ),
                                                    );
                                                    reload_whitelist(
                                                        target_id,
                                                        backend,
                                                        loaded,
                                                    );
                                                }
                                                Err(error) => {
                                                    notice.set(error.to_string());
                                                }
                                            }
                                        });
                                    }
                                },
                                "Remove"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn reload_whitelist(
    target_id: String,
    backend: RemoteControlBackend,
    mut loaded: Signal<LoadedWhitelist>,
) {
    spawn(async move {
        match backend.inventory_controllers(&target_id).await {
            Ok(hashes) => loaded.set(LoadedWhitelist::Ready(hashes)),
            Err(error) => loaded.set(LoadedWhitelist::Failed(error.to_string())),
        }
    });
}

#[component]
fn InterfaceAccordion(
    host: InterfaceHost,
    loaded: LoadedInterfaces,
    mut expanded_interfaces: Signal<HashSet<String>>,
    mut drafts: Signal<HashMap<String, InterfaceDraft>>,
    mut editing: Signal<HashSet<String>>,
    saving: Signal<HashSet<String>>,
    mut save_notices: Signal<HashMap<String, String>>,
    mut focused_interface: Signal<Option<String>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut screen: Signal<Screen>,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    peer_aliases: Signal<HashMap<String, String>>,
    pairing: Signal<PairingState>,
) -> Element {
    let scope = host.scope_id().to_string();
    match loaded {
        LoadedInterfaces::Idle => rsx! {
            p { class: "note", "No interface inventory yet. Press Connect to refresh." }
        },
        LoadedInterfaces::Loading => rsx! {
            p { class: "note", "Loading interfaces…" }
        },
        LoadedInterfaces::Failed(message) => rsx! {
            p { class: "error", "{message}" }
            button {
                class: "button",
                onclick: {
                    let host = host.clone();
                    move |_| {
                        match host.clone() {
                            InterfaceHost::Controller => load_controller_interfaces(
                                backend(),
                                interfaces_by_target,
                            ),
                            InterfaceHost::Target(target) => load_interfaces_for_target(
                                backend(),
                                target,
                                interfaces_by_target,
                                true,
                                RemoteControlAnnounceWait::UntilHeard,
                            ),
                        }
                    }
                },
                "Retry"
            }
        },
        LoadedInterfaces::Ready { entries, .. } if entries.is_empty() => rsx! {
            div { class: "heading-row",
                p { class: "note",
                    if matches!(host, InterfaceHost::Controller) {
                        "This controller has no local transports attached yet."
                    } else {
                        "This target has not advertised any interfaces."
                    }
                }
                RefreshButton {
                    label: "Refresh interfaces".to_string(),
                    on_refresh: {
                        let host = host.clone();
                        move |_| {
                            refresh_host_interfaces(
                                host.clone(),
                                backend(),
                                interfaces_by_target,
                                pairing(),
                            );
                        }
                    },
                }
            }
        },
        LoadedInterfaces::Ready { entries, .. } => rsx! {
            div { class: "interface-list",
                for entry in entries {
                    {
                        let key = interface_key(&scope, &entry.id);
                        let draft = draft_or_saved(&drafts(), &key, &entry);
                        let dirty = draft.is_dirty(&entry);
                        let saving_this = saving().contains(&key);
                        let configuring = editing().contains(&key);
                        let expanded = expanded_interfaces().contains(&key);
                        let address = if can_edit_tcp_target(&entry) {
                            let target = saved_tcp_target(&entry);
                            if target.is_empty() {
                                format!(
                                    "{} · {} · {}",
                                    entry.kind,
                                    interface_mode_label(entry.mode),
                                    entry.connection
                                )
                            } else {
                                format!("{} · {target} · {}", entry.kind, entry.connection)
                            }
                        } else {
                            format!(
                                "{} · {} · {}",
                                entry.kind,
                                interface_mode_label(entry.mode),
                                entry.connection
                            )
                        };
                        rsx! {
                            section {
                                key: "{key}",
                                class: if expanded { "interface-item open" } else { "interface-item" },
                                div { class: "twisty-bar",
                                button {
                                    class: "twisty-row",
                                    aria_expanded: if expanded { "true" } else { "false" },
                                    onclick: {
                                        let key = key.clone();
                                        move |_| {
                                            request_interface_toggle(
                                                key.clone(),
                                                expanded_interfaces,
                                                editing,
                                                drafts,
                                                focused_interface,
                                                unsaved,
                                                interfaces_by_target,
                                            );
                                        }
                                    },
                                    span { class: if expanded { "twisty open" } else { "twisty" }, aria_hidden: "true" }
                                    div { class: "twisty-copy",
                                        span { class: "twisty-title", "{entry.name}" }
                                        if let Some(arrived_at) = entry.arrived_at.as_ref() {
                                            span { class: "twisty-as-of", "as of {arrived_at}" }
                                        }
                                        span { class: "twisty-address", "{address}" }
                                    }
                                }
                                button {
                                    class: if matches!(entry.power, InterfacePower::On) {
                                        "button primary interface-power"
                                    } else {
                                        "button interface-power"
                                    },
                                    r#type: "button",
                                    onclick: {
                                        let interface_id = entry.id.clone();
                                        let scope = scope.clone();
                                        let host = host.clone();
                                        move |_| {
                                            toggle_interface_power(
                                                host.clone(),
                                                scope.clone(),
                                                interface_id.clone(),
                                                backend(),
                                                interfaces_by_target,
                                                activity_log,
                                            );
                                        }
                                    },
                                    "{start_stop_label(&entry.power)}"
                                }
                                RefreshButton {
                                    label: format!("Refresh {}", entry.name),
                                    on_refresh: {
                                        let host = host.clone();
                                        let interface_id = entry.id.clone();
                                        move |_| {
                                            refresh_one_interface(
                                                host.clone(),
                                                interface_id.clone(),
                                                backend(),
                                                interfaces_by_target,
                                                pairing(),
                                            );
                                        }
                                    },
                                }
                                }
                                if expanded {
                                    div { class: "accordion-body",
                                        div { class: if configuring { "interface-toolbar editing" } else { "interface-toolbar" },
                                            div { class: "toolbar-track",
                                                div { class: "toolbar-pane",
                                                    button {
                                                        class: "button",
                                                        r#type: "button",
                                                        onclick: {
                                                            let key = key.clone();
                                                            move |_| {
                                                                editing.write().insert(key.clone());
                                                            }
                                                        },
                                                        "Configure"
                                                    }
                                                }
                                                div { class: "toolbar-pane",
                                                    button {
                                                        class: "button",
                                                        disabled: saving_this,
                                                        r#type: "button",
                                                        onclick: {
                                                            let key = key.clone();
                                                            move |_| {
                                                                request_close_editor(
                                                                    key.clone(),
                                                                    drafts,
                                                                    editing,
                                                                    save_notices,
                                                                    unsaved,
                                                                    interfaces_by_target,
                                                                );
                                                            }
                                                        },
                                                        if dirty {
                                                            "Cancel"
                                                        } else {
                                                            "Back"
                                                        }
                                                    }
                                                    button {
                                                        class: if saving_this { "button busy" } else { "button" },
                                                        disabled: !dirty || saving_this,
                                                        r#type: "button",
                                                        aria_busy: if saving_this { "true" } else { "false" },
                                                        onclick: {
                                                            let key = key.clone();
                                                            move |_| {
                                                                save_interface_drafts(
                                                                    vec![key.clone()],
                                                                    Some(UnsavedAfter::CloseEditor { key: key.clone() }),
                                                                    drafts,
                                                                    saving,
                                                                    save_notices,
                                                                    editing,
                                                                    focused_interface,
                                                                    unsaved,
                                                                    expanded_interfaces,
                                                                    expanded_targets,
                                                                    interfaces_by_target,
                                                                    screen,
                                                                    backend(),
                                                                    activity_log,
                                                                );
                                                            }
                                                        },
                                                        if saving_this {
                                                            span { class: "spinner", aria_hidden: "true" }
                                                            "Saving"
                                                        } else {
                                                            "Save"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: if configuring { "deck editing" } else { "deck" },
                                            div { class: "deck-track",
                                                div { class: "deck-pane",
                                                    dl { class: "facts",
                                                        div { dt { "Kind" } dd { "{entry.kind}" } }
                                                        div { dt { "Mode" } dd { "{interface_mode_label(entry.mode)}" } }
                                                        div { dt { "Connection" } dd { "{entry.connection}" } }
                                                        if entry.kind == "auto-wifi" {
                                                            // Station LL is an IPv6 fact; BLE group UI does not apply.
                                                        } else if let Some(group) = entry.group.as_ref().filter(|value| !value.trim().is_empty()) {
                                                            div { dt { "Group" } dd { "{group}" } }
                                                        } else if entry.kind == "bluetooth-auto" {
                                                            div { dt { "Group" } dd { "{saved_group(&entry)}" } }
                                                        }
                                                        if let Some(ifac) = entry.ifac_bytes {
                                                            div { dt { "IFAC" } dd { "{ifac}" } }
                                                        }
                                                        if let Some(gravity) = entry.gravity {
                                                            div { dt { "Gravity" } dd { "{gravity}" } }
                                                        }
                                                        if let Some(failure) = entry.failure.as_ref() {
                                                            div { class: "failure", dt { "Failure" } dd { "{failure}" } }
                                                        }
                                                        div { dt { "TX" } dd { "{format_byte_count(entry.tx_bytes)}" } }
                                                        div { dt { "RX" } dd { "{format_byte_count(entry.rx_bytes)}" } }
                                                        if let Some(tx_bps) = entry.tx_bps {
                                                            div { dt { "TX rate" } dd { "{format_rate(tx_bps / 8)}" } }
                                                        }
                                                        if let Some(rx_bps) = entry.rx_bps {
                                                            div { dt { "RX rate" } dd { "{format_rate(rx_bps / 8)}" } }
                                                        }
                                                        div { dt { "Links" } dd { "{entry.links}" } }
                                                        if entry.transported_links > 0 {
                                                            div { dt { "Carried" } dd { "{entry.transported_links}" } }
                                                        }
                                                        div { dt { "Destinations" } dd { "{entry.destinations}" } }
                                                        div { dt { "Rate" } dd { "{format_rate(entry.rate_bytes_per_sec)}" } }
                                                        div { dt { "Activity" } dd { "{format_activity_age(entry.last_activity_secs)}" } }
                                                        for fact in entry.extras.iter().filter(|fact| {
                                                            !(fact.label == "IFAC" && entry.ifac_bytes.is_some())
                                                        }) {
                                                            div { dt { "{fact.label}" } dd { "{fact.value}" } }
                                                        }
                                                    }
                                                    if entry.shows_peers || !entry.peers.is_empty() {
                                                        p { class: "note", "Peers {entry.peers.len()}" }
                                                        if let Some(note) = auto_wifi_peer_list_note(&entry.kind) {
                                                            p { class: "note", "{note}" }
                                                        }
                                                        if let Some(note) = bluetooth_auto_peer_list_note(&entry.kind) {
                                                            p { class: "note", "{note}" }
                                                        }
                                                        if let Some(error) = entry.peers_error.as_ref() {
                                                            p { class: "note", "Could not load peers: {error}" }
                                                        } else if entry.peers.is_empty() {
                                                            p { class: "note", "No peers on this interface." }
                                                        } else {
                                                            ul { class: "peer-list",
                                                                for peer in entry.peers.iter() {
                                                                    li { class: "peer-card",
                                                                        div { class: "peer-head",
                                                                            div { class: "peer-title",
                                                                                input {
                                                                                    class: "peer-alias",
                                                                                    r#type: "text",
                                                                                    placeholder: "Alias",
                                                                                    value: "{peer_aliases().get(&peer.id).cloned().unwrap_or_default()}",
                                                                                    aria_label: format!("Alias for {}", peer.name),
                                                                                    oninput: {
                                                                                        let peer_id = peer.id.clone();
                                                                                        move |event| {
                                                                                            let value = event.value();
                                                                                            if value.is_empty() {
                                                                                                peer_aliases.write().remove(&peer_id);
                                                                                            } else {
                                                                                                peer_aliases.write().insert(peer_id.clone(), value);
                                                                                            }
                                                                                        }
                                                                                    },
                                                                                    onblur: {
                                                                                        let peer_id = peer.id.clone();
                                                                                        let label = peer.name.clone();
                                                                                        move |_| {
                                                                                            persist_peer_alias(
                                                                                                peer_id.clone(),
                                                                                                label.clone(),
                                                                                                backend(),
                                                                                                peer_aliases,
                                                                                                activity_log,
                                                                                            );
                                                                                        }
                                                                                    },
                                                                                    onkeydown: {
                                                                                        let peer_id = peer.id.clone();
                                                                                        let label = peer.name.clone();
                                                                                        move |event| {
                                                                                            if event.key() == Key::Enter {
                                                                                                persist_peer_alias(
                                                                                                    peer_id.clone(),
                                                                                                    label.clone(),
                                                                                                    backend(),
                                                                                                    peer_aliases,
                                                                                                    activity_log,
                                                                                                );
                                                                                            }
                                                                                        }
                                                                                    },
                                                                                }
                                                                                span { class: "peer-subtitle", "{peer.name}" }
                                                                            }
                                                                        }
                                                                        dl { class: "facts",
                                                                            div { dt { "Status" } dd { "{peer.connection}" } }
                                                                            if let Some(health) = peer.health {
                                                                                div {
                                                                                    class: if health.is_radio_only() { "health-warn" } else { "health-ok" },
                                                                                    dt { "Health" }
                                                                                    dd { "{health.label()}" }
                                                                                }
                                                                            }
                                                                            if peer.details.is_applicable() {
                                                                                div {
                                                                                    dt { "Details" }
                                                                                    dd { "{peer.details}" }
                                                                                }
                                                                            }
                                                                            div { dt { "Path" } dd { "{peer.role}" } }
                                                                            if let (Some(label), Some(endpoint)) = (peer.endpoint_label.as_ref(), peer.endpoint.as_ref()) {
                                                                                div { dt { "{label}" } dd { class: "wrap", "{endpoint}" } }
                                                                            }
                                                                            if let (Some(label), Some(endpoint)) = (peer.local_endpoint_label.as_ref(), peer.local_endpoint.as_ref()) {
                                                                                div { dt { "{label}" } dd { class: "wrap", "{endpoint}" } }
                                                                            }
                                                                            div { dt { "What" } dd { class: "wrap", "{peer.detail}" } }
                                                                            div { dt { "ID" } dd { "{peer.id}" } }
                                                                            div { dt { "TX" } dd { "{format_byte_count(peer.tx_bytes)}" } }
                                                                            div { dt { "RX" } dd { "{format_byte_count(peer.rx_bytes)}" } }
                                                                            div { dt { "Links" } dd { "{peer.links}" } }
                                                                            div { dt { "Destinations" } dd { "{peer.destinations}" } }
                                                                            div { dt { "Rate" } dd { "{format_rate(peer.rate_bytes_per_sec)}" } }
                                                                            div { dt { "Activity" } dd { "{format_activity_age(peer.last_activity_secs)}" } }
                                                                            for fact in radio_facts(peer.radio) {
                                                                                div { dt { "{fact.label}" } dd { "{fact.value}" } }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                div { class: "deck-pane",
                                                    div { class: "edit-card",
                                                        h3 { "Configure {entry.name}" }
                                                        if let Some(notice) = save_notices().get(&key) {
                                                            p { class: "error", "{notice}" }
                                                        }
                                                        dl { class: "facts",
                                                            div {
                                                                class: if draft.field_changed(&entry, InterfaceField::Mode) { "changed" } else { "" },
                                                                dt { "Mode" }
                                                                dd {
                                                                    select {
                                                                        value: "{draft.mode.wire_value()}",
                                                                        disabled: saving_this,
                                                                        onchange: {
                                                                            let key = key.clone();
                                                                            let saved = entry.clone();
                                                                            move |event| {
                                                                                let Some(mode) = event
                                                                                    .value()
                                                                                    .parse::<u8>()
                                                                                    .ok()
                                                                                    .and_then(InterfaceMode::from_wire)
                                                                                else {
                                                                                    return;
                                                                                };
                                                                                let mut draft = draft_or_saved(
                                                                                    &drafts(),
                                                                                    &key,
                                                                                    &saved,
                                                                                );
                                                                                draft.mode = mode;
                                                                                save_notices.write().remove(&key);
                                                                                put_draft(
                                                                                    &mut drafts.write(),
                                                                                    key.clone(),
                                                                                    &saved,
                                                                                    draft,
                                                                                );
                                                                            }
                                                                        },
                                                                        for mode in InterfaceMode::ALL {
                                                                            option {
                                                                                value: "{mode.wire_value()}",
                                                                                selected: draft.mode == mode,
                                                                                "{interface_mode_label(mode)}"
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            if can_edit_tcp_target(&entry) && scope == CONTROLLER_SCOPE {
                                                                div {
                                                                    class: if draft.field_changed(&entry, InterfaceField::TcpTarget) { "changed" } else { "" },
                                                                    dt { "Target" }
                                                                    dd {
                                                                        input {
                                                                            r#type: "text",
                                                                            value: "{draft.tcp_target}",
                                                                            placeholder: "host:port",
                                                                            disabled: saving_this,
                                                                            oninput: {
                                                                                let key = key.clone();
                                                                                let saved = entry.clone();
                                                                                move |event| {
                                                                                    let mut draft = draft_or_saved(
                                                                                        &drafts(),
                                                                                        &key,
                                                                                        &saved,
                                                                                    );
                                                                                    draft.tcp_target = event.value();
                                                                                    save_notices.write().remove(&key);
                                                                                    put_draft(
                                                                                        &mut drafts.write(),
                                                                                        key.clone(),
                                                                                        &saved,
                                                                                        draft,
                                                                                    );
                                                                                }
                                                                            },
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            if can_edit_wifi_station(&entry) && scope != CONTROLLER_SCOPE {
                                                                div {
                                                                    class: if draft.field_changed(&entry, InterfaceField::WifiStation) { "changed" } else { "" },
                                                                    dt { "SSID" }
                                                                    dd {
                                                                        input {
                                                                            r#type: "text",
                                                                            value: "{draft.wifi_ssid}",
                                                                            maxlength: "32",
                                                                            disabled: saving_this,
                                                                            oninput: {
                                                                                let key = key.clone();
                                                                                let saved = entry.clone();
                                                                                move |event| {
                                                                                    let mut draft = draft_or_saved(
                                                                                        &drafts(),
                                                                                        &key,
                                                                                        &saved,
                                                                                    );
                                                                                    draft.wifi_ssid = event.value();
                                                                                    save_notices.write().remove(&key);
                                                                                    put_draft(
                                                                                        &mut drafts.write(),
                                                                                        key.clone(),
                                                                                        &saved,
                                                                                        draft,
                                                                                    );
                                                                                }
                                                                            },
                                                                        }
                                                                    }
                                                                }
                                                                div {
                                                                    class: if draft.wifi_password.is_empty() { "" } else { "changed" },
                                                                    dt { "Password" }
                                                                    dd {
                                                                        input {
                                                                            r#type: "password",
                                                                            value: "{draft.wifi_password}",
                                                                            maxlength: "64",
                                                                            placeholder: "empty for open network",
                                                                            disabled: saving_this,
                                                                            oninput: {
                                                                                let key = key.clone();
                                                                                let saved = entry.clone();
                                                                                move |event| {
                                                                                    let mut draft = draft_or_saved(
                                                                                        &drafts(),
                                                                                        &key,
                                                                                        &saved,
                                                                                    );
                                                                                    draft.wifi_password = event.value();
                                                                                    save_notices.write().remove(&key);
                                                                                    put_draft(
                                                                                        &mut drafts.write(),
                                                                                        key.clone(),
                                                                                        &saved,
                                                                                        draft,
                                                                                    );
                                                                                }
                                                                            },
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            if can_edit_group(&entry) {
                                                                div {
                                                                    class: if draft.field_changed(&entry, InterfaceField::Group) { "changed" } else { "" },
                                                                    dt { "Group" }
                                                                    dd {
                                                                        input {
                                                                            r#type: "text",
                                                                            value: "{draft.group}",
                                                                            maxlength: "32",
                                                                            disabled: saving_this,
                                                                            oninput: {
                                                                                let key = key.clone();
                                                                                let saved = entry.clone();
                                                                                move |event| {
                                                                                    let mut draft = draft_or_saved(
                                                                                        &drafts(),
                                                                                        &key,
                                                                                        &saved,
                                                                                    );
                                                                                    draft.group = event.value();
                                                                                    save_notices.write().remove(&key);
                                                                                    put_draft(
                                                                                        &mut drafts.write(),
                                                                                        key.clone(),
                                                                                        &saved,
                                                                                        draft,
                                                                                    );
                                                                                }
                                                                            },
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            if let Some(profile) = draft.lora.filter(|_| can_edit_lora(&entry)) {
                                                                {
                                                                    let Modulation::Lora {
                                                                        spreading_factor,
                                                                        bandwidth,
                                                                        coding_rate,
                                                                    } = profile.modulation();
                                                                    let preset = ModemPreset::matching(profile.modulation());
                                                                    rsx! {
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::Region) { "changed" } else { "" },
                                                                            dt { "Region" }
                                                                            dd {
                                                                                select {
                                                                                    value: "{profile.region().label()}",
                                                                                    disabled: saving_this,
                                                                                    onchange: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(region) = Region::ALL
                                                                                                .into_iter()
                                                                                                .find(|region| region.label() == event.value())
                                                                                            else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                apply_lora_region(profile, region)
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                    for region in Region::ALL {
                                                                                        option {
                                                                                            value: "{region.label()}",
                                                                                            selected: profile.region() == SubGRegion::Regulated(region),
                                                                                            "{region.label()}"
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::Frequency) { "changed" } else { "" },
                                                                            dt { "Frequency" }
                                                                            dd { class: "with-unit",
                                                                                input {
                                                                                    r#type: "text",
                                                                                    value: "{format_lora_frequency_input(profile.frequency().hz())}",
                                                                                    disabled: saving_this,
                                                                                    oninput: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(hz) = parse_lora_frequency_mhz(&event.value()) else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                clamp_lora_frequency(profile, hz)
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                }
                                                                                span { class: "unit", "MHz" }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::Preset) { "changed" } else { "" },
                                                                            dt { "Preset" }
                                                                            dd {
                                                                                select {
                                                                                    value: "{preset.map(ModemPreset::label).unwrap_or(\"Custom\")}",
                                                                                    disabled: saving_this,
                                                                                    onchange: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(preset) = ModemPreset::ALL
                                                                                                .into_iter()
                                                                                                .find(|preset| preset.label() == event.value())
                                                                                            else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                apply_lora_preset(profile, preset)
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                    if preset.is_none() {
                                                                                        option {
                                                                                            value: "Custom",
                                                                                            selected: true,
                                                                                            disabled: true,
                                                                                            "Custom"
                                                                                        }
                                                                                    }
                                                                                    for choice in ModemPreset::ALL {
                                                                                        option {
                                                                                            value: "{choice.label()}",
                                                                                            selected: preset == Some(choice),
                                                                                            "{choice.label()}"
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::SpreadingFactor) { "changed" } else { "" },
                                                                            dt { "SF" }
                                                                            dd {
                                                                                select {
                                                                                    value: "{spreading_factor as u8}",
                                                                                    disabled: saving_this,
                                                                                    onchange: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(spreading_factor) = event
                                                                                                .value()
                                                                                                .parse::<u8>()
                                                                                                .ok()
                                                                                                .and_then(SpreadingFactor::from_number)
                                                                                            else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                let Modulation::Lora { bandwidth, coding_rate, .. } =
                                                                                                    profile.modulation();
                                                                                                set_lora_modulation(
                                                                                                    profile,
                                                                                                    spreading_factor,
                                                                                                    bandwidth,
                                                                                                    coding_rate,
                                                                                                )
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                    for sf in 5u8..=12 {
                                                                                        option {
                                                                                            value: "{sf}",
                                                                                            selected: spreading_factor as u8 == sf,
                                                                                            "{sf}"
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::Bandwidth) { "changed" } else { "" },
                                                                            dt { "Bandwidth" }
                                                                            dd {
                                                                                select {
                                                                                    value: "{bandwidth.hz() / 1_000}",
                                                                                    disabled: saving_this,
                                                                                    onchange: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(bandwidth) = event
                                                                                                .value()
                                                                                                .parse::<u32>()
                                                                                                .ok()
                                                                                                .and_then(LoraBandwidth::from_khz)
                                                                                            else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                let Modulation::Lora { spreading_factor, coding_rate, .. } =
                                                                                                    profile.modulation();
                                                                                                set_lora_modulation(
                                                                                                    profile,
                                                                                                    spreading_factor,
                                                                                                    bandwidth,
                                                                                                    coding_rate,
                                                                                                )
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                    for khz in [125u32, 250, 500] {
                                                                                        option {
                                                                                            value: "{khz}",
                                                                                            selected: bandwidth.hz() / 1_000 == khz,
                                                                                            "{khz} kHz"
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::CodingRate) { "changed" } else { "" },
                                                                            dt { "CR" }
                                                                            dd {
                                                                                select {
                                                                                    value: "{coding_rate.denominator()}",
                                                                                    disabled: saving_this,
                                                                                    onchange: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(coding_rate) = event
                                                                                                .value()
                                                                                                .parse::<u8>()
                                                                                                .ok()
                                                                                                .and_then(CodingRate::from_denominator)
                                                                                            else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                let Modulation::Lora { spreading_factor, bandwidth, .. } =
                                                                                                    profile.modulation();
                                                                                                set_lora_modulation(
                                                                                                    profile,
                                                                                                    spreading_factor,
                                                                                                    bandwidth,
                                                                                                    coding_rate,
                                                                                                )
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                    for cr in 5u8..=8 {
                                                                                        option {
                                                                                            value: "{cr}",
                                                                                            selected: coding_rate.denominator() == cr,
                                                                                            "4/{cr}"
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::TxPower) { "changed" } else { "" },
                                                                            dt { "TX power" }
                                                                            dd { class: "with-unit",
                                                                                input {
                                                                                    r#type: "number",
                                                                                    value: "{profile.tx_power().dbm()}",
                                                                                    min: "{LORA_TX_POWER_MIN_DBM}",
                                                                                    max: "{profile.region().max_tx_power().dbm()}",
                                                                                    disabled: saving_this,
                                                                                    oninput: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(dbm) = event.value().parse::<i8>().ok() else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                clamp_lora_tx_power(profile, dbm)
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                }
                                                                                span { class: "unit", "dBm" }
                                                                            }
                                                                        }
                                                                        div {
                                                                            class: if draft.lora_control_changed(&entry, LoRaTuneControl::Preamble) { "changed" } else { "" },
                                                                            dt { "Preamble" }
                                                                            dd {
                                                                                input {
                                                                                    r#type: "number",
                                                                                    value: "{profile.preamble().count()}",
                                                                                    min: "1",
                                                                                    disabled: saving_this,
                                                                                    oninput: {
                                                                                        let key = key.clone();
                                                                                        let saved = entry.clone();
                                                                                        move |event| {
                                                                                            let Some(count) = event.value().parse::<u16>().ok() else {
                                                                                                return;
                                                                                            };
                                                                                            write_lora_draft(&mut drafts, &mut save_notices, &key, &saved, |profile| {
                                                                                                clamp_lora_preamble(profile, count)
                                                                                            });
                                                                                        }
                                                                                    },
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    }
}

#[component]
fn PairingPanel(
    target_id: String,
    mut invitation_code: Signal<String>,
    mut pairing: Signal<PairingState>,
    mut pairing_error: Signal<String>,
    mut pairing_target: Signal<String>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
    target_aliases: Signal<HashMap<String, String>>,
    mut targets: Signal<Vec<TargetAccess>>,
    mut selected_target: Signal<String>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    backend: Signal<RemoteControlBackend>,
) -> Element {
    let initial_open = targets()
        .iter()
        .find(|item| item.id == target_id)
        .and_then(|item| item.pairing_expires_in_secs)
        .unwrap_or(0);
    let mut open_remaining = use_signal(|| initial_open);
    let tick_stop = use_hook(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let tick_cancel = tick_stop.clone();
    use_drop(move || {
        tick_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    use_hook({
        let stop = tick_stop.clone();
        let target_id = target_id.clone();
        move || {
            let stop = stop.clone();
            let target_id = target_id.clone();
            spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let remaining = backend().pairing_expires_in_secs(&target_id).unwrap_or(0);
                    if open_remaining() != remaining {
                        open_remaining.set(remaining);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
    });
    rsx! {
        div { class: "stack",
            if matches!(pairing(), PairingState::Idle | PairingState::Connecting) {
                p { class: "note", "This announcement is not paired yet. Enter the eight invitation digits shown on the OLED right now. Hex case does not matter. Awaiting rows are newest-first; use the one with the most open time left after a fresh Pair remote." }
                p { class: "note", "Open window remaining: {format_pairing_open_label(open_remaining())} (~3 minutes from Pair remote)." }
            }
            if !pairing_error().is_empty() {
                p { class: "error", "{pairing_error}" }
            }
            if matches!(pairing(), PairingState::Idle | PairingState::Connecting) {
                label {
                    "Invitation code"
                    input {
                        value: "{invitation_code}",
                        placeholder: "XXXX-XXXX",
                        disabled: pairing() == PairingState::Connecting,
                        oninput: move |event| invitation_code.set(event.value())
                    }
                }
                button {
                    class: "button primary",
                    disabled: pairing() == PairingState::Connecting,
                    onclick: {
                        let announcement_id = target_id.clone();
                        move |_| {
                            let code = invitation_code();
                            let announcement_id = announcement_id.clone();
                            let backend = backend();
                            pairing_target.set(announcement_id.clone());
                            pairing_error.set(String::new());
                            pairing.set(PairingState::Connecting);
                            spawn(async move {
                                match backend.begin_pairing(&announcement_id, &code).await {
                                    Ok(state) => {
                                        pairing_error.set(String::new());
                                        pairing.set(state);
                                    }
                                    Err(error) => {
                                        let message = format!("Pairing failed: {error}");
                                        pairing.set(PairingState::Idle);
                                        pairing_error.set(message.clone());
                                        push_activity(activity_log, message);
                                    }
                                }
                            });
                        }
                    },
                    if pairing() == PairingState::Connecting {
                        "Connecting…"
                    } else {
                        "Continue"
                    }
                }
            }
            if pairing() == PairingState::Connecting {
                p { class: "note", "Establishing the pairing link. This can take a few seconds." }
            }
            if let PairingState::AwaitingConfirmation { digits } = pairing() {
                div {
                    p { class: "note", "Confirm these digits match the OLED, then Approve here and Yes on the face (either order):" }
                    p { class: "digits", "{digits}" }
                    p { class: "note", "Both sides must finish before the attempt window ends (~2 minutes after digits appear)." }
                    div { class: "actions",
                        button {
                            class: "button primary",
                            onclick: {
                                move |_| {
                                    let backend = backend();
                                    pairing_error.set(String::new());
                                    spawn(async move {
                                        pairing.set(PairingState::WaitingForTarget);
                                        match backend.approve_pairing().await {
                                            Ok(state) => {
                                                pairing.set(state);
                                                invitation_code.set(String::new());
                                                pairing_error.set(String::new());
                                                push_activity(activity_log, "Paired a target.");
                                                adopt_missing_aliases(
                                                    target_aliases,
                                                    backend.target_aliases(),
                                                );
                                                if let Ok(items) = backend.targets().await {
                                                    if let Some(paired) = items.iter().rev().find(|item| {
                                                        item.status != TargetStatus::AwaitingPairing
                                                    }) {
                                                        selected_target.set(paired.id.clone());
                                                        expanded_targets.write().insert(paired.id.clone());
                                                        load_interfaces_for_target(
                                                            backend.clone(),
                                                            paired.id.clone(),
                                                            interfaces_by_target,
                                                            true,
                                                            RemoteControlAnnounceWait::UntilHeard,
                                                        );
                                                    }
                                                    targets.set(items);
                                                }
                                            }
                                            Err(error) => {
                                                let message = format_approve_failure(&error);
                                                pairing.set(PairingState::Idle);
                                                pairing_error.set(message.clone());
                                                push_activity(activity_log, message);
                                            }
                                        }
                                    });
                                }
                            },
                            "Approve"
                        }
                        button {
                            class: "button danger",
                            onclick: {
                                move |_| {
                                    let backend = backend();
                                    spawn(async move {
                                        match backend.reject_pairing().await {
                                            Ok(state) => {
                                                pairing.set(state);
                                                pairing_error.set(String::new());
                                                push_activity(activity_log, "Rejected pairing.");
                                            }
                                            Err(error) => {
                                                let message = format!(
                                                    "Pairing rejection failed: {error}"
                                                );
                                                pairing_error.set(message.clone());
                                                push_activity(activity_log, message);
                                            }
                                        }
                                    });
                                }
                            },
                            "Reject"
                        }
                    }
                }
            }
            if pairing() == PairingState::WaitingForTarget {
                p { class: "note", "Controller Approve sent Commit. If the OLED already said Yes, waiting for the board to finish writing the grant and return Completed — not another approve. This can take a bit; a Timeout here means Completed never arrived." }
            }
            if pairing() == PairingState::Approved {
                p { class: "status online", "Pairing complete. Expand the paired target to manage interfaces." }
            }
            if pairing() == PairingState::Rejected {
                p { class: "status", "Pairing rejected." }
            }
        }
    }
}

#[component]
fn UnsavedChangesDialog(
    prompt: UnsavedPrompt,
    mut drafts: Signal<HashMap<String, InterfaceDraft>>,
    editing: Signal<HashSet<String>>,
    saving: Signal<HashSet<String>>,
    save_notices: Signal<HashMap<String, String>>,
    mut focused_interface: Signal<Option<String>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
    mut expanded_interfaces: Signal<HashSet<String>>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    mut screen: Signal<Screen>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let noun = if prompt.keys.len() == 1 {
        "This interface has unsaved changes."
    } else {
        "These interfaces have unsaved changes."
    };
    let busy = prompt.keys.iter().any(|key| saving().contains(key));
    rsx! {
        div { class: "unsaved-overlay",
            div { class: "unsaved-dialog",
                p { "{noun} Save them, discard them, or stay here." }
                div { class: "actions",
                    SaveButton {
                        busy,
                        disabled: false,
                        on_save: {
                            let keys = prompt.keys.clone();
                            let after = prompt.after.clone();
                            move |_| {
                                save_interface_drafts(
                                    keys.clone(),
                                    Some(after.clone()),
                                    drafts,
                                    saving,
                                    save_notices,
                                    editing,
                                    focused_interface,
                                    unsaved,
                                    expanded_interfaces,
                                    expanded_targets,
                                    interfaces_by_target,
                                    screen,
                                    backend(),
                                    activity_log,
                                );
                            }
                        },
                    }
                    button {
                        class: "button",
                        disabled: busy,
                        onclick: {
                            let keys = prompt.keys.clone();
                            let after = prompt.after.clone();
                            move |_| {
                                revert_drafts(&mut drafts.write(), &keys);
                                unsaved.set(None);
                                finish_unsaved_after(
                                    after.clone(),
                                    expanded_interfaces,
                                    expanded_targets,
                                    editing,
                                    focused_interface,
                                    screen,
                                );
                            }
                        },
                        "Discard"
                    }
                    button {
                        class: "button",
                        disabled: busy,
                        onclick: move |_| unsaved.set(None),
                        "Stay"
                    }
                }
            }
        }
    }
}

#[component]
fn SaveButton(busy: bool, disabled: bool, on_save: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button {
            class: if busy { "button busy" } else { "button" },
            disabled: disabled || busy,
            aria_busy: if busy { "true" } else { "false" },
            onclick: move |event| on_save.call(event),
            if busy {
                span { class: "spinner", aria_hidden: "true" }
                "Saving"
            } else {
                "Save"
            }
        }
    }
}

#[component]
fn InfoHint(label: String, mut open: Signal<bool>) -> Element {
    rsx! {
        button {
            class: if open() { "info open" } else { "info" },
            r#type: "button",
            title: "{label}",
            aria_label: "{label}",
            aria_expanded: if open() { "true" } else { "false" },
            onclick: move |_| {
                let next = !open();
                open.set(next);
            },
            "i"
        }
    }
}

#[component]
fn RefreshButton(label: String, on_refresh: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button {
            class: "refresh",
            r#type: "button",
            title: "{label}",
            aria_label: "{label}",
            onclick: move |event| on_refresh.call(event),
            "↻"
        }
    }
}

fn refresh_target_list(backend: RemoteControlBackend, mut targets: Signal<Vec<TargetAccess>>) {
    spawn(async move {
        if let Ok(items) = backend.targets().await {
            targets.set(items);
        }
    });
}

fn refresh_one_interface(
    host: InterfaceHost,
    interface_id: String,
    backend: RemoteControlBackend,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    pairing: PairingState,
) {
    match host {
        InterfaceHost::Controller => {
            if let Ok(entry) = backend.refresh_local_interface(&interface_id) {
                replace_interface_entry(
                    &mut interfaces_by_target,
                    CONTROLLER_SCOPE,
                    interface_id,
                    entry,
                );
            }
        }
        InterfaceHost::Target(target_id) => {
            if pairing_in_progress(&pairing) {
                return;
            }
            if !backend.is_monitoring_target(&target_id) {
                return;
            }
            spawn(async move {
                if let Ok(entry) = backend
                    .refresh_target_interface(&target_id, &interface_id)
                    .await
                {
                    replace_interface_entry(
                        &mut interfaces_by_target,
                        &target_id,
                        interface_id,
                        entry,
                    );
                }
            });
        }
    }
}

fn replace_interface_entry(
    interfaces_by_target: &mut Signal<HashMap<String, LoadedInterfaces>>,
    scope: &str,
    interface_id: String,
    entry: InterfaceEntry,
) {
    if let Some(items) = interfaces_by_target
        .write()
        .get_mut(scope)
        .and_then(LoadedInterfaces::entries_mut)
    {
        if let Some(slot) = items.iter_mut().find(|item| item.id == interface_id) {
            *slot = entry;
        }
    }
}

fn refresh_host_interfaces(
    host: InterfaceHost,
    backend: RemoteControlBackend,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    pairing: PairingState,
) {
    match host {
        InterfaceHost::Controller => {
            load_controller_interfaces(backend, interfaces_by_target);
        }
        InterfaceHost::Target(target) => {
            if pairing_in_progress(&pairing) {
                return;
            }
            load_interfaces_for_target(
                backend,
                target,
                interfaces_by_target,
                true,
                RemoteControlAnnounceWait::UntilHeard,
            );
        }
    }
}

fn load_controller_interfaces(
    backend: RemoteControlBackend,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
) {
    match backend.local_interfaces() {
        Ok(items) => {
            interfaces_by_target
                .write()
                .insert(CONTROLLER_SCOPE.to_string(), LoadedInterfaces::ready(items));
        }
        Err(error) => {
            interfaces_by_target.write().insert(
                CONTROLLER_SCOPE.to_string(),
                LoadedInterfaces::Failed(format!("Controller interface refresh failed: {error}")),
            );
        }
    }
}

fn load_interfaces_for_target(
    backend: RemoteControlBackend,
    target_id: String,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    force: bool,
    wait: RemoteControlAnnounceWait,
) {
    let current = interfaces_by_target().get(&target_id).cloned();
    if !force && matches!(current, Some(LoadedInterfaces::Loading)) {
        return;
    }
    if !force && matches!(current, Some(LoadedInterfaces::Ready { .. })) {
        return;
    }
    if !backend.is_monitoring_target(&target_id) {
        return;
    }
    if !backend.is_connected() {
        interfaces_by_target.write().insert(
            target_id,
            LoadedInterfaces::Failed("The controller node is not running.".to_string()),
        );
        return;
    }
    if !matches!(current, Some(LoadedInterfaces::Ready { .. })) {
        interfaces_by_target
            .write()
            .insert(target_id.clone(), LoadedInterfaces::Loading);
    }
    spawn(async move {
        match backend.interfaces_after_announce(&target_id, wait).await {
            Ok(items) => {
                interfaces_by_target
                    .write()
                    .insert(target_id, LoadedInterfaces::ready(items));
            }
            Err(error) => {
                interfaces_by_target.write().insert(
                    target_id,
                    LoadedInterfaces::Failed(format!("Interface refresh failed: {error}")),
                );
            }
        }
    });
}

#[allow(non_snake_case)]
#[component]
fn FlashSection(
    backend: Signal<RemoteControlBackend>,
    screen: Signal<Screen>,
    drafts: Signal<HashMap<String, InterfaceDraft>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    unsaved: Signal<Option<UnsavedPrompt>>,
    mut targets: Signal<Vec<TargetAccess>>,
    selected_target: Signal<String>,
    expanded_targets: Signal<HashSet<String>>,
    mut selected_flash_board: Signal<Option<String>>,
    mut flash_configuring: Signal<bool>,
    mut flash_forms: Signal<HashMap<String, FlashDraft>>,
    mut flash_status: Signal<String>,
    mut flashing: Signal<bool>,
    mut flash_progress: Signal<Option<FlashProgress>>,
    flash_info: Signal<bool>,
) -> Element {
    #[cfg(target_os = "android")]
    {
        let _ = (
            backend,
            screen,
            drafts,
            interfaces_by_target,
            unsaved,
            targets,
            selected_target,
            expanded_targets,
            selected_flash_board,
            flash_configuring,
            flash_forms,
            flash_status,
            flashing,
            flash_progress,
            flash_info,
        );
        return rsx! {
            h1 { "Flash" }
            p { class: "lead", "Flashing and enrollment are available in the desktop PRNS Controller." }
        };
    }

    #[cfg(not(target_os = "android"))]
    {
        let mut probable_slugs = use_signal(Vec::<String>::new);
        let downloading = use_signal(|| false);
        let importing = use_signal(|| false);
        let exporting = use_signal(|| false);
        let building = use_signal(|| false);
        let mut build_status = use_signal(String::new);
        let catalog_tick = use_signal(|| 0u64);
        let published_tips = use_signal(|| None::<crate::flash::PublishedChannelTips>);
        let mut check_error = use_signal(|| None::<String>);
        let checkout_available = crate::flash::checkout_available();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancelled.clone();
        let poll = cancelled.clone();
        let check_poll = cancelled.clone();
        use_drop(move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        use_effect(move || {
            let cancelled = poll.clone();
            spawn(async move {
                while !cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    if !flashing() && !crate::flash::uf2_volume_probes_held() {
                        let found =
                            tokio::task::spawn_blocking(crate::flash::detect_probable_slugs)
                                .await
                                .unwrap_or_default();
                        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        probable_slugs.set(found);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
        });
        use_effect(move || {
            let cancelled = check_poll.clone();
            let mut published_tips = published_tips;
            spawn(async move {
                loop {
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let result =
                        tokio::task::spawn_blocking(|| crate::flash::check_published_tips(None))
                            .await;
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    match result {
                        Ok(Ok(tips)) => {
                            published_tips.set(Some(tips));
                            check_error.set(None);
                        }
                        Ok(Err(error)) => {
                            if published_tips().as_ref().is_none_or(|tips| tips.is_empty()) {
                                check_error.set(Some(error.to_string()));
                            }
                        }
                        Err(error) => {
                            if published_tips().as_ref().is_none_or(|tips| tips.is_empty()) {
                                check_error.set(Some(error.to_string()));
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                }
            });
        });
        let probable = probable_slugs();
        let mut boards = crate::flash::catalog_boards().unwrap_or_default();
        boards.sort_by_key(|board| !probable.iter().any(|slug| slug == &board.slug));
        let update_note = match (published_tips().as_ref(), check_error().as_ref()) {
            (Some(tips), _) => tips.summary_note(),
            (None, Some(_)) => Some(
                "Could not reach the published channels yet — Import and local images still work."
                    .to_string(),
            ),
            (None, None) => Some("Checking for published updates…".to_string()),
        };
        rsx! {
            div { class: "heading-row",
                h1 { "Flash" }
                InfoHint {
                    label: "About Flash".to_string(),
                    open: flash_info,
                }
            }
            div { class: "section-intro",
                p { class: "lead", "Select a board, open Configure and flash to pick a firmware image and options, then Flash. Enrollment writes this Operator’s grant when the board supports it." }
                if let Some(note) = update_note {
                    p { class: "note", "{note}" }
                }
                if !checkout_available {
                    p { class: "note", "On Configure and flash, choose a published tip or Import a shared artifact. Build is available when Controller detects a Personal Reticulum checkout." }
                }
                if !probable.is_empty() {
                    p { class: "note", "A connected bootloader or USB device matches the highlighted boards. Confirm the model if more than one lights up — some boards share a Board-ID or an ESP USB identity." }
                }
                if flash_info() {
                    p { class: "note info-note", "Desktop only. Configure and flash uses an image dropdown (stable tip, preview tip, then local catalog images) plus Import, Build (checkout), and Export (shareable zip). Tips fetch on select if needed. Flash uses --candidate for published images and --developer-artifacts for local, imported, and bundled catalog images. Set HOPSPOT_FLASH or put hopspot-flash on PATH (checkout cargo fallback still works). PRNS_CONTROLLER_FLASH_LOCAL_BUILD=1 remains an emergency compile-at-flash escape hatch. UF2 and ESP boards can also receive a Remote Control identity and this app’s allow-list grant. An empty SSID leaves any existing station credentials in place." }
                }
            }
            div { class: "accordion",
                for board in boards {
                    {
                        let slug = board.slug.clone();
                        let selected_slug = selected_flash_board();
                        let is_selected = selected_slug.as_deref() == Some(slug.as_str());
                        let is_probable = probable.iter().any(|seen| seen == &slug);
                        let configuring = is_selected && flash_configuring();
                        let form = flash_forms()
                            .get(&slug)
                            .cloned()
                            .unwrap_or_default();
                        let catalog_image = {
                            let _ = catalog_tick();
                            crate::image_catalog::current_image(&slug).ok().flatten()
                        };
                        let image_badge = crate::flash::catalog_image_badge(catalog_image.as_ref());
                        let pick_options = crate::flash::image_pick_options(
                            &slug,
                            published_tips().as_ref(),
                        )
                        .unwrap_or_default();
                        let pick_value = crate::flash::selected_image_pick_value(
                            catalog_image.as_ref(),
                            &pick_options,
                        );
                        let flash_busy = flashing()
                            || downloading()
                            || importing()
                            || exporting()
                            || building();
                        let flash_has_image = catalog_image.is_some()
                            || std::env::var_os("PRNS_CONTROLLER_FLASH_LOCAL_BUILD").is_some();
                        let interfaces = board.interfaces.join(", ");
                        let item_class = match (is_selected, is_probable) {
                            (true, true) => "accordion-item open probable",
                            (true, false) => "accordion-item open",
                            (false, true) => "accordion-item probable",
                            (false, false) => "accordion-item",
                        };
                        rsx! {
                            section {
                                class: "{item_class}",
                                button {
                                    class: "twisty-row",
                                    aria_expanded: if is_selected { "true" } else { "false" },
                                    onclick: {
                                        let slug = slug.clone();
                                        move |_| {
                                            if selected_flash_board().as_deref() == Some(slug.as_str()) {
                                                selected_flash_board.set(None);
                                                flash_configuring.set(false);
                                            } else {
                                                selected_flash_board.set(Some(slug.clone()));
                                                flash_configuring.set(false);
                                            }
                                        }
                                    },
                                    span { class: if is_selected { "twisty open" } else { "twisty" }, aria_hidden: "true" }
                                    div { class: "twisty-copy",
                                        span { class: "twisty-title", "{board.display_name}" }
                                        span { class: "twisty-address", "{board.silicon} · {board.transport} · {board.availability}" }
                                    }
                                    if is_probable {
                                        span { class: "status online", "Connected" }
                                    }
                                }
                                if is_selected {
                                    div { class: "accordion-body",
                                        div { class: if configuring { "interface-toolbar editing" } else { "interface-toolbar" },
                                            div { class: "toolbar-track",
                                                div { class: "toolbar-pane",
                                                    button {
                                                        class: "button",
                                                        r#type: "button",
                                                        disabled: flashing(),
                                                        onclick: move |_| flash_configuring.set(true),
                                                        "Configure and flash"
                                                    }
                                                }
                                                div { class: "toolbar-pane",
                                                    button {
                                                        class: "button",
                                                        r#type: "button",
                                                        disabled: flashing(),
                                                        onclick: move |_| {
                                                            flash_configuring.set(false);
                                                            build_status.set(String::new());
                                                            clear_flash_progress(flash_progress, flash_status);
                                                        },
                                                        "Back"
                                                    }
                                                    button {
                                                        class: if flashing() { "button busy" } else { "button" },
                                                        r#type: "button",
                                                        disabled: flash_busy || !flash_has_image,
                                                        aria_busy: if flashing() { "true" } else { "false" },
                                                        onclick: {
                                                            let board = board.clone();
                                                            let form = form.clone();
                                                            move |_| {
                                                                build_status.set(String::new());
                                                                start_flash(
                                                                    board.slug.clone(),
                                                                    board.display_name.clone(),
                                                                    board.enrollable
                                                                        && form.enrol_for_management,
                                                                    board.supports_wifi,
                                                                    board.supports_tcp,
                                                                    form.clone(),
                                                                    backend,
                                                                    screen,
                                                                    drafts,
                                                                    interfaces_by_target,
                                                                    unsaved,
                                                                    targets,
                                                                    flash_status,
                                                                    flashing,
                                                                    flash_progress,
                                                                );
                                                            }
                                                        },
                                                        if flashing() {
                                                            span { class: "spinner", aria_hidden: "true" }
                                                            "Flashing"
                                                        } else {
                                                            "Flash"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if configuring {
                                            if let Some(progress) = flash_progress() {
                                                {
                                                    let stages = FlashStage::stages(progress.enrollable);
                                                    rsx! {
                                                        ol {
                                                            class: "flash-stages",
                                                            "data-count": "{stages.len()}",
                                                            for stage in stages {
                                                                {
                                                                    let state = progress.stage_state(*stage);
                                                                    let class = match state {
                                                                        FlashStageState::Pending => "flash-stage pending",
                                                                        FlashStageState::Current => "flash-stage current",
                                                                        FlashStageState::Done => "flash-stage done",
                                                                        FlashStageState::Failed => "flash-stage failed",
                                                                        FlashStageState::Skipped => "flash-stage skipped",
                                                                    };
                                                                    rsx! {
                                                                        li { class,
                                                                            span { class: "dot" }
                                                                            span { class: "label", "{stage.label()}" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        p { class: "note flash-stage-detail",
                                                            if let Some(percent) = progress.write_percent {
                                                                "{progress.detail} ({percent}%)"
                                                            } else {
                                                                "{progress.detail}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            {
                                                let status = flash_status();
                                                let repeats_progress = flash_progress().as_ref().is_some_and(|progress| {
                                                    progress.detail == status
                                                });
                                                rsx! {
                                                    if !status.is_empty() && !repeats_progress {
                                                        p { class: "note flash-stage-detail", "{status}" }
                                                    }
                                                }
                                            }
                                            {
                                                let offer = flash_progress()
                                                    .as_ref()
                                                    .and_then(FlashProgress::manage_offer)
                                                    .cloned();
                                                rsx! {
                                                    if let Some(enrolled) = offer {
                                                        div { class: "actions flash-manage",
                                                            button {
                                                                class: "button primary",
                                                                r#type: "button",
                                                                onclick: move |_| {
                                                                    flash_configuring.set(false);
                                                                    clear_flash_progress(
                                                                        flash_progress,
                                                                        flash_status,
                                                                    );
                                                                    open_enrolled_flash_target(
                                                                        enrolled.clone(),
                                                                        backend(),
                                                                        screen,
                                                                        drafts,
                                                                        interfaces_by_target,
                                                                        unsaved,
                                                                        selected_target,
                                                                        expanded_targets,
                                                                    );
                                                                },
                                                                "Manage {enrolled.display_name}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: if configuring { "deck editing" } else { "deck" },
                                            div { class: "deck-track",
                                                div { class: "deck-pane",
                                                    dl { class: "facts",
                                                        div { dt { "Silicon" } dd { "{board.silicon}" } }
                                                        div { dt { "Transport" } dd { "{board.transport}" } }
                                                        div { dt { "Interfaces" } dd { "{interfaces}" } }
                                                        div { dt { "Catalog" } dd { "{image_badge}" } }
                                                    }
                                                    ul {
                                                        for step in crate::flash::preparation_steps(&board.preparation_profile) {
                                                            li { "{step}" }
                                                        }
                                                    }
                                                    p { class: "note",
                                                        if board.enrollable {
                                                            "Open Configure and flash to choose an image and options. Flash writes firmware plus this Operator’s grant. When that succeeds, this page offers to open the node under Managed Nodes."
                                                        } else {
                                                            "Open Configure and flash to choose an image and options, then Flash. Pair the node from Managed Nodes afterward."
                                                        }
                                                    }
                                                }
                                                div { class: "deck-pane",
                                                    div { class: "flash-image-pick",
                                                        label { "Firmware image"
                                                            select {
                                                                value: "{pick_value}",
                                                                disabled: flash_busy,
                                                                onchange: {
                                                                    let slug = slug.clone();
                                                                    let pick_options = pick_options.clone();
                                                                    move |event| {
                                                                        let value = event.value();
                                                                        let Some(option) =
                                                                            crate::flash::image_pick_option_from_value(
                                                                                &pick_options,
                                                                                &value,
                                                                            )
                                                                            .cloned()
                                                                        else {
                                                                            return;
                                                                        };
                                                                        build_status.set(String::new());
                                                                        start_select_image(
                                                                            slug.clone(),
                                                                            option,
                                                                            flash_status,
                                                                            downloading,
                                                                            catalog_tick,
                                                                            published_tips,
                                                                        );
                                                                    }
                                                                },
                                                                for option in pick_options.iter() {
                                                                    option {
                                                                        value: "{option.value()}",
                                                                        selected: option.value() == pick_value,
                                                                        "{option.label()}"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        div { class: "flash-actions",
                                                            button {
                                                                class: "button",
                                                                r#type: "button",
                                                                disabled: flash_busy,
                                                                onclick: {
                                                                    let slug = slug.clone();
                                                                    move |_| {
                                                                        build_status.set(String::new());
                                                                        start_import(
                                                                            slug.clone(),
                                                                            flash_status,
                                                                            importing,
                                                                            catalog_tick,
                                                                        );
                                                                    }
                                                                },
                                                                "Import"
                                                            }
                                                            button {
                                                                class: "button",
                                                                r#type: "button",
                                                                disabled: flash_busy || !checkout_available,
                                                                title: if checkout_available {
                                                                    ""
                                                                } else {
                                                                    "Build requires a Personal Reticulum checkout"
                                                                },
                                                                onclick: {
                                                                    let slug = slug.clone();
                                                                    move |_| {
                                                                        start_build(
                                                                            slug.clone(),
                                                                            build_status,
                                                                            flash_status,
                                                                            building,
                                                                            catalog_tick,
                                                                        );
                                                                    }
                                                                },
                                                                "Build"
                                                            }
                                                            button {
                                                                class: "button",
                                                                r#type: "button",
                                                                disabled: flash_busy || catalog_image.is_none(),
                                                                onclick: {
                                                                    let slug = slug.clone();
                                                                    move |_| {
                                                                        build_status.set(String::new());
                                                                        start_export(
                                                                            slug.clone(),
                                                                            flash_status,
                                                                            exporting,
                                                                        );
                                                                    }
                                                                },
                                                                "Export"
                                                            }
                                                        }
                                                        {
                                                            let status = build_status();
                                                            rsx! {
                                                                if !status.is_empty() {
                                                                    p { class: "note flash-build-status", "{status}" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    {flash_options_form(
                                                        board.clone(),
                                                        slug.clone(),
                                                        form.clone(),
                                                        flashing(),
                                                        flash_forms,
                                                    )}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn flash_options_form(
    board: crate::flash::CatalogBoard,
    slug: String,
    form: FlashDraft,
    busy: bool,
    mut flash_forms: Signal<HashMap<String, FlashDraft>>,
) -> Element {
    rsx! {
        div { class: "edit-card",
            h3 { "Flash options" }
            p { class: "note", "These settings are written with the firmware. Leave Wi-Fi blank to keep whatever is already on the board." }
            if board.enrollable {
                label {
                    class: "flash-check",
                    input {
                        r#type: "checkbox",
                        checked: form.enrol_for_management,
                        disabled: busy,
                        onchange: {
                            let slug = slug.clone();
                            move |event| {
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.enrol_for_management = event.checked();
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                    }
                    "Enrol for management"
                }
            }
            if board.supports_wifi {
                label { "Station SSID"
                    input {
                        r#type: "text",
                        value: "{form.wifi_ssid}",
                        maxlength: "32",
                        placeholder: "Leave empty to preserve existing Wi-Fi",
                        disabled: busy,
                        oninput: {
                            let slug = slug.clone();
                            move |event| {
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.wifi_ssid = event.value();
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                    }
                }
                label { "Station password"
                    input {
                        r#type: "password",
                        value: "{form.wifi_password}",
                        maxlength: "64",
                        placeholder: "empty for an open network",
                        disabled: busy,
                        oninput: {
                            let slug = slug.clone();
                            move |event| {
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.wifi_password = event.value();
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                    }
                }
            }
            if board.supports_tcp {
                label { "TCP client"
                    input {
                        r#type: "text",
                        value: "{form.tcp_client}",
                        placeholder: "Optional IPv4, hostname, or host:port",
                        disabled: busy,
                        oninput: {
                            let slug = slug.clone();
                            move |event| {
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.tcp_client = event.value();
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                    }
                }
            }
            if board.has_lora {
                label { "LoRa region"
                    select {
                        value: "{form.lora_region.label()}",
                        disabled: busy,
                        onchange: {
                            let slug = slug.clone();
                            move |event| {
                                let Some(region) = Region::ALL
                                    .into_iter()
                                    .find(|region| region.label() == event.value())
                                else {
                                    return;
                                };
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.lora_region = region;
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                        for region in Region::ALL {
                            option {
                                value: "{region.label()}",
                                selected: form.lora_region == region,
                                "{region.label()}"
                            }
                        }
                    }
                }
                label { "LoRa preset"
                    select {
                        value: "{form.lora_preset.label()}",
                        disabled: busy,
                        onchange: {
                            let slug = slug.clone();
                            move |event| {
                                let Some(preset) = ModemPreset::ALL
                                    .into_iter()
                                    .find(|preset| preset.label() == event.value())
                                else {
                                    return;
                                };
                                let mut next = flash_forms()
                                    .get(&slug)
                                    .cloned()
                                    .unwrap_or_default();
                                next.lora_preset = preset;
                                flash_forms.write().insert(slug.clone(), next);
                            }
                        },
                        for preset in ModemPreset::ALL {
                            option {
                                value: "{preset.label()}",
                                selected: form.lora_preset == preset,
                                "{preset.label()}"
                            }
                        }
                    }
                }
                p { class: "note", "US 915 MediumFast is the firmware default. A different region or preset is applied after the node comes up." }
            }
            if !board.supports_wifi && !board.has_lora {
                p { class: "note", "This board has no flash-time station or LoRa options." }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn start_select_image(
    slug: String,
    option: crate::flash::ImagePickOption,
    mut flash_status: Signal<String>,
    mut downloading: Signal<bool>,
    mut catalog_tick: Signal<u64>,
    mut published_tips: Signal<Option<crate::flash::PublishedChannelTips>>,
) {
    downloading.set(true);
    flash_status.set(format!("Selecting firmware for {slug}…"));
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn(async move {
        while let Some(message) = status_rx.recv().await {
            flash_status.set(message);
        }
    });
    spawn(async move {
        let result = tokio::task::spawn_blocking({
            let slug = slug.clone();
            let option = option.clone();
            move || {
                crate::flash::select_image_pick(&slug, &option, |message| {
                    let _ = status_tx.send(message);
                })
            }
        })
        .await;
        downloading.set(false);
        match result {
            Ok(Ok(image)) => {
                catalog_tick.set(catalog_tick() + 1);
                if image.provenance == crate::image_catalog::ImageProvenance::Published {
                    let mut tips = published_tips().unwrap_or_default();
                    tips.record_published_selection(&image);
                    published_tips.set(Some(tips));
                }
                let badge = crate::flash::catalog_image_badge(Some(&image));
                flash_status.set(format!("Selected {badge} for {slug}."));
            }
            Ok(Err(error)) => {
                flash_status.set(format!("Image select failed: {error}"));
            }
            Err(error) => {
                flash_status.set(format!("Image select failed: {error}"));
            }
        }
    });
}

#[cfg(not(target_os = "android"))]
fn start_import(
    slug: String,
    mut flash_status: Signal<String>,
    mut importing: Signal<bool>,
    mut catalog_tick: Signal<u64>,
) {
    let Some(path) = crate::flash::pick_board_import_path() else {
        return;
    };
    importing.set(true);
    flash_status.set(format!("Importing firmware for {slug}…"));
    spawn(async move {
        let result = tokio::task::spawn_blocking({
            let slug = slug.clone();
            let path = path.clone();
            move || crate::flash::import_board_image(&slug, &path)
        })
        .await;
        importing.set(false);
        match result {
            Ok(Ok(image)) => {
                catalog_tick.set(catalog_tick() + 1);
                let badge = crate::flash::catalog_image_badge(Some(&image));
                flash_status.set(format!("Imported {badge} for {slug}."));
            }
            Ok(Err(error)) => {
                flash_status.set(format!("Import failed: {error}"));
            }
            Err(error) => {
                flash_status.set(format!("Import failed: {error}"));
            }
        }
    });
}

#[cfg(not(target_os = "android"))]
fn start_export(slug: String, mut flash_status: Signal<String>, mut exporting: Signal<bool>) {
    let version = crate::image_catalog::current_image(&slug)
        .ok()
        .flatten()
        .map(|image| image.version)
        .unwrap_or_else(|| "firmware".into());
    let Some(path) = crate::flash::pick_board_export_path(&slug, &version) else {
        return;
    };
    exporting.set(true);
    flash_status.set(format!("Exporting firmware for {slug}…"));
    spawn(async move {
        let result = tokio::task::spawn_blocking({
            let slug = slug.clone();
            let path = path.clone();
            move || crate::flash::export_board_image(&slug, &path)
        })
        .await;
        exporting.set(false);
        match result {
            Ok(Ok(exported)) => {
                flash_status.set(format!(
                    "Exported shareable bundle for {slug} to {}.",
                    exported.display()
                ));
            }
            Ok(Err(error)) => {
                flash_status.set(format!("Export failed: {error}"));
            }
            Err(error) => {
                flash_status.set(format!("Export failed: {error}"));
            }
        }
    });
}

#[cfg(not(target_os = "android"))]
fn start_build(
    slug: String,
    mut build_status: Signal<String>,
    mut flash_status: Signal<String>,
    mut building: Signal<bool>,
    mut catalog_tick: Signal<u64>,
) {
    building.set(true);
    flash_status.set(String::new());
    build_status.set(format!("Building local firmware for {slug}…"));
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn(async move {
        while let Some(message) = status_rx.recv().await {
            build_status.set(message);
        }
    });
    spawn(async move {
        let result = tokio::task::spawn_blocking({
            let slug = slug.clone();
            move || {
                crate::flash::build_local_board(&slug, |message| {
                    let _ = status_tx.send(message);
                })
            }
        })
        .await;
        building.set(false);
        match result {
            Ok(Ok(image)) => {
                catalog_tick.set(catalog_tick() + 1);
                let badge = crate::flash::catalog_image_badge(Some(&image));
                build_status.set(format!("Built {badge} for {slug}."));
            }
            Ok(Err(error)) => {
                build_status.set(format!("Build failed: {error}"));
            }
            Err(error) => {
                build_status.set(format!("Build failed: {error}"));
            }
        }
    });
}

fn start_flash(
    slug: String,
    display_name: String,
    enrollable: bool,
    supports_wifi: bool,
    supports_tcp: bool,
    form: FlashDraft,
    backend: Signal<RemoteControlBackend>,
    screen: Signal<Screen>,
    drafts: Signal<HashMap<String, InterfaceDraft>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    unsaved: Signal<Option<UnsavedPrompt>>,
    mut targets: Signal<Vec<TargetAccess>>,
    mut flash_status: Signal<String>,
    mut flashing: Signal<bool>,
    mut flash_progress: Signal<Option<FlashProgress>>,
) {
    #[cfg(target_os = "android")]
    {
        let _ = (
            slug,
            display_name,
            enrollable,
            supports_wifi,
            supports_tcp,
            form,
            backend,
            screen,
            drafts,
            interfaces_by_target,
            unsaved,
            targets,
            flash_status,
            flashing,
            flash_progress,
        );
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (screen, drafts, interfaces_by_target, unsaved);
        if flashing() {
            return;
        }
        let wifi = match crate::flash::wifi_flash_plan(&form, supports_wifi, supports_tcp) {
            Ok(plan) => plan,
            Err(error) => {
                flash_status.set(format!("Flash failed: {error}"));
                return;
            }
        };
        let lora = form.custom_lora_profile();
        flashing.set(true);
        flash_status.set(String::new());
        let first_stage = if enrollable {
            FlashStage::Enroll
        } else {
            FlashStage::Prepare
        };
        flash_progress.set(Some(FlashProgress::running(
            enrollable,
            first_stage,
            first_stage.label(),
        )));
        let backend = backend();
        let allow_list_key = match backend.controller_identity() {
            Ok(identity) => identity.allow_list_key,
            Err(error) => {
                flashing.set(false);
                flash_status.set(format!("Flash failed: {error}"));
                return;
            }
        };
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                flash_progress.set(Some(progress));
            }
        });
        spawn(async move {
            let flash_result = tokio::task::spawn_blocking({
                let slug = slug.clone();
                let allow_list_key = allow_list_key.clone();
                let progress_tx = progress_tx.clone();
                let wifi = wifi.clone();
                move || -> Result<Option<crate::flash::Enrollment>, crate::flash::FlashError> {
                    let enrollment = if enrollable {
                        let _ = progress_tx.send(FlashProgress::running(
                            enrollable,
                            FlashStage::Enroll,
                            FlashStage::Enroll.label(),
                        ));
                        Some(crate::flash::mint_enrollment(&allow_list_key)?)
                    } else {
                        None
                    };
                    let _ = progress_tx.send(FlashProgress::running(
                        enrollable,
                        FlashStage::Prepare,
                        FlashStage::Prepare.label(),
                    ));
                    crate::flash::flash_enrolled_board(
                        &slug,
                        enrollment.as_ref().map(|item| item.vault_page.as_slice()),
                        enrollable,
                        &wifi,
                        |progress| {
                            let _ = progress_tx.send(progress);
                        },
                    )?;
                    Ok(enrollment)
                }
            })
            .await;
            match flash_result {
                Ok(Ok(Some(enrollment))) => {
                    match backend
                        .enroll_flashed_target(enrollment.access, &display_name)
                        .await
                    {
                        Ok(target_id) => {
                            let mut detail = format!(
                                "{display_name} is flashed and listed under Managed Nodes."
                            );
                            if let Some(profile) = lora {
                                match apply_flashed_lora(&backend, &enrollment.target_id, profile)
                                    .await
                                {
                                    Ok(()) => {
                                        detail.push_str(" Custom LoRa settings were applied.");
                                    }
                                    Err(error) => {
                                        detail.push_str(&format!(
                                            " Set LoRa from the node’s LoRa card if needed ({error})."
                                        ));
                                    }
                                }
                            }
                            flash_status.set(detail.clone());
                            flash_progress.set(Some(FlashProgress {
                                enrollable,
                                stage: FlashStage::Complete,
                                detail,
                                write_percent: None,
                                outcome: FlashRunOutcome::Succeeded,
                                enrolled: Some(EnrolledFlashTarget {
                                    id: target_id,
                                    display_name: display_name.clone(),
                                }),
                            }));
                            if let Ok(items) = backend.targets().await {
                                targets.set(items);
                            }
                        }
                        Err(error) => {
                            flash_status.set(format!(
                                "Flashed {display_name}, but listing it under Managed Nodes failed: {error}"
                            ));
                            flash_progress.set(Some(FlashProgress {
                                enrollable,
                                stage: FlashStage::Complete,
                                detail: error.to_string(),
                                write_percent: None,
                                outcome: FlashRunOutcome::Failed,
                                enrolled: None,
                            }));
                        }
                    }
                }
                Ok(Ok(None)) => {
                    let detail = format!(
                        "{display_name} firmware flashed. Pair it from Managed Nodes to manage it."
                    );
                    flash_status.set(detail.clone());
                    flash_progress.set(Some(FlashProgress {
                        enrollable,
                        stage: FlashStage::Complete,
                        detail,
                        write_percent: None,
                        outcome: FlashRunOutcome::Succeeded,
                        enrolled: None,
                    }));
                }
                Ok(Err(error)) => {
                    let detail = format!("Flash failed: {error}");
                    flash_status.set(detail.clone());
                    if let Some(mut progress) = flash_progress() {
                        progress.outcome = FlashRunOutcome::Failed;
                        progress.detail = detail;
                        flash_progress.set(Some(progress));
                    }
                }
                Err(error) => {
                    let detail = format!("Flash failed: {error}");
                    flash_status.set(detail.clone());
                    if let Some(mut progress) = flash_progress() {
                        progress.outcome = FlashRunOutcome::Failed;
                        progress.detail = detail;
                        flash_progress.set(Some(progress));
                    }
                }
            }
            flashing.set(false);
        });
    }
}

#[cfg(not(target_os = "android"))]
async fn apply_flashed_lora(
    backend: &RemoteControlBackend,
    target_id: &str,
    profile: RadioProfile,
) -> Result<(), BackendError> {
    let interfaces = backend
        .interfaces_after_announce(target_id, RemoteControlAnnounceWait::UntilHeard)
        .await?;
    let Some(lora) = interfaces.iter().find(|entry| entry.kind == "lora") else {
        return Err(BackendError::Operation {
            operation: "set interface LoRa profile",
            detail: "the flashed node has no LoRa card yet".to_string(),
        });
    };
    backend
        .set_interface_lora_profile(target_id, &lora.id, profile)
        .await
}

#[cfg(not(target_os = "android"))]
fn clear_flash_progress(
    mut flash_progress: Signal<Option<FlashProgress>>,
    mut flash_status: Signal<String>,
) {
    flash_progress.set(None);
    flash_status.set(String::new());
}

#[cfg(not(target_os = "android"))]
fn open_enrolled_flash_target(
    enrolled: EnrolledFlashTarget,
    backend: RemoteControlBackend,
    screen: Signal<Screen>,
    drafts: Signal<HashMap<String, InterfaceDraft>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    unsaved: Signal<Option<UnsavedPrompt>>,
    mut selected_target: Signal<String>,
    mut expanded_targets: Signal<HashSet<String>>,
) {
    selected_target.set(enrolled.id.clone());
    expanded_targets.write().insert(enrolled.id.clone());
    load_interfaces_for_target(
        backend,
        enrolled.id,
        interfaces_by_target,
        true,
        RemoteControlAnnounceWait::UntilHeard,
    );
    request_screen(Screen::Nodes, screen, drafts, interfaces_by_target, unsaved);
}

fn request_screen(
    next: Screen,
    mut screen: Signal<Screen>,
    drafts: Signal<HashMap<String, InterfaceDraft>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
) {
    if screen() == next {
        return;
    }
    let keys = dirty_keys_matching(
        &drafts(),
        &saved_interfaces_by_key(&interfaces_by_target()),
        None,
    );
    if !keys.is_empty() {
        unsaved.set(Some(UnsavedPrompt {
            keys,
            after: UnsavedAfter::ChangeScreen { screen: next },
        }));
        return;
    }
    screen.set(next);
}

fn request_interface_toggle(
    key: String,
    mut expanded_interfaces: Signal<HashSet<String>>,
    mut editing: Signal<HashSet<String>>,
    drafts: Signal<HashMap<String, InterfaceDraft>>,
    mut focused_interface: Signal<Option<String>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
) {
    let opening = !expanded_interfaces().contains(&key);
    let saved = saved_interfaces_by_key(&interfaces_by_target());
    let leaving = if opening {
        focused_interface()
            .filter(|current| current != &key)
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        vec![key.clone()]
    };
    let dirty = dirty_keys_matching(&drafts(), &saved, None)
        .into_iter()
        .filter(|dirty| leaving.contains(dirty))
        .collect::<Vec<_>>();
    if !dirty.is_empty() {
        unsaved.set(Some(UnsavedPrompt {
            keys: dirty,
            after: UnsavedAfter::ToggleInterface { key },
        }));
        return;
    }
    toggle_id(&mut expanded_interfaces, &key);
    if expanded_interfaces().contains(&key) {
        focused_interface.set(Some(key));
    } else {
        editing.write().remove(&key);
        if focused_interface() == Some(key) {
            focused_interface.set(None);
        }
    }
}

fn request_close_editor(
    key: String,
    mut drafts: Signal<HashMap<String, InterfaceDraft>>,
    mut editing: Signal<HashSet<String>>,
    mut save_notices: Signal<HashMap<String, String>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
    interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
) {
    let saved = saved_interfaces_by_key(&interfaces_by_target());
    if dirty_keys_matching(&drafts(), &saved, None).contains(&key) {
        unsaved.set(Some(UnsavedPrompt {
            keys: vec![key.clone()],
            after: UnsavedAfter::CloseEditor { key },
        }));
        return;
    }
    revert_drafts(&mut drafts.write(), &[key.clone()]);
    save_notices.write().remove(&key);
    editing.write().remove(&key);
}

fn save_interface_drafts(
    keys: Vec<String>,
    after: Option<UnsavedAfter>,
    mut drafts: Signal<HashMap<String, InterfaceDraft>>,
    mut saving: Signal<HashSet<String>>,
    mut save_notices: Signal<HashMap<String, String>>,
    editing: Signal<HashSet<String>>,
    focused_interface: Signal<Option<String>>,
    mut unsaved: Signal<Option<UnsavedPrompt>>,
    expanded_interfaces: Signal<HashSet<String>>,
    expanded_targets: Signal<HashSet<String>>,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    screen: Signal<Screen>,
    backend: RemoteControlBackend,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let mut pending = Vec::new();
    for key in keys {
        if saving().contains(&key) {
            continue;
        }
        let Some((scope, interface_id)) = key.rsplit_once(':') else {
            continue;
        };
        let scope = scope.to_string();
        let interface_id = interface_id.to_string();
        let Some(saved) = saved_interfaces_by_key(&interfaces_by_target())
            .get(&key)
            .cloned()
        else {
            let message = format!("Save failed: interface {key} is no longer listed.");
            save_notices.write().insert(key, message.clone());
            push_activity(activity_log, message);
            return;
        };
        let draft = draft_or_saved(&drafts(), &key, &saved);
        if !draft.is_dirty(&saved) {
            drafts.write().remove(&key);
            continue;
        }
        pending.push((key, scope, interface_id, saved, draft));
    }
    if pending.is_empty() {
        return;
    }
    let in_flight: Vec<String> = pending
        .iter()
        .map(|(key, _, _, _, _)| key.clone())
        .collect();
    saving.write().extend(in_flight.iter().cloned());
    spawn(async move {
        let mut applied = Vec::new();
        for (key, scope, interface_id, previous, draft) in pending {
            for field in draft.changed_fields(&previous) {
                let result = match field {
                    InterfaceField::Mode => {
                        if scope == CONTROLLER_SCOPE {
                            backend.set_local_interface_mode(&interface_id, draft.mode)
                        } else {
                            backend
                                .set_interface_mode(&scope, &interface_id, draft.mode)
                                .await
                        }
                    }
                    InterfaceField::Group => {
                        if scope == CONTROLLER_SCOPE {
                            backend.set_local_interface_group(&interface_id, &draft.group)
                        } else {
                            backend
                                .set_interface_group(&scope, &interface_id, &draft.group)
                                .await
                        }
                    }
                    InterfaceField::LoRaTune => match draft.lora {
                        Some(_) if scope == CONTROLLER_SCOPE => Err(BackendError::Operation {
                            operation: "set interface LoRa profile",
                            detail: "this controller has no LoRa radio to tune".to_string(),
                        }),
                        Some(profile) => {
                            backend
                                .set_interface_lora_profile(&scope, &interface_id, profile)
                                .await
                        }
                        None => Err(BackendError::Operation {
                            operation: "set interface LoRa profile",
                            detail: "this interface has no radio profile to save".to_string(),
                        }),
                    },
                    InterfaceField::WifiStation => {
                        if scope == CONTROLLER_SCOPE {
                            Err(BackendError::Operation {
                                operation: "set interface Wi-Fi station",
                                detail: "this controller uses the host OS network; SSID and password are only for a Hopspot station".to_string(),
                            })
                        } else {
                            backend
                                .set_interface_wifi_station(
                                    &scope,
                                    &interface_id,
                                    &draft.wifi_ssid,
                                    &draft.wifi_password,
                                )
                                .await
                        }
                    }
                    InterfaceField::TcpTarget => {
                        if scope == CONTROLLER_SCOPE {
                            backend.set_local_tcp_target(&draft.tcp_target)
                        } else {
                            Err(BackendError::Operation {
                                operation: "set controller TCP target",
                                detail: "TCP host:port on this card is only for this app's outbound client".to_string(),
                            })
                        }
                    }
                };
                if let Err(error) = result {
                    for key in &in_flight {
                        saving.write().remove(key);
                    }
                    let message = format!("Save failed: {error}");
                    save_notices.write().insert(key, message.clone());
                    push_activity(activity_log, message);
                    return;
                }
            }
            write_interface_entry(&mut interfaces_by_target, &scope, &interface_id, |item| {
                apply_draft_to_entry(item, &draft);
            });
            drafts.write().remove(&key);
            save_notices.write().remove(&key);
            applied.push(previous.name);
        }
        for key in &in_flight {
            saving.write().remove(key);
        }
        push_activity(
            activity_log,
            match applied.as_slice() {
                [name] => format!("Saved configuration for {name}."),
                _ => format!("Saved configuration for {}.", applied.join(", ")),
            },
        );
        if let Some(after) = after {
            unsaved.set(None);
            finish_unsaved_after(
                after,
                expanded_interfaces,
                expanded_targets,
                editing,
                focused_interface,
                screen,
            );
        }
    });
}

fn write_lora_draft(
    drafts: &mut Signal<HashMap<String, InterfaceDraft>>,
    save_notices: &mut Signal<HashMap<String, String>>,
    key: &str,
    saved: &InterfaceEntry,
    update: impl FnOnce(
        personal_rns::interfaces::lora::RadioProfile,
    ) -> personal_rns::interfaces::lora::RadioProfile,
) {
    let mut draft = draft_or_saved(&drafts(), key, saved);
    let Some(profile) = draft.lora else {
        return;
    };
    draft.lora = Some(update(profile));
    save_notices.write().remove(key);
    put_draft(&mut drafts.write(), key.to_string(), saved, draft);
}

fn write_interface_entry(
    interfaces_by_target: &mut Signal<HashMap<String, LoadedInterfaces>>,
    scope: &str,
    interface_id: &str,
    update: impl FnOnce(&mut InterfaceEntry),
) {
    if let Some(items) = interfaces_by_target
        .write()
        .get_mut(scope)
        .and_then(LoadedInterfaces::entries_mut)
    {
        if let Some(item) = items.iter_mut().find(|item| item.id == interface_id) {
            update(item);
        }
    }
}

fn finish_unsaved_after(
    after: UnsavedAfter,
    mut expanded_interfaces: Signal<HashSet<String>>,
    mut expanded_targets: Signal<HashSet<String>>,
    mut editing: Signal<HashSet<String>>,
    mut focused_interface: Signal<Option<String>>,
    mut screen: Signal<Screen>,
) {
    match after {
        UnsavedAfter::CollapseTarget { target_id } => {
            expanded_targets.write().remove(&target_id);
            let prefix = format!("{target_id}:");
            expanded_interfaces
                .write()
                .retain(|key| !key.starts_with(&prefix));
            editing.write().retain(|key| !key.starts_with(&prefix));
            if focused_interface()
                .as_ref()
                .is_some_and(|key| key.starts_with(&prefix))
            {
                focused_interface.set(None);
            }
        }
        UnsavedAfter::ChangeScreen { screen: next } => screen.set(next),
        UnsavedAfter::ToggleInterface { key } => {
            toggle_id(&mut expanded_interfaces, &key);
            if expanded_interfaces().contains(&key) {
                focused_interface.set(Some(key));
            } else {
                editing.write().remove(&key);
                if focused_interface() == Some(key) {
                    focused_interface.set(None);
                }
            }
        }
        UnsavedAfter::CloseEditor { key } => {
            editing.write().remove(&key);
        }
    }
}

fn saved_interfaces_by_key(
    loaded: &HashMap<String, LoadedInterfaces>,
) -> HashMap<String, InterfaceEntry> {
    let mut saved = HashMap::new();
    for (scope, loaded) in loaded {
        if let Some(items) = loaded.entries() {
            for item in items {
                saved.insert(interface_key(scope, &item.id), item.clone());
            }
        }
    }
    saved
}

fn toggle_id(ids: &mut Signal<HashSet<String>>, id: &str) {
    let mut set = ids.write();
    if !set.remove(id) {
        set.insert(id.to_owned());
    }
}

fn interface_key(target_id: &str, interface_id: &str) -> String {
    format!("{target_id}:{interface_id}")
}

fn adopt_missing_aliases(
    aliases: Signal<HashMap<String, String>>,
    stored: HashMap<String, String>,
) {
    adopt_missing_aliases_preserving(aliases, stored, None);
}

fn adopt_missing_aliases_preserving(
    mut aliases: Signal<HashMap<String, String>>,
    stored: HashMap<String, String>,
    preserve: Option<String>,
) {
    let current = aliases();
    let mut next = current.clone();
    let mut changed = false;
    for (id, name) in stored {
        if preserve.as_deref() == Some(id.as_str()) {
            continue;
        }
        if next.contains_key(&id) {
            continue;
        }
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        next.insert(id, name.to_owned());
        changed = true;
    }
    if changed {
        aliases.set(next);
    }
}

fn aliases_preserving(
    mut stored: HashMap<String, String>,
    current: &HashMap<String, String>,
    preserve: Option<&str>,
) -> HashMap<String, String> {
    if let Some(id) = preserve {
        match current.get(id) {
            Some(typed) => {
                stored.insert(id.to_owned(), typed.clone());
            }
            None => {
                stored.remove(id);
            }
        }
    }
    stored
}

fn target_display_name(target: &TargetAccess) -> String {
    let name = target.name.trim();
    if name.is_empty() {
        target_label(&target.id)
    } else {
        name.to_string()
    }
}

fn network_transport_label(transport: RemoteControlNetworkTransport) -> &'static str {
    match transport {
        RemoteControlNetworkTransport::Enabled => "Enabled",
        RemoteControlNetworkTransport::Disabled => "Disabled",
    }
}

fn network_transport_status(transport: Option<RemoteControlNetworkTransport>) -> &'static str {
    match transport {
        Some(transport) => network_transport_label(transport),
        None => "Unknown",
    }
}

#[component]
fn ControllerConfigurationPanel(
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let mut configuring = use_signal(|| false);
    let mut transport_draft = use_signal(|| backend().local_network_transport());
    let mut saving = use_signal(|| false);
    let version = backend().local_build_version();
    let transport = backend().local_network_transport();
    let dirty = transport_draft() != transport;
    rsx! {
        div { class: "identity-block",
            h2 { "Configuration" }
            p { class: "note", "This Controller's PRNS build and whether it acts as a transport node on the mesh. Configure changes the transport setting for this app only." }
            div { class: if configuring() { "interface-toolbar editing" } else { "interface-toolbar" },
                div { class: "toolbar-track",
                    div { class: "toolbar-pane",
                        button {
                            class: "button",
                            r#type: "button",
                            onclick: move |_| {
                                transport_draft.set(backend().local_network_transport());
                                configuring.set(true);
                            },
                            "Configure"
                        }
                    }
                    div { class: "toolbar-pane",
                        button {
                            class: "button",
                            disabled: saving(),
                            r#type: "button",
                            onclick: move |_| {
                                if transport_draft() != backend().local_network_transport() {
                                    transport_draft.set(backend().local_network_transport());
                                }
                                configuring.set(false);
                            },
                            if dirty {
                                "Cancel"
                            } else {
                                "Back"
                            }
                        }
                        button {
                            class: if saving() { "button busy" } else { "button" },
                            disabled: !dirty || saving(),
                            r#type: "button",
                            aria_busy: if saving() { "true" } else { "false" },
                            onclick: move |_| {
                                let next = transport_draft();
                                if saving() || next == backend().local_network_transport() {
                                    return;
                                }
                                saving.set(true);
                                match backend().set_local_network_transport(next) {
                                    Ok(()) => {
                                        configuring.set(false);
                                        push_activity(
                                            activity_log,
                                            format!(
                                                "Set this Controller's transport to {}.",
                                                network_transport_label(next)
                                            ),
                                        );
                                    }
                                    Err(error) => push_activity(
                                        activity_log,
                                        format!("Configure transport failed: {error}"),
                                    ),
                                }
                                saving.set(false);
                            },
                            if saving() {
                                span { class: "spinner", aria_hidden: "true" }
                                "Saving"
                            } else {
                                "Save"
                            }
                        }
                    }
                }
            }
            div { class: if configuring() { "deck editing" } else { "deck" },
                div { class: "deck-track",
                    div { class: "deck-pane",
                        dl { class: "facts",
                            div { dt { "PRNS" } dd { "{version}" } }
                            div { dt { "Transport" } dd { "{network_transport_label(transport)}" } }
                        }
                    }
                    div { class: "deck-pane",
                        div { class: "edit-card",
                            h3 { "Configure This controller" }
                            dl { class: "facts",
                                div {
                                    class: if dirty { "changed" } else { "" },
                                    dt { "Transport" }
                                    dd {
                                        select {
                                            value: "{transport_draft().wire_value()}",
                                            disabled: saving(),
                                            onchange: move |event| {
                                                let Some(next) = event
                                                    .value()
                                                    .parse::<u8>()
                                                    .ok()
                                                    .and_then(RemoteControlNetworkTransport::from_wire)
                                                else {
                                                    return;
                                                };
                                                transport_draft.set(next);
                                            },
                                            for option in RemoteControlNetworkTransport::ALL {
                                                option {
                                                    value: "{option.wire_value()}",
                                                    selected: transport_draft() == option,
                                                    "{network_transport_label(option)}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn persist_target_alias(
    target_id: String,
    backend: RemoteControlBackend,
    mut target_aliases: Signal<HashMap<String, String>>,
    mut peer_aliases: Signal<HashMap<String, String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let label = backend
        .target_announce_name(&target_id)
        .unwrap_or_else(|| target_label(&target_id));
    let next = target_aliases()
        .get(&target_id)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let previous = backend
        .target_aliases()
        .get(&target_id)
        .cloned()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if next == previous {
        match &next {
            Some(alias) => {
                target_aliases.write().insert(target_id, alias.clone());
            }
            None => {
                target_aliases.write().remove(&target_id);
            }
        }
        return;
    }
    let stored = next.clone().unwrap_or_default();
    if backend.set_target_alias(&target_id, &stored).is_err() {
        return;
    }
    peer_aliases.set(backend.peer_aliases());
    let applied = backend
        .target_aliases()
        .get(&target_id)
        .cloned()
        .unwrap_or_default();
    target_aliases.write().insert(target_id, applied.clone());
    push_activity(activity_log, format!("Set alias for {label} to {applied}."));
}

fn persist_manager_alias(
    hash: String,
    backend: RemoteControlBackend,
    mut manager_aliases: Signal<HashMap<String, String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let next = manager_aliases()
        .get(&hash)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let previous = backend
        .manager_aliases()
        .get(&hash)
        .cloned()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if next == previous {
        match &next {
            Some(alias) => {
                manager_aliases.write().insert(hash, alias.clone());
            }
            None => {
                manager_aliases.write().remove(&hash);
            }
        }
        return;
    }
    let stored = next.clone().unwrap_or_default();
    if backend.set_manager_alias(&hash, &stored).is_err() {
        return;
    }
    match next {
        Some(alias) => {
            manager_aliases.write().insert(hash.clone(), alias.clone());
            push_activity(
                activity_log,
                format!("Set manager alias for {hash} to {alias}."),
            );
        }
        None => {
            manager_aliases.write().remove(&hash);
            push_activity(activity_log, format!("Cleared manager alias for {hash}."));
        }
    }
}

fn persist_sibling_alias(
    instance_hash: String,
    backend: RemoteControlBackend,
    mut sibling_aliases: Signal<HashMap<String, String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let typed = sibling_aliases()
        .get(&instance_hash)
        .cloned()
        .unwrap_or_default();
    if backend.set_sibling_alias(&instance_hash, &typed).is_err() {
        return;
    }
    sibling_aliases.set(backend.sibling_aliases());
    let alias = backend
        .sibling_aliases()
        .get(&instance_hash)
        .cloned()
        .unwrap_or_default();
    if alias.is_empty() {
        return;
    }
    push_activity(activity_log, format!("Set sibling alias to {alias}."));
}

fn persist_peer_alias(
    peer_id: String,
    label: String,
    backend: RemoteControlBackend,
    mut peer_aliases: Signal<HashMap<String, String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let next = peer_aliases()
        .get(&peer_id)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let previous = backend
        .peer_aliases()
        .get(&peer_id)
        .cloned()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if next == previous {
        match &next {
            Some(alias) => {
                peer_aliases.write().insert(peer_id, alias.clone());
            }
            None => {
                peer_aliases.write().remove(&peer_id);
            }
        }
        return;
    }
    let stored = next.clone().unwrap_or_default();
    if backend.set_peer_alias(&peer_id, &stored).is_err() {
        return;
    }
    // Clearing may restore a managed-node auto alias; refresh from backend.
    peer_aliases.set(backend.peer_aliases());
    match backend
        .peer_aliases()
        .get(&peer_id)
        .cloned()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(alias) if next.is_none() => {
            push_activity(
                activity_log,
                format!("Restored auto alias for {label} to {alias}."),
            );
        }
        Some(alias) => {
            push_activity(activity_log, format!("Set alias for {label} to {alias}."));
        }
        None => {
            push_activity(activity_log, format!("Cleared alias for {label}."));
        }
    }
}

fn push_activity(mut log: Signal<Vec<ActivityLogEntry>>, message: impl Into<String>) {
    let entry = ActivityLogEntry {
        at: format_activity_clock(std::time::SystemTime::now()),
        message: message.into(),
    };
    let mut log = log.write();
    log.insert(0, entry);
    log.truncate(ACTIVITY_LOG_LIMIT);
}

fn format_activity_clock(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

#[component]
fn ControllerIdentityCard(
    identity: Result<ControllerIdentity, BackendError>,
    clone: Option<IdentityCloneView>,
    backend: Signal<RemoteControlBackend>,
    sibling_aliases: Signal<HashMap<String, String>>,
    focused_sibling_alias: Signal<Option<String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let identity_info = use_signal(|| false);
    match identity {
        Ok(identity) => rsx! {
            div { class: "heading-row",
                h2 { "Identity management" }
                InfoHint {
                    label: "About Identity management".to_string(),
                    open: identity_info,
                }
            }
            if identity_info() {
                p { class: "note", "This PRNS Controller has two identities: Controller and Sibling. Sibling PRNS Controllers (a process by which different PRNS Controllers can manage the same nodes) share Controller only. The Sibling identity is unique to this phone or computer." }
            }
            div { class: "identity-block",
                p { class: "identity-kicker", "Shared" }
                h4 { "Controller" }
                p { class: "twisty-address",
                    strong { "Controller identity hash: " }
                    "{identity.operator_hash}"
                }
                if identity_info() {
                    p { class: "note", "What pairing writes onto a managed node's controller allow-list. Sibling Controllers share this Controller identity. Adopting a sibling replaces that install's Controller identity with this one. The sibling Controller can then manage the same nodes as this one." }
                    p { class: "note", "For embedded devices that are configured for management during flashing, the full Controller key (not just the hash above) will be needed." }
                }
                p { class: "allow-list-key",
                    strong { "Controller identity key: " }
                    "{identity.allow_list_key}"
                }
                if identity_info() {
                    p { class: "note", "Controller secret, minted on first launch if missing:" }
                    p { class: "twisty-address", "{identity.operator_secret_path}" }
                    p { class: "note", "Override the data directory with HOPSPOT_RC_DATA_DIR. Deleting the Controller file mints a new Controller; already-paired targets will not recognize it until you pair again." }
                }
            }
            div { class: "identity-block",
                p { class: "identity-kicker", "This install" }
                h4 { "Sibling" }
                p { class: "twisty-address",
                    strong { "Sibling identity hash: " }
                    "{identity.instance_hash}"
                }
                if identity_info() {
                    p { class: "note", "Unique to this phone or computer. Even after being adopted, sibling Controllers maintain their unique Sibling identifiers. Node management data will sync between siblings using these unique identifiers." }
                    p { class: "note", "Sibling secret:" }
                    p { class: "twisty-address", "{identity.instance_secret_path}" }
                    p { class: "note", "Override the data directory with HOPSPOT_RC_DATA_DIR. Deleting the Sibling file mints a new install id and drops sibling sync." }
                }
            }
            SiblingControllersPanel {
                clone,
                backend,
                sibling_aliases,
                focused_sibling_alias,
                activity_log,
            }
        },
        Err(error) => rsx! {
            div { class: "heading-row",
                h2 { "Identity management" }
                InfoHint {
                    label: "About Identity management".to_string(),
                    open: identity_info,
                }
            }
            p { class: "error", "Controller identity is unavailable: {error}" }
        },
    }
}

#[component]
fn SiblingControllersPanel(
    clone: Option<IdentityCloneView>,
    backend: Signal<RemoteControlBackend>,
    sibling_aliases: Signal<HashMap<String, String>>,
    focused_sibling_alias: Signal<Option<String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let mut configuring = use_signal(|| false);
    let adopt_alias = use_signal(String::new);
    let in_progress = clone.as_ref().is_some_and(|clone| clone.in_progress);
    let open = configuring() || in_progress;
    rsx! {
        div { class: "identity-block",
            h3 { "Sibling Controllers" }
            p { class: "note", "Other PRNS Controller installs that share this Controller identity. Managed node information sync uses their Sibling ids. Configure adopts a new sibling over USB." }
            if let Some(clone) = clone.as_ref() {
                if let Some(notice) = clone.notice.as_deref() {
                    p { class: "note", "{notice}" }
                }
                if let Some(error) = clone.error.as_deref() {
                    p { class: "error", "{error}" }
                }
            }
            div { class: if open { "interface-toolbar editing" } else { "interface-toolbar" },
                div { class: "toolbar-track",
                    div { class: "toolbar-pane",
                        button {
                            class: "button",
                            r#type: "button",
                            onclick: move |_| configuring.set(true),
                            "Configure"
                        }
                    }
                    div { class: "toolbar-pane",
                        button {
                            class: "button",
                            r#type: "button",
                            disabled: in_progress,
                            onclick: move |_| configuring.set(false),
                            "Back"
                        }
                    }
                }
            }
            div { class: if open { "deck editing" } else { "deck" },
                div { class: "deck-track",
                    div { class: "deck-pane",
                        {sibling_controller_list(
                            clone.as_ref(),
                            sibling_aliases,
                            backend,
                            activity_log,
                        )}
                    }
                    div { class: "deck-pane",
                        {sibling_controller_editor(
                            clone.as_ref(),
                            backend,
                            sibling_aliases,
                            focused_sibling_alias,
                            activity_log,
                        )}
                        h4 { "Adopt a sibling" }
                        p { class: "note", "Adoption copies the Operator secret, managed nodes, pairing names, aliases, and sibling pins from this adopting Controller onto the other adoptee Controller. Does not copy Instance. Currently it works only over USB: stop BLE, Auto Wi-Fi, and TCP. Adoptee Controller initiates the process by pressing \"I am up for adoption\". The adopting Controller then presses \"Adopt\". Visually verify the six digit adoption code, then \"Approve\". Adoptee Controller must quit and reopen after adoption for the new Operator id to take effect." }
                        {clone_identity_controls(clone, backend, adopt_alias, activity_log)}
                    }
                }
            }
        }
    }
}

const THIS_CONTROLLER_ALIAS: &str = "This controller";

fn sibling_display_alias(
    sibling: &SiblingControllerView,
    aliases: &HashMap<String, String>,
) -> String {
    if sibling.is_self {
        THIS_CONTROLLER_ALIAS.to_string()
    } else {
        aliases
            .get(&sibling.instance_hash)
            .cloned()
            .unwrap_or_else(|| sibling.alias.clone())
    }
}

fn sibling_presence_label(sibling: &SiblingControllerView) -> &'static str {
    if sibling.heard {
        "Online"
    } else {
        "Offline"
    }
}

fn sibling_presence_class(sibling: &SiblingControllerView) -> &'static str {
    if sibling.heard {
        "status online"
    } else {
        "status"
    }
}

fn sibling_controller_list(
    clone: Option<&IdentityCloneView>,
    sibling_aliases: Signal<HashMap<String, String>>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let Some(clone) = clone else {
        return rsx! { p { class: "note", "Siblings are unavailable until the controller node starts." } };
    };
    if clone.siblings.is_empty() {
        return rsx! { p { class: "note", "No sibling controllers yet." } };
    }
    rsx! {
        div { class: "sibling-list",
            for sibling in clone.siblings.iter() {
                div {
                    key: "{sibling.instance_hash}",
                    class: "sibling-row",
                    div { class: "twisty-title",
                        span { class: "sibling-alias",
                            "{sibling_display_alias(sibling, &sibling_aliases())}"
                        }
                        if !sibling.is_self {
                            span { class: sibling_presence_class(sibling),
                                "{sibling_presence_label(sibling)}"
                            }
                        }
                    }
                    p { class: "twisty-address", "{sibling.instance_hash}" }
                    if sibling.is_self {
                        div { class: "actions",
                            button {
                                class: "button",
                                r#type: "button",
                                onclick: move |_| {
                                    spawn(async move {
                                        match backend().announce_this_controller().await {
                                            Ok(()) => push_activity(
                                                activity_log,
                                                "Announced this controller's roster-sync destination.",
                                            ),
                                            Err(error) => push_activity(
                                                activity_log,
                                                format!("Announce failed: {error}"),
                                            ),
                                        }
                                    });
                                },
                                "Announce"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn sibling_controller_editor(
    clone: Option<&IdentityCloneView>,
    backend: Signal<RemoteControlBackend>,
    mut sibling_aliases: Signal<HashMap<String, String>>,
    focused_sibling_alias: Signal<Option<String>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let Some(clone) = clone else {
        return rsx! {};
    };
    if clone.siblings.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "sibling-list",
            for sibling in clone.siblings.iter() {
                div {
                    key: "{sibling.instance_hash}",
                    class: "sibling-row",
                    div { class: "twisty-title",
                        if sibling.is_self {
                            span { class: "sibling-alias", "{THIS_CONTROLLER_ALIAS}" }
                        } else {
                            SiblingAliasField {
                                instance_hash: sibling.instance_hash.clone(),
                                fallback_alias: sibling.alias.clone(),
                                sibling_aliases,
                                focused_sibling_alias,
                                backend,
                                activity_log,
                            }
                            span { class: sibling_presence_class(sibling),
                                "{sibling_presence_label(sibling)}"
                            }
                        }
                        if !sibling.is_self {
                            button {
                                class: "button danger whitelist-remove",
                                r#type: "button",
                                onclick: {
                                    let instance_hash = sibling.instance_hash.clone();
                                    let alias = sibling_aliases()
                                        .get(&sibling.instance_hash)
                                        .cloned()
                                        .unwrap_or_else(|| sibling.alias.clone());
                                    move |_| {
                                        if backend().forget_sibling(&instance_hash).is_err() {
                                            return;
                                        }
                                        sibling_aliases.write().remove(&instance_hash);
                                        push_activity(
                                            activity_log,
                                            format!("Removed sibling {alias}."),
                                        );
                                    }
                                },
                                "Remove"
                            }
                        }
                    }
                    p { class: "twisty-address", "{sibling.instance_hash}" }
                    if sibling.is_self {
                        div { class: "actions",
                            button {
                                class: "button",
                                r#type: "button",
                                onclick: move |_| {
                                    spawn(async move {
                                        match backend().announce_this_controller().await {
                                            Ok(()) => push_activity(
                                                activity_log,
                                                "Announced this controller's roster-sync destination.",
                                            ),
                                            Err(error) => push_activity(
                                                activity_log,
                                                format!("Announce failed: {error}"),
                                            ),
                                        }
                                    });
                                },
                                "Announce"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SiblingAliasField(
    instance_hash: String,
    fallback_alias: String,
    mut sibling_aliases: Signal<HashMap<String, String>>,
    mut focused_sibling_alias: Signal<Option<String>>,
    backend: Signal<RemoteControlBackend>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let mut draft = use_signal({
        let instance_hash = instance_hash.clone();
        let fallback_alias = fallback_alias.clone();
        move || {
            sibling_aliases()
                .get(&instance_hash)
                .cloned()
                .unwrap_or(fallback_alias)
        }
    });
    let focused = focused_sibling_alias().as_deref() == Some(instance_hash.as_str());
    if !focused {
        let stored = sibling_aliases()
            .get(&instance_hash)
            .cloned()
            .unwrap_or_else(|| fallback_alias.clone());
        if draft() != stored {
            draft.set(stored);
        }
    }
    rsx! {
        input {
            class: "peer-alias",
            r#type: "text",
            placeholder: "Alias",
            value: "{draft()}",
            aria_label: format!("Alias for {instance_hash}"),
            onfocus: {
                let instance_hash = instance_hash.clone();
                move |_| focused_sibling_alias.set(Some(instance_hash.clone()))
            },
            oninput: {
                let instance_hash = instance_hash.clone();
                move |event| {
                    let value = event.value();
                    draft.set(value.clone());
                    if value.is_empty() {
                        sibling_aliases.write().remove(&instance_hash);
                    } else {
                        sibling_aliases.write().insert(instance_hash.clone(), value);
                    }
                }
            },
            onblur: {
                let instance_hash = instance_hash.clone();
                move |_| {
                    focused_sibling_alias.set(None);
                    persist_sibling_alias(
                        instance_hash.clone(),
                        backend(),
                        sibling_aliases,
                        activity_log,
                    );
                    draft.set(
                        sibling_aliases()
                            .get(&instance_hash)
                            .cloned()
                            .unwrap_or_default(),
                    );
                }
            },
            onkeydown: {
                let instance_hash = instance_hash.clone();
                move |event| {
                    if event.key() == Key::Enter {
                        persist_sibling_alias(
                            instance_hash.clone(),
                            backend(),
                            sibling_aliases,
                            activity_log,
                        );
                    }
                }
            },
        }
    }
}

fn clone_identity_controls(
    clone: Option<IdentityCloneView>,
    backend: Signal<RemoteControlBackend>,
    mut adopt_alias: Signal<String>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) -> Element {
    let Some(clone) = clone else {
        return rsx! { p { class: "note", "Sibling adopt is unavailable until the controller node starts." } };
    };
    let locality_ok = clone.locality.allows_clone();
    rsx! {
        p { class: if locality_ok { "note" } else { "error" }, "{clone.locality.operator_message()}" }
        label {
            "Alias used to refer to the sibling (e.g.: Laptop, phone1)"
            input {
                r#type: "text",
                placeholder: "{backend().suggested_sibling_alias()}",
                value: "{adopt_alias()}",
                aria_label: "Alias used to refer to the sibling (e.g.: Laptop, phone1)",
                oninput: move |event| adopt_alias.set(event.value()),
            }
        }
        if let Some(source) = clone.incoming_source.as_deref() {
            p { class: "note", "The other install (Instance {source}) wants to copy its Operator onto this app. This Instance stays." }
            if let Some(digits) = clone.incoming_digits.as_deref() {
                p { class: "twisty-address", "Confirm {digits}" }
            }
        }
        if let Some(dest) = clone.outgoing_dest.as_deref() {
            p { class: "note", "Pushing this Operator to the other install (Instance {dest}). That Instance stays." }
            if let Some(digits) = clone.outgoing_digits.as_deref() {
                p { class: "twisty-address", "Confirm {digits}" }
            }
        }
        if !clone.in_progress {
            div { class: "actions",
                button {
                    class: "button",
                    disabled: !locality_ok,
                    onclick: move |_| {
                        match backend().arm_clone_dest() {
                            Ok(()) => push_activity(activity_log, "Waiting to be adopted over USB."),
                            Err(error) => push_activity(activity_log, error.to_string()),
                        }
                    },
                    "I am up for adoption"
                }
                button {
                    class: "button",
                    disabled: !locality_ok,
                    onclick: move |_| {
                        spawn(async move {
                            match backend().start_clone_source().await {
                                Ok(()) => push_activity(activity_log, "Adoption announce sent on USB."),
                                Err(error) => push_activity(activity_log, error.to_string()),
                            }
                        });
                    },
                    "Adopt"
                }
            }
        }
        if clone.dest_waiting && clone.incoming_source.is_none() {
            p { class: "note", "Waiting to be adopted. The other app should press Adopt." }
        } else if clone.dest_accepted {
            p { class: "note", "Waiting for the other app to Approve." }
        } else if clone.source_accepted {
            p { class: "note", "Adoption approved. Waiting for the other app to finish." }
        }
        div { class: "actions",
            if clone.incoming_source.is_some() && !clone.dest_accepted {
                button {
                    class: "button",
                    onclick: move |_| {
                        spawn(async move {
                            match backend().accept_clone_dest(&adopt_alias()).await {
                                Ok(()) => push_activity(activity_log, "Adopted the other Operator. Quit and reopen this app."),
                                Err(error) => push_activity(activity_log, error.to_string()),
                            }
                        });
                    },
                    "Approve"
                }
            }
            if clone.outgoing_digits.is_some() && !clone.source_accepted {
                button {
                    class: "button",
                    onclick: move |_| {
                        spawn(async move {
                            match backend().accept_clone_source(&adopt_alias()).await {
                                Ok(()) => push_activity(activity_log, "Approved adoption."),
                                Err(error) => push_activity(activity_log, error.to_string()),
                            }
                        });
                    },
                    "Approve"
                }
            }
            if clone.in_progress {
                button {
                    class: "button",
                    onclick: move |_| {
                        match backend().cancel_clone() {
                            Ok(()) => push_activity(activity_log, "Cancelled sibling adopt."),
                            Err(error) => push_activity(activity_log, error.to_string()),
                        }
                    },
                    "Cancel adopt"
                }
            }
        }
    }
}

fn accordion_item_class(awaiting: bool, expanded: bool) -> &'static str {
    match (awaiting, expanded) {
        (true, true) => "accordion-item awaiting open",
        (true, false) => "accordion-item awaiting",
        (false, true) => "accordion-item open",
        (false, false) => "accordion-item",
    }
}

fn pairing_in_progress(pairing: &PairingState) -> bool {
    matches!(
        pairing,
        PairingState::Connecting
            | PairingState::AwaitingConfirmation { .. }
            | PairingState::WaitingForTarget
    )
}

fn status_label(status: &TargetStatus) -> &'static str {
    match status {
        TargetStatus::Online => "Online",
        TargetStatus::Sleeping => "Sleeping",
        TargetStatus::Offline => "Offline",
        TargetStatus::AwaitingPairing => "Awaiting pairing",
    }
}

fn status_class(status: &TargetStatus) -> &'static str {
    match status {
        TargetStatus::Online => "status online",
        TargetStatus::Sleeping => "status sleeping",
        TargetStatus::Offline => "status",
        TargetStatus::AwaitingPairing => "status awaiting",
    }
}

fn format_byte_count(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

fn format_rate(bytes_per_sec: u32) -> String {
    format!("{}/s", format_byte_count(u64::from(bytes_per_sec)))
}

fn start_stop_label(power: &InterfacePower) -> &'static str {
    match power {
        InterfacePower::On => "Stop",
        InterfacePower::Off => "Start",
    }
}

fn toggle_interface_power(
    host: InterfaceHost,
    scope: String,
    interface_id: String,
    backend: RemoteControlBackend,
    mut interfaces_by_target: Signal<HashMap<String, LoadedInterfaces>>,
    activity_log: Signal<Vec<ActivityLogEntry>>,
) {
    let next_power = interfaces_by_target()
        .get(&scope)
        .and_then(LoadedInterfaces::entries)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.id == interface_id)
                .map(|item| match item.power {
                    InterfacePower::On => InterfacePower::Off,
                    InterfacePower::Off => InterfacePower::On,
                })
        });
    let Some(next_power) = next_power else {
        return;
    };
    let name = interfaces_by_target()
        .get(&scope)
        .and_then(LoadedInterfaces::entries)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.id == interface_id)
                .map(|item| item.name.clone())
        })
        .unwrap_or_else(|| interface_id.clone());
    let host_label = match &host {
        InterfaceHost::Controller => "this controller".to_string(),
        InterfaceHost::Target(_) => "a target".to_string(),
    };
    spawn(async move {
        let result = match &host {
            InterfaceHost::Controller => {
                backend.set_local_interface_power(&interface_id, next_power.clone())
            }
            InterfaceHost::Target(target) => {
                backend
                    .set_interface_power(target, &interface_id, next_power.clone())
                    .await
            }
        };
        match result {
            Ok(()) => {
                if let Some(items) = interfaces_by_target
                    .write()
                    .get_mut(&scope)
                    .and_then(LoadedInterfaces::entries_mut)
                {
                    items
                        .iter_mut()
                        .filter(|item| item.id == interface_id)
                        .for_each(|item| item.power = next_power.clone());
                }
                push_activity(
                    activity_log,
                    match next_power {
                        InterfacePower::On => format!("Started {name} on {host_label}."),
                        InterfacePower::Off => format!("Stopped {name} on {host_label}."),
                    },
                );
            }
            Err(error) => push_activity(
                activity_log,
                format!("Power update failed for {name}: {error}"),
            ),
        }
    });
}

fn format_approve_failure(error: &impl std::fmt::Display) -> String {
    let detail = error.to_string();
    if detail.contains("Timeout") && detail.contains("AwaitingCompletion") {
        return "Pairing approval failed: timed out waiting for Completed after mutual approve. Approve Yes on the OLED and Approve here after the six digits appear (either order). If that fails in about 13 seconds, this Controller build is stale — quit and reopen the rebuilt app. If it waits nearly two minutes then fails, the attempt window ended or the board never returned Completed; open a fresh Pair remote and try again.".to_string();
    }
    if detail.contains("LinkClosed") || detail.contains("NoActiveAttempt") {
        return "Pairing approval failed: the pairing link closed during mutual approve (often because the target stalled while writing the grant). Rebuild this app and reflash the Hopspot target so Completed is sent before flash persist, then open a new Pair remote and try again.".to_string();
    }
    format!("Pairing approval failed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::aliases_preserving;
    use std::collections::HashMap;

    #[test]
    fn aliases_preserving_keeps_the_focused_typed_value() {
        let stored = HashMap::from([
            ("a".into(), "Sibling 2".into()),
            ("b".into(), "Laptop".into()),
        ]);
        let current = HashMap::from([("a".into(), "Phone".into())]);
        let next = aliases_preserving(stored, &current, Some("a"));
        assert_eq!(next.get("a").map(String::as_str), Some("Phone"));
        assert_eq!(next.get("b").map(String::as_str), Some("Laptop"));
    }

    #[test]
    fn aliases_preserving_keeps_a_cleared_focused_alias() {
        let stored = HashMap::from([("a".into(), "Sibling 2".into())]);
        let current = HashMap::new();
        let next = aliases_preserving(stored, &current, Some("a"));
        assert!(!next.contains_key("a"));
    }

    #[test]
    fn aliases_preserving_replaces_when_nothing_is_focused() {
        let stored = HashMap::from([("a".into(), "Sibling 2".into())]);
        let current = HashMap::from([("a".into(), "Phone".into())]);
        let next = aliases_preserving(stored, &current, None);
        assert_eq!(next.get("a").map(String::as_str), Some("Sibling 2"));
    }
}
