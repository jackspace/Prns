use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use personal_rns::identity::in_memory::InMemoryNodeIdentity;
use personal_rns::identity::{
    IdentityPublicKeys, IdentitySigner, IDENTITY_PUBLIC_KEY_LEN, IDENTITY_SECRET_KEY_LEN,
};
use personal_rns::interfaces::lora::{
    ModemPreset, RadioProfile, RegulatoryRegion as Region, DEFAULT_915_PROFILE,
};
use personal_rns::remote_control::{
    encode_remote_control_vault_page, parse_controller_public_keys,
    RemoteControlControllerAuthority, RemoteControlRequestSet, RemoteControlTargetAccess,
    RemoteControlTargetIdentity,
};
use prns_flash_manifest::{
    board_catalog, BoardAvailability, BoardBuild, NrfSerialDfuBuild, TcpClientEndpoint, Transport,
    Uf2BootloaderIdentity, UsbVendorProductId, WifiCredentials,
};
use serialport::SerialPortType;

const INFO_UF2_READ_LIMIT: u64 = 4096;
const ESPRESSIF_NATIVE_USB_VENDOR_ID: u16 = 0x303A;
static UF2_VOLUME_PROBE_HELD: AtomicBool = AtomicBool::new(false);

struct Uf2VolumeProbeHold;

