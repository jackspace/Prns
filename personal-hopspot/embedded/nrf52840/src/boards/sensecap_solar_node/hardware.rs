use core::cell::RefCell;

use embassy_nrf::config::HfclkSource;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::interrupt::{self, InterruptExt, Priority};
use embassy_nrf::mode::Blocking;
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::rng::Rng;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::{bind_interrupts, config, peripherals, usb};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use personal_rns::lora::LoRaInterface;
use personal_rns::radios::sx126x::{BoardConfig, FrontendControl, Sx126x, TcxoVoltage};
use static_cell::StaticCell;

use crate::boards::status_led::StatusLed;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    TWISPI0 => spim::InterruptHandler<peripherals::TWISPI0>;
});

type SolarNodeSpiDevice = ExclusiveDevice<Spim<'static>, Output<'static>, Delay>;

type SolarNodeRadio =
    Sx126x<SolarNodeSpiDevice, Input<'static>, Input<'static>, Output<'static>, Delay>;

pub(crate) type SolarNodeLoraInterface = LoRaInterface<'static, SolarNodeRadio>;

type SolarNodeUsbDriver = Driver<'static, &'static SoftwareVbusDetect>;

pub(crate) struct SolarNodeHardware {
    pub(crate) usb: SolarNodeUsbDriver,
    pub(crate) vbus: &'static SoftwareVbusDetect,
    pub(crate) radio: SolarNodeRadio,
    pub(crate) status_led: StatusLed,
    pub(crate) button: Input<'static>,
}

/// The Wio-SX1262's receive path is gated by a GPIO while DIO2 drives the transmit path.
/// `FrontendControl` hands the driver bare `fn()` pointers with no captured state, so the pin is
/// parked here for those callbacks to reach, the same arrangement the MeshTower V2 uses. The
/// L76K's controls are parked beside it so the receiver stays unpowered behind its load switch.
struct HeldIo {
    radio_rx_enable: Output<'static>,
    _gnss_enable: Output<'static>,
    _gnss_reset: Output<'static>,
    _gnss_wakeup: Output<'static>,
}

static HELD_IO: Mutex<CriticalSectionRawMutex, RefCell<Option<HeldIo>>> =
    Mutex::new(RefCell::new(None));

pub(crate) struct SolarNodeBoard;

