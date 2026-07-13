// SPDX-License-Identifier: MPL-2.0

//! Persistent Wayland capture helper.
//!
//! Uses the XDG Desktop Portal `ScreenCast` API + `PipeWire` for capture.
//! This works in both native and Flatpak builds, unlike the previous
//! `ext-image-copy-capture-v1` approach which failed in the sandbox.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::thread;

use cosmic::cctk::sctk;
use cosmic::cctk::sctk::output::{OutputHandler, OutputInfo, OutputState};
use cosmic::cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic::cctk::sctk::shm::{Shm, ShmHandler};
use cosmic::cctk::wayland_client::{
    Connection, QueueHandle, globals::registry_queue_init, protocol::wl_output,
};
use image::RgbaImage;

// ---------------------------------------------------------------------------
// CaptureHelper – persistent Wayland connection for output discovery only
// ---------------------------------------------------------------------------

/// A persistent helper that owns a dedicated Wayland connection and dispatch
/// thread for output discovery.  Capture itself goes through the portal.
#[derive(Clone)]
pub struct CaptureHelper {
    inner: Arc<CaptureHelperInner>,
}

struct CaptureHelperInner {
    #[allow(dead_code)]
    conn: Connection,
    outputs: Mutex<Vec<wl_output::WlOutput>>,
    output_infos: Mutex<HashMap<wl_output::WlOutput, OutputInfo>>,
    #[allow(dead_code)]
    qh: QueueHandle<AppData>,
}

impl Default for CaptureHelper {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureHelper {
    /// Connect to the Wayland compositor, discover outputs, and spawn a
    /// persistent dispatch thread for output tracking.
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn new() -> Self {
        eprintln!("[capture] CaptureHelper::new() — Wayland connection for output discovery");

        let wayland_display =
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-1".to_string());
        let socket_path = format!(
            "{}/{}",
            std::env::var("XDG_RUNTIME_DIR")
                .expect("XDG_RUNTIME_DIR must be set to connect to Wayland"),
            wayland_display
        );
        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .expect("CaptureHelper: failed to open Wayland socket");
        let conn = Connection::from_socket(stream)
            .expect("CaptureHelper: failed to create Wayland connection");
        let (globals, mut event_queue) =
            registry_queue_init::<AppData>(&conn).expect("CaptureHelper: registry_queue_init");
        let qh = event_queue.handle();

        let registry_state = RegistryState::new(&globals);
        let shm_state = Shm::bind(&globals, &qh).expect("CaptureHelper: Shm::bind");

        let helper = CaptureHelper {
            inner: Arc::new(CaptureHelperInner {
                conn: conn.clone(),
                outputs: Mutex::new(Vec::new()),
                output_infos: Mutex::new(HashMap::new()),
                qh: qh.clone(),
            }),
        };

        let mut data = AppData {
            registry_state,
            output_state: OutputState::new(&globals, &qh),
            shm_state,
            helper: helper.clone(),
        };

        // First roundtrip discovers outputs.
        event_queue
            .roundtrip(&mut data)
            .expect("CaptureHelper: initial roundtrip");

        let n_outputs = helper.inner.outputs.lock().unwrap().len();
        eprintln!(
            "[capture] CaptureHelper initialized — {n_outputs} output(s), spawning dispatch thread"
        );

        // Spawn persistent dispatch thread for output tracking.
        thread::spawn(move || {
            loop {
                if event_queue.blocking_dispatch(&mut data).is_err() {
                    eprintln!("[capture] CaptureHelper dispatch thread: connection lost, exiting");
                    break;
                }
            }
        });

        helper
    }

    /// List all known outputs.
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn outputs(&self) -> Vec<wl_output::WlOutput> {
        self.inner.outputs.lock().unwrap().clone()
    }

    /// Get the output info for a given output.
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn output_info(&self, output: &wl_output::WlOutput) -> Option<OutputInfo> {
        self.inner.output_infos.lock().unwrap().get(output).cloned()
    }