impl Uf2VolumeProbeHold {
    fn acquire() -> Self {
        UF2_VOLUME_PROBE_HELD.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for Uf2VolumeProbeHold {
    fn drop(&mut self) {
        UF2_VOLUME_PROBE_HELD.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn uf2_volume_probes_held() -> bool {
    UF2_VOLUME_PROBE_HELD.load(Ordering::SeqCst)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashStage {
    Enroll,
    Prepare,
    Write,
    Complete,
}

impl FlashStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Enroll => "Generating enrollment",
            Self::Prepare => "Preparing firmware",
            Self::Write => "Writing device",
            Self::Complete => "Complete",
        }
    }

    pub fn stages(enrollable: bool) -> &'static [Self] {
        if enrollable {
            &[Self::Enroll, Self::Prepare, Self::Write, Self::Complete]
        } else {
            &[Self::Prepare, Self::Write, Self::Complete]
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashStageState {
    Pending,
    Current,
    Done,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashRunOutcome {
    Running,
    Failed,
    Succeeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolledFlashTarget {
    pub id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashProgress {
    pub enrollable: bool,
    pub stage: FlashStage,
    pub detail: String,
    pub write_percent: Option<u8>,
    pub outcome: FlashRunOutcome,
    pub enrolled: Option<EnrolledFlashTarget>,
}

impl FlashProgress {
    pub fn running(enrollable: bool, stage: FlashStage, detail: impl Into<String>) -> Self {
        Self {
            enrollable,
            stage,
            detail: detail.into(),
            write_percent: None,
            outcome: FlashRunOutcome::Running,
            enrolled: None,
        }
    }

    pub fn manage_offer(&self) -> Option<&EnrolledFlashTarget> {
        match self.outcome {
            FlashRunOutcome::Succeeded => self.enrolled.as_ref(),
            FlashRunOutcome::Running | FlashRunOutcome::Failed => None,
        }
    }

    pub fn stage_state(&self, stage: FlashStage) -> FlashStageState {
        if !self.enrollable && stage == FlashStage::Enroll {
            return FlashStageState::Skipped;
        }
        let reached = stage_index(self.stage, self.enrollable);
        let this = stage_index(stage, self.enrollable);
        match self.outcome {
            FlashRunOutcome::Succeeded => FlashStageState::Done,
            FlashRunOutcome::Failed if this == reached => FlashStageState::Failed,
            FlashRunOutcome::Failed if this < reached => FlashStageState::Done,
            FlashRunOutcome::Failed => FlashStageState::Pending,
            FlashRunOutcome::Running if this < reached => FlashStageState::Done,
            FlashRunOutcome::Running if this == reached => FlashStageState::Current,
            FlashRunOutcome::Running => FlashStageState::Pending,
        }
    }
}

fn stage_index(stage: FlashStage, enrollable: bool) -> usize {
    FlashStage::stages(enrollable)
        .iter()
        .position(|candidate| *candidate == stage)
        .unwrap_or(0)
}

fn progress_from_hopspot_event(enrollable: bool, event: &HopspotEvent) -> FlashProgress {
    let stage = stage_from_hopspot_phase(&event.phase);
    let mut progress = FlashProgress::running(
        enrollable,
        stage,
        event
            .message
            .clone()
            .unwrap_or_else(|| stage.label().to_string()),
    );
    if event.event == "error" || event.phase == "failed" {
        progress.outcome = FlashRunOutcome::Failed;
    }
    if let (Some(current), Some(total)) = (event.current, event.total) {
        if total > 0 {
            progress.write_percent = Some(current.saturating_mul(100).saturating_div(total) as u8);
        }
    }
    progress
}

fn stage_from_hopspot_phase(phase: &str) -> FlashStage {
    match phase {
        "building"
        | "resolving_release"
        | "validating_manifest"
        | "downloading"
        | "verifying_artifacts"
        | "publishing_cache"
        | "artifact_ready" => FlashStage::Prepare,
        "ready" | "requesting_port" | "connecting" | "verifying_target" | "writing"
        | "verifying_flash" | "resetting" | "monitor" | "complete" => FlashStage::Write,
        "failed" => FlashStage::Write,
        _ => FlashStage::Prepare,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HopspotEvent {
    event: String,
    phase: String,
    message: Option<String>,
    current: Option<u64>,
    total: Option<u64>,
}

fn parse_hopspot_event(line: &str) -> Option<HopspotEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(HopspotEvent {
        event: value.get("event")?.as_str()?.to_string(),
        phase: value.get("phase")?.as_str()?.to_string(),
        message: value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        current: value.get("current").and_then(serde_json::Value::as_u64),
        total: value.get("total").and_then(serde_json::Value::as_u64),
    })
}

fn hopspot_failure_detail(captured: &str, last_json_error: Option<String>) -> String {
    let json = last_json_error.filter(|text| !text.trim().is_empty());
    let diagnostics = summarize_compiler_diagnostics(captured);
    match (json, diagnostics) {
        (Some(json), Some(diagnostics)) if is_opaque_build_exit(&json) => {
            truncate_ui_detail(&format!("{json}\n{diagnostics}"))
        }
        (Some(json), Some(diagnostics)) => {
            if json.contains(diagnostics.lines().next().unwrap_or("")) {
                truncate_ui_detail(&json)
            } else {
                truncate_ui_detail(&format!("{json}\n{diagnostics}"))
            }
        }
        (Some(json), None) => truncate_ui_detail(&json),
        (None, Some(diagnostics)) => diagnostics,
        (None, None) => captured
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty() && !is_cargo_noise(line))
            .unwrap_or("hopspot-flash failed")
            .to_string(),
    }
}

fn is_opaque_build_exit(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("exited with")
}

fn summarize_compiler_diagnostics(captured: &str) -> Option<String> {
    let lines: Vec<&str> = captured.lines().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("error:") || trimmed.starts_with("error[") {
            out.push(trimmed.to_string());
            if let Some(next) = lines.get(index + 1).map(|line| line.trim_start()) {
                if next.starts_with("-->") {
                    out.push(next.to_string());
                    index += 1;
                }
            }
        }
        index += 1;
    }
    if out.is_empty() {
        None
    } else {
        Some(truncate_ui_detail(&out.join("\n")))
    }
}

fn truncate_ui_detail(text: &str) -> String {
    const MAX_CHARS: usize = 1_200;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let mut truncated = trimmed.chars().take(MAX_CHARS).collect::<String>();
    truncated.push('…');
    truncated
}

fn is_cargo_noise(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Compiling ")
        || trimmed.starts_with("Finished ")
        || trimmed.starts_with("Running `")
        || trimmed.starts_with("Running ")
        || trimmed.starts_with("Blocking ")
        || trimmed.contains("profile [")
        || trimmed.starts_with("Downloading ")
        || trimmed.starts_with("Downloaded ")
        || trimmed.starts_with("Checking ")
        || trimmed.starts_with("Installing ")
}

/// Drain hopspot-flash stderr on a side thread so a full pipe cannot deadlock stdout progress.
fn spawn_stderr_collector(
    stderr: Option<std::process::ChildStderr>,
) -> Option<std::thread::JoinHandle<String>> {
    stderr.map(|stderr| {
        std::thread::spawn(move || {
            let mut collected = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                // Keep a copy in the Controller process log for operators watching the terminal.
                eprintln!("hopspot-flash: {line}");
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
            }
            collected
        })
    })
}

fn join_stderr_collector(handle: Option<std::thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UsbIdentity {
    vendor_id: u16,
    product_id: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogBoard {
    pub slug: String,
    pub display_name: String,
    pub silicon: String,
    pub transport: &'static str,
    pub availability: &'static str,
    pub interfaces: Vec<String>,
    pub preparation_profile: String,
    pub enrollable: bool,
    pub supports_wifi: bool,
    pub supports_tcp: bool,
    pub has_lora: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashDraft {
    pub wifi_ssid: String,
    pub wifi_password: String,
    pub tcp_client: String,
    pub lora_region: Region,
    pub lora_preset: ModemPreset,
    pub enrol_for_management: bool,
}

impl Default for FlashDraft {
    fn default() -> Self {
        Self {
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            tcp_client: String::new(),
            lora_region: match DEFAULT_915_PROFILE.region() {
                personal_rns::interfaces::lora::SubGRegion::Regulated(region) => region,
                personal_rns::interfaces::lora::SubGRegion::Custom => Region::Us915,
            },
            lora_preset: ModemPreset::matching(DEFAULT_915_PROFILE.modulation())
                .unwrap_or(ModemPreset::MediumFast),
            enrol_for_management: true,
        }
    }
}

impl FlashDraft {
    pub fn lora_profile(&self) -> RadioProfile {
        let regioned = crate::edits::apply_lora_region(DEFAULT_915_PROFILE, self.lora_region);
        match ModemPreset::matching(DEFAULT_915_PROFILE.modulation()) {
            Some(default_preset) if self.lora_preset == default_preset => regioned,
            None if self.lora_preset == ModemPreset::MediumFast => regioned,
            _ => crate::edits::apply_lora_preset(regioned, self.lora_preset),
        }
    }

    pub fn custom_lora_profile(&self) -> Option<RadioProfile> {
        let profile = self.lora_profile();
        (profile != DEFAULT_915_PROFILE).then_some(profile)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiFlashPlan {
    Preserve,
    Configure {
        ssid: String,
        password: String,
        tcp_client: Option<String>,
    },
}

pub fn wifi_flash_plan(
    draft: &FlashDraft,
    supports_wifi: bool,
    supports_tcp: bool,
) -> Result<WifiFlashPlan, FlashError> {
    let ssid = draft.wifi_ssid.trim();
    let tcp = draft.tcp_client.trim();
    if ssid.is_empty() && tcp.is_empty() {
        return Ok(WifiFlashPlan::Preserve);
    }
    if !supports_wifi {
        return Err(FlashError::Message(
            "this board does not have a Wi-Fi provisioning slot".to_string(),
        ));
    }
    if ssid.is_empty() {
        return Err(FlashError::Message(
            "a station SSID is required to write Wi-Fi or TCP settings".to_string(),
        ));
    }
    let credentials = WifiCredentials {
        ssid: ssid.to_string(),
        password: draft.wifi_password.clone(),
    };
    credentials
        .validate()
        .map_err(|error| FlashError::Message(error.to_string()))?;
    let tcp_client = if tcp.is_empty() {
        None
    } else {
        if !supports_tcp {
            return Err(FlashError::Message(
                "this board does not have room for TCP client provisioning".to_string(),
            ));
        }
        TcpClientEndpoint::parse(tcp).map_err(|error| FlashError::Message(error.to_string()))?;
        Some(tcp.to_string())
    };
    Ok(WifiFlashPlan::Configure {
        ssid: credentials.ssid,
        password: credentials.password,
        tcp_client,
    })
}

fn apply_wifi_plan(command: &mut Command, plan: &WifiFlashPlan) {
    let WifiFlashPlan::Configure {
        ssid,
        password,
        tcp_client,
    } = plan
    else {
        return;
    };
    command
        .arg("--wifi")
        .arg("configure")
        .arg("--wifi-from-env");
    command.env("HOPSPOT_WIFI_SSID", ssid);
    command.env("HOPSPOT_WIFI_PASSWORD", password);
    if let Some(target) = tcp_client {
        command.arg("--tcp-client").arg(target);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FlashError {
    #[error("{0}")]
    Message(String),
}

pub fn catalog_boards() -> Result<Vec<CatalogBoard>, FlashError> {
    let catalog = board_catalog().map_err(|error| FlashError::Message(error.to_string()))?;
    Ok(catalog
        .boards
        .iter()
        .map(|board| CatalogBoard {
            slug: board.slug.clone(),
            display_name: board.display_name.clone(),
            silicon: board.silicon.clone(),
            transport: match board.transport {
                Transport::EspSerial => "USB serial",
                Transport::Uf2MassStorage => "UF2 drive",
                Transport::NrfSerialDfu => "Nordic serial DFU",
            },
            availability: match board.availability {
                BoardAvailability::Shipping => "Shipping",
                BoardAvailability::Qualification => "Qualification",
            },
            interfaces: board.interfaces.clone(),
            preparation_profile: board.preparation_profile.clone(),
            enrollable: identity_offset(&board.slug).is_some()
                && board.transport != Transport::NrfSerialDfu,
            supports_wifi: board.supports_provisioning(),
            supports_tcp: board.supports_tcp_client_provisioning(),
            has_lora: board
                .interfaces
                .iter()
                .any(|interface| interface.eq_ignore_ascii_case("LoRa")),
        })
        .collect())
}

pub fn detect_probable_slugs() -> Vec<String> {
    let uf2 = if uf2_volume_probes_held() {
        Vec::new()
    } else {
        connected_uf2_identities()
    };
    slugs_matching_devices(&uf2, &connected_usb_identities())
}

fn slugs_matching_devices(uf2: &[Uf2BootloaderIdentity], usb: &[UsbIdentity]) -> Vec<String> {
    let Ok(catalog) = board_catalog() else {
        return Vec::new();
    };
    catalog
        .boards
        .iter()
        .filter(|board| board_looks_connected(board, uf2, usb))
        .map(|board| board.slug.clone())
        .collect()
}

fn board_looks_connected(
    board: &prns_flash_manifest::BoardCatalogEntry,
    uf2: &[Uf2BootloaderIdentity],
    usb: &[UsbIdentity],
) -> bool {
    match &board.build {
        BoardBuild::Uf2(build) => build
            .board_identity
            .validated()
            .is_ok_and(|rule| uf2.iter().any(|seen| seen.matches_board(&rule))),
        BoardBuild::NrfSerialDfu(build) => {
            nrf_probe_usb_identities(build)
                .iter()
                .any(|wanted| usb.contains(wanted))
                || build
                    .recovery
                    .board_identity
                    .validated()
                    .is_ok_and(|rule| uf2.iter().any(|seen| seen.matches_board(&rule)))
        }
        BoardBuild::Esp(_) => usb
            .iter()
            .any(|seen| seen.vendor_id == ESPRESSIF_NATIVE_USB_VENDOR_ID),
    }
}

fn nrf_probe_usb_identities(build: &NrfSerialDfuBuild) -> Vec<UsbIdentity> {
    [
        &build.serial.touch_application_and_bootloader.usb,
        &build.serial.recovery_bootloader.usb,
    ]
    .into_iter()
    .filter_map(parse_usb_identity)
    .collect()
}

fn parse_usb_identity(usb: &UsbVendorProductId) -> Option<UsbIdentity> {
    Some(UsbIdentity {
        vendor_id: parse_hex_u16(&usb.vendor_id)?,
        product_id: parse_hex_u16(&usb.product_id)?,
    })
}

fn parse_hex_u16(value: &str) -> Option<u16> {
    let digits = value.strip_prefix("0x")?;
    u16::from_str_radix(digits, 16).ok()
}

fn connected_usb_identities() -> Vec<UsbIdentity> {
    serialport::available_ports()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(usb) => Some(UsbIdentity {
                vendor_id: usb.vid,
                product_id: usb.pid,
            }),
            _ => None,
        })
        .collect()
}

fn connected_uf2_identities() -> Vec<Uf2BootloaderIdentity> {
    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    if let Some(path) = std::env::var_os("HOPSPOT_TECHOBOOT") {
        push_uf2_identity(&mut identities, &mut seen, PathBuf::from(path));
    }
    for root in ["/Volumes", "/mnt", "/media", "/run/media"] {
        scan_uf2_root(Path::new(root), 2, &mut identities, &mut seen);
    }
    #[cfg(windows)]
    for letter in b'D'..=b'Z' {
        push_uf2_identity(
            &mut identities,
            &mut seen,
            PathBuf::from(format!("{}:\\", letter as char)),
        );
    }
    identities
}

fn scan_uf2_root(
    root: &Path,
    depth: usize,
    identities: &mut Vec<Uf2BootloaderIdentity>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            push_uf2_identity(identities, seen, path.clone());
            scan_uf2_root(&path, depth - 1, identities, seen);
        }
    }
}

fn push_uf2_identity(
    identities: &mut Vec<Uf2BootloaderIdentity>,
    seen: &mut HashSet<PathBuf>,
    path: PathBuf,
) {
    if !seen.insert(path.clone()) {
        return;
    }
    if let Some(identity) = read_uf2_identity(&path) {
        identities.push(identity);
    }
}

fn read_uf2_identity(path: &Path) -> Option<Uf2BootloaderIdentity> {
    let info = path.join("INFO_UF2.TXT");
    let file = fs::File::open(info).ok()?;
    let mut bytes = Vec::new();
    file.take(INFO_UF2_READ_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    Uf2BootloaderIdentity::parse(&bytes).ok()
}

pub fn identity_offset(slug: &str) -> Option<u32> {
    match slug {
        "heltec-v4" | "heltec-v4-r8" | "heltec-e290" => Some(0x00E7_D000),
        "t-beam-supreme" => Some(0x0067_D000),
        "xiao-esp32-c6" => Some(0x003D_F000),
        "t-echo" => Some(0x000E_2000),
        "t114" | "t096" => Some(0x000E_1000),
        "mesh-tower-v2" => Some(0x000E_2000),
        "t1000-e" => Some(0x000E_9000),
        _ => None,
    }
}

pub fn preparation_steps(profile: &str) -> &'static [&'static str] {
    match profile {
        "esp-usb-boot" => &[
            "Use a USB data cable and close other serial monitors using the board.",
            "Put the board in download mode if automatic reset fails: hold BOOT, tap RESET, release BOOT.",
            "Leave this app’s BLE on if you want the node reachable immediately after flash.",
        ],
        "techo-uf2" => &[
            "Double-reset the T-Echo until the TECHOBOOT drive appears.",
            "Keep the USB data cable connected until the drive disappears after flash.",
        ],
        "t114-uf2" => &[
            "Double-reset the board until the HT-n5262 drive appears.",
            "Keep the USB data cable connected until the drive disappears after flash.",
        ],
        "t096-uf2" => &[
            "Double-reset the board until the HT-n5262G drive appears.",
            "Keep the USB data cable connected until the drive disappears after flash.",
        ],
        "t1000e-nrf-dfu" => &[
            "Connect the T1000-E over USB. This app flashes firmware but cannot write the enrollment vault over serial DFU yet; pair the device after it boots.",
        ],
        _ => &["Prepare the board using the cataloged bootloader sequence for this target."],
    }
}

pub struct Enrollment {
    pub vault_page: [u8; personal_rns::remote_control::REMOTE_CONTROL_IDENTITY_VAULT_PAGE_LEN],
    pub access: RemoteControlTargetAccess,
    pub target_id: String,
}

pub fn mint_enrollment(allow_list_key: &str) -> Result<Enrollment, FlashError> {
    let controller = parse_allow_list_key(allow_list_key)?;
    let mut target_secret = [0u8; IDENTITY_SECRET_KEY_LEN];
    getrandom::getrandom(&mut target_secret).map_err(|error| {
        FlashError::Message(format!("could not mint a target identity: {error}"))
    })?;
    let identity = InMemoryNodeIdentity::from_secret_key_bytes(&target_secret);
    let public = IdentityPublicKeys {
        encryption: identity.encryption_public_key(),
        signing: identity.signing_public_key(),
    };
    if public.identity_hash() == controller.identity_hash() {
        return Err(FlashError::Message(
            "the minted target identity collided with this Operator; try again".to_string(),
        ));
    }
    let vault_page = encode_remote_control_vault_page(&target_secret, controller.public_keys())
        .map_err(|_| FlashError::Message("could not encode the enrollment vault".to_string()))?;
    let access = RemoteControlTargetAccess::new(
        RemoteControlTargetIdentity::new(public),
        RemoteControlControllerAuthority::Administrator,
        RemoteControlRequestSet::all(),
    )
    .map_err(|_| FlashError::Message("the enrollment request set is empty".to_string()))?;
    Ok(Enrollment {
        vault_page,
        target_id: encode_hex(access.target().identity_hash().as_bytes()),
        access,
    })
}

pub fn flash_enrolled_board(
    slug: &str,
    vault_page: Option<&[u8]>,
    enrollable: bool,
    wifi: &WifiFlashPlan,
    on_progress: impl Fn(FlashProgress),
) -> Result<(), FlashError> {
    let local_build_escape = std::env::var_os("PRNS_CONTROLLER_FLASH_LOCAL_BUILD").is_some();
    let invocation = if local_build_escape {
        FlashInvocation {
            slug,
            local_build: true,
            candidate: None,
            developer_artifacts: None,
        }
    } else {
        let image = crate::image_catalog::current_image(slug)
            .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?
            .ok_or_else(|| {
                FlashError::Message(format!(
                    "no catalog image for {slug} — Download, Import, or Build firmware first"
                ))
            })?;
        match image.provenance {
            crate::image_catalog::ImageProvenance::Published => {
                let image_dir = crate::image_catalog::current_image_dir(slug)
                    .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?
                    .ok_or_else(|| {
                        FlashError::Message(format!("catalog image directory missing for {slug}"))
                    })?;
                FlashInvocation {
                    slug,
                    local_build: false,
                    candidate: Some(image_dir),
                    developer_artifacts: None,
                }
            }
            // hopspot-flash dropped --developer-artifacts; local catalog images rebuild
            // from the checkout so Controller Flash stays on current tree firmware.
            crate::image_catalog::ImageProvenance::LocalBuild => FlashInvocation {
                slug,
                local_build: true,
                candidate: None,
                developer_artifacts: None,
            },
            crate::image_catalog::ImageProvenance::Imported
            | crate::image_catalog::ImageProvenance::Bundled => {
                return Err(FlashError::Message(format!(
                    "unsigned {slug} catalog image ({}) cannot be flashed: hopspot-flash no longer accepts --developer-artifacts. Choose a published tip, or Build then Flash a local image",
                    match image.provenance {
                        crate::image_catalog::ImageProvenance::Imported => "imported",
                        crate::image_catalog::ImageProvenance::Bundled => "bundled",
                        _ => "unsigned",
                    }
                )));
            }
        }
    };
    let vault_path = vault_page
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            let offset = identity_offset(slug).ok_or_else(|| {
                FlashError::Message(format!(
                    "{slug} does not have a Remote Control identity page"
                ))
            })?;
            let path = std::env::temp_dir().join(format!("prns-rc-vault-{slug}.bin"));
            std::fs::write(&path, bytes).map_err(|error| {
                FlashError::Message(format!("could not write the enrollment vault: {error}"))
            })?;
            Ok::<_, FlashError>((path, offset))
        })
        .transpose()?;
    let _hold = Uf2VolumeProbeHold::acquire();
    let mut command = hopspot_flash_command(invocation)?;
    if let Some((path, offset)) = &vault_path {
        command
            .arg("--rc-vault")
            .arg(path)
            .arg("--rc-vault-offset")
            .arg(format!("0x{offset:x}"));
    }
    apply_wifi_plan(&mut command, wifi);
    eprintln!("controller flash: {}", command_argv(&command));
    on_progress(FlashProgress::running(
        enrollable,
        FlashStage::Prepare,
        format!("Starting hopspot-flash for {slug}…"),
    ));
    let mut child = command
        .spawn()
        .map_err(|error| FlashError::Message(format!("could not start hopspot-flash: {error}")))?;
    let stderr_collector = spawn_stderr_collector(child.stderr.take());
    let mut last_json_error = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some(event) = parse_hopspot_event(&line) else {
                continue;
            };
            if event.event == "error" || event.phase == "failed" {
                last_json_error = event.message.clone().or(last_json_error);
            }
            on_progress(progress_from_hopspot_event(enrollable, &event));
        }
    }
    let status = child
        .wait()
        .map_err(|error| FlashError::Message(format!("hopspot-flash did not finish: {error}")))?;
    let captured = join_stderr_collector(stderr_collector);
    if let Some((path, _)) = vault_path {
        let _ = std::fs::remove_file(path);
    }
    if status.success() {
        return Ok(());
    }
    Err(FlashError::Message(hopspot_failure_detail(
        &captured,
        last_json_error,
    )))
}

/// Build developer firmware for `slug` and register it into the Controller catalog.
pub fn build_local_board(
    slug: &str,
    on_progress: impl Fn(String),
) -> Result<crate::image_catalog::CatalogImage, FlashError> {
    let repo = repo_root().ok_or_else(|| {
        FlashError::Message(
            "Build requires a Personal Reticulum checkout (open Controller from the repo or set cwd)"
                .to_string(),
        )
    })?;
    let _ = crate::image_catalog::ensure_catalog()
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
    let mut command = hopspot_flash_build_command(slug)?;
    eprintln!("controller build: {}", command_argv(&command));
    on_progress(format!("Building local firmware for {slug}…"));
    let mut child = command
        .spawn()
        .map_err(|error| FlashError::Message(format!("could not start hopspot-flash: {error}")))?;
    let stderr_collector = spawn_stderr_collector(child.stderr.take());
    let mut artifact_dir = None;
    let mut last_error = None;
    let mut stdout_capture = String::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if !stdout_capture.is_empty() {
                stdout_capture.push('\n');
            }
            stdout_capture.push_str(&line);
            if let Some(path) = line.strip_prefix("artifact directory: ") {
                artifact_dir = Some(PathBuf::from(path.trim()));
                on_progress(format!("Artifacts at {}", path.trim()));
            } else if !line.trim().is_empty() {
                on_progress(line.clone());
            }
            if let Some(event) = parse_hopspot_event(&line) {
                if let Some(message) = &event.message {
                    on_progress(message.clone());
                }
                if event.event == "error" || event.phase == "failed" {
                    last_error = event.message.clone().or(last_error);
                }
            }
        }
    }
    let status = child.wait().map_err(|error| {
        FlashError::Message(format!("hopspot-flash build did not finish: {error}"))
    })?;
    let stderr = join_stderr_collector(stderr_collector);
    let captured = if stderr.is_empty() {
        stdout_capture
    } else if stdout_capture.is_empty() {
        stderr
    } else {
        format!("{stdout_capture}\n{stderr}")
    };
    if !status.success() {
        return Err(FlashError::Message(hopspot_failure_detail(
            &captured, last_error,
        )));
    }
    let artifact_dir = artifact_dir.ok_or_else(|| {
        FlashError::Message("build succeeded but did not report an artifact directory".into())
    })?;
    let version = version_from_board_artifact_dir(&artifact_dir).ok_or_else(|| {
        FlashError::Message(format!(
            "could not determine build version from {}",
            artifact_dir.display()
        ))
    })?;
    let git_sha = git_head_sha(&repo).unwrap_or_else(|| "unknown".to_string());
    crate::image_catalog::register_local_image(slug, &version, &git_sha, Some(&repo), &artifact_dir)
        .map_err(|error| {
            FlashError::Message(format!("could not register local catalog image: {error}"))
        })
}

/// Native picker for a board-artifact zip, or (if cancelled) an unzipped folder.
pub fn pick_board_import_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import board firmware (.zip)")
        .add_filter("Board artifact zip", &["zip"])
        .pick_file()
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("Import board artifact folder")
                .pick_folder()
        })
}

