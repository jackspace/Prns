//! The nRF52840 platform layer: everything that is true of any board running this firmware under
//! the S140 SoftDevice, plus the seam each board implements. No pin numbers live here; they all
//! belong to `boards/<board>.rs`.

use embassy_nrf::gpio::{Input, Output};
use embassy_nrf::interrupt::{self, InterruptExt, Priority};
use embassy_nrf::mode::Blocking;
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::rng::Rng;
use embassy_nrf::saadc;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::{bind_interrupts, config, peripherals, usb, Peri};
use embassy_time::Delay;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::DrawTarget;
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;

use personal_hopspot_core as hopspot;
use personal_rns::interfaces::InterfaceId;
use personal_rns::radios::sx126x::Sx126x;

bind_interrupts!(pub(crate) struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
    SPIM3 => spim::InterruptHandler<peripherals::SPI3>;
    TWISPI0 => spim::InterruptHandler<peripherals::TWISPI0>;
    SAADC => saadc::InterruptHandler;
});

/// Which SPIM instance drives which bus is a firmware choice rather than a board fact: every SPIM
/// on this part routes to any GPIO through its PSEL registers. Boards pick pins, not instances.
pub(crate) type ExclusiveSpi = ExclusiveDevice<Spim<'static>, Output<'static>, Delay>;

pub(crate) type Radio =
    Sx126x<ExclusiveSpi, Input<'static>, Input<'static>, Output<'static>, Delay>;

/// The SoftDevice reserves interrupt priorities P0, P1, and P4, so no application interrupt may
/// sit there. USB keeps the priority the bring-up validated; the buses and the ADC sit one step
/// below so a BLE radio event can preempt them.
const DRIVER_INTERRUPT_PRIORITY: Priority = Priority::P2;
const BUS_INTERRUPT_PRIORITY: Priority = Priority::P3;

/// Whether this board's panel can afford a moving picture. E-ink pays a panel refresh for every
/// changed pixel, so it pins the clock and its charge glyph never blinks; a backlit panel redraws
/// for free and looks broken when nothing ever moves.
pub(crate) enum AnimationClock {
    Still,
    Running,
}

impl AnimationClock {
    pub(crate) const fn millis(&self, now_ms: u64) -> u64 {
        match self {
            Self::Still => 0,
            Self::Running => now_ms,
        }
    }
}

pub(crate) struct UsbHardware {
    pub(crate) driver: Driver<'static, &'static SoftwareVbusDetect>,
    pub(crate) vbus: &'static SoftwareVbusDetect,
}

/// What a board hands back from the window before `Softdevice::enable`: the parts the firmware
/// needs immediately, and the pins it has claimed but not yet configured.
pub(crate) struct EarlyHardware<Battery, Deferred> {
    pub(crate) usb: UsbHardware,
    pub(crate) battery: Battery,
    pub(crate) status_led: Output<'static>,
    pub(crate) deferred: Deferred,
}

/// What a board hands back once the SoftDevice is up and the remaining buses can be started. A
/// `None` display is a panel that failed to initialise: the firmware keeps running without a face.
pub(crate) struct RuntimeHardware<Display, Illumination> {
    pub(crate) radio: Radio,
    pub(crate) display: Option<Display>,
    pub(crate) button: Input<'static>,
    pub(crate) illumination: Illumination,
}

pub(crate) fn platform_config() -> config::Config {
    let mut config = config::Config::default();
    config.gpiote_interrupt_priority = DRIVER_INTERRUPT_PRIORITY;
    config.time_interrupt_priority = DRIVER_INTERRUPT_PRIORITY;
    config
}

pub(crate) fn apply_interrupt_priorities() {
    interrupt::USBD.set_priority(DRIVER_INTERRUPT_PRIORITY);
    interrupt::SPI2.set_priority(BUS_INTERRUPT_PRIORITY);
    // SPIM3 is the only instance on this part that runs above 8 MHz; a board with a fast panel
    // puts that panel here. A binding for an instance a board never starts costs a vector table
    // entry and nothing else.
    interrupt::SPIM3.set_priority(BUS_INTERRUPT_PRIORITY);
    interrupt::TWISPI0.set_priority(BUS_INTERRUPT_PRIORITY);
    interrupt::SAADC.set_priority(BUS_INTERRUPT_PRIORITY);
}

/// Read the identity vaults in the only window that exists for it. `Softdevice::enable` takes the
/// flash, so a board that does not read its vaults here does not get to read them at all.
pub(crate) fn read_identity_vaults<Identities>(
    nvmc: Peri<'static, peripherals::NVMC>,
    rng: Peri<'static, peripherals::RNG>,
    vaults: impl FnOnce(&mut Nvmc<'static>, &mut Rng<'static, Blocking>) -> Identities,
) -> Identities {
    let mut nvmc = Nvmc::new(nvmc);
    let mut rng = Rng::new_blocking(rng);
    vaults(&mut nvmc, &mut rng)
}

pub(crate) fn usb_hardware(usbd: Peri<'static, peripherals::USBD>) -> UsbHardware {
    static SOFTWARE_VBUS: StaticCell<SoftwareVbusDetect> = StaticCell::new();
    let vbus: &'static SoftwareVbusDetect =
        &*SOFTWARE_VBUS.init(SoftwareVbusDetect::new(true, true));
    UsbHardware {
        driver: Driver::new(usbd, Irqs, vbus),
        vbus,
    }
}

#[allow(async_fn_in_trait)]
pub(crate) trait Nrf52840Board {
    /// This node's `lxmf.delivery` announce app_data: `msgpack([display_name, stamp_cost])`. The
    /// name's length is encoded in the bytes, so a rename has to recount it.
    const ANNOUNCE_APP_DATA: &'static [u8];
    const NODE_ANNOUNCE_APP_DATA: &'static [u8];
    /// This board's USB-auto interface id, the always-present top-level wire.
    const USB_INTERFACE_ID: InterfaceId;
    const USB_PRODUCT: &'static str;
    const USB_SERIAL_NUMBER: &'static str;
    const ANIMATION_CLOCK: AnimationClock;

    type Battery;
    type Deferred;
    type Display: DrawTarget<Color = BinaryColor>;
    type Illumination;

    /// Claim the board's pins and read its identity vaults, before the SoftDevice is enabled.
    /// Runs `vaults` between `embassy_nrf::init` and the first driver that could preempt it.
    fn claim<Identities>(
        vaults: impl FnOnce(&mut Nvmc<'static>, &mut Rng<'static, Blocking>) -> Identities,
    ) -> (Identities, EarlyHardware<Self::Battery, Self::Deferred>);

    /// Start the buses the SoftDevice had to settle first: the radio, the panel, and the button.
    async fn finish(deferred: Self::Deferred)
        -> RuntimeHardware<Self::Display, Self::Illumination>;

    /// This board's cell voltage. Async because the nRF SAADC probe is, which is why no board here
    /// can implement core's synchronous `BatterySource`.
    async fn battery_millivolts(battery: &mut Self::Battery) -> Option<u32>;

    /// Put the drawn frame on the glass, on whatever schedule the panel demands. The board owns
    /// its own refresh policy and its own frame dedup; the loop only knows why it woke.
    fn present(display: &mut Self::Display, now_ms: u64, urgency: &hopspot::EinkRefreshUrgency);

    /// Drive the panel light for the life of the program. It runs as its own task because the
    /// button signals it on the falling edge, before a press has been classified as short or long.
    async fn drive_illumination(illumination: Self::Illumination) -> !;
}
