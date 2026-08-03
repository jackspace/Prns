//! LilyGO T-Echo: nRF52840 with an SX1262 and a 200x200 SSD1681 e-ink panel.

use core::convert::Infallible;

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::mode::Blocking;
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::peripherals;
use embassy_nrf::rng::Rng;
use embassy_nrf::saadc::{ChannelConfig, Config as SaadcConfig, Gain, Reference, Saadc};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::Peri;
use embassy_time::{Delay, Duration, Timer};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use embedded_hal_bus::spi::ExclusiveDevice;
use epd_waveshare::color::Color as EpdColor;
use epd_waveshare::epd1in54_v2::Display1in54;

use personal_hopspot_core as hopspot;
use personal_rns::interfaces::InterfaceId;
use personal_rns::radios::sx126x::{BoardConfig, Sx126x, TcxoVoltage};

use crate::board::{
    self, AnimationClock, EarlyHardware, ExclusiveSpi, Irqs, Nrf52840Board, RuntimeHardware,
};
use crate::display::frame_hash;
use crate::input;
use crate::panels::ssd1681::Ssd1681;

/// This board's USB-auto interface id (the always-present top-level wire on pool slot 0).
const USB_INTERFACE_ID: InterfaceId = InterfaceId::new(*b"techousb");

/// This node's `lxmf.delivery` announce app_data: `msgpack([display_name, stamp_cost])`. The
/// `\x17` is the name's own length, so a rename has to recount it.
const ANNOUNCE_APP_DATA: &[u8] = b"\x92\xc4\x17Personal Hopspot T-Echo\xc0";
const NODE_ANNOUNCE_APP_DATA: &[u8] = b"Personal Hopspot T-Echo";

const PARTIAL_REFRESH_LIMIT: u32 = 64;
const FULL_REFRESH_MAX_AGE_MS: u64 = 30 * 60 * 1_000;
const TELEMETRY_MIN_INTERVAL_MS: u64 = 5_000;

const FRONTLIGHT_HOLD: Duration = Duration::from_secs(8);

const PANEL_SIZE: i32 = 200;
const SCREEN_WIDTH: i32 = 64;
const SCREEN_HEIGHT: i32 = 128;
const SCALE_NUM: i32 = 3;
const SCALE_DEN: i32 = 2;
const SCALED_SHORT: i32 = SCREEN_WIDTH * SCALE_NUM / SCALE_DEN;
const SCALED_LONG: i32 = SCREEN_HEIGHT * SCALE_NUM / SCALE_DEN;
const SCALED_ORIGIN_X: i32 = (PANEL_SIZE - SCALED_LONG) / 2;
const SCALED_ORIGIN_Y: i32 = (PANEL_SIZE - SCALED_SHORT) / 2;

type TechoEink =
    Ssd1681<ExclusiveSpi, Input<'static>, Output<'static>, Output<'static>, Delay>;

/// The T-Echo's face: the e-ink driver, the panel buffer the shared screen draws into, its
/// refresh policy, and the hash of what is currently on the glass. The rail rides along so the
/// panel stays powered exactly as long as the display exists.
pub struct TechoDisplay {
    driver: TechoEink,
    panel: Display1in54,
    policy: hopspot::EinkRefreshPolicy,
    presented: Option<u64>,
    _rail: Output<'static>,
}

impl OriginDimensions for TechoDisplay {
    fn size(&self) -> Size {
        Size::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32)
    }
}

impl DrawTarget for TechoDisplay {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let panel_color = match color {
                BinaryColor::On => EpdColor::Black,
                BinaryColor::Off => EpdColor::White,
            };
            let sx0 = point.x * SCALE_NUM / SCALE_DEN;
            let sx1 = (point.x + 1) * SCALE_NUM / SCALE_DEN;
            let sy0 = point.y * SCALE_NUM / SCALE_DEN;
            let sy1 = (point.y + 1) * SCALE_NUM / SCALE_DEN;
            let top_left = Point::new(
                SCALED_ORIGIN_X + sy0,
                SCALED_ORIGIN_Y + (SCALED_SHORT - sx1),
            );
            let size = Size::new((sy1 - sy0) as u32, (sx1 - sx0) as u32);
            let _ = self
                .panel
                .fill_solid(&Rectangle::new(top_left, size), panel_color);
        }
        Ok(())
    }

    /// Overridden so a clear wipes the whole panel, letterbox borders included, exactly as the
    /// render loop's direct `panel.clear` did before the panel moved behind this type.
    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let panel_color = match color {
            BinaryColor::On => EpdColor::Black,
            BinaryColor::Off => EpdColor::White,
        };
        let _ = self.panel.clear(panel_color);
        Ok(())
    }
}

