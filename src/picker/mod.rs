// SPDX-License-Identifier: MPL-2.0

//! Desktop-wide eyedropper / color picker mode.
//!
//! This module provides a one-shot screen-capture API and the
//! [`PickerController`] that manages the interactive picking session.

mod controller;
pub use controller::{PickerController, PickerState};
mod wayland;

/// Pixel data from a single captured output.
///
/// The `data` vector contains raw 32-bit pixel data with one of the
/// compositor-advertised SHM formats (`Xbgr8888` or `Abgr8888`).  Both
/// layouts store the same RGB byte order in memory (little-endian):
///
/// | byte 0 | byte 1 | byte 2 | byte 3 |
/// |--------|--------|--------|--------|
/// | R      | G      | B      | X / A  |
///
/// This matches the `image` crate's `Rgba8` layout (4 bytes per pixel:
/// R, G, B, A).
///
/// ## Coordinate system
///
/// All position / size fields are in **logical** (compositor) coordinates
/// unless stated otherwise.  Multiply by the per-axis scale factor
/// (`width / logical_width`) to obtain buffer-pixel coordinates.
#[derive(Clone, Debug)]
pub struct CapturedOutput {
    /// Connector name (e.g. `"DP-1"`, `"eDP-1"`).
    pub name: String,
    /// Raw pixel data, row-major, top-left origin.  Pixel format is
    /// `Xbgr8888` or `Abgr8888`: byte 0 = R, byte 1 = G, byte 2 = B,
    /// byte 3 = X or A.
    pub data: Vec<u8>,
    /// Pixel width of the captured image.
    pub width: u32,
    /// Pixel height of the captured image.
    pub height: u32,
    /// Width of this output in logical (compositor) coordinates.
    ///
    /// Together with `width` this defines the scale factor for
    /// cursor-to-pixel mapping.
    pub logical_width: u32,
    /// Height of this output in logical (compositor) coordinates.
    pub logical_height: u32,
    /// X offset of this output in compositor (logical) coordinate space.
    pub pos_x: i32,
    /// Y offset of this output in compositor (logical) coordinate space.
    pub pos_y: i32,
}

impl CapturedOutput {
    /// Sample a pixel at the given buffer-local (x, y) coordinate.
    ///
    /// Returns `None` if the coordinate is out of range.  Returns the pixel
    /// as `(R, G, B)` — the buffer is stored in `Xbgr8888` / `Abgr8888`
    /// format where byte 0 = Red, byte 1 = Green, byte 2 = Blue.
    pub fn pixel_at(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        if idx + 3 >= self.data.len() {
            return None;
        }
        Some((self.data[idx], self.data[idx + 1], self.data[idx + 2]))
    }
}

/// A colour sampled from the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    /// Red component (0-255)
    pub r: u8,
    /// Green component (0-255)
    pub g: u8,
    /// Blue component (0-255)
    pub b: u8,
    /// Alpha component (0-255)
    pub a: u8,
}

impl Color {
    /// Returns the HEX string (e.g. `#FF8800`).
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Returns the RGB string (e.g. `rgb(255, 136, 0)`).
    pub fn rgb(&self) -> String {
        format!("rgb({}, {}, {})", self.r, self.g, self.b)
    }

    /// Returns the HSL string.
    pub fn hsl(&self) -> String {
        let (h, s, l) = self.hsl_values();
        format!("hsl({:.0}, {:.0}%, {:.0}%)", h, s * 100.0, l * 100.0)
    }

    /// Compute HSL from sRGB.
    fn hsl_values(&self) -> (f64, f64, f64) {
        let r = self.r as f64 / 255.0;
        let g = self.g as f64 / 255.0;
        let b = self.b as f64 / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let l = (max + min) / 2.0;

        if delta == 0.0 {
            return (0.0, 0.0, l);
        }

        let s = if l > 0.5 {
            delta / (2.0 - max - min)
        } else {
            delta / (max + min)
        };

        let h = if max == r {
            (g - b) / delta + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };

        (h * 60.0, s, l)
    }
}

impl From<Color> for (String, String, String) {
    fn from(color: Color) -> Self {
        (color.hex(), color.rgb(), color.hsl())
    }
}

/// Capture all connected outputs and return their pixel data.
///
/// This spawns a dedicated OS thread with its own Wayland connection, captures
/// each output via `ext-image-copy-capture-v1`, and returns the results as
/// an async future.  Call via `Task::perform` from the iced event loop.
pub async fn capture_all_outputs() -> Result<Vec<CapturedOutput>, anyhow::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        // Catch panics in the capture thread so we can report the panic
        // message instead of silently dropping the oneshot sender.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wayland::capture_all_outputs_sync()
        }));
        let _ = tx.send(match result {
            Ok(Ok(outputs)) => Ok(outputs),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                eprintln!("[capture] THREAD PANICKED: {msg}");
                Err(anyhow::anyhow!("Capture thread panicked: {msg}"))
            }
        });
    });

    rx.await
        .map_err(|_| anyhow::anyhow!("Capture thread panicked or was cancelled"))?
}
