// SPDX-License-Identifier: MPL-2.0

//! Desktop-wide eyedropper / color picker mode.
//!
//! This module provides a one-shot screen-capture API and the
//! [`PickerController`] that manages the interactive picking session.

use ::image::EncodableLayout;

mod controller;
pub use controller::{PickerController, PickerState};
pub mod capture;
mod wayland;

/// Pixel data from a single captured output.
///
/// Matches `xdg-desktop-portal-cosmic`'s `ScreenshotImage`:
/// stores both the `RgbaImage` (for pixel sampling and PNG export)
/// and a pre-built GPU `Handle` (for efficient overlay rendering).
///
/// All position / size fields are in **logical** (compositor) coordinates
/// unless stated otherwise.  Multiply by the per-axis scale factor
/// (`width / logical_width`) to obtain buffer-pixel coordinates.
#[derive(Clone)]
pub struct CapturedOutput {
    /// Connector name (e.g. `"DP-1"`, `"eDP-1"`).
    pub name: String,
    /// RGBA pixel data (R, G, B, A bytes, row-major, top-left origin).
    /// Matches the portal's `ScreenshotImage.rgba`.
    pub rgba: image::RgbaImage,
    /// Pre-built GPU texture handle for this output.
    /// Built on the capture thread to avoid blocking the iced event loop.
    /// Matches the portal's `ScreenshotImage.handle`.
    pub image_handle: cosmic::widget::image::Handle,
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

impl std::fmt::Debug for CapturedOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedOutput")
            .field("name", &self.name)
            .field("rgba_size", &self.rgba.as_bytes().len())
            .field("image_handle", &"Some(Handle)")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("logical_width", &self.logical_width)
            .field("logical_height", &self.logical_height)
            .field("pos_x", &self.pos_x)
            .field("pos_y", &self.pos_y)
            .finish()
    }
}
impl CapturedOutput {
    /// Sample a pixel at the given buffer-local (x, y) coordinate.
    ///
    /// Returns `None` if the coordinate is out of range.  Returns the pixel
    /// as `(R, G, B)`.
    pub fn pixel_at(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        let pixel = self.rgba.get_pixel_checked(x, y)?;
        Some((pixel[0], pixel[1], pixel[2]))
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
    #[must_use]
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Returns the RGB string (e.g. `rgb(255, 136, 0)`).
    #[must_use]
    pub fn rgb(&self) -> String {
        format!("rgb({}, {}, {})", self.r, self.g, self.b)
    }

    /// Returns the HSL string.
    #[must_use]
    pub fn hsl(&self) -> String {
        let (h, s, l) = self.hsl_values();
        format!("hsl({:.0}, {:.0}%, {:.0}%)", h, s * 100.0, l * 100.0)
    }

    /// Compute HSL from sRGB.
    #[allow(
        clippy::float_cmp,
        clippy::manual_midpoint,
        clippy::trivially_copy_pass_by_ref
    )]
    fn hsl_values(&self) -> (f64, f64, f64) {
        let red = f64::from(self.r) / 255.0;
        let green = f64::from(self.g) / 255.0;
        let blue = f64::from(self.b) / 255.0;

        let max_component = red.max(green).max(blue);
        let min_component = red.min(green).min(blue);
        let delta = max_component - min_component;

        let lightness = (max_component + min_component) / 2.0;

        if delta == 0.0 {
            return (0.0, 0.0, lightness);
        }

        let saturation = if lightness > 0.5 {
            delta / (2.0 - max_component - min_component)
        } else {
            delta / (max_component + min_component)
        };

        let hue = if max_component == red {
            (green - blue) / delta + if green < blue { 6.0 } else { 0.0 }
        } else if max_component == green {
            (blue - red) / delta + 2.0
        } else {
            (red - green) / delta + 4.0
        };

        (hue * 60.0, saturation, lightness)
    }
}

impl From<Color> for (String, String, String) {
    fn from(color: Color) -> Self {
        (color.hex(), color.rgb(), color.hsl())
    }
}

/// Capture all connected outputs using the XDG Desktop Portal Screenshot API.
///
/// This is the same approach Flameshot uses:
/// [`crate::picker::wayland::capture_outputs`] calls
/// `org.freedesktop.portal.Screenshot` with `interactive=false` — a single
/// portal D-Bus call that returns a full-desktop image URI.  The image is
/// then cropped per-output using Wayland output geometry.
///
/// Compared to the old `ScreenCast` + `PipeWire` approach, this is much simpler:
/// no `PipeWire`, no sessions, no restore tokens.  Permission persistence
/// is handled automatically by the portal ("Remember" checkbox on first
/// prompt).
pub use wayland::capture_outputs;
