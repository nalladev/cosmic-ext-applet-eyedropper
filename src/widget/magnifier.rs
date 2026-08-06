// SPDX-License-Identifier: MPL-2.0

//! Magnifier lens widget and state.
//!
//! The magnified cells are rendered as a single RGBA texture ([`lens_rgba`])
//! using a precomputed per-zoom lens mask ([`MASKS`]).  The inside/outside
//! circle geometry depends only on the cell size, so it is computed once and
//! then looked up — the GPU work is one textured quad per frame instead of
//! one draw call per cell.  [`MagnifierProgram`] draws the overlay strokes
//! (grid, centre ring, border) on top of the texture.

use std::sync::LazyLock;

use cosmic::iced::mouse;
use cosmic::iced::widget::canvas::{self, Path, Stroke};

/// Number of cells per side of the lens grid (odd, for a centred cell).
pub const GRID_SIZE: usize = 17;

/// Minimum zoom level (cell size in logical pixels).
pub const MIN_ZOOM: f32 = 8.0;

/// Maximum zoom level (cell size in logical pixels).
pub const MAX_ZOOM: f32 = 24.0;

/// Number of discrete zoom levels (`MAX_ZOOM - MIN_ZOOM + 1`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const ZOOM_LEVELS: usize = (MAX_ZOOM - MIN_ZOOM) as usize + 1;

/// Mask sentinel: the lens pixel falls outside the circular lens.
const OUTSIDE: u16 = u16::MAX;

// ---------------------------------------------------------------------------
// Persistent state (held in AppModel between frames)
// ---------------------------------------------------------------------------

/// Zoom and pixel-buffer state for the magnifier lens.
///
/// Lives in [`AppModel`](crate::app::AppModel) so the buffer is reused
/// across pointer-motion frames without re-allocation.
#[derive(Debug, Clone)]
pub struct MagnifierState {
    /// Pre-allocated flat RGB buffer for the magnifier grid (stride 3).
    /// Avoids a per-frame heap allocation on every pointer move.
    pub buf: [u8; 873],
    /// Current zoom level (whole-number logical pixels per magnified cell).
    /// Integer values keep the lens texture at exact 1:1 resolution.
    pub zoom_level: f32,
    /// Accumulated scroll delta — applied once per frame via `FrameTick`.
    pub pending_zoom_delta: f32,
}

impl MagnifierState {
    /// Create a new [`MagnifierState`] with default zoom.
    #[must_use]
    pub fn new() -> Self {
        MagnifierState {
            buf: [0u8; 873],
            zoom_level: MIN_ZOOM,
            pending_zoom_delta: 0.0,
        }
    }

    /// Reset zoom to defaults (e.g. when starting a new picking session).
    pub fn reset(&mut self) {
        self.zoom_level = MIN_ZOOM;
        self.pending_zoom_delta = 0.0;
    }
}

impl Default for MagnifierState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Per-zoom lens mask (computed once, looked up every frame)
// ---------------------------------------------------------------------------

/// Maps every pixel of the lens texture to the source cell it magnifies.
///
/// A mask exists per discrete zoom level: which pixels fall inside the circle
/// and which source cell each magnifies depends only on the cell size, so it
/// never changes while the pointer moves.
#[derive(Debug)]
pub struct MagnifierMask {
    /// Texture resolution per side (`GRID_SIZE * zoom`, integer).
    pub resolution: u32,
    /// `resolution²` entries — source cell index (`0..GRID_SIZE²`) or
    /// [`OUTSIDE`] for pixels beyond the circular lens.
    pub cells: Vec<u16>,
}

/// All lens masks (`MIN_ZOOM..=MAX_ZOOM`), built once on first use.
static MASKS: LazyLock<[MagnifierMask; ZOOM_LEVELS]> = LazyLock::new(build_masks);

/// Build the mask for every zoom level in one pass.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn build_masks() -> [MagnifierMask; ZOOM_LEVELS] {
    std::array::from_fn(|i| {
        let zoom = MIN_ZOOM + i as f32;
        let resolution = (GRID_SIZE as f32 * zoom).round() as u32;
        let radius = resolution as f32 / 2.0;
        let radius_sq = radius * radius;
        let mut cells = Vec::with_capacity((resolution * resolution) as usize);
        for y in 0..resolution {
            for x in 0..resolution {
                // Pixel centre relative to the circle centre.
                let dx = x as f32 + 0.5 - radius;
                let dy = y as f32 + 0.5 - radius;
                let cell = if dx * dx + dy * dy <= radius_sq {
                    let col = ((x as f32 + 0.5) / zoom) as usize;
                    let row = ((y as f32 + 0.5) / zoom) as usize;
                    (row * GRID_SIZE + col) as u16
                } else {
                    OUTSIDE
                };
                cells.push(cell);
            }
        }
        MagnifierMask { resolution, cells }
    })
}