impl SolarNodeBoard {
    pub(crate) async fn initialize<R>(
        bootstrap: impl FnOnce(&mut Nvmc<'static>, Rng<'static, Blocking>) -> R,
    ) -> (R, SolarNodeHardware) {
        let mut nrf_config = config::Config::default();
        nrf_config.hfclk_source = HfclkSource::ExternalXtal;
        // The carrier populates a 32.768 kHz crystal (both reference firmwares declare USE_LFXO),
        // but the internal RC is kept, as the headless bring-up did: S140 is configured for the
        // RC source here, and an absent LFXO would stall a board that has no SWD to say so.
        nrf_config.gpiote_interrupt_priority = Priority::P2;
        nrf_config.time_interrupt_priority = Priority::P2;
        let peripherals = embassy_nrf::init(nrf_config);

        pet_bootloader_watchdog();
        disable_leftover_softdevice();
        pet_bootloader_watchdog();
        // UF2 / leftover Meshtastic may have S140 on USB; let POWER settle before NVMC/USBD.
        Timer::after_millis(100).await;
        pet_bootloader_watchdog();

        let identity = {
            let mut nvmc = Nvmc::new(peripherals.NVMC);
            let rng = Rng::new_blocking(peripherals.RNG);
            pet_bootloader_watchdog();
            let identity = bootstrap(&mut nvmc, rng);
            pet_bootloader_watchdog();
            identity
        };

        // SoftDevice reserves P0/P1; keep app interrupts off those. USB at P2, SPI at P3 so a BLE
        // radio event can preempt LoRa SPI.
        interrupt::USBD.set_priority(Priority::P2);
        interrupt::TWISPI0.set_priority(Priority::P3);
        static SOFTWARE_VBUS: StaticCell<SoftwareVbusDetect> = StaticCell::new();
        let vbus = crate::runtime::software_vbus::initialize(&SOFTWARE_VBUS);
        let usb = Driver::new(peripherals.USBD, Irqs, vbus);

        // XIAO L76K: enable is the active-high gate of a TPS22916 load switch (P1.05), reset is
        // active-low (P1.03), wake-up is held high to leave standby (P0.02). All three low keeps
        // the receiver genuinely unpowered; nothing on a headless board consumes a fix.
        let gnss_enable = Output::new(peripherals.P1_05, Level::Low, OutputDrive::Standard);
        let gnss_reset = Output::new(peripherals.P1_03, Level::Low, OutputDrive::Standard);
        let gnss_wakeup = Output::new(peripherals.P0_02, Level::Low, OutputDrive::Standard);

        let mut radio_spim_config = spim::Config::default();
        radio_spim_config.frequency = spim::Frequency::M4;
        // Wio-SX1262 on the XIAO SPI: SCK P1.13, MISO P1.14, MOSI P1.15, NSS P0.04 (D4).
        let radio_bus = Spim::new(
            peripherals.TWISPI0,
            Irqs,
            peripherals.P1_13,
            peripherals.P1_14,
            peripherals.P1_15,
            radio_spim_config,
        );
        let radio_cs = Output::new(peripherals.P0_04, Level::High, OutputDrive::Standard);
        let radio_spi = ExclusiveDevice::new(radio_bus, radio_cs, Delay).unwrap();
        // BUSY D3 / P0.29, DIO1 D1 / P0.03, RESET D2 / P0.28, RXEN D5 / P0.05; DIO2 owns TXEN.
        let radio_busy = Input::new(peripherals.P0_29, Pull::None);
        let radio_dio1 = Input::new(peripherals.P0_03, Pull::None);

        HELD_IO.lock(|held| {
            *held.borrow_mut() = Some(HeldIo {
                radio_rx_enable: Output::new(peripherals.P0_05, Level::Low, OutputDrive::Standard),
                _gnss_enable: gnss_enable,
                _gnss_reset: gnss_reset,
                _gnss_wakeup: gnss_wakeup,
            });
        });

        let mut radio_reset = Output::new(peripherals.P0_28, Level::Low, OutputDrive::Standard);
        Timer::after_millis(10).await;
        radio_reset.set_high();
        let radio = Sx126x::new(
            radio_spi,
            radio_busy,
            radio_dio1,
            radio_reset,
            Delay,
            BoardConfig {
                tcxo_voltage: Some(TcxoVoltage::V1_8),
                use_dcdc: true,
                rx_boost: true,
                dio2_as_rf_switch: true,
                external_rx_gain_db: 0,
                external_power_amplifier: None,
                frontend_control: FrontendControl::TxRx {
                    enter_transmit,
                    enter_receive,
                },
            },
        );

        // Mesh LED (Meshtastic PIN_LED2, D12 / P0.19), active-high.
        let status_led = StatusLed::active_high(Output::new(
            peripherals.P0_19,
            Level::Low,
            OutputDrive::Standard,
        ));
        // User key D13 / P1.01, active-low with pull-up (see mod.rs for the vendor ambiguity).
        let button = Input::new(peripherals.P1_01, Pull::Up);

        (
            identity,
            SolarNodeHardware {
                usb,
                vbus,
                radio,
                status_led,
                button,
            },
        )
    }
}

fn enter_transmit() {
    HELD_IO.lock(|held| {
        if let Some(io) = held.borrow_mut().as_mut() {
            io.radio_rx_enable.set_low();
        }
    });
}

fn enter_receive() {
    HELD_IO.lock(|held| {
        if let Some(io) = held.borrow_mut().as_mut() {
            io.radio_rx_enable.set_high();
        }
    });
}

fn pet_bootloader_watchdog() {
    let wdt = embassy_nrf::pac::WDT;
    if wdt.runstatus().read().runstatus() {
        for index in 0..8 {
            wdt.rr(index)
                .write(|register| register.set_rr(embassy_nrf::pac::wdt::vals::Rr::RELOAD));
        }
    }
}

fn disable_leftover_softdevice() {
    let mut enabled = 0_u8;
    // SAFETY: The Adafruit MBR implements this SVC whether S140 is on or off.
    let _ = unsafe { nrf_softdevice::raw::sd_softdevice_is_enabled(&mut enabled) };
    if enabled != 0 {
        // SAFETY: S140 is enabled; disable returns the RNG/NVMC peripherals to the application.
        let _ = unsafe { nrf_softdevice::raw::sd_softdevice_disable() };
    }
}