/// Import a shared board-artifact zip or folder into the Controller catalog for `slug`.
pub fn import_board_image(
    slug: &str,
    path: &Path,
) -> Result<crate::image_catalog::CatalogImage, FlashError> {
    let _ = crate::image_catalog::ensure_catalog()
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
    let (board_dir, cleanup) = resolve_import_board_dir(path)?;
    let result = (|| {
        let (board_slug, version) = read_import_target_identity(&board_dir)?;
        if board_slug != slug {
            return Err(FlashError::Message(format!(
                "artifact is for board {board_slug}, but Flash selection is {slug}"
            )));
        }
        crate::image_catalog::register_imported_image(slug, &version, &board_dir).map_err(|error| {
            FlashError::Message(format!(
                "could not register imported catalog image: {error}"
            ))
        })
    })();
    if let Some(dir) = cleanup {
        let _ = fs::remove_dir_all(dir);
    }
    result
}

/// Native save picker for a shareable board-artifact zip.
pub fn pick_board_export_path(slug: &str, version: &str) -> Option<PathBuf> {
    let suggested = format!("{slug}-{version}.zip");
    rfd::FileDialog::new()
        .set_title("Export board firmware (.zip)")
        .set_file_name(&suggested)
        .add_filter("Board artifact zip", &["zip"])
        .save_file()
}

/// Zip the current catalog image for `slug` into an Import-compatible archive.
pub fn export_board_image(slug: &str, dest_zip: &Path) -> Result<PathBuf, FlashError> {
    let _ = crate::image_catalog::ensure_catalog()
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
    let image = crate::image_catalog::current_image(slug)
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?
        .ok_or_else(|| {
            FlashError::Message(format!(
                "no catalog image for {slug} — Import, Build, or select a tip first"
            ))
        })?;
    let image_dir = crate::image_catalog::current_image_dir(slug)
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?
        .ok_or_else(|| {
            FlashError::Message(format!("catalog image directory missing for {slug}"))
        })?;
    let board_dir = locate_board_artifact_dir(&image_dir).map_err(|_| {
        FlashError::Message(format!(
            "catalog image for {slug} has no target.json and cannot be exported — Import, Build, or Bundled images export; published tips need a local board artifact"
        ))
    })?;
    write_board_artifact_zip(&board_dir, slug, dest_zip)?;
    let _ = image;
    Ok(dest_zip.to_path_buf())
}

