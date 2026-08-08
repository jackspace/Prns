//! Heltec Mesh Node T114 (HT-n5262): nRF52840 with an SX1262 and a 240x135 ST7789V2 colour TFT.
//!
//! Every pin below is confirmed by at least two independent sources: Heltec's published
//! MeshNode-T114 schematics (V2.0 and V2.1 agree on every net-to-ball assignment), the
//! Meshtastic `heltec_mesh_node_t114` variant, Heltec's nRF52 BSP `HT-n5262` variant, and the
//! community bootloader port. Values with only one source carry a PROVISIONAL marker.
//!
//! The stock bootloader is the blocker to know about. Heltec ships S140 6.1.1 with the
//! application at 0x26000, read out of the UICR words and SoftDevice information structure in
//! their own bootloader image. The `nrf-softdevice` binding this firmware uses supports S140 v7
//! only, so this board runs Prns only after re-bootloadering with an Adafruit_nRF52_Bootloader
//! build carrying S140 7.3.0, exactly the treatment the T-Echo already requires. After that the
//! application base is 0x27000 and the board shares `memory-s140-7.x` with the T-Echo. Never
//! chip-erase this board: UICR holds the bootloader start pointer and PSELRESET, and wiping it
//! costs the reset pin and the bootloader in one stroke, recoverable only over SWD.
//!
//! Hardware facts the code below encodes:
//!
//! - The TFT's panel rail (P0.03) and backlight (P0.15) are both gated by P-channel MOSFETs with
//!   pull-ups, so both are active LOW and both are off at reset. A dark screen with a live CPU is
//!   the expected reset state, not a fault.
//! - The SX1262 has no discrete crystal: DIO3 powers a 32 MHz TCXO at 1.8 V, DIO2 drives the
//!   antenna switch, and the radio's DC-DC is fitted. Same `BoardConfig` as the T-Echo.
//! - The battery divider (390k/100k, ratio 4.9 on every board revision) is disconnected until
//!   P0.06 is driven high, so every read gates it on, waits, samples, and gates it off.

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
use embedded_graphics::pixelcolor::{BinaryColor, Rgb565};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use embedded_hal_bus::spi::ExclusiveDevice;

use personal_hopspot_core as hopspot;
use personal_rns::interfaces::InterfaceId;
use personal_rns::radios::sx126x::{BoardConfig, Sx126x, TcxoVoltage};

use crate::board::{
    self, AnimationClock, EarlyHardware, ExclusiveSpi, Irqs, Nrf52840Board, RuntimeHardware,
};
use crate::display::frame_hash;
use crate::input;
use crate::panels::st7789::{
    ColorOrder, Palette, PanelConfig, PanelFrame, PanelOrigin, PanelRect, PanelRotation,
    PixelInversion, PixelState, St7789, HEIGHT as PANEL_HEIGHT, WIDTH as PANEL_WIDTH,
};

/// This board's USB-auto interface id (the always-present top-level wire on pool slot 0).
const USB_INTERFACE_ID: InterfaceId = InterfaceId::new(*b"t114-usb");

/// This node's `lxmf.delivery` announce app_data: `msgpack([display_name, stamp_cost])`. The
/// `\x15` is the name's own length, so a rename has to recount it.
const ANNOUNCE_APP_DATA: &[u8] = b"\x92\xc4\x15Personal Hopspot T114\xc0";
const NODE_ANNOUNCE_APP_DATA: &[u8] = b"Personal Hopspot T114";

/// How long the backlight stays lit after a press. Longer than the T-Echo's frontlight hold
/// because an unlit TFT shows nothing at all, where unlit e-ink is still readable paper. Not
/// forever, because a lit backlight outdraws both radios at idle.
const BACKLIGHT_HOLD: Duration = Duration::from_secs(30);

/// 3000 mV of SAADC full scale (0.6 V internal reference, gain 1/5) times the 490/100 divider.
/// R32 390k and R33 100k are identical on every published schematic revision.
const VBAT_FULL_SCALE_MV: u32 = 14_700;
const SAADC_FULL_SCALE_COUNTS: u32 = 4_096;

/// PROVISIONAL: single source. Meshtastic waits 10 ms between raising the divider gate and
/// sampling. V2.1 boards replace the gate resistors with 10M parts, which slows the gate's
/// turn-off but not its turn-on, so the delay stays adequate on both revisions. Do not shorten it.
const ADC_GATE_SETTLE: Duration = Duration::from_millis(10);

