use esp_hal::gpio::{Input, InputConfig};

use embassy_executor::Spawner;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::Pixel;

use personal_rns::interfaces::InterfaceId;

use personal_hopspot_core as screen;

use crate::s3::{
    self, BoardDisplay, BoardFace, Esp32S3Board, S3BoardHardware, S3InterfaceHardware,
    S3ManifoldHardware,
};

/// This board's USB-auto interface id (the always-present top-level wire on pool slot 0).
const USB_INTERFACE_ID: InterfaceId = InterfaceId::new(*b"thalowat");

/// This node's `lxmf.delivery` announce app_data: `msgpack([display_name, stamp_cost])`
/// = `fixarray(2)` ‖ `bin8("Personal Hopspot T-Halow")` ‖ `nil`, the shape LXMF apps parse.
const ANNOUNCE_APP_DATA: &[u8] = b"\x92\xc4\x18Personal Hopspot T-Halow\xc0";
const NODE_ANNOUNCE_APP_DATA: &[u8] = b"Personal Hopspot T-Halow";

/// The T-Halow carries no display, but [`Esp32S3Board::Display`] is a mandatory associated type:
/// the shared render loop draws into this sink and every pixel lands nowhere. `initialized: false`
/// keeps the loop on its render-unavailable path, so the cost is one skipped branch per tick.
pub struct HeadlessDisplay;

impl OriginDimensions for HeadlessDisplay {
    fn size(&self) -> Size {
        // The renderer composes for the fleet-standard 128x64 canvas; give it that geometry.
        Size::new(128, 64)
    }
}

impl DrawTarget for HeadlessDisplay {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, _pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        Ok(())
    }
}

/// No battery sense is wired on the T-Halow; the gauge reads unknown and the UI shows no cell.
pub struct NoBattery;

impl screen::BatterySource for NoBattery {
    fn read_millivolts(&mut self) -> Option<u32> {
        None
    }

    fn is_charging(&mut self) -> bool {
        false
    }
}

/// The LilyGO T-Halow's board half: an ESP32-S3 N16R8 (16 MB flash, 8 MB Octal PSRAM — the same
/// SiP profile as the Heltec V4-R8) carrying a Taixin TX-AH Wi-Fi HaLow module on a UART
/// (GPIO4 module → ESP32, GPIO5 ESP32 → module, 115200 8N1), no display, no SX1262. Bring-up
/// hands the split async UART to the `halow_at` interface; everything past it is the shared
/// [`s3`] core.
pub struct THalowBoard;

impl Esp32S3Board for THalowBoard {
    const ANNOUNCE_APP_DATA: &'static [u8] = ANNOUNCE_APP_DATA;
    const NODE_ANNOUNCE_APP_DATA: &'static [u8] = NODE_ANNOUNCE_APP_DATA;
    const BOOT_BANNER: &'static str = "HOPSPOT_T_HALOW";
    const USB_INTERFACE_ID: InterfaceId = USB_INTERFACE_ID;
    const FLASH_LAYOUT: screen::HopspotS3FlashLayout = screen::S3_16_MIB_FLASH_LAYOUT;
    type Display = HeadlessDisplay;
    type Battery = NoBattery;

    fn flush(_display: &mut Self::Display) {}

    fn set_display_awake(_display: &mut Self::Display, _awake: bool) {}

    async fn bringup(
        p: esp_hal::peripherals::Peripherals,
    ) -> S3BoardHardware<Self::Display, Self::Battery> {
        // Octal 8 MiB at 40 MHz, split between a private low engine window and a global high
        // `esp_alloc` window — the same N16R8 boot profile as the Heltec V4-R8.
        let (sw_int1, timebase, rtc) = s3::boot_common!(
            p,
            Self::BOOT_BANNER,
            ::esp_hal::psram::PsramConfig {
                mode: ::esp_hal::psram::PsramMode::OctalSpi,
                size: ::esp_hal::psram::PsramSize::Size(8 * 1024 * 1024),
                ram_frequency: ::esp_hal::psram::SpiRamFreq::Freq40m,
                ..::core::default::Default::default()
            }
        );

        log::info!("headless board: no display, battery sense, or SX1262 to bring up");

        // The HaLow module's AT console. Pin roles per LilyGO's own AT_test sketch:
        // GPIO4 is the ESP32's RX (module talks on it), GPIO5 the ESP32's TX. The module boots
        // on its own (~2 s to banner); the interface's init resets it anyway.
        #[cfg(feature = "halow-at")]
        let halow_uart = {
            let uart = esp_hal::uart::Uart::new(p.UART1, esp_hal::uart::Config::default())
                .expect("halow uart1")
                .with_rx(p.GPIO4)
                .with_tx(p.GPIO5)
                .into_async();
            uart.split()
        };

        S3BoardHardware {
            face: BoardFace {
                display: BoardDisplay {
                    device: HeadlessDisplay,
                    initialized: false,
                },
                battery: NoBattery,
                button: Input::new(
                    p.GPIO0,
                    InputConfig::default().with_pull(esp_hal::gpio::Pull::Up),
                ),
            },
            interface_hardware: S3InterfaceHardware {
                usb_device: p.USB_DEVICE,
                #[cfg(feature = "halow-at")]
                halow_uart,
                #[cfg(feature = "wifi-auto")]
                wifi: p.WIFI,
                #[cfg(feature = "bluetooth-auto")]
                bluetooth: p.BT,
            },
            manifold: S3ManifoldHardware {
                cpu_control: p.CPU_CTRL,
                software_interrupt: sw_int1,
                timebase,
                rtc,
            },
        }
    }
}

pub async fn run(spawner: Spawner) {
    s3::run::<THalowBoard>(spawner).await
}