fn write_board_artifact_zip(
    board_dir: &Path,
    slug: &str,
    dest_zip: &Path,
) -> Result<(), FlashError> {
    if let Some(parent) = dest_zip.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            FlashError::Message(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    let file = fs::File::create(dest_zip).map_err(|error| {
        FlashError::Message(format!("could not create {}: {error}", dest_zip.display()))
    })?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let prefix = format!("{slug}/");
    zip.add_directory(&prefix, options).map_err(|error| {
        FlashError::Message(format!("could not write zip directory entry: {error}"))
    })?;
    add_dir_to_zip(&mut zip, board_dir, &prefix, options)?;
    zip.finish()
        .map_err(|error| FlashError::Message(format!("could not finish export zip: {error}")))?;
    Ok(())
}

fn add_dir_to_zip(
    zip: &mut zip::ZipWriter<fs::File>,
    src: &Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<(), FlashError> {
    let entries = fs::read_dir(src).map_err(|error| {
        FlashError::Message(format!("could not read {}: {error}", src.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            FlashError::Message(format!("could not read export contents: {error}"))
        })?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        // Skip catalog bookkeeping files; Import only needs the flash artifacts.
        if matches!(
            name_str,
            "release.json" | "local-build.json" | "bundled.json" | "current" | "index.json"
        ) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            FlashError::Message(format!("could not read {}: {error}", path.display()))
        })?;
        if file_type.is_dir() {
            let nested = format!("{prefix}{name_str}/");
            zip.add_directory(&nested, options).map_err(|error| {
                FlashError::Message(format!("could not write zip directory {nested}: {error}"))
            })?;
            add_dir_to_zip(zip, &path, &nested, options)?;
        } else if file_type.is_file() {
            let entry_name = format!("{prefix}{name_str}");
            zip.start_file(&entry_name, options).map_err(|error| {
                FlashError::Message(format!("could not start zip entry {entry_name}: {error}"))
            })?;
            let bytes = fs::read(&path).map_err(|error| {
                FlashError::Message(format!("could not read {}: {error}", path.display()))
            })?;
            use std::io::Write;
            zip.write_all(&bytes).map_err(|error| {
                FlashError::Message(format!("could not write zip entry {entry_name}: {error}"))
            })?;
        }
    }
    Ok(())
}

fn resolve_import_board_dir(path: &Path) -> Result<(PathBuf, Option<PathBuf>), FlashError> {
    if path.is_dir() {
        let board_dir = locate_board_artifact_dir(path)?;
        return Ok((board_dir, None));
    }
    let is_zip = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !is_zip {
        return Err(FlashError::Message(format!(
            "import path must be a .zip or a board artifact folder: {}",
            path.display()
        )));
    }
    let extract_root = std::env::temp_dir().join(format!(
        "prns-controller-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&extract_root).map_err(|error| {
        FlashError::Message(format!("could not create import extract dir: {error}"))
    })?;
    if let Err(error) = extract_zip_archive(path, &extract_root) {
        let _ = fs::remove_dir_all(&extract_root);
        return Err(error);
    }
    match locate_board_artifact_dir(&extract_root) {
        Ok(board_dir) => Ok((board_dir, Some(extract_root))),
        Err(error) => {
            let _ = fs::remove_dir_all(&extract_root);
            Err(error)
        }
    }
}

fn extract_zip_archive(zip_path: &Path, dest: &Path) -> Result<(), FlashError> {
    let file = fs::File::open(zip_path).map_err(|error| {
        FlashError::Message(format!("could not open {}: {error}", zip_path.display()))
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        FlashError::Message(format!("invalid zip {}: {error}", zip_path.display()))
    })?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| FlashError::Message(format!("could not read zip entry: {error}")))?;
        let Some(enclosed) = entry.enclosed_name().map(|name| name.to_path_buf()) else {
            continue;
        };
        let out_path = dest.join(&enclosed);
        if entry.name().ends_with('/') {
            fs::create_dir_all(&out_path).map_err(|error| {
                FlashError::Message(format!("could not create {}: {error}", out_path.display()))
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                FlashError::Message(format!("could not create {}: {error}", parent.display()))
            })?;
        }
        let mut out = fs::File::create(&out_path).map_err(|error| {
            FlashError::Message(format!("could not write {}: {error}", out_path.display()))
        })?;
        std::io::copy(&mut entry, &mut out).map_err(|error| {
            FlashError::Message(format!("could not extract {}: {error}", out_path.display()))
        })?;
    }
    Ok(())
}

fn locate_board_artifact_dir(root: &Path) -> Result<PathBuf, FlashError> {
    if root.join("target.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let mut matches = Vec::new();
    let entries = fs::read_dir(root).map_err(|error| {
        FlashError::Message(format!("could not read {}: {error}", root.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            FlashError::Message(format!("could not read import contents: {error}"))
        })?;
        let path = entry.path();
        if path.is_dir() && path.join("target.json").is_file() {
            matches.push(path);
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(FlashError::Message(
            "import package has no target.json (expected at zip root or in one top-level folder)"
                .into(),
        )),
        _ => Err(FlashError::Message(
            "import package is ambiguous: multiple folders contain target.json".into(),
        )),
    }
}

fn read_import_target_identity(board_dir: &Path) -> Result<(String, String), FlashError> {
    let target_path = board_dir.join("target.json");
    let bytes = fs::read(&target_path).map_err(|error| {
        FlashError::Message(format!("could not read {}: {error}", target_path.display()))
    })?;
    let wire: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| FlashError::Message(format!("invalid target.json: {error}")))?;
    let board_slug = wire
        .get("board_slug")
        .and_then(|value| value.as_str())
        .filter(|slug| !slug.is_empty())
        .ok_or_else(|| FlashError::Message("target.json is missing board_slug".into()))?
        .to_owned();
    let version = version_from_import_target(&wire)
        .or_else(|| version_from_board_artifact_dir(board_dir))
        .ok_or_else(|| {
            FlashError::Message(
                "could not determine firmware version from target.json artifact paths".into(),
            )
        })?;
    Ok((board_slug, version))
}

fn version_from_import_target(wire: &serde_json::Value) -> Option<String> {
    let path = wire
        .get("parts")
        .and_then(|parts| parts.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("path"))
        .and_then(|path| path.as_str())
        .or_else(|| {
            wire.get("variants")
                .and_then(|variants| variants.as_array())
                .and_then(|variants| variants.first())
                .and_then(|variant| variant.get("path"))
                .and_then(|path| path.as_str())
        })
        .or_else(|| {
            wire.get("nrf_serial_dfu")
                .and_then(|dfu| dfu.get("application"))
                .and_then(|app| app.get("path"))
                .and_then(|path| path.as_str())
        })?;
    // firmware/hopspot/{board}/{version}/{filename}
    let mut segments = path.split('/');
    let _firmware = segments.next()?;
    let _hopspot = segments.next()?;
    let _board = segments.next()?;
    segments.next().map(str::to_owned)
}

/// True when Controller can see a Personal Reticulum checkout for Build.
pub fn checkout_available() -> bool {
    repo_root().is_some()
}

fn version_from_board_artifact_dir(dir: &Path) -> Option<String> {
    // …/firmware/hopspot/{slug}/{version}
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn git_head_sha(repo: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let sha = sha.trim();
    (!sha.is_empty()).then(|| sha.to_owned())
}

/// Download the published stable image for `slug` into the Controller catalog.
pub fn download_published_board(
    slug: &str,
    channel: &str,
    on_progress: impl Fn(String),
) -> Result<crate::image_catalog::CatalogImage, FlashError> {
    let _ = crate::image_catalog::ensure_catalog()
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
    let staging = std::env::temp_dir().join(format!(
        "prns-controller-fetch-{slug}-{}",
        std::process::id()
    ));
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    let mut command = hopspot_flash_fetch_command(slug, channel, &staging)?;
    eprintln!("controller fetch: {}", command_argv(&command));
    on_progress(format!(
        "Downloading published {channel} firmware for {slug}…"
    ));
    let mut child = command
        .spawn()
        .map_err(|error| FlashError::Message(format!("could not start hopspot-flash: {error}")))?;
    let stderr_collector = spawn_stderr_collector(child.stderr.take());
    let mut last_json_error = None;
    let mut version = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some(event) = parse_hopspot_event(&line) else {
                continue;
            };
            if let Some(message) = &event.message {
                on_progress(message.clone());
                if let Some(found) = version_from_fetch_message(message) {
                    version = Some(found);
                }
            }
            if event.event == "error" || event.phase == "failed" {
                last_json_error = event.message.clone().or(last_json_error);
            }
        }
    }
    let status = child.wait().map_err(|error| {
        FlashError::Message(format!("hopspot-flash fetch did not finish: {error}"))
    })?;
    let captured = join_stderr_collector(stderr_collector);
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(FlashError::Message(hopspot_failure_detail(
            &captured,
            last_json_error,
        )));
    }
    let version = version
        .or_else(|| version_from_candidate_dir(&staging))
        .ok_or_else(|| {
            FlashError::Message("fetch succeeded but did not report a release version".to_string())
        })?;
    let image = crate::image_catalog::register_published_image(slug, &version, channel, &staging)
        .map_err(|error| {
        FlashError::Message(format!("could not register catalog image: {error}"))
    })?;
    let _ = fs::remove_dir_all(&staging);
    Ok(image)
}