    fn set_output_info(&self, output: &wl_output::WlOutput, info: Option<OutputInfo>) {
        let mut map = self.inner.output_infos.lock().unwrap();
        match info {
            Some(i) => {
                map.insert(output.clone(), i);
            }
            None => {
                map.remove(output);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Portal output metadata (returned by prepare phase)
// ---------------------------------------------------------------------------

/// Metadata for a captured output from the portal.
pub struct PortalOutputInfo {
    pub name: String,
    pub pos_x: i32,
    pub pos_y: i32,
    pub logical_width: u32,
    pub logical_height: u32,
}

// ---------------------------------------------------------------------------
// PreparedCapture — portal session data ready for the fast PipeWire step.
// ---------------------------------------------------------------------------

/// Shared state from a portal `ScreenCast` session.
pub(crate) struct PortalSession {
    pipewire_fd: OwnedFd,
}

/// The result of the prepare phase: a portal session with `PipeWire` fd and
/// per-stream info.  Only the (fast) `PipeWire` frame-grab step remains.
pub struct PreparedCapture {
    pub(crate) session: Arc<PortalSession>,
    pub node_id: u32,
    pub width: u32,
    pub height: u32,
}

// ---------------------------------------------------------------------------
// ShmImage — captured frame data
// ---------------------------------------------------------------------------

/// A single captured frame as raw pixel data.
pub struct ShmImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl ShmImage {
    /// Decode as an RGBA image.
    #[allow(clippy::missing_errors_doc)]
    pub fn image(&self) -> anyhow::Result<RgbaImage> {
        RgbaImage::from_raw(self.width, self.height, self.pixels.clone())
            .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))
    }

    /// Like `image()` but applies transform (no-op for portal/`PipeWire` data
    /// which is already in correct orientation).
    #[allow(clippy::missing_errors_doc)]
    pub fn image_transformed(&self) -> anyhow::Result<RgbaImage> {
        self.image()
    }
}

// ---------------------------------------------------------------------------
// Portal + PipeWire capture functions (called from wayland.rs)
// ---------------------------------------------------------------------------

/// Create a portal `ScreenCast` session for all monitors and return
/// the `PipeWire` fd + per-stream metadata.
///
/// This is the async, D-Bus-heavy part of a capture.  Run it in a
/// `Task::perform` so it doesn't block the UI thread.
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub async fn portal_prepare_all(
    helper: &CaptureHelper,
    restore_token: Option<&str>,
) -> Result<(Vec<(PreparedCapture, PortalOutputInfo)>, Option<String>), anyhow::Error> {
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};

    eprintln!("[capture] portal_prepare_all: creating ScreenCast session");

    let proxy = Screencast::new()
        .await
        .map_err(|e| anyhow::anyhow!("Screencast::new failed: {e}"))?;
    let session = proxy
        .create_session()
        .await
        .map_err(|e| anyhow::anyhow!("create_session failed: {e}"))?;

    proxy
        .select_sources(
            &session,
            CursorMode::Hidden,
            SourceType::Monitor.into(),
            true,          // multiple monitors
            restore_token, // pass restore_token here for permission persistence
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(|e| anyhow::anyhow!("select_sources failed: {e}"))?;

    let response = proxy
        .start(&session, None)
        .await
        .map_err(|e| anyhow::anyhow!("start failed: {e}"))?
        .response()
        .map_err(|e| anyhow::anyhow!("start response error: {e}"))?;

    let restore_token = response
        .restore_token()
        .map(std::string::ToString::to_string);
    eprintln!("[capture] portal_prepare_all: got restore_token: {restore_token:?}");

    let streams = response.streams();
    eprintln!(
        "[capture] portal_prepare_all: got {} stream(s)",
        streams.len()
    );

    let pw_fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| anyhow::anyhow!("open_pipe_wire_remote failed: {e}"))?;

    let session = Arc::new(PortalSession { pipewire_fd: pw_fd });

    // Match portal streams to our discovered outputs by position.
    let wl_outputs = helper.outputs();
    let mut results = Vec::new();

    for stream in streams {
        let node_id = stream.pipe_wire_node_id();
        let stream_pos = stream.position();
        let stream_size = stream.size();

        eprintln!("[capture]   stream node={node_id} pos={stream_pos:?} size={stream_size:?}");

        // Find the matching Wayland output by position + size.
        let matched = wl_outputs.iter().find_map(|o| {
            let info = helper.output_info(o)?;
            let (ox, oy) = info.location;
            let (lw, lh) = info.logical_size.unwrap_or((0, 0));
            let (sw, sh) = stream_size.unwrap_or((0, 0));

            if ox == stream_pos.map_or(ox, |p| p.0)
                && oy == stream_pos.map_or(oy, |p| p.1)
                && lw == sw
                && lh == sh
            {
                Some(info)
            } else if wl_outputs.len() == 1 && lw == sw && lh == sh {
                // Single monitor: match by size only.
                Some(info)
            } else {
                None
            }
        });

        let (sw, sh) = stream_size.unwrap_or((0, 0));
        let width = u32::try_from(sw.max(0)).unwrap_or(0);
        let height = u32::try_from(sh.max(0)).unwrap_or(0);

        if width == 0 || height == 0 {
            eprintln!("[capture]   SKIP: zero-sized stream");
            continue;
        }

        let output_info = if let Some(info) = matched {
            PortalOutputInfo {
                name: info.name.clone().unwrap_or_default(),
                pos_x: info.location.0,
                pos_y: info.location.1,
                logical_width: info
                    .logical_size
                    .map_or(width, |s| u32::try_from(s.0.max(0)).unwrap_or(0)),
                logical_height: info
                    .logical_size
                    .map_or(height, |s| u32::try_from(s.1.max(0)).unwrap_or(0)),
            }
        } else {
            eprintln!("[capture]   WARNING: no matching output for stream, using stream metadata");
            PortalOutputInfo {
                name: format!("monitor-{}", results.len()),
                pos_x: stream_pos.map_or(0, |p| p.0),
                pos_y: stream_pos.map_or(0, |p| p.1),
                logical_width: width,
                logical_height: height,
            }
        };

        results.push((
            PreparedCapture {
                session: session.clone(),
                node_id,
                width,
                height,
            },
            output_info,
        ));
    }

    eprintln!(
        "[capture] portal_prepare_all: prepared {} output(s)",
        results.len()
    );
    Ok((results, restore_token))
}

/// Connect to `PipeWire` via the prepared fd and grab one frame per stream.
///
/// This is the fast part — all D-Bus negotiation happened in
/// [`portal_prepare_all`].
#[allow(clippy::missing_errors_doc)]
pub fn pipewire_finish_all(
    prepared: &[(PreparedCapture, PortalOutputInfo)],
) -> Result<Vec<(ShmImage, PortalOutputInfo)>, anyhow::Error> {
    use pipewire as pw;

    if prepared.is_empty() {
        return Err(anyhow::anyhow!("No prepared captures"));
    }

    eprintln!(
        "[capture] pipewire_finish_all: connecting PipeWire for {} stream(s)",
        prepared.len()
    );

    // Initialize PipeWire (idempotent).
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None)
        .map_err(|e| anyhow::anyhow!("MainLoop::new failed: {e}"))?;
    let context = pw::context::Context::new(&mainloop)
        .map_err(|e| anyhow::anyhow!("Context::new failed: {e}"))?;

