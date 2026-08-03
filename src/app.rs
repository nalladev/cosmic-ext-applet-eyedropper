// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use ::image::EncodableLayout;

use crate::activation;
use crate::config::{ColorFormat, Config};
use crate::fl;
use crate::picker::PickerController;
use crate::picker::{self, CapturedOutput, Color};
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use crate::widget::magnifier::{MagnifierProgram, MagnifierState};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::cctk::wayland_client::protocol::wl_output::WlOutput;
use cosmic::iced::clipboard;
use cosmic::iced::core::event::wayland::OutputEvent;
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::{
    applet::{menu_button, padded_control},
    cosmic_config::{self, ConfigSet, CosmicConfigEntry},
    cosmic_theme::Spacing,
    iced::{
        Alignment, Border, ContentFit, Event, Length, Limits, Subscription, event, mouse,
        widget::{MouseArea, Stack, column, container, row, space},
        window::{self, Id},
    },
    prelude::*,
    surface,
    surface::action::LiveSettings,
    theme,
    widget::{
        button, canvas, divider, icon, image, segmented_button, segmented_control, text, toggler,
    },
};

// ---------------------------------------------------------------------------
// Command-line flags
// ---------------------------------------------------------------------------

/// Command-line flags passed to the applet.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    /// Launch directly into colour-picker mode (`--pick`).
    pub pick: bool,
}

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
// Clipboard helpers
// ---------------------------------------------------------------------------

// Copy feedback tracks the [`ColorFormat`] that was last written to the
// clipboard, reusing the colour-format enum from the configuration.

/// Build a single-select segmented model for the colour-format setting,
/// pre-selecting `active`.
fn build_color_format_model(active: ColorFormat) -> segmented_button::SingleSelectModel {
    let mut model = segmented_button::Model::builder()
        .insert(|b| b.text(fl!("hex")).data(ColorFormat::Hex))
        .insert(|b| b.text(fl!("rgb")).data(ColorFormat::Rgb))
        .insert(|b| b.text(fl!("hsl")).data(ColorFormat::Hsl))
        .build();

    model.activate_position(match active {
        ColorFormat::Hex => 0,
        ColorFormat::Rgb => 1,
        ColorFormat::Hsl => 2,
    });
    model
}