/// Query the signed channel for the published version (no firmware download).
pub fn check_published_release(
    channel: &str,
    board: Option<&str>,
) -> Result<PublishedReleaseCheck, FlashError> {
    let mut command = hopspot_flash_check_command(channel, board)?;
    eprintln!("controller check: {}", command_argv(&command));
    let output = command
        .output()
        .map_err(|error| FlashError::Message(format!("could not start hopspot-flash: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(FlashError::Message(hopspot_failure_detail(
            &format!("{stdout}\n{stderr}"),
            None,
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut check = parse_published_check_json(&stdout)
        .ok_or_else(|| FlashError::Message("hopspot-flash check returned no usable JSON".into()))?;
    // Prefer the requested channel name so tip slots stay ordered/keyed correctly.
    if check.channel != channel {
        check.channel = channel.to_owned();
    }
    Ok(check)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PublishedReleaseCheck {
    pub channel: String,
    pub version: String,
    pub boards: Vec<PublishedBoardAvailability>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PublishedBoardAvailability {
    pub slug: String,
    pub available: bool,
}

impl PublishedReleaseCheck {
    pub fn board_available(&self, slug: &str) -> Option<bool> {
        self.boards
            .iter()
            .find(|board| board.slug == slug)
            .map(|board| board.available)
    }
}

/// Stable tip first, then preview — what the Flash image dropdown offers from the CDN.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishedChannelTips {
    pub stable: Option<PublishedReleaseCheck>,
    pub preview: Option<PublishedReleaseCheck>,
}

impl PublishedChannelTips {
    pub fn is_empty(&self) -> bool {
        self.stable.is_none() && self.preview.is_none()
    }

    pub fn summary_note(&self) -> Option<String> {
        match (&self.stable, &self.preview) {
            (Some(stable), Some(preview)) => Some(format!(
                "Published stable · v{} · preview · v{}",
                stable.version, preview.version
            )),
            (Some(stable), None) => Some(format!("Published stable · v{}", stable.version)),
            (None, Some(preview)) => Some(format!("Published preview · v{}", preview.version)),
            (None, None) => None,
        }
    }

    pub fn record_published_selection(&mut self, image: &crate::image_catalog::CatalogImage) {
        if image.provenance != crate::image_catalog::ImageProvenance::Published {
            return;
        }
        let slot = if image.channel == "preview" {
            &mut self.preview
        } else {
            &mut self.stable
        };
        let check = slot.get_or_insert_with(|| PublishedReleaseCheck {
            channel: image.channel.clone(),
            version: image.version.clone(),
            boards: Vec::new(),
        });
        check.channel = image.channel.clone();
        check.version = image.version.clone();
        if let Some(board) = check
            .boards
            .iter_mut()
            .find(|board| board.slug == image.board_slug)
        {
            board.available = true;
        } else {
            check.boards.push(PublishedBoardAvailability {
                slug: image.board_slug.clone(),
                available: true,
            });
        }
    }
}

/// Query signed stable and preview channels (preview is best-effort).
pub fn check_published_tips(board: Option<&str>) -> Result<PublishedChannelTips, FlashError> {
    let mut tips = PublishedChannelTips::default();
    let mut last_error = None;
    match check_published_release("stable", board) {
        Ok(check) => tips.stable = Some(check),
        Err(error) => last_error = Some(error),
    }
    match check_published_release("preview", board) {
        Ok(check) => tips.preview = Some(check),
        Err(error) => {
            if tips.stable.is_none() {
                last_error = Some(error);
            }
        }
    }
    if tips.is_empty() {
        return Err(last_error.unwrap_or_else(|| {
            FlashError::Message("could not reach stable or preview published channels".into())
        }));
    }
    Ok(tips)
}

/// One row in the Configure-and-flash image `<select>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagePickOption {
    PublishedTip {
        version: String,
        channel: String,
        local_image_id: Option<String>,
    },
    Catalog {
        image: crate::image_catalog::CatalogImage,
    },
}

impl ImagePickOption {
    pub fn value(&self) -> String {
        match self {
            Self::PublishedTip {
                version, channel, ..
            } => {
                format!("tip:{channel}:{version}")
            }
            Self::Catalog { image } => format!("id:{}", image.image_id),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::PublishedTip {
                version, channel, ..
            } => {
                format!("v{version} · {channel}")
            }
            Self::Catalog { image } => catalog_image_badge(Some(image)),
        }
    }
}

/// Build the image dropdown options for one board (tips + catalog entries).
pub fn image_pick_options(
    slug: &str,
    tips: Option<&PublishedChannelTips>,
) -> Result<Vec<ImagePickOption>, FlashError> {
    let locals = crate::image_catalog::list_images(slug)
        .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
    let mut tip_local_ids = Vec::new();
    let mut options = Vec::new();
    for check in tips
        .into_iter()
        .flat_map(|tips| [tips.stable.as_ref(), tips.preview.as_ref()])
        .flatten()
    {
        if check.version.is_empty() || check.channel.is_empty() {
            continue;
        }
        if check.board_available(slug) == Some(false) {
            continue;
        }
        let local_image_id = locals
            .iter()
            .find(|image| {
                image.provenance == crate::image_catalog::ImageProvenance::Published
                    && image.version == check.version
                    && image.channel == check.channel
            })
            .map(|image| image.image_id.clone());
        if let Some(id) = &local_image_id {
            tip_local_ids.push(id.clone());
        }
        options.push(ImagePickOption::PublishedTip {
            version: check.version.clone(),
            channel: check.channel.clone(),
            local_image_id,
        });
    }
    for image in locals {
        if tip_local_ids.iter().any(|id| id == &image.image_id) {
            continue;
        }
        options.push(ImagePickOption::Catalog { image });
    }
    Ok(options)
}

/// Value to bind on the `<select>` from the current catalog image.
pub fn selected_image_pick_value(
    current: Option<&crate::image_catalog::CatalogImage>,
    options: &[ImagePickOption],
) -> String {
    if let Some(current) = current {
        for option in options {
            match option {
                ImagePickOption::PublishedTip {
                    version,
                    channel,
                    local_image_id: _,
                } if current.provenance == crate::image_catalog::ImageProvenance::Published
                    && current.version == *version
                    && current.channel == *channel =>
                {
                    return option.value();
                }
                ImagePickOption::PublishedTip {
                    local_image_id: Some(id),
                    ..
                } if current.image_id == *id => {
                    return option.value();
                }
                ImagePickOption::Catalog { image } if image.image_id == current.image_id => {
                    return option.value();
                }
                _ => {}
            }
        }
        return format!("id:{}", current.image_id);
    }
    options
        .iter()
        .find(|option| matches!(option, ImagePickOption::PublishedTip { .. }))
        .map(ImagePickOption::value)
        .unwrap_or_default()
}

pub fn image_pick_option_from_value<'a>(
    options: &'a [ImagePickOption],
    value: &str,
) -> Option<&'a ImagePickOption> {
    options.iter().find(|option| option.value() == value)
}

/// Select a concrete catalog image (published tip or existing local entry).
pub fn select_image_pick(
    slug: &str,
    option: &ImagePickOption,
    on_progress: impl Fn(String),
) -> Result<crate::image_catalog::CatalogImage, FlashError> {
    match option {
        ImagePickOption::PublishedTip {
            channel,
            local_image_id: Some(image_id),
            ..
        } => {
            let _ = channel;
            crate::image_catalog::set_current(slug, image_id)
                .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
            crate::image_catalog::current_image(slug)
                .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?
                .ok_or_else(|| {
                    FlashError::Message(format!("catalog image {image_id} missing after select"))
                })
        }
        ImagePickOption::PublishedTip { channel, .. } => {
            download_published_board(slug, channel, on_progress)
        }
        ImagePickOption::Catalog { image } => {
            crate::image_catalog::set_current(slug, &image.image_id)
                .map_err(|error| FlashError::Message(format!("image catalog error: {error}")))?;
            Ok(image.clone())
        }
    }
}

/// Catalog badge for the Flash accordion (Published / Local / Imported / Bundled).
pub fn catalog_image_badge(image: Option<&crate::image_catalog::CatalogImage>) -> String {
    match image {
        Some(image) if image.provenance == crate::image_catalog::ImageProvenance::LocalBuild => {
            let sha = image
                .git_sha
                .as_deref()
                .map(|sha| {
                    let trimmed = sha.trim();
                    if trimmed.len() > 12 {
                        trimmed[..12].to_owned()
                    } else {
                        trimmed.to_owned()
                    }
                })
                .filter(|sha| !sha.is_empty())
                .unwrap_or_else(|| "unknown".to_owned());
            format!("Local build · {sha} · v{} (unsigned)", image.version)
        }
        Some(image) if image.provenance == crate::image_catalog::ImageProvenance::Imported => {
            format!("Imported · v{} (unsigned)", image.version)
        }
        Some(image) if image.provenance == crate::image_catalog::ImageProvenance::Bundled => {
            let sha = image
                .git_sha
                .as_deref()
                .map(|sha| {
                    let trimmed = sha.trim();
                    if trimmed.len() > 12 {
                        trimmed[..12].to_owned()
                    } else {
                        trimmed.to_owned()
                    }
                })
                .filter(|sha| !sha.is_empty() && sha != "unknown")
                .unwrap_or_else(|| "tree".to_owned());
            format!("Bundled · {sha} · v{} (unsigned)", image.version)
        }
        Some(image) => format!("Published · {} · v{}", image.channel, image.version),
        None => "No image — choose one under Configure and flash".to_string(),
    }
}

fn parse_published_check_json(stdout: &str) -> Option<PublishedReleaseCheck> {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(check) = serde_json::from_str::<PublishedReleaseCheck>(line) {
            if !check.version.is_empty() && !check.channel.is_empty() {
                return Some(check);
            }
        }
    }
    None
}