const SCREEN_WIDTH: i32 = 64;
const SCREEN_HEIGHT: i32 = 128;
/// The canvas is 64x128 and the glass is 240x135, so the canvas lies across the panel rotated 90
/// degrees, 128 wide by 64 tall. 15/8 is the largest scale that fits: it takes the long axis to
/// exactly 240 with nothing cropped, and the short axis to 120, leaving 15 letterbox rows.
const SCALE_NUM: i32 = 15;
const SCALE_DEN: i32 = 8;
const SCALED_SHORT: i32 = scaled(SCREEN_WIDTH);
const SCALED_LONG: i32 = scaled(SCREEN_HEIGHT);
const SCALED_ORIGIN_X: i32 = (PANEL_WIDTH as i32 - SCALED_LONG) / 2;
const SCALED_ORIGIN_Y: i32 = (PANEL_HEIGHT as i32 - SCALED_SHORT) / 2;

const _: () = assert!(SCALED_LONG == PANEL_WIDTH as i32);
const _: () = assert!(SCALED_SHORT <= PANEL_HEIGHT as i32);

/// The 135x240 glass sits inside the controller's 240x320 frame memory. For the clockwise
/// landscape rotation the window starts at column 40 and row 52, the pairing Meshtastic's driver
/// demonstrably uses on this exact board. Mirroring to the counter-clockwise rotation moves the
/// row offset to 53; do not change one without the other.
const PANEL_ORIGIN: PanelOrigin = PanelOrigin::new(40, 52);

/// Rgb565 keeps the top 5, 6 and 5 bits of an 8 bit channel.
const RED_BITS_DROPPED: u8 = 8 - 5;
const GREEN_BITS_DROPPED: u8 = 8 - 6;
const BLUE_BITS_DROPPED: u8 = 8 - 5;

/// The two-tone pair this UI already shows in. Core owns the colours; this board owns only the
/// encoding into the panel's wire format.
const PALETTE: Palette = Palette {
    lit: quantised(hopspot::MOBILE_LIT_RGBA),
    dark: quantised(hopspot::MOBILE_DARK_RGBA),
};

const fn scaled(canvas: i32) -> i32 {
    canvas * SCALE_NUM / SCALE_DEN
}

const fn quantised(rgba: [u8; 4]) -> Rgb565 {
    Rgb565::new(
        rgba[0] >> RED_BITS_DROPPED,
        rgba[1] >> GREEN_BITS_DROPPED,
        rgba[2] >> BLUE_BITS_DROPPED,
    )
}

const fn panel_pixel(color: BinaryColor) -> PixelState {
    match color {
        BinaryColor::On => PixelState::Lit,
        BinaryColor::Off => PixelState::Dark,
    }
}

type T114Panel = St7789<ExclusiveSpi, Output<'static>, Output<'static>, Delay>;

/// The T114's face: the panel driver, the packed shadow frame the shared screen draws into, and
/// the hash of what is currently on the glass. The panel rail rides along so the panel stays
/// powered exactly as long as the display exists. The letterbox rows are written only by a clear.
pub struct T114Display {
    panel: T114Panel,
    frame: PanelFrame,
    presented: Option<u64>,
    _rail: Output<'static>,
}

impl T114Display {
    /// The one place canvas coordinates become panel coordinates: quarter turn, 15/8 scale,
    /// letterboxed. Same arithmetic as the T-Echo's per-pixel path, one rectangle at a time.
    fn fill_area(&mut self, area: &Rectangle, color: BinaryColor) {
        let area = area.intersection(&self.bounding_box());
        if area.size.width == 0 || area.size.height == 0 {
            return;
        }
        let sx0 = scaled(area.top_left.x);
        let sx1 = scaled(area.top_left.x + area.size.width as i32);
        let sy0 = scaled(area.top_left.y);
        let sy1 = scaled(area.top_left.y + area.size.height as i32);
        self.frame.fill(
            &PanelRect {
                left: (SCALED_ORIGIN_X + sy0) as u16,
                top: (SCALED_ORIGIN_Y + SCALED_SHORT - sx1) as u16,
                width: (sy1 - sy0) as u16,
                height: (sx1 - sx0) as u16,
            },
            panel_pixel(color),
        );
    }
}

impl OriginDimensions for T114Display {
    fn size(&self) -> Size {
        Size::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32)
    }
}

impl DrawTarget for T114Display {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.fill_area(&Rectangle::new(point, Size::new(1, 1)), color);
        }
        Ok(())
    }

    /// Overridden because the default sends every pixel through `draw_iter`, and a clear is a
    /// `fill_solid` over the whole canvas: thousands of rectangle mappings for one repaint.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.fill_area(area, color);
        Ok(())
    }

    /// The letterbox rows lie outside the canvas, so a clear is the only thing that writes them.
    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.frame.fill_all(panel_pixel(color));
        Ok(())
    }
}

