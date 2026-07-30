// SPDX-License-Identifier: MPL-2.0

//! Magnifier lens widget and state.
//!
//! Provides [`MagnifierProgram`] — a circular magnified pixel grid canvas
//! widget for the colour picker overlay — and [`MagnifierState`] for
//! managing the zoom level and pixel buffer outside the widget tree.

use cosmic::iced::mouse;
use cosmic::iced::widget::canvas::{self, Path, Stroke};

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
    /// Current zoom level (logical pixels per magnified cell).
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
            zoom_level: 8.0,
            pending_zoom_delta: 0.0,
        }
    }

    /// Reset zoom to defaults (e.g. when starting a new picking session).
    pub fn reset(&mut self) {
        self.zoom_level = 8.0;
        self.pending_zoom_delta = 0.0;
    }
}

impl Default for MagnifierState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Canvas program — renders the magnified pixel grid
// ---------------------------------------------------------------------------

/// Renders a circular magnified pixel grid centred on the cursor.
///
/// The lens shape is achieved by checking each pixel's centre distance
/// against the circle radius — no clip path required.
pub struct MagnifierProgram {
    /// Flat RGB byte array, row-major (stride 3).
    pub pixels: Vec<u8>,
    /// Number of cells per side (odd, e.g. 21).
    pub grid_size: usize,
    /// Logical-pixel size of each magnified cell.
    pub pixel_size: f32,
}

impl<Message> canvas::Program<Message, cosmic::Theme> for MagnifierProgram {
    type State = ();

    #[allow(clippy::cast_precision_loss, clippy::similar_names)]
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
        let bg = cosmic::iced::Color::from(cosmic.bg_color());

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let cell = self.pixel_size;
        let total = self.grid_size as f32 * cell;
        let radius = total / 2.0;
        let centre = cosmic::iced::Point::new(radius, radius);

        // 1. Semi-transparent circular background matching the theme.
        let circle_bg = Path::circle(centre, radius);
        frame.fill(&circle_bg, cosmic::iced::Color { a: 0.75, ..bg });

        // 2. Draw each magnified pixel, clipped to the circle boundary.
        //
        // Two problems exist with a naive centre-point inclusion test:
        //   a) Overflow: edge cells whose centre is just inside the circle are
        //      drawn as full rectangles, so their corners bleed outside the
        //      border stroke.
        //   b) Dark gap: cells whose centre is just outside the circle but
        //      whose rectangle still overlaps it are excluded entirely.
        //
        // Fix: use a closest-point overlap test for inclusion (fixes b), then
        // clip each drawn rectangle to the horizontal and vertical extents of
        // the circle at the pixel centre (fixes a).  For centre-outside pixels
        // the slab corner always falls inside the circle (no overflow).  For
        // centre-inside pixels the residual overflow is sub-pixel and
        // decreases as pixel_size grows (higher zoom).
        for row in 0..self.grid_size {
            for col in 0..self.grid_size {
                let idx = row * self.grid_size + col;
                if idx >= self.pixels.len() {
                    continue;
                }

                // Pixel bounding rect in canvas coordinates.
                let left = col as f32 * cell;
                let top = row as f32 * cell;
                let right = left + cell;
                let bottom = top + cell;

                // Closest point on the rect to the circle centre.
                let cx = radius.clamp(left, right);
                let cy = radius.clamp(top, bottom);
                let cdx = cx - radius;
                let cdy = cy - radius;
                // Skip pixels whose rectangle doesn't overlap the circle.
                if cdx * cdx + cdy * cdy > radius * radius {
                    continue;
                }

                // Pixel centre and its offset from the circle centre.
                let pcx = left + cell * 0.5;
                let pcy = top + cell * 0.5;
                let dx = pcx - radius;
                let dy = pcy - radius;

                // Horizontal span of the circle at this pixel's centre y,
                // and vertical span at this pixel's centre x.
                let span_x_sq = radius * radius - dy * dy;
                let span_y_sq = radius * radius - dx * dx;
                if span_x_sq < 0.0 || span_y_sq < 0.0 {
                    continue;
                }
                let span_x = span_x_sq.sqrt();
                let span_y = span_y_sq.sqrt();

                // Clip the draw rectangle to those spans.
                let cl = (radius - span_x).max(left);
                let cr = (radius + span_x).min(right);
                let ct = (radius - span_y).max(top);
                let cb = (radius + span_y).min(bottom);

                if cr <= cl || cb <= ct {
                    continue;
                }

                let base = idx * 3;
                let (r, g, b) = (
                    self.pixels[base],
                    self.pixels[base + 1],
                    self.pixels[base + 2],
                );
                let rect = Path::rectangle(
                    cosmic::iced::Point::new(cl, ct),
                    cosmic::iced::Size::new(cr - cl, cb - ct),
                );
                frame.fill(&rect, cosmic::iced::Color::from_rgb8(r, g, b));
            }
        }

        // 3. Small crosshair at centre (3 cells wide — stays well inside circle).
        let half = self.grid_size / 2;
        let cx = half as f32 * cell + cell / 2.0;
        let cy = half as f32 * cell + cell / 2.0;

        let cross_extent = cell * 2.0; // extends 2 cells from centre
        let h_line = Path::line(
            cosmic::iced::Point::new(cx - cross_extent, cy),
            cosmic::iced::Point::new(cx + cross_extent, cy),
        );
        let v_line = Path::line(
            cosmic::iced::Point::new(cx, cy - cross_extent),
            cosmic::iced::Point::new(cx, cy + cross_extent),
        );

        let crosshair_style = Stroke::default().with_color(fg).with_width(1.5);
        frame.stroke(&h_line, crosshair_style);
        frame.stroke(&v_line, crosshair_style);

        // 4. Centre-pixel highlight (bright border).
        let centre_rect = Path::rectangle(
            cosmic::iced::Point::new(half as f32 * cell, half as f32 * cell),
            cosmic::iced::Size::new(cell, cell),
        );
        frame.stroke(
            &centre_rect,
            Stroke::default().with_color(fg).with_width(2.0),
        );

        // 5. Outer circular border.
        let border = Path::circle(centre, radius - 0.5);
        frame.stroke(&border, Stroke::default().with_color(fg).with_width(1.5));

        vec![frame.into_geometry()]
    }
}
