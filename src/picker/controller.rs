// SPDX-License-Identifier: MPL-2.0

//! Active picking session controller.
//!
//! Once the screen has been captured, a [`PickerController`] manages all
//! remaining state for the picking session: overlay windows, cursor tracking,
//! coordinate mapping, and sampling.

use crate::picker::{CapturedOutput, Color};

// ---------------------------------------------------------------------------
// Hover state – immutable value exposed to the overlay for rendering
// ---------------------------------------------------------------------------

/// Snapshot of the cursor state during an active picking session.
///
/// The controller owns all coordinate conversion and sampling.  The overlay
/// simply reads this value to drive its presentation (crosshair, magnifier
/// preview, etc.).
#[derive(Debug, Clone, Copy)]
pub struct HoverInfo {
    /// Index into `captures` / `overlay_ids` for the output under the cursor.
    pub output_index: usize,
    /// Cursor position in this output's local logical coordinates.
    pub local_pos: (f32, f32),
    /// Cursor position in global compositor (logical) coordinate space.
    pub global_pos: (f32, f32),
    /// Pixel position within the captured buffer for this output.
    pub pixel_pos: (u32, u32),
    /// The sampled colour at this position, or `None` if out of bounds.
    pub color: Option<Color>,
}

// ---------------------------------------------------------------------------
// Lifecycle state
// ---------------------------------------------------------------------------

/// Lifecycle state of the active picking session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerState {
    /// Overlays are active with captured image.  Waiting for user click
    /// or cancel.
    Picking,
    /// User clicked.  A colour was selected.
    Completed(Color),
    /// User cancelled (Escape).
    Cancelled,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// A self-contained active picking session.
