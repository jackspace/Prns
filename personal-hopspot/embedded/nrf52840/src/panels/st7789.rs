use embedded_graphics::pixelcolor::{IntoStorage, Rgb565};
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{Error as _, ErrorKind, OutputPin};
use embedded_hal::spi::SpiDevice;

pub const WIDTH: u16 = 240;
pub const HEIGHT: u16 = 135;

/// The controller carries 240x320 of frame memory for a panel that shows 135x240 of it. Every
/// landscape rotation sets the row/column exchange bit, which puts the column counter on the 320
/// side and the row counter on the 240 side, so a window is addressed as `column` along the image's
/// 240 pixel axis and `row` along its 135 pixel axis.
const RAM_COLUMN_EXTENT: u16 = 320;
const RAM_ROW_EXTENT: u16 = 240;

const _: () = assert!(WIDTH <= RAM_COLUMN_EXTENT && HEIGHT <= RAM_ROW_EXTENT);

const SW_RESET: u8 = 0x01;
const SLEEP_OUT: u8 = 0x11;
const NORMAL_DISPLAY_MODE_ON: u8 = 0x13;
const DISPLAY_INVERSION_OFF: u8 = 0x20;
const DISPLAY_INVERSION_ON: u8 = 0x21;
const DISPLAY_ON: u8 = 0x29;
const COLUMN_ADDRESS_SET: u8 = 0x2A;
const ROW_ADDRESS_SET: u8 = 0x2B;
const MEMORY_WRITE: u8 = 0x2C;
const MEMORY_DATA_ACCESS_CONTROL: u8 = 0x36;
const INTERFACE_PIXEL_FORMAT: u8 = 0x3A;

/// `COLMOD`: 65k colour, 16 bits per pixel on both the RGB and the control interface.
const PIXEL_FORMAT_RGB565: u8 = 0x55;

const MADCTL_ROW_ADDRESS_ORDER: u8 = 0x80;
const MADCTL_COLUMN_ADDRESS_ORDER: u8 = 0x40;
const MADCTL_ROW_COLUMN_EXCHANGE: u8 = 0x20;
const MADCTL_BGR: u8 = 0x08;

const RESET_PULSE_US: u32 = 10_000;
/// A hardware reset, a software reset, and a sleep exit each leave the controller deaf for a while:
/// the datasheet's 120 ms, with margin on the software reset because it can arrive while the
/// controller is still asleep and then wants the full sleep-exit wait.
const RESET_SETTLE_US: u32 = 120_000;
const SW_RESET_SETTLE_US: u32 = 150_000;
const SLEEP_OUT_SETTLE_US: u32 = 120_000;
const COMMAND_SETTLE_US: u32 = 10_000;

const PIXELS_PER_BYTE: usize = 8;
const LEFTMOST_PIXEL_MASK: u8 = 0b1000_0000;
const BYTES_PER_PIXEL: usize = 2;
const ROW_BYTES: usize = WIDTH as usize / PIXELS_PER_BYTE;
const FRAME_BYTES: usize = ROW_BYTES * HEIGHT as usize;

const _: () = assert!(WIDTH as usize % PIXELS_PER_BYTE == 0);

/// A whole RGB565 frame is 64,800 bytes, which this part cannot spare, so the frame goes out in
/// bands: 3,840 bytes of transient buffer against 4,050 bytes of resident shadow. 135 rows is not a
/// whole number of bands, so the last one is short.
const PUSH_BAND_ROWS: usize = 8;
const PUSH_ROW_BYTES: usize = WIDTH as usize * BYTES_PER_PIXEL;
const PUSH_BAND_BYTES: usize = PUSH_BAND_ROWS * PUSH_ROW_BYTES;

#[derive(Debug)]
pub enum St7789Error<E> {
    Spi(E),
    ResetPin(ErrorKind),
    DataCommandPin(ErrorKind),
}

pub enum PixelState {
    Lit,
    Dark,
}

impl PixelState {
    const fn packed_byte(&self) -> u8 {
        match self {
            Self::Lit => u8::MAX,
            Self::Dark => 0,
        }
    }
}

/// Which way the landscape image lies on the natively portrait glass. Both cases exchange rows and
/// columns and differ only in the axis they mirror, so they are the same picture 180 degrees apart.
/// The rotation and the [`PanelOrigin`] travel together: turning the image over moves the visible
/// window to the other end of the unused frame memory.
pub enum PanelRotation {
    Clockwise,
    CounterClockwise,
}

impl PanelRotation {
    fn madctl(&self) -> u8 {
        MADCTL_ROW_COLUMN_EXCHANGE
            | match self {
                Self::Clockwise => MADCTL_ROW_ADDRESS_ORDER,
                Self::CounterClockwise => MADCTL_COLUMN_ADDRESS_ORDER,
            }
    }
}

/// The order the module's colour filters are laid out in. Getting this wrong swaps red and blue.
pub enum ColorOrder {
    Rgb,
    Bgr,
}

impl ColorOrder {
    fn madctl(&self) -> u8 {
        match self {
            Self::Rgb => 0,
            Self::Bgr => MADCTL_BGR,
        }
    }
}