    // Clone the fd for the PipeWire core connection.
    let core_fd = prepared[0]
        .0
        .session
        .pipewire_fd
        .try_clone()
        .map_err(|e| anyhow::anyhow!("fd clone failed: {e}"))?;
    let core = context
        .connect_fd(core_fd, None)
        .map_err(|e| anyhow::anyhow!("connect_fd failed: {e}"))?;

    let mut results = Vec::with_capacity(prepared.len());

    for (prep, info) in prepared {
        let node_id = prep.node_id;
        let width = prep.width;
        let height = prep.height;
        eprintln!("[capture]   capturing stream node={node_id} {width}x{height}");

        if let Some(shm) = capture_one_stream(&core, &mainloop, node_id, width, height) {
            eprintln!(
                "[capture]   stream '{}`: captured {}x{}",
                info.name, shm.width, shm.height
            );
            results.push((
                shm,
                PortalOutputInfo {
                    name: info.name.clone(),
                    pos_x: info.pos_x,
                    pos_y: info.pos_y,
                    logical_width: info.logical_width,
                    logical_height: info.logical_height,
                },
            ));
        } else {
            eprintln!("[capture]   stream '{}': FAILED", info.name);
        }
    }

    eprintln!(
        "[capture] pipewire_finish_all: captured {}/{} stream(s)",
        results.len(),
        prepared.len()
    );
    Ok(results)
}

/// Capture a single frame from one `PipeWire` stream.
#[allow(clippy::too_many_lines)]
fn capture_one_stream(
    core: &pipewire::core::Core,
    mainloop: &pipewire::main_loop::MainLoop,
    node_id: u32,
    width: u32,
    height: u32,
) -> Option<ShmImage> {
    use pipewire as pw;
    use pw::properties::properties;
    use pw::spa;

    struct StreamData {
        format: spa::param::video::VideoInfoRaw,
        frame_data: Option<Vec<u8>>,
        frame_width: u32,
        frame_height: u32,
        done: bool,
    }

    let data = Arc::new(Mutex::new(StreamData {
        format: spa::param::video::VideoInfoRaw::default(),
        frame_data: None,
        frame_width: 0,
        frame_height: 0,
        done: false,
    }));

    let stream = pw::stream::Stream::new(
        core,
        &format!("capture-{node_id}"),
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .ok()?;

    let data_clone = data.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data_clone)
        .state_changed(|_, _, old, new| {
            eprintln!("[capture]     PipeWire state: {old:?} -> {new:?}");
        })
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }

