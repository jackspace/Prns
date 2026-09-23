use personal_rns::usb_auto::WebUsbBootloaderEntry;

#[cfg(any(
    feature = "board-t096",
    feature = "board-t1000e",
    feature = "board-mesh-pocket",
    feature = "board-rak4631",
    feature = "board-rak10724",
    feature = "board-sensecap-solar-node"
))]
mod request {
    use core::sync::atomic::{AtomicBool, Ordering};

    use embassy_time::{Duration, Timer};

    const REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(25);
    const CONTROL_RESPONSE_GRACE_PERIOD: Duration = Duration::from_millis(100);

    enum ResetPreparation {
        Ready,
        #[cfg(any(
            feature = "board-t096",
            feature = "board-mesh-pocket",
            feature = "board-rak4631",
            feature = "board-rak10724",
            feature = "board-sensecap-solar-node"
        ))]
        Rejected,
    }

    #[derive(Clone, Copy)]
    pub(crate) enum EntryMode {
        /// The USB mass-storage (UF2) bootloader, for a host holding the cable.
        Uf2,
        /// The bootloader's over-the-air DFU mode, for a paired controller in Bluetooth range.
        #[cfg_attr(feature = "board-t1000e", allow(dead_code))]
        BootloaderOta,
    }

    static REQUESTED: AtomicBool = AtomicBool::new(false);
    static OTA_REQUESTED: AtomicBool = AtomicBool::new(false);

    pub fn request() {
        REQUESTED.store(true, Ordering::SeqCst);
    }

    /// Reboot into the bootloader's over-the-air DFU mode after the control-response grace period.
    #[cfg(any(
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    ))]
    pub(crate) fn request_ota() {
        OTA_REQUESTED.store(true, Ordering::SeqCst);
    }

    pub async fn wait() -> ! {
        loop {
            let mode = if REQUESTED.swap(false, Ordering::SeqCst) {
                Some(EntryMode::Uf2)
            } else if OTA_REQUESTED.swap(false, Ordering::SeqCst) {
                Some(EntryMode::BootloaderOta)
            } else {
                None
            };
            if let Some(mode) = mode {
                Timer::after(CONTROL_RESPONSE_GRACE_PERIOD).await;
                match prepare_bootloader_reset(mode) {
                    ResetPreparation::Ready => cortex_m::peripheral::SCB::sys_reset(),
                    #[cfg(any(
                        feature = "board-t096",
                        feature = "board-mesh-pocket",
                        feature = "board-rak4631",
                        feature = "board-rak10724",
                        feature = "board-sensecap-solar-node"
                    ))]
                    ResetPreparation::Rejected => {}
                }
            }
            Timer::after(REQUEST_POLL_INTERVAL).await;
        }
    }

    #[cfg(feature = "board-t1000e")]
    fn prepare_bootloader_reset(_mode: EntryMode) -> ResetPreparation {
        const ADAFRUIT_SERIAL_ONLY_DFU_GPREGRET: u8 = 0x4e;
        embassy_nrf::pac::POWER
            .gpregret()
            .write(|register| register.set_gpregret(ADAFRUIT_SERIAL_ONLY_DFU_GPREGRET));
        ResetPreparation::Ready
    }

    #[cfg(any(
        feature = "board-t096",
        feature = "board-mesh-pocket",
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    ))]
    fn prepare_bootloader_reset(mode: EntryMode) -> ResetPreparation {
        const ADAFRUIT_UF2_DFU_GPREGRET: u32 = 0x57;
        // Adafruit_nRF52_Bootloader main.c: DFU_MAGIC_OTA_RESET. The bootloader then brings up
        // S140 itself and serves Nordic legacy DFU over BLE until a transfer completes or a reset.
        const ADAFRUIT_OTA_DFU_GPREGRET: u32 = 0xA8;
        let magic = match mode {
            EntryMode::Uf2 => ADAFRUIT_UF2_DFU_GPREGRET,
            EntryMode::BootloaderOta => ADAFRUIT_OTA_DFU_GPREGRET,
        };
        // SAFETY: The enabled S140 SoftDevice owns POWER. This synchronous SVC is the Nordic API
        // for setting GPREGRET while the SoftDevice is active; register 0 and the one-byte
        // bootloader request are valid inputs.
        let result = unsafe { nrf_softdevice::raw::sd_power_gpregret_set(0, magic) };
        match nrf_softdevice::RawError::convert(result) {
            Ok(()) => ResetPreparation::Ready,
            Err(_) => ResetPreparation::Rejected,
        }
    }
}

/// Reboot into the bootloader's over-the-air DFU mode, for the `EnterFirmwareUpdate` verb.
#[cfg(any(
    feature = "board-rak4631",
    feature = "board-rak10724",
    feature = "board-sensecap-solar-node"
))]
pub(crate) fn request_ota() {
    request::request_ota();
}

pub const fn webusb_entry() -> WebUsbBootloaderEntry {
    #[cfg(any(
        feature = "board-t096",
        feature = "board-t1000e",
        feature = "board-mesh-pocket",
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    ))]
    return WebUsbBootloaderEntry::Supported {
        request: request::request,
    };

    #[cfg(not(any(
        feature = "board-t096",
        feature = "board-t1000e",
        feature = "board-mesh-pocket",
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    )))]
    WebUsbBootloaderEntry::Unsupported
}

pub async fn wait() -> ! {
    #[cfg(any(
        feature = "board-t096",
        feature = "board-t1000e",
        feature = "board-mesh-pocket",
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    ))]
    request::wait().await;

    #[cfg(not(any(
        feature = "board-t096",
        feature = "board-t1000e",
        feature = "board-mesh-pocket",
        feature = "board-rak4631",
        feature = "board-rak10724",
        feature = "board-sensecap-solar-node"
    )))]
    core::future::pending().await
}