/// Whether the module drives its pixels inverted. The normally-black IPS glass this controller is
/// usually paired with wants `INVON`; a normally-white part does not. Getting it wrong renders a
/// photographic negative.
pub enum PixelInversion {
    Inverted,
    Normal,
}

impl PixelInversion {
    fn command(&self) -> u8 {
        match self {
            Self::Inverted => DISPLAY_INVERSION_ON,
            Self::Normal => DISPLAY_INVERSION_OFF,
        }
    }
}

/// Where the visible window sits in frame memory. A centred module lands near
/// `(RAM_COLUMN_EXTENT - WIDTH) / 2` and `(RAM_ROW_EXTENT - HEIGHT) / 2`, but 135 does not halve
/// evenly and not every module is centred, so the pair is a property of the glass.
pub struct PanelOrigin {
    column: u16,
    row: u16,
}

impl PanelOrigin {
    /// Bind the result to a `const` in the board module. The window has to sit inside frame memory,
    /// and a build failure reports that better than an image shifted off the glass does.
    pub const fn new(column: u16, row: u16) -> Self {
        assert!(column as usize + WIDTH as usize <= RAM_COLUMN_EXTENT as usize);
        assert!(row as usize + HEIGHT as usize <= RAM_ROW_EXTENT as usize);
        Self { column, row }
    }
}

/// The two colours the two-valued UI is expanded into on the way to the panel.
pub struct Palette {
    pub lit: Rgb565,
    pub dark: Rgb565,
}

impl Palette {
    fn wire_bytes(&self, state: PixelState) -> [u8; BYTES_PER_PIXEL] {
        match state {
            PixelState::Lit => self.lit,
            PixelState::Dark => self.dark,
        }
        .into_storage()
        .to_be_bytes()
    }
}

pub struct PanelConfig {
    pub rotation: PanelRotation,
    pub color_order: ColorOrder,
    pub inversion: PixelInversion,
    pub origin: PanelOrigin,
    pub palette: Palette,
}

/// A rectangle in panel space. Filling one clips it to the glass.
pub struct PanelRect {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
}

/// The 1 bpp shadow of the panel, packed eight pixels to a byte with the leftmost pixel in the most
/// significant bit.
pub struct PanelFrame {
    bits: [u8; FRAME_BYTES],
}

impl PanelFrame {
    pub const fn new() -> Self {
        Self {
            bits: [PixelState::Dark.packed_byte(); FRAME_BYTES],
        }
    }

    /// The packed shadow, for callers that dedup frames by hashing them before pushing.
    pub fn buffer(&self) -> &[u8] {
        &self.bits
    }

    pub fn fill_all(&mut self, state: PixelState) {
        self.bits.fill(state.packed_byte());
    }

    pub fn fill(&mut self, area: &PanelRect, state: PixelState) {
        let left = area.left.min(WIDTH);
        let right = area.left.saturating_add(area.width).min(WIDTH);
        let top = area.top.min(HEIGHT);
        let bottom = area.top.saturating_add(area.height).min(HEIGHT);
        for row in top..bottom {
            let base = row as usize * ROW_BYTES;
            for column in left..right {
                let column = column as usize;
                let mask = LEFTMOST_PIXEL_MASK >> (column % PIXELS_PER_BYTE);
                let byte = &mut self.bits[base + column / PIXELS_PER_BYTE];
                match state {
                    PixelState::Lit => *byte |= mask,
                    PixelState::Dark => *byte &= !mask,
                }
            }
        }
    }

    fn row(&self, row: usize) -> &[u8] {
        let base = row * ROW_BYTES;
        &self.bits[base..base + ROW_BYTES]
    }
}

impl Default for PanelFrame {
    fn default() -> Self {
        Self::new()
    }
}

pub struct St7789<SPI, DC, RST, DELAY> {
    spi: SPI,
    dc: DC,
    rst: RST,
    delay: DELAY,
    config: PanelConfig,
}

