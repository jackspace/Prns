//! The remote face: the screen kept as pixels even when the panel is absent,
//! asleep, or somewhere else. The render loop composes every frame into a
//! [`FaceFrame`] and publishes it to the shared [`FACE_FRAME`](super::FACE_FRAME);
//! the captive portal serves that snapshot raw and feeds remote taps into the
//! same channel the physical button drives.

use core::cell::RefCell;
use core::convert::Infallible;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embedded_graphics::prelude::*;

use super::*;

/// The face frame is the logical (post-rotation) screen: 64 pixels wide, 128
/// tall, one bit per pixel. Bytes are row-major from the top-left, 8 pixels per
/// byte, most significant bit leftmost: row `y`, column `x` lives in
/// `bytes[y * FACE_FRAME_ROW_BYTES + x / 8]` at bit `7 - x % 8`, and a set bit
/// is a lit pixel.
pub(super) const FACE_FRAME_WIDTH: usize = 64;
pub(super) const FACE_FRAME_HEIGHT: usize = 128;
pub(super) const FACE_FRAME_ROW_BYTES: usize = FACE_FRAME_WIDTH / 8;
pub(super) const FACE_FRAME_BYTES: usize = FACE_FRAME_ROW_BYTES * FACE_FRAME_HEIGHT;

/// One composed 1bpp frame in the [`FACE_FRAME_BYTES`] layout. The render loop
/// draws the whole UI into this before anything touches the physical panel, so
/// the frame exists whether or not the panel does.
pub(super) struct FaceFrame {
    bytes: [u8; FACE_FRAME_BYTES],
}

impl FaceFrame {
    pub(super) const fn new() -> Self {
        Self {
            bytes: [0; FACE_FRAME_BYTES],
        }
    }

    /// Blit onto the physical display, pixel by pixel, which is the same path
    /// the UI's text and rectangles take through a buffered SSD1306 anyway.
    pub(super) fn draw_to<D: DrawTarget<Color = BinaryColor>>(&self, display: &mut D) {
        let pixels = (0..FACE_FRAME_HEIGHT).flat_map(|y| {
            (0..FACE_FRAME_WIDTH).map(move |x| {
                let color = if self.lit(x, y) {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                };
                Pixel(Point::new(x as i32, y as i32), color)
            })
        });
        let _ = display.draw_iter(pixels);
    }

    fn lit(&self, x: usize, y: usize) -> bool {
        self.bytes[y * FACE_FRAME_ROW_BYTES + x / 8] & Self::column_mask(x) != 0
    }

    fn column_mask(x: usize) -> u8 {
        0x80 >> (x % 8)
    }
}

impl OriginDimensions for FaceFrame {
    fn size(&self) -> Size {
        Size::new(FACE_FRAME_WIDTH as u32, FACE_FRAME_HEIGHT as u32)
    }
}

impl DrawTarget for FaceFrame {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let outside = point.x < 0
                || point.y < 0
                || point.x >= FACE_FRAME_WIDTH as i32
                || point.y >= FACE_FRAME_HEIGHT as i32;
            if outside {
                continue;
            }
            let (x, y) = (point.x as usize, point.y as usize);
            let index = y * FACE_FRAME_ROW_BYTES + x / 8;
            if color.is_on() {
                self.bytes[index] |= Self::column_mask(x);
            } else {
                self.bytes[index] &= !Self::column_mask(x);
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.bytes.fill(if color.is_on() { 0xFF } else { 0x00 });
        Ok(())
    }
}

/// The cross-task home of the latest composed frame: the render loop (core 0
/// main task) publishes, the captive-portal HTTP tasks snapshot. A blocking
/// critical-section mutex held only for the 1 KiB copy.
pub(super) struct SharedFaceFrame {
    frame: BlockingMutex<Mtx, RefCell<[u8; FACE_FRAME_BYTES]>>,
}

impl SharedFaceFrame {
    pub(super) const fn new() -> Self {
        Self {
            frame: BlockingMutex::new(RefCell::new([0; FACE_FRAME_BYTES])),
        }
    }

    pub(super) fn publish(&self, frame: &FaceFrame) {
        self.frame
            .lock(|cell| cell.borrow_mut().copy_from_slice(&frame.bytes));
    }

    #[cfg(feature = "wifi-auto")]
    pub(super) fn snapshot(&self, out: &mut [u8; FACE_FRAME_BYTES]) {
        self.frame.lock(|cell| out.copy_from_slice(&*cell.borrow()));
    }
}