// ---------------------------------------------------------------------------
// Magnifier canvas program
// ---------------------------------------------------------------------------

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
    /// Command-line flags this instance was launched with.
    flags: Flags,
    /// `true` while the deferred `--pick` capture is waiting for the first
    /// tracked output (overlay creation needs the output list).
    pending_start: bool,
    /// Handle to the COSMIC config, used to persist setting changes.
    config_context: cosmic_config::Config,
    /// Segmented-control model for the default colour-format setting.
    color_format_model: segmented_button::SingleSelectModel,

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

    // ── Pre-created overlay tracking ───────────────────────────────────
    /// Overlay window IDs that have been pre-created (transparent) but
    /// are not yet showing the frozen image.  Populated when entering
    /// picker mode; cleared by `OverlayCreated` or on cancel.
    pending_overlay_ids: Vec<window::Id>,

    // ── Clipboard feedback ───────────────────────────────────────────
    /// Which format was last copied (if any).
    copied_target: Option<ColorFormat>,
    /// When the last copy happened (for auto-clearing feedback).
    copied_at: Option<Instant>,

    // ── Magnifier state ─────────────────────────────────────────────
    magnifier: MagnifierState,
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
    /// The "Copy on selection" toggle was changed.
    SetCopyOnSelect(bool),
    /// A colour format was selected in the segmented control.
    SetDefaultFormat(segmented_button::Entity),

    // ── Capture flow ────────────────────────────────────────────────
    /// The eyedropper button was clicked in the popup.
    EyedropperClicked,
    /// A `pick` request arrived via D-Bus activation (`--pick` forwarded
    /// from a second invocation of the applet).
    DbusPick,
    /// Screenshot captured and per-output data is ready.
    CaptureCompleted(Vec<CapturedOutput>),
    /// The screenshot capture failed with an error message.
    CaptureFailed(String),

    // ── Wayland output tracking ─────────────────────────────────────
    OutputEvent(Box<OutputEvent>, WlOutput),

    // ── Picker mode ─────────────────────────────────────────────────
    /// User pressed Escape or overlay was closed externally.
    PickerCancel,
    /// Pointer moved on a picker overlay window.
    PointerMoved(Id, f32, f32),
    /// Pointer clicked on a picker overlay window.
    PointerClicked(Id),

    // ── Magnifier zoom ────────────────────────────────────────────────
    /// Scroll-delta from a mouse wheel or touchpad pinch.
    MagnifierZoom(f32),
    /// Periodic frame tick — applies pending zoom deltas.
    FrameTick,

    // ── Clipboard copy ───────────────────────────────────────────────
    /// Copy the HEX string to the clipboard.
    CopyHex,
    /// Copy the RGB string to the clipboard.
    CopyRgb,
    /// Copy the HSL string to the clipboard.
    CopyHsl,
    /// Auto-cleared after copy feedback timeout.
    ClearCopyFeedback,

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
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = "io.github.nalladev.CosmicExtAppletEyedropper";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let config_context = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).map_or_else(
            |_| {
                let ctx = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).unwrap();
                (ctx, Config::default())
            },
            |context| match Config::get_entry(&context) {
                Ok(config) => (context, config),
                Err((_errors, config)) => (context, config),
            },
        );

        let (config_context, config_entry) = config_context;
        let color_format_model = build_color_format_model(config_entry.default_color_format);
        let pending_start = flags.pick;

        let app = AppModel {
            core,
            config: config_entry,
            flags,
            pending_start,
            config_context,
            color_format_model,
            popup: None,
            sampled: None,
            error: None,
            hex: String::new(),
            rgb: String::new(),
            hsl: String::new(),
            outputs: Vec::new(),
            picker: None,
            pending_overlay_ids: Vec::new(),
            copied_target: None,
            copied_at: None,
            magnifier: MagnifierState::new(),
        };

        (app, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        // If an overlay window is closed externally, cancel the picker.
        if self
            .picker
            .as_ref()
            .is_some_and(|p| p.overlay_ids.contains(&id))
        {
            return Some(Message::PickerCancel);
        }
        // Otherwise it's the popup.
        if self.popup == Some(id) {
            return Some(Message::PopupClosed(id));
        }
        None
    }

    /// Draw the applet button in the panel.
    fn view(&self) -> Element<'_, Self::Message> {
        self.core
            .applet
            .icon_button("color-select-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }

    /// Draw a window – either the popup or a picker overlay.
    fn view_window(&self, id: Id) -> Element<'_, Self::Message> {
        // Is this a picker overlay (active picker or pre-created)?
        if self
            .picker
            .as_ref()
            .is_some_and(|p| p.overlay_ids.contains(&id))
            || self.pending_overlay_ids.contains(&id)
        {
            return self.view_picker_overlay(id);
        }

        // Is this the popup?
        if self.popup == Some(id) {
            return self.view_popup();
        }

        // Fallback: unknown window — render nothing.
        space::horizontal().width(Length::Fixed(1.0)).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let subs: Vec<Subscription<Self::Message>> = vec![
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
            // D-Bus activation (`--pick` forwarded to this instance)
            activation::subscription().map(|_| Message::DbusPick),
            // ~60 fps tick for throttled zoom application.
            cosmic::iced::time::every(Duration::from_millis(16)).map(|_| Message::FrameTick),
        ];

        Subscription::batch(subs)
    }

    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::TogglePopup => {
                // Ignore while in picker mode.
                if self.picker.is_some() {
                    return Task::none();
                }
                return if let Some(p) = self.popup.take() {
                    surface::surface_task(surface::action::destroy_popup(p))
                } else {
                    surface::surface_task(surface::action::app_popup(
                        |_| LiveSettings::default(),
                        |app: &mut AppModel| {
                            let new_id = Id::unique();
                            app.popup.replace(new_id);
                            let mut popup_settings = app.core.applet.get_popup_settings(
                                app.core.main_window_id().unwrap(),
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
                            popup_settings
                        },
                        None,
                    ))
                };
            }

            // ── Popup was closed ────────────────────────────────────────
            Message::PopupClosed(id) => {
                log::info!("[picker] PopupClosed({id:?})");

                // Normal popup lifecycle (user closed it manually).
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                    self.copied_target = None;
                    self.copied_at = None;
                    log::info!("[picker]   normal popup close — no capture.");
                    // One-shot CLI mode: the picker session is finished once
                    // the result popup is dismissed.
                    if self.flags.pick {
                        std::process::exit(0);
                    }
                }
            }

            Message::UpdateConfig(config) => {
                self.config = config;
                self.sync_color_format_model();
            }

            Message::SetCopyOnSelect(value) => {
                self.config.copy_on_select = value;
                let _ = self.config_context.set("copy_on_select", value);
            }

            Message::SetDefaultFormat(entity) => {
                self.color_format_model.activate(entity);
                if let Some(format) = self.color_format_model.data::<ColorFormat>(entity).copied()
                    && self.config.default_color_format != format
                {
                    self.config.default_color_format = format;
                    let _ = self.config_context.set("default_color_format", format);
                }
            }

            Message::EyedropperClicked => {
                log::info!("[picker] EyedropperClicked — starting Screenshot portal capture");
                return self.start_capture();
            }

            Message::DbusPick => {
                log::info!("[picker] DbusPick — pick requested via D-Bus activation");
                return self.start_capture();
            }

            Message::CaptureCompleted(captures) => {
                let t_capture = std::time::Instant::now();
                log::info!("[picker] CaptureCompleted — {} outputs", captures.len());
                for cap in &captures {
                    log::debug!(
                        "[picker]   output: {} {}x{} @({},{}) logical {}x{} rgba={}b",
                        cap.name,
                        cap.width,
                        cap.height,
                        cap.pos_x,
                        cap.pos_y,
                        cap.logical_width,
                        cap.logical_height,
                        cap.rgba.as_bytes().len(),
                    );
                }

                if captures.is_empty() {
                    log::error!("[picker]   captures is empty — error + cancel");
                    self.error = Some("No outputs captured".into());
                    // One-shot CLI mode: exit with a failure code instead of
                    // reopening the popup.
                    if self.flags.pick {
                        std::process::exit(1);
                    }
                    return self.cancel_picker();
                }

                // If picker mode was cancelled while capture was running,
                // discard the result.
                if self.picker.is_some() {
                    log::warn!(
                        "[picker]   WARNING: picker already exists — discard duplicate capture"
                    );
                    return Task::none();
                }

                log::debug!("[picker]   collecting pre-built image handles...");
                let mut image_handles = Vec::with_capacity(captures.len());
                for (i, cap) in captures.iter().enumerate() {
                    image_handles.push(cap.image_handle.clone());
                    log::debug!("[picker]   image_handle[{i}]: {}x{}", cap.width, cap.height);
                }

                // If overlays were pre-created (transparent) during
                // EyedropperClicked, reuse them — just populate the picker
                // with the captured data.  The overlay views will render
                // the frozen image on the next frame, completing the
                // flicker-free transition.
                if !self.pending_overlay_ids.is_empty() {
                    let overlay_ids = std::mem::take(&mut self.pending_overlay_ids);
                    log::debug!(
                        "[picker]   reusing {} pre-created overlay(s): {:?}",
                        overlay_ids.len(),
                        overlay_ids
                    );
                    let n_overlays = overlay_ids.len();
                    self.picker = Some(PickerController::new_with_captures(
                        captures,
                        image_handles,
                        overlay_ids,
                    ));
                    log::info!(
                        "[picker]   picker created in Picking state with {n_overlays} overlays (pre-created path)"
                    );
                    log::debug!(
                        "[picker]   CaptureCompleted handler took {:?}",
                        t_capture.elapsed(),
                    );
                    return Task::none();
                }

                // Fallback: create overlay windows now (no pre-creation).
                log::debug!(
                    "[picker]   creating overlay windows on {} outputs...",
                    self.outputs.len()
                );
                let mut tasks: Vec<Task<cosmic::Action<Self::Message>>> = Vec::new();
                let mut overlay_ids = Vec::new();

                for (i, output_state) in self.outputs.iter().enumerate() {
                    let overlay_id = output_state.id;
                    overlay_ids.push(overlay_id);
                    log::debug!(
                        "[picker]   creating overlay[{i}] id={overlay_id:?} on output '{}",
                        output_state.name
                    );
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
                    captures,
                    image_handles,
                    overlay_ids,
                ));
                log::info!("[picker]   picker created in Picking state with {n_overlays} overlays");
                log::debug!(
                    "[picker]   CaptureCompleted handler took {:?}",
                    t_capture.elapsed(),
                );

                return Task::batch(tasks);
            }

            // ── Capture failed ──────────────────────────────────────────
            Message::CaptureFailed(msg) => {
                log::error!("[picker] CaptureFailed: {msg}");
                self.error = Some(msg);
                // One-shot CLI mode: exit with a failure code instead of
                // reopening the popup.
                if self.flags.pick {
                    std::process::exit(1);
                }
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
                        if let Some(state) = self.outputs.iter_mut().find(|o| o.output == wl_output)
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

                // If launched with --pick, start the capture as soon as
                // outputs are known — overlay creation needs the tracked
                // output list, which is only populated by these events.
                if self.pending_start && !self.outputs.is_empty() && self.picker.is_none() {
                    self.pending_start = false;
                    return self.start_capture();
                }
            }

            // ── Pointer moved on a picker overlay ─────────────────────
            Message::PointerMoved(id, x, y) => {
                // Clone hover info to release the mutable borrow on
                // self.picker before populating the magnifier buffer.
                //
                // Reduce the sample sensitivity while zoomed in so small
                // cursor movements map to finer pixel steps; restored to
                // 1:1 at base zoom (8.0).
                let sensitivity = (8.0 / self.magnifier.zoom_level).clamp(0.0, 1.0);
                let hover = self
                    .picker
                    .as_mut()
                    .and_then(|p| p.on_pointer_motion(id, x, y, sensitivity));

                if let Some(hover) = hover {
                    const GRID: usize = 17;
                    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                    const HALF: i32 = (GRID / 2) as i32;
                    if let Some(capture) = self
                        .picker
                        .as_ref()
                        .and_then(|p| p.captures.get(hover.output_index))
                    {
                        let (cx, cy) = hover.pixel_pos;
                        let mut i = 0usize;
                        for dy in -HALF..=HALF {
                            for dx in -HALF..=HALF {
                                let px = (cx.cast_signed() + dx)
                                    .max(0)
                                    .min(capture.width.cast_signed() - 1)
                                    .cast_unsigned();
                                let py = (cy.cast_signed() + dy)
                                    .max(0)
                                    .min(capture.height.cast_signed() - 1)
                                    .cast_unsigned();
                                let (r, g, b) = capture.pixel_at(px, py).unwrap_or((128, 128, 128));
                                self.magnifier.buf[i] = r;
                                self.magnifier.buf[i + 1] = g;
                                self.magnifier.buf[i + 2] = b;
                                i += 3;
                            }
                        }
                    }
                } else {
                    log::warn!(
                        "[picker] PointerMoved({id:?}, {x:.0}, {y:.0}) — FAILED (no output match)"
                    );
                }
            }

            // ── Pointer clicked on a picker overlay ───────────────────
            Message::PointerClicked(id) => {
                log::info!("[picker] PointerClicked({id:?})");
                if let Some(picker) = self.picker.as_mut() {
                    log::debug!(
                        "[picker]   picker state={:?}, captures={}",
                        picker.state,
                        picker.captures.len()
                    );
                    if let Some(color) = picker.on_pointer_click(id) {
                        log::info!(
                            "[picker]   COLOUR SELECTED: {} / {} / {}",
                            color.hex(),
                            color.rgb(),
                            color.hsl()
                        );
                        // Colour selected — exit picker mode.
                        let overlays = picker.overlay_ids.clone();
                        self.picker.take();

                        self.sampled = Some(color);
                        self.update_color_strings(color);

                        let mut tasks: Vec<Task<cosmic::Action<Self::Message>>> = Vec::new();

                        // Automatically copy the picked colour when enabled,
                        // using the configured default format.
                        if self.config.copy_on_select {
                            let format = self.config.default_color_format;
                            let text = match format {
                                ColorFormat::Hex => self.hex.clone(),
                                ColorFormat::Rgb => self.rgb.clone(),
                                ColorFormat::Hsl => self.hsl.clone(),
                            };
                            log::info!("[picker]   auto-copy ({format:?}): {text}");
                            self.copied_target = Some(format);
                            self.copied_at = Some(Instant::now());
                            tasks.push(clipboard::write(text));
                        }

                        // Destroy all overlay surfaces.
                        for oid in &overlays {
                            tasks.push(destroy_layer_surface(*oid));
                        }

                        // Reopen the popup.
                        tasks.push(surface::surface_task(surface::action::app_popup(
                            |_| LiveSettings::default(),
                            |app: &mut AppModel| {
                                let new_id = Id::unique();
                                app.popup.replace(new_id);
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    app.core.main_window_id().unwrap(),
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
                                popup_settings
                            },
                            None,
                        )));

                        return Task::batch(tasks);
                    }
                }
                return Task::none().map(cosmic::Action::App);
            }

            // ── Magnifier zoom (scroll / pinch on overlay) ─────────────
            Message::MagnifierZoom(delta_y) => {
                // Accumulate — applied once per frame in FrameTick.
                self.magnifier.pending_zoom_delta += delta_y;
            }

            // ── Frame tick — apply throttled zoom ───────────────────
            Message::FrameTick => {
                let d = self.magnifier.pending_zoom_delta;
                if d != 0.0 {
                    self.magnifier.pending_zoom_delta = 0.0;
                    // Wheel-up (positive y) zooms in; wheel-down zooms out.
                    // The step shrinks as zoom rises (16.0 = midpoint of the
                    // 8..24 range) so high magnification gives finer control.
                    let step = d * 0.75 * (16.0 / self.magnifier.zoom_level);
                    self.magnifier.zoom_level = (self.magnifier.zoom_level + step).clamp(8.0, 24.0);
                }
            }

            // ── Clipboard copy ─────────────────────────────────────────
            Message::CopyHex => {
                let hex = self.hex.clone();
                if !hex.is_empty() {
                    self.copied_target = Some(ColorFormat::Hex);
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
                    self.copied_target = Some(ColorFormat::Rgb);
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
                    self.copied_target = Some(ColorFormat::Hsl);
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

            // ── Picker cancelled (Escape or external close) ────────────
            Message::PickerCancel => {
                log::info!("[picker] PickerCancel received");
                return self.cancel_picker();
            }

            Message::OverlayCreated(id) => {
                log::debug!("[picker] OverlayCreated({id:?}) — overlay surface ready");
            }
        }

        Task::none()
    }

    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::Theme,
    ) -> Task<cosmic::Action<Self::Message>> {
        let _ = new_theme;
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
    /// Update the cached color representation strings (hex, rgb, hsl) from a Color.
    fn update_color_strings(&mut self, color: Color) {
        self.hex = color.hex();
        self.rgb = color.rgb();
        self.hsl = color.hsl();
    }

    /// Begin a screen capture and enter picker mode once it completes.
    ///
    /// Shared by the applet button, the `--pick` command-line option, and
    /// D-Bus activation.  Ignores the request if a picker session is already
    /// active.
    fn start_capture(&mut self) -> Task<cosmic::Action<Message>> {
        // Ignore if already picking.
        if self.picker.is_some() {
            log::warn!("[picker]   WARNING: ignored — picker already active");
            return Task::none();
        }

        self.error = None;
        self.sampled = None;
        self.copied_target = None;
        self.copied_at = None;
        self.magnifier.reset();

        // Start capture in background.
        let capture_task = Task::perform(
            picker::capture_outputs(),
            |result: Result<Vec<CapturedOutput>, anyhow::Error>| match result {
                Ok(captures) => Message::CaptureCompleted(captures),
                Err(e) => Message::CaptureFailed(e.to_string()),
            },
        )
        .map(cosmic::Action::App);

        // Close popup if open.
        if let Some(popup_id) = self.popup.take() {
            return Task::batch(vec![
                surface::surface_task(surface::action::destroy_popup(popup_id)),
                capture_task,
            ]);
        }

        // Popup already closed, just start capture.
        capture_task
    }

    /// Keep the segmented-control selection in sync with the configured
    /// default colour format (e.g. after an external config change).
    fn sync_color_format_model(&mut self) {
        self.color_format_model
            .activate_position(match self.config.default_color_format {
                ColorFormat::Hex => 0,
                ColorFormat::Rgb => 1,
                ColorFormat::Hsl => 2,
            });
    }

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
        target: ColorFormat,
        msg: Message,
    ) -> Element<'_, Message> {
        let just_copied = self
            .copied_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            && self.copied_target == Some(target);

        let Spacing { space_xxs, .. } = theme::active().cosmic().spacing;

        let indicator: Element<'_, Message> = if just_copied {
            icon::from_name("object-select-symbolic")
                .size(14)
                .symbolic(true)
                .into()
        } else {
            icon::from_name("edit-copy-symbolic")
                .size(14)
                .symbolic(true)
                .into()
        };

        let content = row![
            row![
                text::caption_heading(label),
                text::monotext(value.to_owned()),
            ]
            .spacing(f32::from(space_xxs))
            .width(Length::Fill)
            .align_y(Alignment::Center),
            indicator,
        ]
        .spacing(f32::from(space_xxs))
        .align_y(Alignment::Center);

        menu_button(content).on_press(msg).into()
    }

    /// Render the normal eyedropper popup.
    fn view_popup(&self) -> Element<'_, Message> {
        let Spacing {
            space_xxxs: _,
            space_xxs,
            space_xs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;
        let corner_radii = theme::active().cosmic().corner_radii;

        // Derive display strings.
        let (hex_val, rgb_val, hsl_val): (String, String, String) = if let Some(c) = self.sampled {
            (c.hex(), c.rgb(), c.hsl())
        } else {
            (self.hex.clone(), self.rgb.clone(), self.hsl.clone())
        };

        let has_color = self.sampled.is_some();

        // Colour swatch.
        let swatch_color = self.sampled.map_or(cosmic::iced::Color::WHITE, |c| {
            cosmic::iced::Color::from_rgb8(c.r, c.g, c.b)
        });

        let swatch =
            container(space::horizontal())
                .width(32)
                .height(32)
                .style(move |_: &cosmic::Theme| container::Style {
                    background: Some(swatch_color.into()),
                    border: Border {
                        radius: corner_radii.radius_s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });

        // Centre text: HEX value or placeholder.
        let centre: Element<'_, Message> = if has_color {
            container(text::body(hex_val.clone()).align_y(Alignment::Center))
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .into()
        } else {
            container(text::body(fl!("no-color-selected")).align_y(Alignment::Center))
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .into()
        };

        // "Select Colour" button (primary action).
        let select_button =
            button::suggested(fl!("select-colour")).on_press(Message::EyedropperClicked);

        let heading = row![swatch, centre, select_button,]
            .spacing(f32::from(space_xs))
            .align_y(Alignment::Center);

        let mut content = column![padded_control(heading)]
            .padding([space_xxs, 0])
            .spacing(0);

        // ── Colour values section ───────────────────────────────────────
        if has_color {
            content = content
                .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]))
                .push(self.color_row(fl!("hex"), &hex_val, ColorFormat::Hex, Message::CopyHex))
                .push(self.color_row(fl!("rgb"), &rgb_val, ColorFormat::Rgb, Message::CopyRgb))
                .push(self.color_row(fl!("hsl"), &hsl_val, ColorFormat::Hsl, Message::CopyHsl));
        }

        // Status / error message.
        if let Some(ref err) = self.error {
            content = content.push(padded_control(text::body(err)).padding([space_xxs, space_s]));
        }

        // ── Settings: copy on selection ─────────────────────────────────
        content = content
            .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]))
            .push(padded_control(
                row![
                    text::body(fl!("copy-on-select")),
                    space::horizontal(),
                    toggler(self.config.copy_on_select).on_toggle(Message::SetCopyOnSelect),
                ]
                .align_y(Alignment::Center),
            ));

        // The default-format choice only matters when auto-copy is enabled.
        if self.config.copy_on_select {
            content = content
                .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]))
                .push(padded_control(
                    column![text::body(fl!("default-color-format"))]
                        .push(
                            segmented_control::horizontal(&self.color_format_model)
                                .on_activate(Message::SetDefaultFormat),
                        )
                        .spacing(f32::from(space_xxs)),
                ));
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
                log::debug!(
                    "[picker] view_picker_overlay({id:?}) — pre-created, transparent placeholder"
                );
                let event_layer = MouseArea::new(
                    container(space::horizontal())
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .interaction(mouse::Interaction::Crosshair);

                return KeyboardWrapper::new(event_layer, |key, _modifiers| match key {
                    Key::Named(Named::Escape) => Some(Message::PickerCancel),
                    _ => None,
                })
                .into();
            }
            log::debug!("[picker] view_picker_overlay({id:?}) — no picker, rendering placeholder");
            return space::horizontal().width(Length::Fixed(1.0)).into();
        };

        // ── Picking state: full interaction ────────────────────────────
        let on_move = move |point: cosmic::iced::Point| Message::PointerMoved(id, point.x, point.y);

        // Background layer: captured framebuffer (frozen desktop).
        let image_layer: Option<Element<'_, Message>> = {
            let output_idx = picker.overlay_ids.iter().position(|oid| *oid == id);
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
        let on_scroll = move |delta: mouse::ScrollDelta| {
            let y = match delta {
                // Lines (mouse wheel) or Pixels (touchpad two-finger scroll).
                mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => y,
            };
            Message::MagnifierZoom(y)
        };
        let event_layer = MouseArea::new(
            container(space::horizontal())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_move(on_move)
        .on_press(Message::PointerClicked(id))
        .on_scroll(on_scroll)
        .interaction(mouse::Interaction::Crosshair);

        let mut stack = Stack::new();
        if let Some(img) = image_layer {
            stack = stack.push(img);
        }
        stack = stack.push(event_layer);

        if let Some(mag) = self.build_magnifier() {
            stack = stack.push(mag);
        }

        KeyboardWrapper::new(stack, |key, _modifiers| match key {
            Key::Named(Named::Escape) => Some(Message::PickerCancel),
            _ => None,
        })
        .into()
    }

    // ── Magnifier ────────────────────────────────────────────────────

    /// Build a circular magnifier lens positioned near the cursor.
    ///
    /// The magnifier is placed above-right of the cursor and flips to
    /// the other side near screen edges.  No text labels — the magnifier
    /// is purely visual.  Returns `None` if no hover state is available
    /// (e.g. before the first pointer-motion event).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn build_magnifier(&self) -> Option<Element<'static, Message>> {
        const GRID_SIZE: usize = 17; // odd for centred crosshair
        const BELOW_OFFSET: f32 = 14.0;

        let picker = self.picker.as_ref()?;
        let hover = picker.hover.as_ref()?;
        let capture = picker.captures.get(hover.output_index)?;

        let pixel_size = self.magnifier.zoom_level;
        let total = GRID_SIZE as f32 * pixel_size;

        // ── Canvas program (reads pre-filled buffer) ─────────────────
        let program = MagnifierProgram {
            pixels: self.magnifier.buf.to_vec(),
            grid_size: GRID_SIZE,
            pixel_size,
        };

        let mag_canvas = canvas::Canvas::<_, Message, cosmic::Theme>::new(program)
            .width(Length::Fixed(total))
            .height(Length::Fixed(total));

        // ── Cursor-relative positioning ───────────────────────────────
        // The magnifier is placed above-right of the cursor so it never
        // hides the sampled pixel.  Near screen edges it flips sides.

        // Surface-local cursor coordinates (output-relative).
        let (cur_x, cur_y) = hover.local_pos;

        let offset_x = 12.0; // right of cursor
        let offset_y = -(total + 12.0); // above cursor

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

    /// Destroy all overlay surfaces and reopen the popup.
    /// Used when the picker is cancelled or capture fails.
    fn cancel_picker(&mut self) -> Task<cosmic::Action<Message>> {
        log::info!("[picker] cancel_picker()");
        log::info!(
            "[picker]   picker state was {:?}",
            self.picker.as_ref().map(|p| p.state)
        );

        // One-shot CLI mode (--pick): the picker session has ended — exit
        // instead of reopening the popup.
        if self.flags.pick {
            std::process::exit(0);
        }

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
            tasks.push(surface::surface_task(surface::action::app_popup(
                |_| LiveSettings::default(),
                |app: &mut AppModel| {
                    let new_id = Id::unique();
                    app.popup.replace(new_id);
                    let mut popup_settings = app.core.applet.get_popup_settings(
                        app.core.main_window_id().unwrap(),
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
                    popup_settings
                },
                None,
            )));
        }

        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }
}