/// Look up the lens mask for a zoom level, snapping to the nearest
/// supported integer level.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn mask_for_zoom(zoom: f32) -> &'static MagnifierMask {
    let zoom = zoom.round().clamp(MIN_ZOOM, MAX_ZOOM);
    &MASKS[zoom as usize - MIN_ZOOM as usize]
}

/// Fill an RGBA texture for the lens from the captured cell colours.
///
/// Pixels outside the circular lens are transparent; every inside pixel is
/// the opaque colour of its source cell.
#[must_use]
pub fn lens_rgba(mask: &MagnifierMask, buf: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(mask.cells.len() * 4);
    for &cell in &mask.cells {
        if cell == OUTSIDE {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let base = cell as usize * 3;
            rgba.extend_from_slice(&[buf[base], buf[base + 1], buf[base + 2], 255]);
        }
    }
    rgba
}

// ---------------------------------------------------------------------------
// Overlay canvas — grid, centre ring and border
// ---------------------------------------------------------------------------

/// Draws the lens overlay strokes above the cell texture.
///
/// The cells themselves are a single image widget (see [`lens_rgba`]); this
/// canvas only adds the grid, the centre-cell ring and the outer border.
pub struct MagnifierProgram {
    /// Number of cells per side (odd, e.g. 17).
    pub grid_size: usize,
    /// Logical-pixel size of each magnified cell.
    pub pixel_size: f32,
}

impl<Message> canvas::Program<Message, cosmic::Theme> for MagnifierProgram {
    type State = ();

    #[allow(clippy::cast_precision_loss)]
    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let cosmic = theme.cosmic();
        let fg = cosmic::iced::Color::from(cosmic.on_bg_color());

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let cell = self.pixel_size;
        let radius = self.grid_size as f32 * cell / 2.0;
        let centre = cosmic::iced::Point::new(radius, radius);

        // Subtle grid: one stroke per internal cell boundary, drawn once. A
        // box per cell would double every line (adjacent cells share it). The
        // frame clip is rectangle-only, so each line is clipped to the circle
        // by computing its chord half-length from the distance to the centre.
        let grid_color = cosmic::iced::Color {
            r: fg.r,
            g: fg.g,
            b: fg.b,
            a: 0.2,
        };
        let grid_style = Stroke::default().with_color(grid_color).with_width(1.0);
        let radius_sq = radius * radius;
        for i in 1..self.grid_size {
            let x = i as f32 * cell;
            let dx = x - centre.x;
            if dx * dx <= radius_sq {
                let half = (radius_sq - dx * dx).sqrt();
                frame.stroke(
                    &Path::line(
                        cosmic::iced::Point::new(x, centre.y - half),
                        cosmic::iced::Point::new(x, centre.y + half),
                    ),
                    grid_style,
                );
            }
            let y = i as f32 * cell;
            let dy = y - centre.y;
            if dy * dy <= radius_sq {
                let half = (radius_sq - dy * dy).sqrt();
                frame.stroke(
                    &Path::line(
                        cosmic::iced::Point::new(centre.x - half, y),
                        cosmic::iced::Point::new(centre.x + half, y),
                    ),
                    grid_style,
                );
            }
        }

        // Centre-cell ring (replaces the old crosshair).
        let half = self.grid_size / 2;
        let centre_rect = Path::rectangle(
            cosmic::iced::Point::new(half as f32 * cell, half as f32 * cell),
            cosmic::iced::Size::new(cell, cell),
        );
        frame.stroke(
            &centre_rect,
            Stroke::default()
                .with_color(cosmic::iced::Color::from_rgb(1.0, 0.0, 0.0))
                .with_width(2.0),
        );

        // Outer circular border.
        let border = Path::circle(centre, radius - 0.5);
        frame.stroke(&border, Stroke::default().with_color(fg).with_width(1.5));

        vec![frame.into_geometry()]
    }
}