fn version_from_fetch_message(message: &str) -> Option<String> {
    // "Fetched stable 0.3.7 for heltec-v4-r8 …" or "Exported stable 0.3.7 candidate…"
    for (prefix, _) in [("Fetched ", 2usize), ("Exported ", 2usize)] {
        if let Some(rest) = message.strip_prefix(prefix) {
            let mut parts = rest.split_whitespace();
            let _channel = parts.next()?;
            let version = parts.next()?;
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
}

fn version_from_candidate_dir(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("VERSION")).ok()?;
    let version = text.trim();
    (!version.is_empty()).then(|| version.to_string())
}

struct FlashInvocation<'a> {
    slug: &'a str,
    local_build: bool,
    candidate: Option<PathBuf>,
    developer_artifacts: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HopspotFlashLaunch {
    Binary(PathBuf),
    Cargo { repo: PathBuf },
}

fn command_argv(command: &Command) -> String {
    std::iter::once(command.get_program().to_string_lossy().into_owned())
        .chain(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve `hopspot-flash`: `HOPSPOT_FLASH` → sidecar → PATH → optional cargo checkout.
fn resolve_hopspot_flash(allow_cargo_fallback: bool) -> Result<HopspotFlashLaunch, FlashError> {
    resolve_hopspot_flash_with(
        std::env::var_os("HOPSPOT_FLASH").map(PathBuf::from),
        current_exe_sidecar_candidates(),
        path_lookup_dirs(),
        repo_root(),
        allow_cargo_fallback,
    )
}

fn resolve_hopspot_flash_with(
    env_path: Option<PathBuf>,
    sidecar_candidates: Vec<PathBuf>,
    path_dirs: Vec<PathBuf>,
    repo: Option<PathBuf>,
    allow_cargo_fallback: bool,
) -> Result<HopspotFlashLaunch, FlashError> {
    if let Some(path) = env_path {
        if path.as_os_str().is_empty() {
            return Err(FlashError::Message(
                "HOPSPOT_FLASH is set but empty".to_string(),
            ));
        }
        if !path.is_file() {
            return Err(FlashError::Message(format!(
                "HOPSPOT_FLASH points to a missing file: {}",
                path.display()
            )));
        }
        return Ok(HopspotFlashLaunch::Binary(path));
    }
    for candidate in sidecar_candidates {
        if candidate.is_file() {
            return Ok(HopspotFlashLaunch::Binary(candidate));
        }
    }
    if let Some(path) = find_hopspot_flash_on_path(&path_dirs) {
        return Ok(HopspotFlashLaunch::Binary(path));
    }
    if allow_cargo_fallback {
        if let Some(repo) = repo {
            return Ok(HopspotFlashLaunch::Cargo { repo });
        }
    }
    Err(FlashError::Message(
        "could not find hopspot-flash; set HOPSPOT_FLASH, install it beside the Controller, put it on PATH, or run from a Personal Reticulum checkout"
            .to_string(),
    ))
}

fn current_exe_sidecar_candidates() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    sidecar_candidates_for_exe(&exe)
}

fn sidecar_candidates_for_exe(exe: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(dir) = exe.parent() else {
        return out;
    };
    out.push(dir.join(hopspot_flash_bin_name()));
    // Packaged macOS .app: Contents/MacOS/<exe> → Contents/Resources/hopspot-flash
    if dir.file_name().and_then(|name| name.to_str()) == Some("MacOS") {
        if let Some(contents) = dir.parent() {
            out.push(contents.join("Resources").join(hopspot_flash_bin_name()));
        }
    }
    out
}

fn hopspot_flash_bin_name() -> &'static str {
    if cfg!(windows) {
        "hopspot-flash.exe"
    } else {
        "hopspot-flash"
    }
}

fn path_lookup_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn find_hopspot_flash_on_path(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = hopspot_flash_bin_name();
    for dir in path_dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn hopspot_flash_base_command(allow_cargo_fallback: bool) -> Result<Command, FlashError> {
    match resolve_hopspot_flash(allow_cargo_fallback)? {
        HopspotFlashLaunch::Binary(path) => {
            let mut command = Command::new(path);
            configure_hopspot_stdio(&mut command);
            Ok(command)
        }
        HopspotFlashLaunch::Cargo { repo } => Ok(cargo_hopspot_flash_command(&repo)),
    }
}

fn cargo_hopspot_flash_command(repo: &Path) -> Command {
    let manifest = repo
        .join("personal-hopspot")
        .join("flasher")
        .join("Cargo.toml");
    let mut command = Command::new("cargo");
    // Pin the flasher crate by manifest path. Controller is often launched from the
    // nested `remote-control-desktop` workspace, where `cargo -p hopspot-flash` fails.
    command
        .arg("run")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--")
        .current_dir(repo)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_TARGET_DIR");
    configure_hopspot_stdio(&mut command);
    command
}

fn configure_hopspot_stdio(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

fn hopspot_flash_command(invocation: FlashInvocation<'_>) -> Result<Command, FlashError> {
    // Local-build flash always uses the checkout cargo path.
    let allow_cargo = true;
    let mut command = if invocation.local_build {
        let repo = repo_root().ok_or_else(|| {
            FlashError::Message(
                "PRNS_CONTROLLER_FLASH_LOCAL_BUILD requires a Personal Reticulum checkout"
                    .to_string(),
            )
        })?;
        cargo_hopspot_flash_command(&repo)
    } else {
        hopspot_flash_base_command(allow_cargo)?
    };
    command.arg("flash").arg(invocation.slug);
    if invocation.local_build {
        command.arg("--local-build");
    } else if let Some(candidate) = &invocation.candidate {
        command.arg("--candidate").arg(candidate);
    } else if invocation.developer_artifacts.is_some() {
        return Err(FlashError::Message(
            "hopspot-flash no longer accepts --developer-artifacts; use a local build or a signed --candidate".to_string(),
        ));
    }
    command.arg("--yes").arg("--json");
    Ok(command)
}

fn hopspot_flash_build_command(slug: &str) -> Result<Command, FlashError> {
    let mut command = hopspot_flash_base_command(true)?;
    command.arg("build").arg(slug);
    Ok(command)
}

fn hopspot_flash_fetch_command(
    slug: &str,
    channel: &str,
    output: &Path,
) -> Result<Command, FlashError> {
    let mut command = hopspot_flash_base_command(true)?;
    command
        .arg("fetch")
        .arg(slug)
        .arg("--channel")
        .arg(channel)
        .arg("--output")
        .arg(output)
        .arg("--json");
    Ok(command)
}

fn hopspot_flash_check_command(channel: &str, board: Option<&str>) -> Result<Command, FlashError> {
    let mut command = hopspot_flash_base_command(true)?;
    command
        .arg("check")
        .arg("--channel")
        .arg(channel)
        .arg("--json");
    if let Some(board) = board {
        command.arg("--board").arg(board);
    }
    Ok(command)
}

fn parse_allow_list_key(
    key: &str,
) -> Result<personal_rns::remote_control::RemoteControlControllerIdentity, FlashError> {
    let trimmed: String = key
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-')
        .collect();
    if trimmed.len() != IDENTITY_PUBLIC_KEY_LEN * 2 {
        return Err(FlashError::Message(
            "this install’s allow-list key is not 128 hex characters".to_string(),
        ));
    }
    let mut bytes = [0u8; IDENTITY_PUBLIC_KEY_LEN];
    for (index, pair) in trimmed.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] =
            u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16).map_err(|_| {
                FlashError::Message("this install’s allow-list key is not hex".to_string())
            })?;
    }
    parse_controller_public_keys(&bytes)
        .ok_or_else(|| FlashError::Message("this install’s allow-list key is invalid".to_string()))
}

