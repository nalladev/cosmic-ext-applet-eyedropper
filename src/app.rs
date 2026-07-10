// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use ::image::EncodableLayout;

use crate::config::Config;
use crate::fl;
use crate::picker::{self, CapturedOutput, Color};
use crate::picker::PickerController;
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use cosmic::{
    applet::padded_control,
    cosmic_config::{self, CosmicConfigEntry},
    cosmic_theme::Spacing,
    iced::{
        Alignment, Border, ContentFit, Event, Length, Limits, Subscription,
        event, mouse,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        widget::{column, container, row, space, MouseArea, Stack},
        window::{self, Id},
    },
    prelude::*,
    theme,
    widget::{button, canvas, divider, icon, image, text},
};
use cosmic::iced::clipboard;
use cosmic::iced::core::event::wayland::OutputEvent;
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::cctk::wayland_client::protocol::wl_output::WlOutput;

// ---------------------------------------------------------------------------
// Output tracking
// ---------------------------------------------------------------------------

/// Tracked state for a single output (monitor).
///
/// Mirrors the `OutputState` from `xdg-desktop-portal-cosmic`/`app.rs`.
/// `WlOutput` proxies are `Clone + Send`, so they can be passed through
/// iced messages safely.
#[derive(Debug, Clone)]
pub struct OutputState {
    /// The Wayland output object (from the iced/event-loop connection).
    pub output: WlOutput,
    /// Pre-allocated window id used for the layer-surface overlay on this output.
    pub id: window::Id,
    /// Connector name (e.g. `"DP-1"`, `"eDP-1"`).
    pub name: String,
    /// Logical size in compositor coordinates.
    pub logical_size: (u32, u32),
    /// Logical position in compositor coordinate space.
    pub logical_pos: (i32, i32),
}

// ---------------------------------------------------------------------------
// Copy feedback / Clipboard helpers
// ---------------------------------------------------------------------------

/// Which colour representation was copied to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyTarget {
    Hex,
    Rgb,
    Hsl,
}

// ---------------------------------------------------------------------------
// Magnifier canvas program
// ---------------------------------------------------------------------------

/// Renders a circular magnified pixel grid centred on the cursor.
///
/// The lens shape is achieved by checking each pixel's centre distance
/// against the circle radius — no clip path required.
struct MagnifierProgram {
    /// Flat array of `(R, G, B)` tuples, row-major.
    pixels: Vec<(u8, u8, u8)>,
    /// Number of cells per side (odd, e.g. 21).
    grid_size: usize,
    /// Logical-pixel size of each magnified cell.
    pixel_size: f32,
}

impl<Message> canvas::Program<Message, cosmic::Theme> for MagnifierProgram {
    type State = ();

    #[allow(clippy::cast_precision_loss)]
    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::Renderer,
        _theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        use canvas::{Path, Stroke};

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let cell = self.pixel_size;
        let total = self.grid_size as f32 * cell;
        let radius = total / 2.0;
        let centre = cosmic::iced::Point::new(radius, radius);

        // 1. Dark circular background.
        let circle_bg = Path::circle(centre, radius);
        frame.fill(
            &circle_bg,
            cosmic::iced::Color::from_rgba(0.0, 0.0, 0.0, 0.75),
        );