pub struct TechoDeferredHardware {
    radio_bus: Peri<'static, peripherals::TWISPI0>,
    radio_sck: Peri<'static, peripherals::P0_19>,
    radio_mosi: Peri<'static, peripherals::P0_23>,
    radio_miso: Peri<'static, peripherals::P0_22>,
    radio_cs: Peri<'static, peripherals::P0_24>,
    radio_busy: Peri<'static, peripherals::P0_17>,
    radio_dio1: Peri<'static, peripherals::P0_20>,
    radio_reset: Peri<'static, peripherals::P0_25>,
    eink_bus: Peri<'static, peripherals::SPI2>,
    eink_sck: Peri<'static, peripherals::P0_31>,
    eink_mosi: Peri<'static, peripherals::P1_06>,
    eink_miso: Peri<'static, peripherals::P0_29>,
    eink_cs: Peri<'static, peripherals::P0_30>,
    eink_dc: Peri<'static, peripherals::P0_28>,
    eink_reset: Peri<'static, peripherals::P0_02>,
    eink_busy: Peri<'static, peripherals::P0_03>,
    eink_rail: Output<'static>,
    button: Peri<'static, peripherals::P1_10>,
    frontlight: Peri<'static, peripherals::P1_11>,
}

/// The T-Echo's board half: pins, panel, and battery. Everything past it is the shared nRF52840
/// firmware.
pub struct TechoBoard;

impl Nrf52840Board for TechoBoard {
    const ANNOUNCE_APP_DATA: &'static [u8] = ANNOUNCE_APP_DATA;
    const NODE_ANNOUNCE_APP_DATA: &'static [u8] = NODE_ANNOUNCE_APP_DATA;
    const USB_INTERFACE_ID: InterfaceId = USB_INTERFACE_ID;
    const USB_PRODUCT: &'static str = "Personal Hopspot (T-Echo)";
    const USB_SERIAL_NUMBER: &'static str = "PERSONAL-RNS-TECHO-HOP";
    const ANIMATION_CLOCK: AnimationClock = AnimationClock::Still;

    type Battery = Saadc<'static, 1>;
    type Deferred = TechoDeferredHardware;
    type Display = TechoDisplay;
    type Illumination = Output<'static>;

    fn claim<Identities>(
        vaults: impl FnOnce(&mut Nvmc<'static>, &mut Rng<'static, Blocking>) -> Identities,
    ) -> (Identities, EarlyHardware<Self::Battery, Self::Deferred>) {
        let peripherals = embassy_nrf::init(board::platform_config());
        let identities = board::read_identity_vaults(peripherals.NVMC, peripherals.RNG, vaults);

        let eink_rail = Output::new(peripherals.P0_12, Level::High, OutputDrive::Standard);
        let status_led = Output::new(peripherals.P1_01, Level::High, OutputDrive::Standard);

        board::apply_interrupt_priorities();
        let usb = board::usb_hardware(peripherals.USBD);

        // Battery sense: VBAT on a 2:1 divider into AIN2 (P0.04), sampled by the SAADC against the
        // 3.0 V internal reference, so VBAT_mV = raw * 6000 / 4096.
        let mut battery_channel = ChannelConfig::single_ended(peripherals.P0_04);
        battery_channel.reference = Reference::INTERNAL;
        battery_channel.gain = Gain::GAIN1_5;
        let battery = Saadc::new(
            peripherals.SAADC,
            Irqs,
            SaadcConfig::default(),
            [battery_channel],
        );

        let hardware = EarlyHardware {
            usb,
            battery,
            status_led,
            deferred: TechoDeferredHardware {
                radio_bus: peripherals.TWISPI0,
                radio_sck: peripherals.P0_19,
                radio_mosi: peripherals.P0_23,
                radio_miso: peripherals.P0_22,
                radio_cs: peripherals.P0_24,
                radio_busy: peripherals.P0_17,
                radio_dio1: peripherals.P0_20,
                radio_reset: peripherals.P0_25,
                eink_bus: peripherals.SPI2,
                eink_sck: peripherals.P0_31,
                eink_mosi: peripherals.P1_06,
                eink_miso: peripherals.P0_29,
                eink_cs: peripherals.P0_30,
                eink_dc: peripherals.P0_28,
                eink_reset: peripherals.P0_02,
                eink_busy: peripherals.P0_03,
                eink_rail,
                button: peripherals.P1_10,
                frontlight: peripherals.P1_11,
            },
        };
        (identities, hardware)
    }