fn repo_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    for start in candidates {
        let mut dir = start;
        loop {
            if dir
                .join("release")
                .join("flash")
                .join("boards.json")
                .is_file()
                && dir
                    .join("personal-hopspot")
                    .join("flasher")
                    .join("Cargo.toml")
                    .is_file()
            {
                return Some(dir);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_catalog::CATALOG_ENV_LOCK;

    #[test]
    fn catalog_lists_shipping_boards_and_marks_dfu_unenrollable() {
        let boards = catalog_boards().expect("the embedded catalog parses");
        assert!(boards.iter().any(|board| board.slug == "heltec-v4"));
        let t114 = boards
            .iter()
            .find(|board| board.slug == "t114")
            .expect("T114 is in the catalog");
        assert!(t114.enrollable);
        assert!(!t114.supports_wifi);
        assert!(t114.has_lora);
        assert!(boards.iter().any(|board| board.slug == "mesh-pocket-5000"));
        let tracker = boards
            .iter()
            .find(|board| board.slug == "t1000-e")
            .expect("T1000-E is in the catalog");
        assert!(!tracker.enrollable);
        let hv4 = boards
            .iter()
            .find(|board| board.slug == "heltec-v4")
            .expect("HV4 is in the catalog");
        assert!(hv4.supports_wifi);
        assert!(hv4.supports_tcp);
        assert!(hv4.has_lora);
    }

    #[test]
    fn empty_flash_form_preserves_existing_wifi() {
        let plan = wifi_flash_plan(&FlashDraft::default(), true, true).expect("empty form");
        assert_eq!(plan, WifiFlashPlan::Preserve);
    }

    #[test]
    fn station_form_configures_wifi_and_tcp() {
        let draft = FlashDraft {
            wifi_ssid: "cabin".to_string(),
            wifi_password: "secret".to_string(),
            tcp_client: "192.0.2.10:4242".to_string(),
            ..FlashDraft::default()
        };
        let plan = wifi_flash_plan(&draft, true, true).expect("station form");
        assert_eq!(
            plan,
            WifiFlashPlan::Configure {
                ssid: "cabin".to_string(),
                password: "secret".to_string(),
                tcp_client: Some("192.0.2.10:4242".to_string()),
            }
        );
    }

    #[test]
    fn tcp_without_ssid_is_rejected() {
        let draft = FlashDraft {
            tcp_client: "192.0.2.10".to_string(),
            ..FlashDraft::default()
        };
        assert!(wifi_flash_plan(&draft, true, true).is_err());
    }

    #[test]
    fn default_lora_form_does_not_need_a_post_flash_write() {
        assert_eq!(FlashDraft::default().custom_lora_profile(), None);
        let draft = FlashDraft {
            lora_region: Region::Eu868,
            ..FlashDraft::default()
        };
        let profile = draft.custom_lora_profile().expect("EU868 is custom");
        assert_eq!(
            profile.region(),
            personal_rns::interfaces::lora::SubGRegion::Regulated(Region::Eu868)
        );
    }

    #[test]
    fn a_shared_board_id_highlights_every_matching_catalog_entry() {
        let t114 = Uf2BootloaderIdentity::parse(
            [
                "UF2 Bootloader 0.9.0-2-g836c8dc-dirty",
                "Model: HT-n5262",
                "Board-ID: HT-n5262",
                "Date: Jul  9 2024",
                "SoftDevice: S140 6.1.1",
            ]
            .join("\n")
            .as_bytes(),
        )
        .expect("T114 INFO_UF2");
        let slugs = slugs_matching_devices(&[t114], &[]);
        assert_eq!(
            slugs,
            vec![
                "t114".to_string(),
                "mesh-pocket-5000".to_string(),
                "mesh-pocket-10000".to_string()
            ]
        );
    }

    #[test]
    fn t096_board_id_does_not_highlight_t114() {
        let t096 = Uf2BootloaderIdentity::parse(
            [
                "UF2 Bootloader 0.6.1-2-g1224915",
                "Model: LilyGo T-Echo",
                "Board-ID: HT-n5262G",
                "SoftDevice: S140 version 6.1.1",
                "Date: Oct 13 2021",
            ]
            .join("\n")
            .as_bytes(),
        )
        .expect("T096 INFO_UF2");
        let slugs = slugs_matching_devices(&[t096], &[]);
        assert_eq!(slugs, vec!["t096".to_string()]);
    }

    #[test]
    fn espressif_native_usb_highlights_every_esp_catalog_board() {
        let slugs = slugs_matching_devices(
            &[],
            &[UsbIdentity {
                vendor_id: ESPRESSIF_NATIVE_USB_VENDOR_ID,
                product_id: 0x1001,
            }],
        );
        assert!(slugs.contains(&"heltec-v4".to_string()));
        assert!(slugs.contains(&"heltec-v4-r8".to_string()));
        assert!(slugs.contains(&"heltec-e290".to_string()));
        assert!(slugs.contains(&"t-beam-supreme".to_string()));
        assert!(slugs.contains(&"xiao-esp32-c6".to_string()));
        assert!(!slugs.contains(&"t114".to_string()));
    }

    #[test]
    fn t1000e_bootloader_usb_highlights_only_that_board() {
        let slugs = slugs_matching_devices(
            &[],
            &[UsbIdentity {
                vendor_id: 0x2886,
                product_id: 0x0057,
            }],
        );
        assert_eq!(slugs, vec!["t1000-e".to_string()]);
    }

    #[test]
    fn hopspot_application_usb_does_not_count_as_a_flash_target() {
        let slugs = slugs_matching_devices(
            &[],
            &[UsbIdentity {
                vendor_id: 0x1209,
                product_id: 0x0001,
            }],
        );
        assert!(slugs.is_empty());
    }

    #[test]
    fn enrollment_vault_is_a_full_erase_page() {
        let mut secret = [0u8; IDENTITY_SECRET_KEY_LEN];
        secret[0] = 1;
        let identity = InMemoryNodeIdentity::from_secret_key_bytes(&secret);
        let keys = IdentityPublicKeys {
            encryption: identity.encryption_public_key(),
            signing: identity.signing_public_key(),
        };
        let enrollment =
            mint_enrollment(&encode_hex(&keys.public_key_bytes())).expect("enrollment mints");
        assert_eq!(
            enrollment.vault_page.len(),
            personal_rns::remote_control::REMOTE_CONTROL_IDENTITY_VAULT_PAGE_LEN
        );
        assert!(!enrollment.target_id.is_empty());
    }

    #[test]
    fn cargo_finished_is_not_the_flash_failure() {
        let detail = hopspot_failure_detail(
            "   Compiling hopspot-flash v0.3.7\n    Finished `release` profile [optimized + debuginfo] target(s) in 40.46s\n",
            None,
        );
        assert_eq!(detail, "hopspot-flash failed");
        let detail = hopspot_failure_detail(
            "    Finished `release` profile [optimized + debuginfo] target(s) in 40.46s\nerror: no usable serial device was found\n",
            None,
        );
        assert_eq!(detail, "error: no usable serial device was found");
        let detail = hopspot_failure_detail(
            "    Finished `release` profile [optimized + debuginfo] target(s) in 40.46s\n",
            Some("wrong chip: expected esp32s3, detected esp32c6".to_string()),
        );
        assert_eq!(detail, "wrong chip: expected esp32s3, detected esp32c6");
    }

    #[test]
    fn opaque_build_exit_includes_rustc_diagnostics() {
        let captured = "\
   Compiling personal-hopspot-esp32 v0.3.7
error[E0425]: cannot find value `peer_rssi` in this scope
  --> personal-hopspot/embedded/esp32/src/s3/mod.rs:88:20
error: could not compile `personal-hopspot-esp32` (lib) due to 1 previous error
";
        let detail = hopspot_failure_detail(
            captured,
            Some("embedded ESP cargo build exited with exit status: 101".into()),
        );
        assert!(detail.contains("exited with exit status: 101"));
        assert!(detail.contains("error[E0425]: cannot find value `peer_rssi`"));
        assert!(detail.contains("--> personal-hopspot/embedded/esp32/src/s3/mod.rs:88:20"));
        assert!(detail.contains("error: could not compile `personal-hopspot-esp32`"));
    }

    #[test]
    fn controller_flash_uses_catalog_candidate_by_default() {
        let candidate = PathBuf::from("/tmp/catalog/heltec-v4-r8/published-stable-0.3.7");
        let command = hopspot_flash_command(FlashInvocation {
            slug: "heltec-v4-r8",
            local_build: false,
            candidate: Some(candidate),
            developer_artifacts: None,
        })
        .expect("resolves hopspot-flash from checkout cargo fallback");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        // Binary path skips cargo args; cargo fallback keeps run --manifest-path …
        if command.get_program().to_string_lossy().ends_with("cargo")
            || command.get_program() == "cargo"
        {
            assert!(args.windows(2).any(|w| {
                w[0] == "--candidate" && w[1] == "/tmp/catalog/heltec-v4-r8/published-stable-0.3.7"
            }));
            assert!(args.contains(&"flash".to_string()));
            assert!(args.contains(&"heltec-v4-r8".to_string()));
            assert!(args.contains(&"--yes".to_string()));
            assert!(args.contains(&"--json".to_string()));
            assert!(command
                .get_envs()
                .any(|(key, value)| { key == "CARGO_TARGET_DIR" && value.is_none() }));
        } else {
            assert_eq!(
                args,
                [
                    "flash",
                    "heltec-v4-r8",
                    "--candidate",
                    "/tmp/catalog/heltec-v4-r8/published-stable-0.3.7",
                    "--yes",
                    "--json",
                ]
            );
        }
    }

    #[test]
    fn controller_flash_local_build_escape_hatch_keeps_compile_flag() {
        let command = hopspot_flash_command(FlashInvocation {
            slug: "mesh-tower-v2",
            local_build: true,
            candidate: None,
            developer_artifacts: None,
        })
        .expect("local build requires checkout");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--local-build".to_string()));
        assert!(args.contains(&"mesh-tower-v2".to_string()));
        assert_eq!(command.get_program(), "cargo");
    }

    #[test]
    fn controller_flash_rejects_removed_developer_artifacts_flag() {
        let artifacts = PathBuf::from("/tmp/catalog/heltec-v4-r8/local-0.3.7-abcdef012345");
        let error = hopspot_flash_command(FlashInvocation {
            slug: "heltec-v4-r8",
            local_build: false,
            candidate: None,
            developer_artifacts: Some(artifacts),
        })
        .expect_err("developer-artifacts must fail closed");
        assert!(
            error
                .to_string()
                .contains("no longer accepts --developer-artifacts"),
            "{error}"
        );
    }

    #[test]
    fn controller_flash_local_catalog_uses_local_build() {
        let command = hopspot_flash_command(FlashInvocation {
            slug: "heltec-v4-r8",
            local_build: true,
            candidate: None,
            developer_artifacts: None,
        })
        .expect("local build requires checkout");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--local-build".to_string()));
        assert!(!args.iter().any(|arg| arg == "--candidate"));
        assert!(!args.iter().any(|arg| arg == "--developer-artifacts"));
    }

    #[test]
    fn hopspot_flash_resolution_prefers_env_then_sidecar_then_path_then_cargo() {
        let root =
            std::env::temp_dir().join(format!("prns-hopspot-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).expect("bin");
        fs::create_dir_all(root.join("sidecar")).expect("sidecar");
        fs::create_dir_all(root.join("path")).expect("path");
        let env_bin = root.join("bin").join(hopspot_flash_bin_name());
        let sidecar_bin = root.join("sidecar").join(hopspot_flash_bin_name());
        let path_bin = root.join("path").join(hopspot_flash_bin_name());
        fs::write(&env_bin, b"env").expect("env bin");
        fs::write(&sidecar_bin, b"sidecar").expect("sidecar bin");
        fs::write(&path_bin, b"path").expect("path bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&env_bin, &sidecar_bin, &path_bin] {
                let mut perms = fs::metadata(path).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(path, perms).unwrap();
            }
        }

        let resolved = resolve_hopspot_flash_with(
            Some(env_bin.clone()),
            vec![sidecar_bin.clone()],
            vec![root.join("path")],
            None,
            true,
        )
        .expect("env wins");
        assert_eq!(resolved, HopspotFlashLaunch::Binary(env_bin));

        let resolved = resolve_hopspot_flash_with(
            None,
            vec![sidecar_bin.clone()],
            vec![root.join("path")],
            None,
            true,
        )
        .expect("sidecar wins");
        assert_eq!(resolved, HopspotFlashLaunch::Binary(sidecar_bin));

        let resolved = resolve_hopspot_flash_with(
            None,
            vec![root.join("sidecar").join("missing")],
            vec![root.join("path")],
            None,
            true,
        )
        .expect("path wins");
        assert_eq!(resolved, HopspotFlashLaunch::Binary(path_bin));

        let repo = root.join("repo");
        fs::create_dir_all(repo.join("personal-hopspot").join("flasher")).unwrap();
        fs::write(
            repo.join("personal-hopspot")
                .join("flasher")
                .join("Cargo.toml"),
            b"[package]\nname=\"hopspot-flash\"\n",
        )
        .unwrap();
        fs::create_dir_all(repo.join("release").join("flash")).unwrap();
        fs::write(
            repo.join("release").join("flash").join("boards.json"),
            b"[]",
        )
        .unwrap();
        let resolved = resolve_hopspot_flash_with(None, vec![], vec![], Some(repo.clone()), true)
            .expect("cargo fallback");
        assert_eq!(resolved, HopspotFlashLaunch::Cargo { repo });

        assert!(resolve_hopspot_flash_with(None, vec![], vec![], None, true).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn macos_app_sidecar_looks_in_resources() {
        let exe = Path::new("/Applications/PRNS Controller.app/Contents/MacOS/prns-controller");
        let candidates = sidecar_candidates_for_exe(exe);
        assert!(candidates.iter().any(|path| {
            path.ends_with("Contents/Resources/hopspot-flash")
                || path.ends_with("Contents/Resources/hopspot-flash.exe")
        }));
        assert!(candidates.iter().any(|path| {
            path.ends_with("Contents/MacOS/hopspot-flash")
                || path.ends_with("Contents/MacOS/hopspot-flash.exe")
        }));
    }

    #[test]
    fn portable_win_linux_sidecar_is_beside_the_exe() {
        // Use forward-slash layout paths so the test is host-OS independent;
        // packaging places hopspot-flash next to the Controller binary.
        let win_exe = Path::new("/opt/PRNS-Controller/personal-hopspot-remote-control-desktop.exe");
        let win_candidates = sidecar_candidates_for_exe(win_exe);
        assert!(
            win_candidates.iter().any(|path| {
                path.ends_with("PRNS-Controller/hopspot-flash.exe")
                    || path.ends_with("PRNS-Controller/hopspot-flash")
            }),
            "windows portable layout should resolve hopspot-flash next to the Controller exe; got {win_candidates:?}"
        );
        assert!(
            !win_candidates
                .iter()
                .any(|path| path.components().any(|c| c.as_os_str() == "Resources")),
            "flat Win/Linux packages do not use Contents/Resources"
        );

        let linux_exe = Path::new("/opt/PRNS-Controller/personal-hopspot-remote-control-desktop");
        let linux_candidates = sidecar_candidates_for_exe(linux_exe);
        assert!(
            linux_candidates.iter().any(|path| {
                path.ends_with("PRNS-Controller/hopspot-flash")
                    || path.ends_with("PRNS-Controller/hopspot-flash.exe")
            }),
            "linux portable layout should resolve hopspot-flash next to the Controller exe; got {linux_candidates:?}"
        );
        assert_eq!(linux_candidates.len(), 1);
    }

    #[test]
    fn catalog_image_badge_labels_provenance() {
        let published = crate::image_catalog::CatalogImage {
            board_slug: "heltec-v4-r8".into(),
            image_id: "id".into(),
            created_at: "now".into(),
            provenance: crate::image_catalog::ImageProvenance::Published,
            channel: "stable".into(),
            version: "0.3.7".into(),
            manifest_sha256: "abc".into(),
            complete: true,
            git_sha: None,
            worktree_path: None,
        };
        assert_eq!(
            catalog_image_badge(Some(&published)),
            "Published · stable · v0.3.7"
        );
        let local = crate::image_catalog::CatalogImage {
            provenance: crate::image_catalog::ImageProvenance::LocalBuild,
            channel: "local".into(),
            git_sha: Some("abcdef0123456789".into()),
            ..published.clone()
        };
        assert_eq!(
            catalog_image_badge(Some(&local)),
            "Local build · abcdef012345 · v0.3.7 (unsigned)"
        );
        let imported = crate::image_catalog::CatalogImage {
            provenance: crate::image_catalog::ImageProvenance::Imported,
            channel: "import".into(),
            git_sha: None,
            ..published.clone()
        };
        assert_eq!(
            catalog_image_badge(Some(&imported)),
            "Imported · v0.3.7 (unsigned)"
        );
        let bundled = crate::image_catalog::CatalogImage {
            provenance: crate::image_catalog::ImageProvenance::Bundled,
            channel: "bundled".into(),
            git_sha: Some("abcdef0123456789".into()),
            ..published
        };
        assert_eq!(
            catalog_image_badge(Some(&bundled)),
            "Bundled · abcdef012345 · v0.3.7 (unsigned)"
        );
    }

    #[test]
    fn image_pick_options_merges_tips_and_locals() {
        let _guard = CATALOG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = std::env::temp_dir().join(format!(
            "prns-image-pick-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("PRNS_CONTROLLER_IMAGES", &root);

        let tips = PublishedChannelTips {
            stable: Some(PublishedReleaseCheck {
                channel: "stable".into(),
                version: "0.3.7".into(),
                boards: vec![PublishedBoardAvailability {
                    slug: "heltec-v4-r8".into(),
                    available: true,
                }],
            }),
            preview: Some(PublishedReleaseCheck {
                channel: "preview".into(),
                version: "0.3.8-dev.1".into(),
                boards: vec![PublishedBoardAvailability {
                    slug: "heltec-v4-r8".into(),
                    available: true,
                }],
            }),
        };
        let without_local = image_pick_options("heltec-v4-r8", Some(&tips)).unwrap();
        assert!(matches!(
            &without_local[0],
            ImagePickOption::PublishedTip {
                version,
                channel,
                local_image_id: None,
            } if version == "0.3.7" && channel == "stable"
        ));
        assert!(matches!(
            &without_local[1],
            ImagePickOption::PublishedTip {
                version,
                channel,
                local_image_id: None,
            } if version == "0.3.8-dev.1" && channel == "preview"
        ));
        assert_eq!(without_local.len(), 2);

        let tip_src = root.join("tip-src");
        fs::create_dir_all(tip_src.join("channels")).unwrap();
        fs::write(tip_src.join("flash-manifest.json"), b"{\"schema\":1}").unwrap();
        fs::write(tip_src.join("minisign.pub"), b"key\n").unwrap();
        fs::write(tip_src.join("channels/stable.json"), b"{}").unwrap();
        let tip = crate::image_catalog::register_published_image(
            "heltec-v4-r8",
            "0.3.7",
            "stable",
            &tip_src,
        )
        .unwrap();
        let imported_src = root.join("import-src");
        fs::create_dir_all(&imported_src).unwrap();
        fs::write(
            imported_src.join("target.json"),
            br#"{"board_slug":"heltec-v4-r8","parts":[{"path":"firmware/hopspot/heltec-v4-r8/0.2.0/application.bin"}]}"#,
        )
        .unwrap();
        let imported =
            crate::image_catalog::register_imported_image("heltec-v4-r8", "0.2.0", &imported_src)
                .unwrap();

        let merged = image_pick_options("heltec-v4-r8", Some(&tips)).unwrap();
        assert!(matches!(
            &merged[0],
            ImagePickOption::PublishedTip {
                channel,
                local_image_id: Some(id),
                ..
            } if channel == "stable" && id == &tip.image_id
        ));
        assert!(matches!(
            &merged[1],
            ImagePickOption::PublishedTip {
                channel,
                local_image_id: None,
                ..
            } if channel == "preview"
        ));
        assert!(merged.iter().any(|o| matches!(
            o,
            ImagePickOption::Catalog { image } if image.image_id == imported.image_id
        )));
        assert!(
            !merged.iter().any(|o| matches!(
                o,
                ImagePickOption::Catalog { image } if image.image_id == tip.image_id
            )),
            "tip local must stay folded into the PublishedTip row"
        );
        assert_eq!(
            selected_image_pick_value(Some(&tip), &merged),
            merged[0].value()
        );

        crate::image_catalog::set_current("heltec-v4-r8", &imported.image_id).unwrap();
        let current = crate::image_catalog::current_image("heltec-v4-r8")
            .unwrap()
            .unwrap();
        assert_eq!(current.image_id, imported.image_id);
        assert_eq!(
            selected_image_pick_value(Some(&current), &merged),
            format!("id:{}", imported.image_id)
        );

        std::env::remove_var("PRNS_CONTROLLER_IMAGES");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn import_board_image_from_zip_registers_imported_catalog() {
        use std::io::Write;
        let _guard = CATALOG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = std::env::temp_dir().join(format!(
            "prns-import-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("PRNS_CONTROLLER_IMAGES", &root);

        let board_src = root.join("payload");
        fs::create_dir_all(&board_src).unwrap();
        let target = serde_json::json!({
            "board_slug": "heltec-v4-r8",
            "parts": [{
                "path": "firmware/hopspot/heltec-v4-r8/0.3.7/application.bin"
            }]
        });
        fs::write(
            board_src.join("target.json"),
            serde_json::to_vec_pretty(&target).unwrap(),
        )
        .unwrap();
        fs::write(board_src.join("application.bin"), b"fw").unwrap();

        let zip_path = root.join("heltec-v4-r8-0.3.7.zip");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.add_directory("heltec-v4-r8/", options).unwrap();
            zip.start_file("heltec-v4-r8/target.json", options).unwrap();
            zip.write_all(serde_json::to_vec_pretty(&target).unwrap().as_slice())
                .unwrap();
            zip.start_file("heltec-v4-r8/application.bin", options)
                .unwrap();
            zip.write_all(b"fw").unwrap();
            zip.finish().unwrap();
        }

        let image = import_board_image("heltec-v4-r8", &zip_path).expect("import zip");
        assert_eq!(
            image.provenance,
            crate::image_catalog::ImageProvenance::Imported
        );
        assert_eq!(image.version, "0.3.7");
        assert!(image.image_id.starts_with("import-0.3.7-"));

        let mismatch = import_board_image("heltec-v4", &zip_path);
        assert!(mismatch.is_err());

        std::env::remove_var("PRNS_CONTROLLER_IMAGES");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn export_board_image_roundtrips_through_import() {
        let _guard = CATALOG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = std::env::temp_dir().join(format!(
            "prns-export-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("PRNS_CONTROLLER_IMAGES", &root);

        let board_src = root.join("payload");
        fs::create_dir_all(&board_src).unwrap();
        let target = serde_json::json!({
            "board_slug": "heltec-v4-r8",
            "parts": [{
                "path": "firmware/hopspot/heltec-v4-r8/0.3.7/application.bin"
            }]
        });
        fs::write(
            board_src.join("target.json"),
            serde_json::to_vec_pretty(&target).unwrap(),
        )
        .unwrap();
        fs::write(board_src.join("application.bin"), b"fw-bytes").unwrap();
        let image =
            crate::image_catalog::register_imported_image("heltec-v4-r8", "0.3.7", &board_src)
                .unwrap();
        assert_eq!(image.version, "0.3.7");

        let zip_path = root.join("share-heltec-v4-r8-0.3.7.zip");
        export_board_image("heltec-v4-r8", &zip_path).expect("export zip");
        assert!(zip_path.is_file());

        // Clear catalog current so re-import registers cleanly under a fresh root.
        let import_root = root.join("import-catalog");
        fs::create_dir_all(&import_root).unwrap();
        std::env::set_var("PRNS_CONTROLLER_IMAGES", &import_root);
        let imported = import_board_image("heltec-v4-r8", &zip_path).expect("re-import zip");
        assert_eq!(imported.version, "0.3.7");
        assert_eq!(
            imported.provenance,
            crate::image_catalog::ImageProvenance::Imported
        );

        std::env::remove_var("PRNS_CONTROLLER_IMAGES");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_check_json_accepts_controller_contract() {
        let check = parse_published_check_json(
            r#"{"channel":"stable","version":"0.3.7","boards":[{"slug":"heltec-v4-r8","available":true}]}"#,
        )
        .expect("parses");
        assert_eq!(check.version, "0.3.7");
        assert_eq!(check.board_available("heltec-v4-r8"), Some(true));
    }

    #[test]
    fn a_controller_flash_holds_uf2_volume_probes() {
        assert!(!uf2_volume_probes_held());
        {
            let _hold = Uf2VolumeProbeHold::acquire();
            assert!(uf2_volume_probes_held());
        }
        assert!(!uf2_volume_probes_held());
    }

    #[test]
    fn hopspot_phases_advance_the_progress_diagram() {
        let building = parse_hopspot_event(
            r#"{"schema":1,"event":"phase","phase":"building","message":"Building Heltec V4"}"#,
        )
        .expect("building event");
        let progress = progress_from_hopspot_event(true, &building);
        assert_eq!(progress.stage, FlashStage::Prepare);
        assert_eq!(
            progress.stage_state(FlashStage::Enroll),
            FlashStageState::Done
        );
        assert_eq!(
            progress.stage_state(FlashStage::Prepare),
            FlashStageState::Current
        );
        let writing = parse_hopspot_event(
            r#"{"schema":1,"event":"progress","phase":"writing","current":1024,"total":4096}"#,
        )
        .expect("writing event");
        let progress = progress_from_hopspot_event(true, &writing);
        assert_eq!(progress.stage, FlashStage::Write);
        assert_eq!(progress.write_percent, Some(25));
        assert!(progress.manage_offer().is_none());
    }

    #[test]
    fn successful_enrollment_offers_the_new_node() {
        let progress = FlashProgress {
            enrollable: true,
            stage: FlashStage::Complete,
            detail: "Heltec MeshTower V2 is flashed and listed under Managed Nodes.".to_string(),
            write_percent: None,
            outcome: FlashRunOutcome::Succeeded,
            enrolled: Some(EnrolledFlashTarget {
                id: "aabbccddeeff0011".to_string(),
                display_name: "Heltec MeshTower V2".to_string(),
            }),
        };
        let offer = progress
            .manage_offer()
            .expect("enrolled flash offers the node");
        assert_eq!(offer.id, "aabbccddeeff0011");
        assert_eq!(offer.display_name, "Heltec MeshTower V2");
    }

    #[test]
    fn flash_without_enrollment_does_not_offer_a_node() {
        let progress = FlashProgress {
            enrollable: false,
            stage: FlashStage::Complete,
            detail: "firmware flashed".to_string(),
            write_percent: None,
            outcome: FlashRunOutcome::Succeeded,
            enrolled: None,
        };
        assert!(progress.manage_offer().is_none());
    }

    #[test]
    fn failed_complete_does_not_offer_a_node() {
        let progress = FlashProgress {
            enrollable: true,
            stage: FlashStage::Complete,
            detail: "listing failed".to_string(),
            write_percent: None,
            outcome: FlashRunOutcome::Failed,
            enrolled: Some(EnrolledFlashTarget {
                id: "aabbccddeeff0011".to_string(),
                display_name: "Heltec MeshTower V2".to_string(),
            }),
        };
        assert!(progress.manage_offer().is_none());
    }
}