        // 2. Draw each magnified pixel, but only if it lies within the circle.
        for y in 0..self.grid_size {
            for x in 0..self.grid_size {
                let idx = y * self.grid_size + x;
                if idx >= self.pixels.len() {
                    continue;
                }

                // Pixel centre in the canvas coordinate space.
                let pcx = x as f32 * cell + cell / 2.0;
                let pcy = y as f32 * cell + cell / 2.0;
                let dx = pcx - radius;
                let dy = pcy - radius;

                if dx * dx + dy * dy <= radius * radius {
                    let (r, g, b) = self.pixels[idx];
                    let rect = Path::rectangle(
                        cosmic::iced::Point::new(x as f32 * cell, y as f32 * cell),
                        cosmic::iced::Size::new(cell, cell),
                    );
                    frame.fill(&rect, cosmic::iced::Color::from_rgb8(r, g, b));
                }
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

        let crosshair_style = Stroke::default()
            .with_color(cosmic::iced::Color::WHITE)
            .with_width(1.5);
        frame.stroke(&h_line, crosshair_style);
        frame.stroke(&v_line, crosshair_style);

        // 4. Centre-pixel highlight (bright border).
        let centre_rect = Path::rectangle(
            cosmic::iced::Point::new(half as f32 * cell, half as f32 * cell),
            cosmic::iced::Size::new(cell, cell),
        );
        frame.stroke(
            &centre_rect,
            Stroke::default()
                .with_color(cosmic::iced::Color::WHITE)
                .with_width(2.0),
        );

        // 5. Outer circular border.
        let border = Path::circle(centre, radius - 0.5);
        frame.stroke(
            &border,
            Stroke::default()
                .with_color(cosmic::iced::Color::WHITE)
                .with_width(1.5),
        );

        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// Application model
// ---------------------------------------------------------------------------

/// The application model stores app-specific state used to describe its
/// interface and drive its logic.
pub struct AppModel {
    /// Application state which is managed by the COSMIC runtime.
    core: cosmic::Core,
    /// The popup id.
    popup: Option<Id>,
    /// Configuration data that persists between application runs.
    config: Config,

    // ── Eyedropper / colour-picker state ────────────────────────────
    /// The most recently sampled colour (if any).
    sampled: Option<Color>,
    /// Error message, if something went wrong.
    error: Option<String>,

    // ── Derived display values ──────────────────────────────────────
    hex: String,
    rgb: String,
    hsl: String,

    // ── Output tracking (from iced Wayland events) ──────────────────
    outputs: Vec<OutputState>,

    // ── Active picking session ──────────────────────────────────────
    /// `Some` while the user is in picker mode (overlays are visible).
    /// `None` during normal operation.
    picker: Option<PickerController>,

    // ── Deferred capture synchronisation ──────────────────────────────
    /// When entering picker mode, the popup must be fully gone before
    /// capture starts (otherwise the popup appears in the screenshot).
    /// This field holds the destroyed popup's ID; when `PopupClosed`
    /// fires with a matching ID we know the compositor has removed the
    /// popup and it is safe to begin capture.
    pending_popup_close: Option<Id>,

    // ── Two-phase capture (flicker-free entry into picker mode) ──────────
    // Session negotiation (`prepare_all_outputs`) runs concurrently with
    // closing the popup; the fast frame-grab (`finish_all_outputs`) only
    // starts once *both* are done, whichever finishes last.
    /// Pre-negotiated capture sessions, ready for the final (fast) frame
    /// grab.  `None` until `SessionsPrepared` arrives.
    prepared_sessions: Option<Vec<picker::PreparedOutputCapture>>,
    /// Set if session negotiation failed; the fallback is to run the full
    /// (slower) single-shot capture once the popup is confirmed closed.
    sessions_prepare_failed: bool,
    /// Set once the popup is confirmed closed while entering picker mode —
    /// i.e. we're just waiting on `prepared_sessions` / `sessions_prepare_failed`
    /// before starting the final capture.
    popup_confirmed_for_picker: bool,
    /// Hand-off slot for the (non-`Clone`) prepared sessions produced by the
    /// `prepare_all_outputs` background task.  The task writes into this
    /// slot and emits the lightweight `Message::SessionsPrepared`; the
    /// handler then `take()`s the value out into `prepared_sessions`.
    prepared_sessions_slot:
        std::sync::Arc<std::sync::Mutex<Option<Vec<picker::PreparedOutputCapture>>>>,

    // ── Pre-created overlay tracking ───────────────────────────────────
    /// Overlay window IDs that have been pre-created (transparent) but
    /// are not yet showing the frozen image.  Populated when entering
    /// picker mode; cleared by `OverlayCreated` or on cancel.
    pending_overlay_ids: Vec<window::Id>,

    // ── Clipboard feedback ───────────────────────────────────────────
    /// Which target was last copied (if any).
    copied_target: Option<CopyTarget>,
    /// When the last copy happened (for auto-clearing feedback).
    copied_at: Option<Instant>,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Messages emitted by the application and its widgets.
#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    UpdateConfig(Config),

    // ── Capture flow ────────────────────────────────────────────────
    /// The eyedropper button was clicked in the popup.
    EyedropperClicked,
    /// Raw captured output data is ready.
    CaptureCompleted(Vec<CapturedOutput>),
    /// The capture failed with an error message.
    CaptureFailed(String),
    /// Capture sessions have been negotiated (formats + buffers ready) for
    /// all outputs.  The actual prepared data is handed off out-of-band
    /// (see `prepared_sessions_slot`) since it isn't `Clone`.
    SessionsPrepared,
    /// Session negotiation failed; fall back to the single-shot capture
    /// pipeline once the popup is confirmed closed.
    SessionsPrepareFailed(String),

    // ── Wayland output tracking ─────────────────────────────────────
    OutputEvent(Box<OutputEvent>, WlOutput),

    // ── Picker mode ─────────────────────────────────────────────────
    /// User pressed Escape or overlay was closed externally.
    PickerCancel,
    /// Pointer moved on a picker overlay window.
    PointerMoved(Id, f32, f32),
    /// Pointer clicked on a picker overlay window.
    PointerClicked(Id),

    // ── Clipboard copy ───────────────────────────────────────────────
    /// Copy the HEX string to the clipboard.
    CopyHex,
    /// Copy the RGB string to the clipboard.
    CopyRgb,
    /// Copy the HSL string to the clipboard.
    CopyHsl,
    /// Auto-cleared after copy feedback timeout.
    ClearCopyFeedback,

    // ── Frame tick (keeps overlay redrawing during picker mode) ────────
    FrameTick,

    // ── Pre-created overlay lifecycle ──────────────────────────────────
    /// A pre-created overlay surface has been acknowledged by the
    /// compositor (configure received).
    #[allow(dead_code)]
    OverlayCreated(Id),
}

// ---------------------------------------------------------------------------
// Application trait implementation
// ---------------------------------------------------------------------------

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.github.nalladev.CosmicExtAppletEyedropper";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
            core: cosmic::Core,
            _flags: Self::Flags,
        ) -> (Self, Task<cosmic::Action<Self::Message>>) {
            let t_start = std::time::Instant::now();
            eprintln!("[startup] init() called at t={:?}", t_start.elapsed());
            eprintln!("[startup] before config load at t={:?}", t_start.elapsed());
            let config_entry = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
                .map(|context| match Config::get_entry(&context) {
                    Ok(config) => config,
                    Err((_errors, config)) => config,
                })
                .unwrap_or_default();
            eprintln!("[startup] after config load at t={:?}", t_start.elapsed());

                let app = AppModel {
                core,
                config: config_entry,
                popup: None,
                sampled: None,
                error: None,
                hex: String::new(),
                rgb: String::new(),
                hsl: String::new(),
                outputs: Vec::new(),
                picker: None,
                pending_popup_close: None,
                prepared_sessions: None,
                sessions_prepare_failed: false,
                popup_confirmed_for_picker: false,
                prepared_sessions_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                pending_overlay_ids: Vec::new(),
                copied_target: None,
                copied_at: None,
            };

            eprintln!("[startup] init() returning at t={:?}", t_start.elapsed());
            let r = (app, Task::none());
            eprintln!("[startup] init() done at t={:?}", t_start.elapsed());
            r
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        eprintln!("[DEBUG] on_close_requested(id={id:?})");
        eprintln!("[DEBUG]   popup={:?}, pending={:?}, picker_overlays={:?}",
            self.popup, self.pending_popup_close,
            self.picker.as_ref().map(|p| &p.overlay_ids));
        // If an overlay window is closed externally, cancel the picker.
        if self
            .picker
            .as_ref()
            .is_some_and(|p| p.overlay_ids.contains(&id))
        {
            eprintln!("[DEBUG]   -> overlay close -> PickerCancel");
            return Some(Message::PickerCancel);
        }
        // Otherwise it's the popup.  Also match popups that were closed
        // as part of entering picker mode (pending_popup_close) — their
        // close notification must still arrive even though `self.popup`
        // was already cleared to None.  See EyedropperClicked.
        if self.popup == Some(id) || self.pending_popup_close == Some(id) {
            eprintln!("[DEBUG]   -> popup close -> PopupClosed");
            return Some(Message::PopupClosed(id));
        }
        eprintln!("[DEBUG]   -> no match, returning None");
        None
    }

    /// Draw the applet button in the panel.
    fn view(&self) -> Element<'_, Self::Message> {
        static FIRST_VIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        if FIRST_VIEW.swap(false, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[startup] first view() call — applet button rendered");
        }
        self.core
            .applet
            .icon_button("color-select-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }

    /// Draw a window – either the popup or a picker overlay.
    fn view_window(&self, id: Id) -> Element<'_, Self::Message> {
        static VW_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let count = VW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!("[DEBUG] view_window(id={id:?}) call #{count}");
        eprintln!("[DEBUG]   picker={:?}, popup={:?}",
            self.picker.as_ref().map(|p| &p.overlay_ids),
            self.popup);

        // Is this a picker overlay (active picker or pre-created)?
        if self
            .picker
            .as_ref()
            .is_some_and(|p| p.overlay_ids.contains(&id))
            || self.pending_overlay_ids.contains(&id)
        {
            eprintln!("[DEBUG]   -> routing to view_picker_overlay");
            return self.view_picker_overlay(id);
        }

        // Is this the popup?
        if self.popup == Some(id) {
            eprintln!("[DEBUG]   -> routing to view_popup");
            return self.view_popup();
        }

        // Fallback: unknown window — render nothing.
        eprintln!("[DEBUG]   -> UNKNOWN window id, rendering placeholder");
        space::horizontal().width(Length::Fixed(1.0)).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        static FIRST_SUB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        if FIRST_SUB.swap(false, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[startup] first subscription() call");
        }

        let mut subs: Vec<Subscription<Self::Message>> = vec![
            // Config changes
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
            // Wayland output events (monitor hotplug, geometry changes)
            event::listen_with(|e, _, _| match e {
                Event::PlatformSpecific(event::PlatformSpecific::Wayland(
                    event::wayland::Event::Output(o_event, wl_output),
                )) => Some(Message::OutputEvent(Box::new(o_event), wl_output)),
                _ => None,
            }),
        ];

        // Keep the UI thread ticking during picker mode so the magnifier
        // overlay continuously redraws.
        if self.picker.is_some() {
            subs.push(
                cosmic::iced::time::every(Duration::from_millis(16))
                    .map(|_| Message::FrameTick),
            );
        }

        Subscription::batch(subs)
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        static FIRST_UPDATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        if FIRST_UPDATE.swap(false, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[startup] first update() received: {message:?}");
        }

        match message {
            // ── Toggle the eyedropper popup ─────────────────────────────
            Message::TogglePopup => {
                // Ignore while in picker mode or waiting for popup close.
                if self.picker.is_some() || self.pending_popup_close.is_some() {
                    return Task::none();
                }
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(372.0)
                        .min_width(300.0)
                        .min_height(200.0)
                        .max_height(1080.0);
                    get_popup(popup_settings)
                };
            }

            // ── Popup was closed ────────────────────────────────────────
            Message::PopupClosed(id) => {
                eprintln!("[picker] PopupClosed({id:?})");
                eprintln!("[picker]   self.popup={:?}, pending_popup_close={:?}",
                    self.popup, self.pending_popup_close);

                // Normal popup lifecycle (user closed it manually).
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                    self.copied_target = None;
                    self.copied_at = None;
                    eprintln!("[picker]   normal popup close — no capture.");
                }

                // Deferred capture: the popup was closed as part of
                // entering picker mode.  The compositor has now confirmed
                // the popup is gone; the *fast* final capture step can
                // start as soon as sessions are also ready (see
                // `maybe_start_final_capture`) — this is what keeps the
                // live-desktop gap (and thus the flicker) as short as
                // possible.
                if self.pending_popup_close == Some(id) {
                    self.pending_popup_close = None;
                    self.popup_confirmed_for_picker = true;
                    eprintln!("[picker]   MATCH! popup confirmed closed, checking sessions.");
                    return self.maybe_start_final_capture();
                }
                eprintln!("[picker]   no match (pending={:?}, normal={:?})",
                    self.pending_popup_close, self.popup);
            }

            // ── Capture sessions negotiated (formats + buffers ready) ────
            Message::SessionsPrepared => {
                let prepared = self.prepared_sessions_slot.lock().unwrap().take();
                eprintln!("[picker] SessionsPrepared — {} output(s) ready",
                    prepared.as_ref().map_or(0, std::vec::Vec::len));
                self.prepared_sessions = prepared;
                return self.maybe_start_final_capture();
            }

            // ── Capture session negotiation failed ───────────────────────
            Message::SessionsPrepareFailed(msg) => {
                eprintln!("[picker] SessionsPrepareFailed: {msg}");
                self.sessions_prepare_failed = true;
                return self.maybe_start_final_capture();
            }

            // ── Configuration updated externally ────────────────────────
            Message::UpdateConfig(config) => {
                self.config = config;
            }

            // ── Eyedropper button clicked ───────────────────────────────
            Message::EyedropperClicked => {
                eprintln!("[picker] EyedropperClicked — entering picker mode");

                // Ignore if already in picker mode.
                if self.picker.is_some() || self.pending_popup_close.is_some() || !self.pending_overlay_ids.is_empty() {
                    eprintln!("[picker]   WARNING: ignored — picker={}, pending_close={}, pending_overlays={}",
                        self.picker.is_some(),
                        self.pending_popup_close.is_some(),
                        self.pending_overlay_ids.len(),
                    );
                    return Task::none();
                }

                self.error = None;
                self.sampled = None;
                self.copied_target = None;
                self.copied_at = None;

                eprintln!("[picker]   tracked outputs: {}", self.outputs.len());

                self.prepared_sessions = None;
                self.sessions_prepare_failed = false;
                self.popup_confirmed_for_picker = false;

                // ── Pre-create transparent overlay surfaces ────────────
                //
                // Layer surfaces are created *before* destroying the popup
                // so that when the popup disappears, the compositor already
                // has the overlay committed and ready to display.  The
                // overlay starts transparent (no captures yet) and shows
                // the frozen image once the capture completes — eliminating
                // the visible flicker of the live desktop.
                //
                // The slow session/format negotiation runs concurrently
                // with both the overlay creation and popup destruction.
                if let Some(popup_id) = self.popup.take() {
                    self.pending_popup_close = Some(popup_id);
                    self.pending_overlay_ids.clear();
                    eprintln!("[picker]   popup {popup_id:?} removed, pending_popup_close set.");

                    // 1. Create transparent overlay surfaces on all outputs.
                    let mut overlay_tasks: Vec<Task<cosmic::Action<Self::Message>>> = Vec::new();
                    let mut overlay_ids = Vec::new();
                    for (i, output_state) in self.outputs.iter().enumerate() {
                        let overlay_id = output_state.id;
                        overlay_ids.push(overlay_id);
                        eprintln!("[picker]   pre-creating overlay[{i}] id={overlay_id:?} on output '{}'", output_state.name);
                        overlay_tasks.push(get_layer_surface(SctkLayerSurfaceSettings {
                            id: overlay_id,
                            layer: Layer::Overlay,
                            keyboard_interactivity: KeyboardInteractivity::Exclusive,
                            anchor: Anchor::all(),
                            output: IcedOutput::Output(output_state.output.clone()),
                            namespace: "color-picker".to_string(),
                            size: Some((None, None)),
                            exclusive_zone: -1,
                            size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
                            ..Default::default()
                        }));
                    }
                    self.pending_overlay_ids = overlay_ids;

                    // 2. Start session negotiation (slow) concurrently.
                    let slot = self.prepared_sessions_slot.clone();
                    let prepare_task = Task::perform(
                        async move {
                            match picker::prepare_all_outputs().await {
                                Ok(prepared) => {
                                    *slot.lock().unwrap() = Some(prepared);
                                    Ok(())
                                }
                                Err(e) => Err(e.to_string()),
                            }
                        },
                        |result: Result<(), String>| {
                            let msg = match result {
                                Ok(()) => Message::SessionsPrepared,
                                Err(e) => Message::SessionsPrepareFailed(e),
                            };
                            cosmic::Action::App(msg)
                        },
                    );

                    // All three happen concurrently: overlays appear (transparent),
                    // popup disappears, and session negotiation runs in background.
                    // When the capture completes, the frozen image populates the
                    // already-visible overlay — no flicker.
                    let mut tasks: Vec<Task<cosmic::Action<Self::Message>>> = overlay_tasks;
                    tasks.push(destroy_popup(popup_id));
                    tasks.push(prepare_task);
                    return Task::batch(tasks);
                }
                eprintln!("[picker]   popup was already closed — starting capture immediately.");
                return Task::perform(
                    picker::capture_all_outputs(),
                    |result| {
                        let msg = match result {
                            Ok(outputs) => Message::CaptureCompleted(outputs),
                            Err(e) => Message::CaptureFailed(e.to_string()),
                        };
                        cosmic::Action::App(msg)
                    },
                );
            }

            // ── Capture completed successfully ──────────────────────────
            Message::CaptureCompleted(captures) => {
                let t_capture = std::time::Instant::now();
                eprintln!("[picker] CaptureCompleted — {} outputs", captures.len());
                for cap in &captures {
                    eprintln!("[picker]   output: {} {}x{} @({},{}) logical {}x{} rgba={}b",
                        cap.name, cap.width, cap.height,
                        cap.pos_x, cap.pos_y,
                        cap.logical_width, cap.logical_height,
                        cap.rgba.as_bytes().len(),
                    );
                }

                if captures.is_empty() {
                    eprintln!("[picker]   captures is empty — error + cancel");
                    self.error = Some("No outputs captured".into());
                    return self.cancel_picker();
                }

                // If picker mode was cancelled while capture was running,
                // discard the result.
                if self.picker.is_some() {
                    eprintln!("[picker]   WARNING: picker already exists — discard duplicate capture");
                    return Task::none();
                }

                eprintln!("[picker]   collecting pre-built image handles...");
                let mut image_handles = Vec::with_capacity(captures.len());
                for (i, cap) in captures.iter().enumerate() {
                    image_handles.push(cap.image_handle.clone());
                    eprintln!("[picker]   image_handle[{i}]: {}x{}", cap.width, cap.height);
                }

                // If overlays were pre-created (transparent) during
                // EyedropperClicked, reuse them — just populate the picker
                // with the captured data.  The overlay views will render
                // the frozen image on the next frame, completing the
                // flicker-free transition.
                if !self.pending_overlay_ids.is_empty() {
                    let overlay_ids = std::mem::take(&mut self.pending_overlay_ids);
                    eprintln!("[picker]   reusing {} pre-created overlay(s): {:?}", overlay_ids.len(), overlay_ids);
                    let n_overlays = overlay_ids.len();
                    self.picker = Some(PickerController::new_with_captures(
                        captures, image_handles, overlay_ids,
                    ));
                    eprintln!("[picker]   picker created in Picking state with {n_overlays} overlays (pre-created path)");
                    eprintln!(
                        "[picker]   CaptureCompleted handler took {:?}",
                        t_capture.elapsed(),
                    );
                    return Task::none();
                }

                // Fallback: create overlay windows now (no pre-creation).
                eprintln!("[picker]   creating overlay windows on {} outputs...", self.outputs.len());
                let mut tasks: Vec<Task<cosmic::Action<Self::Message>>> = Vec::new();
                let mut overlay_ids = Vec::new();

                for (i, output_state) in self.outputs.iter().enumerate() {
                    let overlay_id = output_state.id;
                    overlay_ids.push(overlay_id);
                    eprintln!("[picker]   creating overlay[{i}] id={overlay_id:?} on output '{}",
                        output_state.name);
                    tasks.push(get_layer_surface(SctkLayerSurfaceSettings {
                        id: overlay_id,
                        layer: Layer::Overlay,
                        keyboard_interactivity: KeyboardInteractivity::Exclusive,
                        anchor: Anchor::all(),
                        output: IcedOutput::Output(output_state.output.clone()),
                        namespace: "color-picker".to_string(),
                        size: Some((None, None)),
                        exclusive_zone: -1,
                        size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
                        ..Default::default()
                    }));
                }

                let n_overlays = overlay_ids.len();
                self.picker = Some(PickerController::new_with_captures(
                    captures, image_handles, overlay_ids,
                ));
                eprintln!("[picker]   picker created in Picking state with {n_overlays} overlays");
                eprintln!(
                    "[picker]   CaptureCompleted handler took {:?}",
                    t_capture.elapsed(),
                );

                return Task::batch(tasks);
            }

            // ── Capture failed ──────────────────────────────────────────
            Message::CaptureFailed(msg) => {
                eprintln!("[picker] CaptureFailed: {msg}");
                self.error = Some(msg);
                // Exit picker mode cleanly (destroy overlays, reopen popup).
                return self.cancel_picker();
            }

            // ── Wayland output event (hotplug / geometry) ───────────────
            Message::OutputEvent(o_event, wl_output) => {
                match *o_event {
                    OutputEvent::Created(Some(info))
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        self.outputs.push(OutputState {
                            output: wl_output,
                            id: window::Id::unique(),
                            name: info.name.unwrap(),
                            logical_size: info
                                .logical_size
                                .map(|(w, h)| (w.cast_unsigned(), h.cast_unsigned()))
                                .unwrap(),
                            logical_pos: info.logical_position.unwrap(),
                        });
                    }
                    OutputEvent::Removed => {
                        self.outputs.retain(|o| o.output != wl_output);
                    }
                    OutputEvent::InfoUpdate(info)
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        if let Some(state) =
                            self.outputs.iter_mut().find(|o| o.output == wl_output)
                        {
                            state.name = info.name.unwrap();
                            state.logical_size = info
                                .logical_size
                                .map(|(w, h)| (w.cast_unsigned(), h.cast_unsigned()))
                                .unwrap();
                            state.logical_pos = info.logical_position.unwrap();
                        } else {
                            // Output appeared without a Created event –
                            // treat as new.
                            self.outputs.push(OutputState {
                                output: wl_output,
                                id: window::Id::unique(),
                                name: info.name.unwrap(),
                                logical_size: info
                                    .logical_size
                                    .map(|(w, h)| (w.cast_unsigned(), h.cast_unsigned()))
                                    .unwrap(),
                                logical_pos: info.logical_position.unwrap(),
                            });
                        }
                    }
                    _ => {
                        // Ignore incomplete or unhandled events.
                    }
                }
            }

            // ── Pointer moved on a picker overlay ─────────────────────
            Message::PointerMoved(id, x, y) => {
                if let Some(picker) = self.picker.as_mut() {
                    let result = picker.on_pointer_motion(id, x, y);
                    if result.is_none() {
                        eprintln!("[picker] PointerMoved({id:?}, {x:.0}, {y:.0}) — FAILED (no output match)");
                    }
                } else {
                    eprintln!("[picker] PointerMoved({id:?}) — ignored, no picker");
                }
            }

            // ── Pointer clicked on a picker overlay ───────────────────
            Message::PointerClicked(id) => {
                eprintln!("[picker] PointerClicked({id:?})");
                if let Some(picker) = self.picker.as_mut() {
                    eprintln!("[picker]   picker state={:?}, captures={}", picker.state, picker.captures.len());
                    if let Some(color) = picker.on_pointer_click(id) {
                        eprintln!("[picker]   COLOUR SELECTED: {} / {} / {}",
                            color.hex(), color.rgb(), color.hsl());
                        // Colour selected — exit picker mode.
                        let overlays = picker.overlay_ids.clone();
                        self.picker.take();

                        self.sampled = Some(color);
                        self.hex = color.hex();
                        self.rgb = color.rgb();
                        self.hsl = color.hsl();

                        let mut tasks: Vec<Task<cosmic::Action<Self::Message>>> = Vec::new();

                        // Destroy all overlay surfaces.
                        for oid in &overlays {
                            tasks.push(destroy_layer_surface(*oid));
                        }

                        // Reopen the popup.
                        let new_id = Id::unique();
                        self.popup.replace(new_id);
                        let mut popup_settings = self.core.applet.get_popup_settings(
                            self.core.main_window_id().unwrap(),
                            new_id,
                            None,
                            None,
                            None,
                        );
                        popup_settings.positioner.size_limits = Limits::NONE
                            .max_width(372.0)
                            .min_width(300.0)
                            .min_height(200.0)
                            .max_height(1080.0);
                        tasks.push(get_popup(popup_settings));

                        return Task::batch(tasks);
                    }
                }
            }

            // ── Clipboard copy ─────────────────────────────────────────
            Message::CopyHex => {
                let hex = self.hex.clone();
                if !hex.is_empty() {
                    self.copied_target = Some(CopyTarget::Hex);
                    self.copied_at = Some(Instant::now());
                    return Task::batch(vec![
                        clipboard::write(hex),
                        Task::perform(
                            async {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                Message::ClearCopyFeedback
                            },
                            cosmic::Action::App,
                        ),
                    ]);
                }
            }
            Message::CopyRgb => {
                let rgb = self.rgb.clone();
                if !rgb.is_empty() {
                    self.copied_target = Some(CopyTarget::Rgb);
                    self.copied_at = Some(Instant::now());
                    return Task::batch(vec![
                        clipboard::write(rgb),
                        Task::perform(
                            async {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                Message::ClearCopyFeedback
                            },
                            cosmic::Action::App,
                        ),
                    ]);
                }
            }
            Message::CopyHsl => {
                let hsl = self.hsl.clone();
                if !hsl.is_empty() {
                    self.copied_target = Some(CopyTarget::Hsl);
                    self.copied_at = Some(Instant::now());
                    return Task::batch(vec![
                        clipboard::write(hsl),
                        Task::perform(
                            async {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                Message::ClearCopyFeedback
                            },
                            cosmic::Action::App,
                        ),
                    ]);
                }
            }
            Message::ClearCopyFeedback => {
                self.copied_target = None;
                self.copied_at = None;
            }

            // ── Frame tick (when picker is active) ──────────────────────
            Message::FrameTick => {
                // Sanity check: Picking state must have captures.
                if let Some(p) = self.picker.as_ref()
                    && p.captures.is_empty()
                {
                    eprintln!("[picker] FrameTick — state={:?} but captures empty!", p.state);
                }
            }

            // ── Picker cancelled (Escape or external close) ────────────
            Message::PickerCancel => {
                eprintln!("[picker] PickerCancel received");
                return self.cancel_picker();
            }

            // ── Pre-created overlay acknowledged by compositor ─────────
            Message::OverlayCreated(id) => {
                eprintln!("[picker] OverlayCreated({id:?}) — overlay surface ready");
            }
        }

        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