/// The battery probe and the divider gate travel together, because a read without the gate is a
/// read of a disconnected divider.
pub struct T114Battery {
    adc: Saadc<'static, 1>,
    gate: Output<'static>,
}

pub struct T114DeferredHardware {
    radio_bus: Peri<'static, peripherals::TWISPI0>,
    radio_sck: Peri<'static, peripherals::P0_19>,
    radio_miso: Peri<'static, peripherals::P0_23>,
    radio_mosi: Peri<'static, peripherals::P0_22>,
    radio_cs: Peri<'static, peripherals::P0_24>,
    radio_busy: Peri<'static, peripherals::P0_17>,
    radio_dio1: Peri<'static, peripherals::P0_20>,
    radio_reset: Peri<'static, peripherals::P0_25>,
    panel_bus: Peri<'static, peripherals::SPI3>,
    panel_sck: Peri<'static, peripherals::P1_08>,
    panel_mosi: Peri<'static, peripherals::P1_09>,
    panel_cs: Peri<'static, peripherals::P0_11>,
    panel_dc: Peri<'static, peripherals::P0_12>,
    panel_reset: Peri<'static, peripherals::P0_02>,
    panel_power: Peri<'static, peripherals::P0_03>,
    panel_backlight: Peri<'static, peripherals::P0_15>,
    button: Peri<'static, peripherals::P1_10>,
}

/// The T114's board half: pins, panel, and battery. Everything past it is the shared nRF52840
/// firmware.
pub struct HeltecT114Board;

impl Nrf52840Board for HeltecT114Board {
    const ANNOUNCE_APP_DATA: &'static [u8] = ANNOUNCE_APP_DATA;
    const NODE_ANNOUNCE_APP_DATA: &'static [u8] = NODE_ANNOUNCE_APP_DATA;
    const USB_INTERFACE_ID: InterfaceId = USB_INTERFACE_ID;
    const USB_PRODUCT: &'static str = "Personal Hopspot (T114)";
    const USB_SERIAL_NUMBER: &'static str = "PERSONAL-RNS-T114-HOP";
    /// A backlit colour panel redraws for free; a charge glyph that never blinks reads as broken.
    const ANIMATION_CLOCK: AnimationClock = AnimationClock::Running;

    type Battery = T114Battery;
    type Deferred = T114DeferredHardware;
    type Display = T114Display;
    type Illumination = Output<'static>;

    fn claim<Identities>(
        vaults: impl FnOnce(&mut Nvmc<'static>, &mut Rng<'static, Blocking>) -> Identities,
    ) -> (Identities, EarlyHardware<Self::Battery, Self::Deferred>) {
        let peripherals = embassy_nrf::init(board::platform_config());
        let identities = board::read_identity_vaults(peripherals.NVMC, peripherals.RNG, vaults);

        // The green LED sits between the 3V3 rail and P1.03, so it is active low; high is dark.
        let status_led = Output::new(peripherals.P1_03, Level::High, OutputDrive::Standard);

        board::apply_interrupt_priorities();
        let usb = board::usb_hardware(peripherals.USBD);

        // Battery sense: VBAT on a 390k/100k divider into AIN2 (P0.04), disconnected until the
        // P0.06 gate is driven high, sampled against the 3.0 V internal full scale, so
        // VBAT_mV = raw * 14700 / 4096.
        let mut battery_channel = ChannelConfig::single_ended(peripherals.P0_04);
        battery_channel.reference = Reference::INTERNAL;
        battery_channel.gain = Gain::GAIN1_5;
        let battery = T114Battery {
            adc: Saadc::new(
                peripherals.SAADC,
                Irqs,
                SaadcConfig::default(),
                [battery_channel],
            ),
            gate: Output::new(peripherals.P0_06, Level::Low, OutputDrive::Standard),
        };

        let hardware = EarlyHardware {
            usb,
            battery,
            status_led,
            deferred: T114DeferredHardware {
                radio_bus: peripherals.TWISPI0,
                radio_sck: peripherals.P0_19,
                radio_miso: peripherals.P0_23,
                radio_mosi: peripherals.P0_22,
                radio_cs: peripherals.P0_24,
                radio_busy: peripherals.P0_17,
                radio_dio1: peripherals.P0_20,
                radio_reset: peripherals.P0_25,
                panel_bus: peripherals.SPI3,
                panel_sck: peripherals.P1_08,
                panel_mosi: peripherals.P1_09,
                panel_cs: peripherals.P0_11,
                panel_dc: peripherals.P0_12,
                panel_reset: peripherals.P0_02,
                panel_power: peripherals.P0_03,
                panel_backlight: peripherals.P0_15,
                button: peripherals.P1_10,
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
            deferred.radio_miso,
            deferred.radio_mosi,
            radio_spim_config,
        );
        let radio_cs = Output::new(deferred.radio_cs, Level::High, OutputDrive::Standard);
        let radio_spi = ExclusiveDevice::new(radio_bus, radio_cs, Delay).unwrap();
        let radio_busy = Input::new(deferred.radio_busy, Pull::None);
        let radio_dio1 = Input::new(deferred.radio_dio1, Pull::None);
        let radio_reset = Output::new(deferred.radio_reset, Level::High, OutputDrive::Standard);
        // No discrete crystal: DIO3 powers the 32 MHz TCXO at 1.8 V, DIO2 drives the antenna
        // switch, and L11 fits the radio's DC-DC. Identical configuration to the T-Echo.
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
            },
        );