            let Ok((media_type, media_subtype)) = pw::spa::param::format_utils::parse_format(param)
            else {
                return;
            };

            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            let mut d = user_data.lock().unwrap();
            d.format.parse(param).ok();
            eprintln!(
                "[capture]     PipeWire format: {}x{}",
                d.format.size().width,
                d.format.size().height,
            );
        })
        .process(|stream, user_data| match stream.dequeue_buffer() {
            None => {
                eprintln!("[capture]     PipeWire: out of buffers");
            }
            Some(mut buffer) => {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let chunk = data.chunk();
                let size = chunk.size() as usize;

                if let Some(slice) = data.data() {
                    let mut d = user_data.lock().unwrap();
                    let buf_size = d.format.size();
                    d.frame_width = buf_size.width;
                    d.frame_height = buf_size.height;
                    d.frame_data = Some(slice[..size].to_vec());
                    d.done = true;
                    eprintln!(
                        "[capture]     PipeWire: got frame {} bytes, {}x{}",
                        size, d.frame_width, d.frame_height
                    );
                }
            }
        })
        .register()
        .ok()?;

    // Negotiate format: ask for BGRx (common for screen capture).
    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRA,
            pw::spa::param::video::VideoFormat::RGBA,
            pw::spa::param::video::VideoFormat::RGBx,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle { width, height },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pw::spa::utils::Rectangle {
                width: 7680,
                height: 4320
            }
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .ok()?
    .0
    .into_inner();

    let mut params = [spa::pod::Pod::from_bytes(&values).unwrap()];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .ok()?;

    // Run the main loop until we get a frame (with timeout).
    let data_ref = data.clone();
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);

    loop {
        if start.elapsed() > timeout {
            eprintln!("[capture]     PipeWire: timeout waiting for frame");
            break;
        }
        mainloop
            .loop_()
            .iterate(std::time::Duration::from_millis(100));

        let d = data_ref.lock().unwrap();
        if d.done {
            let pw = d.frame_data.clone();
            let w = d.frame_width;
            let h = d.frame_height;
            drop(d);

            if let Some(pixels) = pw {
                eprintln!(
                    "[capture]     PipeWire: captured {w}x{h} ({} bytes)",
                    pixels.len()
                );
                return Some(ShmImage {
                    pixels,
                    width: w,
                    height: h,
                });
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Wayland output tracking (unchanged from before, minus screencopy)
// ---------------------------------------------------------------------------

struct AppData {
    registry_state: RegistryState,
    output_state: OutputState,
    shm_state: Shm,
    helper: CaptureHelper,
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    fn runtime_add_global(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _name: u32,
        _interface: &str,
        _version: u32,
    ) {
    }

    fn runtime_remove_global(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _name: u32,
        _interface: &str,
    ) {
    }
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        output: wl_output::WlOutput,
    ) {
        let info = self.output_state.info(&output);
        self.helper.set_output_info(&output, info);
        self.helper.inner.outputs.lock().unwrap().push(output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        output: wl_output::WlOutput,
    ) {
        let info = self.output_state.info(&output);
        self.helper.set_output_info(&output, info);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        output: wl_output::WlOutput,
    ) {
        self.helper.set_output_info(&output, None);
        let mut outputs = self.helper.inner.outputs.lock().unwrap();
        if let Some(idx) = outputs.iter().position(|o| *o == output) {
            outputs.remove(idx);
        }
    }
}

// ---------------------------------------------------------------------------
// Delegation macros
// ---------------------------------------------------------------------------

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_shm!(AppData);