// ---------------------------------------------------------------------------
// Helper methods on AppModel
// ---------------------------------------------------------------------------

impl AppModel {
    /// Build a single colour-representation row (label + value + copy button).
    ///
    /// The copy-area shows a symbolic copy icon when a colour is available,
    /// a temporary checkmark after copying, or empty space when no colour
    /// has been selected.
    #[allow(clippy::needless_pass_by_value)]
    fn color_row(
        &self,
        label: String,
        value: &str,
        target: CopyTarget,
        msg: Message,
    ) -> Element<'_, Message> {
        let has_color = !value.is_empty();
        let just_copied = self
            .copied_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            && self.copied_target == Some(target);

        let copy_widget: Element<'_, Message> = if just_copied {
            container(icon::from_name("object-select-symbolic").size(14).symbolic(true))
                .center(Length::Fixed(24.0))
                .into()
        } else if has_color {
            let handle = icon::from_name("edit-copy-symbolic").size(14).symbolic(true).handle();
            button::icon(handle)
                .on_press(msg)
                .padding(0)
                .into()
        } else {
            space::horizontal().width(Length::Fixed(24.0)).into()
        };

        padded_control(
            row![
                text::body(format!("{label}: {value}"))
                    .width(Length::Fill)
                    .height(Length::Fixed(24.0))
                    .align_y(Alignment::Center),
                copy_widget,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .into()
    }

    /// Render the normal eyedropper popup.
    fn view_popup(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxxs: _,
            space_xxs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;

        // Derive display strings.
        let (hex_val, rgb_val, hsl_val): (String, String, String) =
            if let Some(c) = self.sampled {
                (c.hex(), c.rgb(), c.hsl())
            } else {
                (self.hex.clone(), self.rgb.clone(), self.hsl.clone())
            };

        let has_color = self.sampled.is_some();

        // Colour swatch.
        let swatch_color = self
            .sampled
            .map_or(cosmic::iced::Color::WHITE, |c| cosmic::iced::Color::from_rgb8(c.r, c.g, c.b));

        let swatch = container(space::horizontal())
            .width(32)
            .height(32)
            .style(move |_: &cosmic::Theme| container::Style {
                background: Some(swatch_color.into()),
                border: Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        // Centre text: HEX value or placeholder.
        let centre: Element<'_, Message> = if has_color {
            container(text::body(hex_val.clone()).size(14).align_y(Alignment::Center))
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .into()
        } else {
            container(text::body(fl!("no-color-selected")).size(14).align_y(Alignment::Center))
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .into()
        };

        // "Select Colour" button (primary action).
        let select_button = button::suggested(fl!("select-colour"))
            .on_press(Message::EyedropperClicked);

        let heading = row![
            swatch,
            centre,
            select_button,
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let mut content = column![padded_control(heading)]
            .padding([8, 0])
            .spacing(0);

        // ── Copy rows ─────────────────────────────────────────────────
        content = content
            .push(
                padded_control(divider::horizontal::default())
                    .padding([space_xxs, space_s]),
            )
            .push(self.color_row(fl!("hex"), &hex_val, CopyTarget::Hex, Message::CopyHex))
            .push(
                padded_control(divider::horizontal::default())
                    .padding([space_xxs, space_s]),
            )
            .push(self.color_row(fl!("rgb"), &rgb_val, CopyTarget::Rgb, Message::CopyRgb))
            .push(
                padded_control(divider::horizontal::default())
                    .padding([space_xxs, space_s]),
            )
            .push(self.color_row(fl!("hsl"), &hsl_val, CopyTarget::Hsl, Message::CopyHsl));

        // Status / error message.
        if let Some(ref err) = self.error {
            content = content.push(
                padded_control(text::body(err)).padding([space_xxs, space_s]),
            );
        }

        self.core.applet.popup_container(content).into()
    }

    /// Render a picker overlay window.
    ///
    /// Renders the captured framebuffer fullscreen with pointer tracking,
    /// crosshair, and optional magnifier.
    fn view_picker_overlay(&self, id: Id) -> Element<'_, Message> {
        let Some(picker) = self.picker.as_ref() else {
            // Pre-created overlay: picker doesn't exist yet (capture in
            // progress).  Render a full-screen transparent surface with
            // keyboard support so Escape works immediately.
            if self.pending_overlay_ids.contains(&id) {
                eprintln!("[picker] view_picker_overlay({id:?}) — pre-created, transparent placeholder");
                let event_layer = MouseArea::new(
                    container(space::horizontal())
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .interaction(mouse::Interaction::Crosshair);

                return KeyboardWrapper::new(
                    event_layer,
                    |key, _modifiers| match key {
                        Key::Named(Named::Escape) => Some(Message::PickerCancel),
                        _ => None,
                    },
                )
                .into();
            }
            eprintln!("[picker] view_picker_overlay({id:?}) — no picker, rendering placeholder");
            return space::horizontal().width(Length::Fixed(1.0)).into();
        };

        // ── Picking state: full interaction ────────────────────────────
        let on_move = move |point: cosmic::iced::Point| {
            Message::PointerMoved(id, point.x, point.y)
        };

        // Background layer: captured framebuffer (frozen desktop).
        let image_layer: Option<Element<'_, Message>> = {
            let output_idx = picker
                .overlay_ids
                .iter()
                .position(|oid| *oid == id);
            output_idx
                .and_then(|idx| picker.image_handles.get(idx))
                .map(|handle| {
                    image::Image::new(handle.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .content_fit(ContentFit::Fill)
                        .into()
                })
        };

        // Event layer: transparent overlay for pointer tracking.
        let event_layer = MouseArea::new(
            container(space::horizontal())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_move(on_move)
        .on_press(Message::PointerClicked(id))
        .interaction(mouse::Interaction::Crosshair);

        let mut stack = Stack::new();
        if let Some(img) = image_layer {
            stack = stack.push(img);
        }
        stack = stack.push(event_layer);

        if let Some(mag) = self.build_magnifier() {
            stack = stack.push(mag);
        }

        KeyboardWrapper::new(
            stack,
            |key, _modifiers| match key {
                Key::Named(Named::Escape) => Some(Message::PickerCancel),
                _ => None,
            },
        )
        .into()
    }

    // ── Magnifier ────────────────────────────────────────────────────

    /// Build a circular magnifier lens positioned near the cursor.
    ///
    /// The magnifier is placed above-right of the cursor and flips to
    /// the other side near screen edges.  No text labels — the magnifier
    /// is purely visual.  Returns `None` if no hover state is available
    /// (e.g. before the first pointer-motion event).
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    fn build_magnifier(&self) -> Option<Element<'static, Message>> {
        const GRID_SIZE: usize = 17;   // odd for centred crosshair
        const PIXEL_SCALE: f32 = 8.0;  // logical pixels per magnified cell
        const HALF: i32 = (GRID_SIZE / 2) as i32;
        const BELOW_OFFSET: f32 = 14.0;

        let picker = self.picker.as_ref()?;
        let hover = picker.hover.as_ref()?;
        let capture = picker.captures.get(hover.output_index)?;
        let (cx, cy) = hover.pixel_pos;

        let total = GRID_SIZE as f32 * PIXEL_SCALE;

        // ── Extract pixel grid ────────────────────────────────────────
        let mut pixels = Vec::with_capacity(GRID_SIZE * GRID_SIZE);
        for dy in -HALF..=HALF {
            for dx in -HALF..=HALF {
                let px = (cx as i32 + dx).max(0) as u32;
                let py = (cy as i32 + dy).max(0) as u32;
                pixels.push(capture.pixel_at(px, py).unwrap_or((128, 128, 128)));
            }
        }

        // ── Canvas program ─────────────────────────────────────────────
        let program = MagnifierProgram {
            pixels,
            grid_size: GRID_SIZE,
            pixel_size: PIXEL_SCALE,
        };

        let mag_canvas = canvas::Canvas::<_, Message, cosmic::Theme>::new(program)
            .width(Length::Fixed(total))
            .height(Length::Fixed(total));

        // ── Cursor-relative positioning ───────────────────────────────
        // The magnifier is placed above-right of the cursor so it never
        // hides the sampled pixel.  Near screen edges it flips sides.

        // Surface-local cursor coordinates (output-relative).
        let (cur_x, cur_y) = hover.local_pos;

        let offset_x = 12.0;  // right of cursor
        let offset_y = -(total + 12.0);  // above cursor

        let mut mag_x = cur_x + offset_x;
        let mut mag_y = cur_y + offset_y;

        let margin = 8.0;
        let ow = capture.logical_width as f32;
        let oh = capture.logical_height as f32;

        // Flip horizontally if magnifier overflows right edge.
        if mag_x + total > ow - margin {
            mag_x = cur_x - total - offset_x;
        }
        // Flip vertically if magnifier overflows top edge.
        if mag_y < margin {
            mag_y = cur_y + BELOW_OFFSET;
        }

        // Final clamping to stay within overlay bounds.
        mag_x = mag_x.max(margin).min((ow - total - margin).max(margin));
        mag_y = mag_y.max(margin).min((oh - total - margin).max(margin));

        // Position the fixed-size canvas inside a full-size transparent
        // container using the padding trick: padding from top & left
        // pushes the child to (mag_x, mag_y).
        Some(
            container(mag_canvas)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([mag_y, 0.0, 0.0, mag_x])
                .into(),
        )
    }

    /// Start the final (fast) capture step once *both* of the following are
    /// true: the popup has been confirmed closed, and we know whether the
    /// pre-negotiated sessions are ready (or failed).  Called from the
    /// `PopupClosed`, `SessionsPrepared`, and `SessionsPrepareFailed`
    /// handlers — whichever of the two conditions is satisfied last is the
    /// one that actually kicks off the capture.
    fn maybe_start_final_capture(&mut self) -> Task<cosmic::Action<Message>> {
        if !self.popup_confirmed_for_picker {
            return Task::none();
        }

        if let Some(prepared) = self.prepared_sessions.take() {
            self.popup_confirmed_for_picker = false;
            eprintln!("[picker]   sessions ready — starting final (fast) capture.");
            return Task::perform(picker::finish_all_outputs(prepared), |result| {
                let msg = match result {
                    Ok(outputs) => Message::CaptureCompleted(outputs),
                    Err(e) => Message::CaptureFailed(e.to_string()),
                };
                cosmic::Action::App(msg)
            });
        }

        if self.sessions_prepare_failed {
            self.popup_confirmed_for_picker = false;
            self.sessions_prepare_failed = false;
            eprintln!("[picker]   sessions failed — falling back to single-shot capture.");
            return Task::perform(picker::capture_all_outputs(), |result| {
                let msg = match result {
                    Ok(outputs) => Message::CaptureCompleted(outputs),
                    Err(e) => Message::CaptureFailed(e.to_string()),
                };
                cosmic::Action::App(msg)
            });
        }

        // Popup is closed but sessions aren't ready or failed yet — wait for
        // `SessionsPrepared` / `SessionsPrepareFailed`.
        Task::none()
    }

    /// Destroy all overlay surfaces and reopen the popup.
    /// Used when the picker is cancelled or capture fails.
    fn cancel_picker(&mut self) -> Task<cosmic::Action<Message>> {
        eprintln!("[picker] cancel_picker()");
        eprintln!("[picker]   pending_popup_close was {:?}, clearing", self.pending_popup_close);
        eprintln!("[picker]   picker state was {:?}",
            self.picker.as_ref().map(|p| p.state));
        self.pending_popup_close = None;
        self.popup_confirmed_for_picker = false;
        self.prepared_sessions = None;
        self.sessions_prepare_failed = false;
        *self.prepared_sessions_slot.lock().unwrap() = None;

        let mut tasks: Vec<Task<cosmic::Action<Message>>> = Vec::new();

        // Destroy all overlay surfaces if picker exists.
        if let Some(picker) = self.picker.take() {
            for id in &picker.overlay_ids {
                tasks.push(destroy_layer_surface(*id));
            }
        }

        // Destroy any pre-created (transparent) overlays that haven't been
        // populated with captures yet.
        for id in self.pending_overlay_ids.drain(..) {
            tasks.push(destroy_layer_surface(id));
        }

        // Reopen the popup if it's not already open.
        // Always reopen – even when picker was None (e.g. Escape pressed
        // before capture completed) – to avoid leaving the user without UI.
        if self.popup.is_none() {
            let new_id = Id::unique();
            self.popup.replace(new_id);
            let mut popup_settings = self.core.applet.get_popup_settings(
                self.core.main_window_id().unwrap(),
                new_id,
                None,
                None,
                None,
            );
            popup_settings.positioner.size_limits = Limits::NONE
                .max_width(372.0)
                .min_width(300.0)
                .min_height(200.0)
                .max_height(1080.0);
            tasks.push(get_popup(popup_settings));
        }

        if tasks.is_empty() { Task::none() } else { Task::batch(tasks) }
    }
}
