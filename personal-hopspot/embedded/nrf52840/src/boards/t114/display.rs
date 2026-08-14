//! The T114's 240x135 ST7789V2 colour panel.
//!
//! Every constant here was measured on the board rather than taken from a datasheet, because the
//! datasheet does not carry them. The window origin, the rail polarity and the settle delay are
//! the three that cost the most to discover.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::Peri;
use embassy_time::{Delay, Timer};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_hal_bus::spi::ExclusiveDevice;
use personal_hopspot_core::{MOBILE_DARK_RGBA, MOBILE_LIT_RGBA};

use crate::panels::st7789::{
    ColorOrder, Palette, PanelConfig, PanelOrigin, PanelRotation, PixelInversion, St7789,
};

/// The 135x240 glass sits inside the controller's 240x320 frame memory, so a centred window starts
/// at column (320-240)/2 = 40 and row (240-135)/2 = 52. That derivation and the value measured
/// during bring-up agree, and it is the pairing Meshtastic's driver uses on this exact board.
/// The 105 spare rows are odd, so the window sits 52 above and 53 below. Mirroring to the
/// counter-clockwise rotation moves the row offset to 53; do not change one without the other.
const PANEL_ORIGIN: PanelOrigin = PanelOrigin::new(40, 52);

/// Rgb565 keeps the top 5, 6 and 5 bits of an 8 bit channel.
const RED_BITS_DROPPED: u8 = 8 - 5;
const GREEN_BITS_DROPPED: u8 = 8 - 6;
const BLUE_BITS_DROPPED: u8 = 8 - 5;

/// Core owns the colours; this board owns only the encoding into the panel's wire format.
const PALETTE: Palette = Palette {
    lit: quantised(MOBILE_LIT_RGBA),
    dark: quantised(MOBILE_DARK_RGBA),
};

const fn quantised(rgba: [u8; 4]) -> Rgb565 {
    Rgb565::new(
        rgba[0] >> RED_BITS_DROPPED,
        rgba[1] >> GREEN_BITS_DROPPED,
        rgba[2] >> BLUE_BITS_DROPPED,
    )
}

/// The glass needs this long between rail-up and controller init. Inherited from the T-Echo's
/// bring-up margin rather than from a datasheet number, and not yet trimmed.
const PANEL_SETTLE_MS: u64 = 150;

type T114PanelSpi = ExclusiveDevice<Spim<'static>, Output<'static>, Delay>;
pub(crate) type T114Panel = St7789<T114PanelSpi, Output<'static>, Output<'static>, Delay>;

/// The pins this panel owns, kept together so the board module does not have to know them.
pub(crate) struct PanelPins {
    pub(crate) bus: Peri<'static, peripherals::SPI3>,
    pub(crate) sck: Peri<'static, peripherals::P1_08>,
    pub(crate) mosi: Peri<'static, peripherals::P1_09>,
    pub(crate) cs: Peri<'static, peripherals::P0_11>,
    pub(crate) dc: Peri<'static, peripherals::P0_12>,
    pub(crate) reset: Peri<'static, peripherals::P0_02>,
    pub(crate) power: Peri<'static, peripherals::P0_03>,
}

/// A live panel plus the rail that must outlive it.
pub(crate) struct T114Display {
    pub(crate) panel: T114Panel,
    /// Held, not used: dropping this output would cut power to the glass.
    _rail: Output<'static>,
}

/// Raise the panel rail, let the glass settle, then bring the controller up.
///
/// Returns `None` if the controller refuses initialisation, so a display fault cannot take the
/// radio down with it. A headless T114 is still a useful node.
pub(crate) async fn initialise<IRQ>(pins: PanelPins, irqs: IRQ) -> Option<T114Display>
where
    IRQ: embassy_nrf::interrupt::typelevel::Binding<
            <peripherals::SPI3 as spim::Instance>::Interrupt,
            spim::InterruptHandler<peripherals::SPI3>,
        > + 'static,
{
    // The rail is a P-FET gated by P0.03 and is ACTIVE LOW: driving it low powers the panel.
    let rail = Output::new(pins.power, Level::Low, OutputDrive::Standard);
    Timer::after_millis(PANEL_SETTLE_MS).await;

    let mut config = spim::Config::default();
    // SPIM0 through SPIM2 top out at 8 MHz on this part; SPIM3 reaches 32 MHz, which brings a full
    // 64,800 byte frame push from about 65 ms of bus time down to about 16 ms.
    config.frequency = spim::Frequency::M32;
    let bus = Spim::new_txonly(pins.bus, irqs, pins.sck, pins.mosi, config);

    let cs = Output::new(pins.cs, Level::High, OutputDrive::Standard);
    let dc = Output::new(pins.dc, Level::Low, OutputDrive::Standard);
    let reset = Output::new(pins.reset, Level::High, OutputDrive::Standard);
    let spi = ExclusiveDevice::new(bus, cs, Delay).ok()?;

    St7789::new(
        spi,
        dc,
        reset,
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
    .map(|panel| T114Display { panel, _rail: rail })
}