        // The panel rail is a P-FET gated by P0.03, active low. Power it, give the glass the same
        // settle the T-Echo grants its panel, then initialise the controller.
        let panel_rail = Output::new(deferred.panel_power, Level::Low, OutputDrive::Standard);
        Timer::after(Duration::from_millis(150)).await;
        let mut panel_spim_config = spim::Config::default();
        // SPIM0 through SPIM2 top out at 8 MHz on this part; SPIM3 reaches 32 MHz, which brings a
        // full 64,800 byte frame push from about 65 ms of bus time down to about 16 ms.
        panel_spim_config.frequency = spim::Frequency::M32;
        let panel_bus = Spim::new_txonly(
            deferred.panel_bus,
            Irqs,
            deferred.panel_sck,
            deferred.panel_mosi,
            panel_spim_config,
        );
        let panel_cs = Output::new(deferred.panel_cs, Level::High, OutputDrive::Standard);
        let panel_dc = Output::new(deferred.panel_dc, Level::Low, OutputDrive::Standard);
        let panel_reset = Output::new(deferred.panel_reset, Level::High, OutputDrive::Standard);
        let panel_spi = ExclusiveDevice::new(panel_bus, panel_cs, Delay).unwrap();
        let display = St7789::new(
            panel_spi,
            panel_dc,
            panel_reset,
            Delay,
            PanelConfig {
                rotation: PanelRotation::Clockwise,
                color_order: ColorOrder::Rgb,
                inversion: PixelInversion::Inverted,
                origin: PANEL_ORIGIN,
                palette: PALETTE,
            },
        )
        .ok()
        .map(|panel| T114Display {
            panel,
            frame: PanelFrame::new(),
            presented: None,
            _rail: panel_rail,
        });

        RuntimeHardware {
            radio,
            display,
            button: Input::new(deferred.button, Pull::Up),
            // The backlight is a P-FET gated by P0.15, active low; high is dark until a press.
            illumination: Output::new(
                deferred.panel_backlight,
                Level::High,
                OutputDrive::Standard,
            ),
        }
    }

    async fn battery_millivolts(battery: &mut Self::Battery) -> Option<u32> {
        battery.gate.set_high();
        Timer::after(ADC_GATE_SETTLE).await;
        let mut sample = [0i16; 1];
        battery.adc.sample(&mut sample).await;
        battery.gate.set_low();
        Some((sample[0].max(0) as u32) * VBAT_FULL_SCALE_MV / SAADC_FULL_SCALE_COUNTS)
    }

    /// A backlit panel has no refresh budget: it either shows the frame it was given or a frame
    /// it was given earlier. Urgency is the e-ink's problem; a changed frame goes out on the spot.
    fn present(display: &mut Self::Display, _now_ms: u64, _urgency: &hopspot::EinkRefreshUrgency) {
        let hash = frame_hash(display.frame.buffer());
        if display.presented == Some(hash) {
            return;
        }
        if display.panel.present(&display.frame).is_ok() {
            display.presented = Some(hash);
        }
    }

    async fn drive_illumination(backlight: Self::Illumination) -> ! {
        input::drive_panel_light(backlight, Level::Low, BACKLIGHT_HOLD).await
    }
}

pub async fn run(spawner: Spawner) -> ! {
    crate::firmware::run::<HeltecT114Board>(spawner).await
}