///
/// This object is created after the screen has been captured and manages
/// all state needed for the overlay, pointer tracking, and sampling phases.
/// It is dropped when the picker exits.
pub struct PickerController {
    /// Captured pixel data and metadata for all outputs.
    pub captures: Vec<CapturedOutput>,
    /// GPU texture handles for each captured output (uploaded once after
    /// capture).  Parallel to `captures`.  Used by the overlay to render
    /// the captured framebuffer as an opaque frozen background.
    pub image_handles: Vec<cosmic::widget::image::Handle>,
    /// Window IDs for the layer-surface overlay on each output.
    /// Populated by Milestone 3; parallel to `captures`.
    pub overlay_ids: Vec<cosmic::iced::window::Id>,
    /// Current hover state (set by pointer-motion handler).
    pub hover: Option<HoverInfo>,
    /// Current lifecycle state.
    pub state: PickerState,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
impl PickerController {
    /// Create a new controller already in [`PickerState::Picking`] with
    /// captures, image handles and overlay IDs all provided at once.
    ///
    /// This is used when capture has already completed and overlays are
    /// about to be created — there is no intermediate [`Capturing`] phase.
    #[must_use]
    pub fn new_with_captures(
        captures: Vec<CapturedOutput>,
        image_handles: Vec<cosmic::widget::image::Handle>,
        overlay_ids: Vec<cosmic::iced::window::Id>,
    ) -> Self {
        let n = captures.len();
        eprintln!(
            "[picker] PickerController::new_with_captures({} outputs, {} overlays)",
            n,
            overlay_ids.len()
        );
        for (i, oid) in overlay_ids.iter().enumerate() {
            eprintln!("[picker]   overlay[{i}] id={oid:?}");
        }
        PickerController {
            captures,
            image_handles,
            overlay_ids,
            hover: None,
            state: PickerState::Picking,
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Return the index of the output associated with a given overlay
    /// window ID, or `None` if the ID is unknown.
    fn overlay_output_index(&self, overlay_id: cosmic::iced::window::Id) -> Option<usize> {
        self.overlay_ids.iter().position(|id| *id == overlay_id)
    }

    /// Determine which output contains the given logical (compositor-space)
    /// point.  Returns the index into `self.captures`, or `None` if the point
    /// falls outside all outputs.
    #[must_use]
    pub fn output_at(&self, x: f32, y: f32) -> Option<usize> {
        self.captures.iter().position(|o| {
            let right = o.pos_x + o.logical_width as i32;
            let bottom = o.pos_y + o.logical_height as i32;
            x >= o.pos_x as f32 && x < right as f32 && y >= o.pos_y as f32 && y < bottom as f32
        })
    }

    /// Sample the pixel at the given logical (compositor-space) coordinates.
    ///
    /// Returns `None` if the point falls outside all outputs or if the
    /// underlying buffer does not contain the computed pixel offset.
    #[must_use]
    pub fn sample_at(&self, x: f32, y: f32) -> Option<Color> {
        let idx = self.output_at(x, y)?;
        let output = &self.captures[idx];

        // Compute the scale factor from logical → buffer pixels.
        let scale_x = if output.logical_width > 0 {
            output.width as f64 / output.logical_width as f64
        } else {
            1.0
        };
        let scale_y = if output.logical_height > 0 {
            output.height as f64 / output.logical_height as f64
        } else {
            1.0
        };

        let px = ((x - output.pos_x as f32) as f64 * scale_x) as u32;
        let py = ((y - output.pos_y as f32) as f64 * scale_y) as u32;

        output
            .pixel_at(px, py)
            .map(|(r, g, b)| Color { r, g, b, a: 255 })
    }

    // ------------------------------------------------------------------
    // Pointer event handlers (called from app update)
    // ------------------------------------------------------------------

    /// Process a pointer-motion event on an overlay window.
    ///
    /// Converts the surface-local cursor coordinates into the hover state
    /// (output index, pixel position, sampled colour) and stores it in
    /// `self.hover`.
    ///
    /// Returns the new [`HoverInfo`] on success, or `None` if the overlay
    /// window ID is unknown.
    pub fn on_pointer_motion(
        &mut self,
        overlay_id: cosmic::iced::window::Id,
        surface_x: f32,
        surface_y: f32,
    ) -> Option<HoverInfo> {
        let output_idx = self.overlay_output_index(overlay_id)?;
        let output = &self.captures[output_idx];

        // Surface-local coordinates are already output-local logical
        // coordinates (the overlay spans the entire output).
        //
        // Global compositor position = output origin + surface offset.
        let global_x = surface_x + output.pos_x as f32;
        let global_y = surface_y + output.pos_y as f32;

        // Scale factors for logical → buffer coordinate conversion.
        let scale_x = if output.logical_width > 0 {
            output.width as f64 / output.logical_width as f64
        } else {
            1.0
        };
        let scale_y = if output.logical_height > 0 {
            output.height as f64 / output.logical_height as f64
        } else {
            1.0
        };

        let pixel_x = (surface_x as f64 * scale_x) as u32;
        let pixel_y = (surface_y as f64 * scale_y) as u32;

        let color = output
            .pixel_at(pixel_x, pixel_y)
            .map(|(r, g, b)| Color { r, g, b, a: 255 });

        let info = HoverInfo {
            output_index: output_idx,
            local_pos: (surface_x, surface_y),
            global_pos: (global_x, global_y),
            pixel_pos: (pixel_x, pixel_y),
            color,
        };

        self.hover = Some(info);

        Some(info)
    }

    /// Process a pointer-click event on an overlay window.
    ///
    /// Uses the **last known hover position** (updated by
    /// [`on_pointer_motion`]) to sample the colour.  Returns the selected
    /// colour and transitions to [`PickerState::Completed`].
    ///
    /// If no hover state is available (e.g. click before any motion),
    /// returns `None` and the caller should treat the click as a no-op.
    pub fn on_pointer_click(&mut self, _overlay_id: cosmic::iced::window::Id) -> Option<Color> {
        let hover = self.hover?;
        let color = self.sample_at(hover.global_pos.0, hover.global_pos.1)?;
        self.state = PickerState::Completed(color);
        Some(color)
    }
}