    async fn finish(
        deferred: Self::Deferred,
    ) -> RuntimeHardware<Self::Display, Self::Illumination> {
        let mut radio_spim_config = spim::Config::default();
        radio_spim_config.frequency = spim::Frequency::M4;
        let radio_bus = Spim::new(
            deferred.radio_bus,
            Irqs,
            deferred.radio_sck,
            deferred.radio_mosi,
            deferred.radio_miso,
            radio_spim_config,
        );
        let radio_cs = Output::new(deferred.radio_cs, Level::High, OutputDrive::Standard);
        let radio_spi = ExclusiveDevice::new(radio_bus, radio_cs, Delay).unwrap();
        let radio_busy = Input::new(deferred.radio_busy, Pull::None);
        let radio_dio1 = Input::new(deferred.radio_dio1, Pull::None);
        let radio_reset = Output::new(deferred.radio_reset, Level::High, OutputDrive::Standard);
        let radio = Sx126x::new(
            radio_spi,
            radio_busy,
            radio_dio1,
            radio_reset,
            Delay,
            BoardConfig {
                // LilyGo's factory firmware initializes this HPD16A through RadioLib's 1.6 V
                // TCXO default.
                tcxo_voltage: Some(TcxoVoltage::V1_6),
                use_dcdc: true,
                rx_boost: true,
                dio2_as_rf_switch: true,
                external_rx_gain_db: 0,
            },
        );

        let mut eink_spim_config = spim::Config::default();
        eink_spim_config.frequency = spim::Frequency::M4;
        let eink_bus = Spim::new(
            deferred.eink_bus,
            Irqs,
            deferred.eink_sck,
            deferred.eink_mosi,
            deferred.eink_miso,
            eink_spim_config,
        );
        let eink_cs = Output::new(deferred.eink_cs, Level::High, OutputDrive::Standard);
        let eink_dc = Output::new(deferred.eink_dc, Level::Low, OutputDrive::Standard);
        let eink_reset = Output::new(deferred.eink_reset, Level::High, OutputDrive::Standard);
        let eink_busy = Input::new(deferred.eink_busy, Pull::None);
        Timer::after(Duration::from_millis(150)).await;
        let eink_spi = ExclusiveDevice::new(eink_bus, eink_cs, Delay).unwrap();
        let panel = Display1in54::default();
        let display = Ssd1681::new(eink_spi, eink_busy, eink_dc, eink_reset, Delay)
            .ok()
            .map(|driver| TechoDisplay {
                driver,
                panel,
                policy: hopspot::EinkRefreshPolicy::new(
                    PARTIAL_REFRESH_LIMIT,
                    FULL_REFRESH_MAX_AGE_MS,
                    TELEMETRY_MIN_INTERVAL_MS,
                ),
                presented: None,
                _rail: deferred.eink_rail,
            });

        RuntimeHardware {
            radio,
            display,
            button: Input::new(deferred.button, Pull::Up),
            illumination: Output::new(deferred.frontlight, Level::Low, OutputDrive::Standard),
        }
    }

    async fn battery_millivolts(battery: &mut Self::Battery) -> Option<u32> {
        let mut sample = [0i16; 1];
        battery.sample(&mut sample).await;
        Some((sample[0].max(0) as u32) * 6000 / 4096)
    }

    fn present(display: &mut Self::Display, now_ms: u64, urgency: &hopspot::EinkRefreshUrgency) {
        let hash = frame_hash(display.panel.buffer());
        if display.presented == Some(hash) {
            return;
        }
        match display.policy.for_changed_frame(now_ms, urgency) {
            hopspot::EinkRefresh::Deferred => {}
            hopspot::EinkRefresh::Full => {
                if display.driver.full_update(display.panel.buffer()).is_ok() {
                    display.policy.full_refresh_succeeded(now_ms);
                    display.presented = Some(hash);
                } else {
                    display.policy.refresh_failed();
                }
            }
            hopspot::EinkRefresh::Partial => {
                if display.driver.partial_update(display.panel.buffer()).is_ok() {
                    display.policy.partial_refresh_succeeded(now_ms);
                    display.presented = Some(hash);
                } else {
                    display.policy.refresh_failed();
                }
            }
        }
    }

    async fn drive_illumination(frontlight: Self::Illumination) -> ! {
        input::drive_panel_light(frontlight, Level::High, FRONTLIGHT_HOLD).await
    }
}

pub async fn run(spawner: Spawner) -> ! {
    crate::firmware::run::<TechoBoard>(spawner).await
}