impl<SPI, DC, RST, DELAY> St7789<SPI, DC, RST, DELAY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    RST: OutputPin,
    DELAY: DelayNs,
{
    pub fn new(
        spi: SPI,
        dc: DC,
        rst: RST,
        delay: DELAY,
        config: PanelConfig,
    ) -> Result<Self, St7789Error<SPI::Error>> {
        let mut driver = Self {
            spi,
            dc,
            rst,
            delay,
            config,
        };
        driver.reset()?;
        driver.init()?;
        Ok(driver)
    }

    pub fn present(&mut self, frame: &PanelFrame) -> Result<(), St7789Error<SPI::Error>> {
        let lit = self.config.palette.wire_bytes(PixelState::Lit);
        let dark = self.config.palette.wire_bytes(PixelState::Dark);
        self.push_bands(|first_row, band| {
            for (offset, line) in band.chunks_exact_mut(PUSH_ROW_BYTES).enumerate() {
                let source = frame.row(first_row + offset);
                for (column, pixel) in line.chunks_exact_mut(BYTES_PER_PIXEL).enumerate() {
                    let mask = LEFTMOST_PIXEL_MASK >> (column % PIXELS_PER_BYTE);
                    let lit_here = source[column / PIXELS_PER_BYTE] & mask != 0;
                    pixel.copy_from_slice(if lit_here { &lit } else { &dark });
                }
            }
        })
    }

    fn init(&mut self) -> Result<(), St7789Error<SPI::Error>> {
        self.cmd(SW_RESET)?;
        self.delay.delay_us(SW_RESET_SETTLE_US);
        self.cmd(SLEEP_OUT)?;
        self.delay.delay_us(SLEEP_OUT_SETTLE_US);
        self.cmd_data(INTERFACE_PIXEL_FORMAT, &[PIXEL_FORMAT_RGB565])?;
        self.delay.delay_us(COMMAND_SETTLE_US);
        let madctl = self.config.rotation.madctl() | self.config.color_order.madctl();
        self.cmd_data(MEMORY_DATA_ACCESS_CONTROL, &[madctl])?;
        let inversion = self.config.inversion.command();
        self.cmd(inversion)?;
        self.delay.delay_us(COMMAND_SETTLE_US);
        self.cmd(NORMAL_DISPLAY_MODE_ON)?;
        self.delay.delay_us(COMMAND_SETTLE_US);
        // Frame memory powers up undefined, so it has to hold a known image before the panel is
        // switched on, or the first thing the glass shows is noise.
        self.blank(PixelState::Dark)?;
        self.cmd(DISPLAY_ON)?;
        self.delay.delay_us(COMMAND_SETTLE_US);
        Ok(())
    }

    fn blank(&mut self, state: PixelState) -> Result<(), St7789Error<SPI::Error>> {
        let color = self.config.palette.wire_bytes(state);
        self.push_bands(|_, band| {
            for pixel in band.chunks_exact_mut(BYTES_PER_PIXEL) {
                pixel.copy_from_slice(&color);
            }
        })
    }

    /// The only path that writes frame memory. `fill` receives the panel row the band starts at and
    /// the exact slice that will go out, so a short final band cannot be over-read or over-written.
    fn push_bands<F>(&mut self, mut fill: F) -> Result<(), St7789Error<SPI::Error>>
    where
        F: FnMut(usize, &mut [u8]),
    {
        self.set_address_window()?;
        self.cmd(MEMORY_WRITE)?;
        self.data_mode()?;
        let mut band = [0u8; PUSH_BAND_BYTES];
        for first_row in (0..HEIGHT as usize).step_by(PUSH_BAND_ROWS) {
            let rows = PUSH_BAND_ROWS.min(HEIGHT as usize - first_row);
            let pushed = rows * PUSH_ROW_BYTES;
            fill(first_row, &mut band[..pushed]);
            self.spi.write(&band[..pushed]).map_err(St7789Error::Spi)?;
        }
        Ok(())
    }

    fn set_address_window(&mut self) -> Result<(), St7789Error<SPI::Error>> {
        let first_column = self.config.origin.column;
        let first_row = self.config.origin.row;
        self.cmd_data(
            COLUMN_ADDRESS_SET,
            &window_bounds(first_column, first_column + WIDTH - 1),
        )?;
        self.cmd_data(
            ROW_ADDRESS_SET,
            &window_bounds(first_row, first_row + HEIGHT - 1),
        )?;
        Ok(())
    }

    fn cmd(&mut self, command: u8) -> Result<(), St7789Error<SPI::Error>> {
        self.command_mode()?;
        self.spi.write(&[command]).map_err(St7789Error::Spi)
    }

    fn cmd_data(&mut self, command: u8, data: &[u8]) -> Result<(), St7789Error<SPI::Error>> {
        self.cmd(command)?;
        self.data_mode()?;
        self.spi.write(data).map_err(St7789Error::Spi)
    }

    fn command_mode(&mut self) -> Result<(), St7789Error<SPI::Error>> {
        self.dc
            .set_low()
            .map_err(|error| St7789Error::DataCommandPin(error.kind()))
    }

    fn data_mode(&mut self) -> Result<(), St7789Error<SPI::Error>> {
        self.dc
            .set_high()
            .map_err(|error| St7789Error::DataCommandPin(error.kind()))
    }

    fn reset(&mut self) -> Result<(), St7789Error<SPI::Error>> {
        let pin =
            |error: RST::Error| -> St7789Error<SPI::Error> { St7789Error::ResetPin(error.kind()) };
        self.rst.set_high().map_err(pin)?;
        self.delay.delay_us(RESET_PULSE_US);
        self.rst.set_low().map_err(pin)?;
        self.delay.delay_us(RESET_PULSE_US);
        self.rst.set_high().map_err(pin)?;
        self.delay.delay_us(RESET_SETTLE_US);
        Ok(())
    }
}

fn window_bounds(first: u16, last: u16) -> [u8; 4] {
    let [first_high, first_low] = first.to_be_bytes();
    let [last_high, last_low] = last.to_be_bytes();
    [first_high, first_low, last_high, last_low]
}
