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
use personal_rns::interfaces::lora::{ModemPreset, RadioProfile, SubGRegion, DEFAULT_915_PROFILE};
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
    Compile,
    Write,
    Complete,
}

impl FlashStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Enroll => "Generating enrollment",
            Self::Compile => "Compiling firmware",
            Self::Write => "Writing device",
            Self::Complete => "Complete",
        }
    }

    pub fn stages(enrollable: bool) -> &'static [Self] {
        if enrollable {
            &[Self::Enroll, Self::Compile, Self::Write, Self::Complete]
        } else {
            &[Self::Compile, Self::Write, Self::Complete]
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
        | "publishing_cache" => FlashStage::Compile,
        "artifact_ready" | "ready" | "requesting_port" | "connecting" | "verifying_target"
        | "writing" | "verifying_flash" | "resetting" | "monitor" | "complete" => FlashStage::Write,
        "failed" => FlashStage::Write,
        _ => FlashStage::Compile,
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

fn hopspot_failure_detail(stderr: &str, last_json_error: Option<String>) -> String {
    if let Some(error) = last_json_error.filter(|text| !text.trim().is_empty()) {
        return error;
    }
    stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && !is_cargo_noise(line))
        .unwrap_or("hopspot-flash failed")
        .to_string()
}

fn is_cargo_noise(line: &str) -> bool {
    line.starts_with("Compiling ")
        || line.starts_with("Finished ")
        || line.starts_with("Running ")
        || line.starts_with("Blocking ")
        || line.contains("profile [")
        || line.starts_with("Downloading ")
        || line.starts_with("Checking ")
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
    pub lora_region: SubGRegion,
    pub lora_preset: ModemPreset,
    pub enrol_for_management: bool,
}

impl Default for FlashDraft {
    fn default() -> Self {
        Self {
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            tcp_client: String::new(),
            lora_region: DEFAULT_915_PROFILE.region(),
            lora_preset: ModemPreset::matching(DEFAULT_915_PROFILE.modulation())
                .unwrap_or(ModemPreset::MediumFast),
            enrol_for_management: true,
        }
    }
}

impl FlashDraft {
    pub fn lora_profile(&self) -> RadioProfile {
        let with_region = crate::edits::apply_lora_region(DEFAULT_915_PROFILE, self.lora_region);
        let default_preset = ModemPreset::matching(DEFAULT_915_PROFILE.modulation())
            .unwrap_or(ModemPreset::MediumFast);
        match ModemPreset::matching(with_region.modulation()) {
            Some(current) if current == self.lora_preset => with_region,
            None if self.lora_preset == default_preset => with_region,
            _ => crate::edits::apply_lora_preset(with_region, self.lora_preset),
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
        "t114" | "t096" | "mesh-pocket-5000" | "mesh-pocket-10000" => Some(0x000E_1000),
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
        "t114-uf2" | "mesh-pocket-uf2" => &[
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
    let repo = repo_root().ok_or_else(|| {
        FlashError::Message(
            "run PRNS Controller from a Personal Reticulum checkout so it can find hopspot-flash"
                .to_string(),
        )
    })?;
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
    let mut command = hopspot_flash_command(&repo, slug);
    if let Some((path, offset)) = &vault_path {
        command
            .arg("--rc-vault")
            .arg(path)
            .arg("--rc-vault-offset")
            .arg(format!("0x{offset:x}"));
    }
    apply_wifi_plan(&mut command, wifi);
    on_progress(FlashProgress::running(
        enrollable,
        FlashStage::Compile,
        format!("Starting hopspot-flash for {slug}…"),
    ));
    let mut child = command
        .spawn()
        .map_err(|error| FlashError::Message(format!("could not start hopspot-flash: {error}")))?;
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
    if let Some((path, _)) = vault_path {
        let _ = std::fs::remove_file(path);
    }
    if status.success() {
        return Ok(());
    }
    Err(FlashError::Message(hopspot_failure_detail(
        "",
        last_json_error,
    )))
}

fn hopspot_flash_command(repo: &Path, slug: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("--locked")
        .arg("-p")
        .arg("hopspot-flash")
        .arg("--")
        .arg("flash")
        .arg(slug)
        .arg("--local-build")
        .arg("--yes")
        .arg("--json")
        .current_dir(repo)
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO_TARGET_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
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
        let pocket = boards
            .iter()
            .find(|board| board.slug == "mesh-pocket-5000")
            .expect("MeshPocket is in the catalog");
        assert!(pocket.enrollable);
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
            lora_region: SubGRegion::Regulated(
                personal_rns::interfaces::lora::RegulatoryRegion::Eu868,
            ),
            ..FlashDraft::default()
        };
        let profile = draft.custom_lora_profile().expect("EU868 is custom");
        assert_eq!(
            profile.region(),
            SubGRegion::Regulated(personal_rns::interfaces::lora::RegulatoryRegion::Eu868)
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
                "mesh-pocket-10000".to_string(),
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
    fn controller_flash_invokes_hopspot_flash_like_the_cli() {
        let command = hopspot_flash_command(Path::new("/repo"), "t114");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "run",
                "--locked",
                "-p",
                "hopspot-flash",
                "--",
                "flash",
                "t114",
                "--local-build",
                "--yes",
                "--json",
            ]
        );
        assert!(command
            .get_envs()
            .any(|(key, value)| { key == "CARGO_TARGET_DIR" && value.is_none() }));
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
        assert_eq!(progress.stage, FlashStage::Compile);
        assert_eq!(
            progress.stage_state(FlashStage::Enroll),
            FlashStageState::Done
        );
        assert_eq!(
            progress.stage_state(FlashStage::Compile),
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
