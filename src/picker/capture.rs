// SPDX-License-Identifier: MPL-2.0

//! Persistent Wayland capture helper.
//!
//! This module is a faithful copy of the architecture from
//! `xdg-desktop-portal-cosmic`'s `src/wayland/mod.rs`.
//!
//! Instead of creating a fresh Wayland connection per capture, we maintain a
//! persistent connection with a dedicated background dispatch thread.  Capture
//! sessions are created on this connection and results are synchronised back
//! via condvars for blocking waits.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

use cosmic::cctk::screencopy::{
    CaptureFrame, CaptureOptions, CaptureSession as CtkSession, Capturer,
    FailureReason, Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt,
    ScreencopyHandler, ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
};
use cosmic::cctk::sctk::output::{OutputHandler, OutputInfo, OutputState};
use cosmic::cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic::cctk::sctk::shm::{Shm, ShmHandler};
use cosmic::cctk::sctk::{self};
use cosmic::cctk::wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm, wl_shm_pool},
};
use image::RgbaImage;

pub use cosmic::cctk::screencopy::{CaptureSource, Rect};

// ---------------------------------------------------------------------------
// memfd creation (copied from portal's src/buffer.rs)
// ---------------------------------------------------------------------------

fn create_memfd(width: u32, height: u32) -> OwnedFd {
    let name = c"pipewire-screencopy";
    let fd =
        rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC).expect("memfd_create");
    rustix::fs::ftruncate(&fd, (width as u64 * height as u64 * 4) as _).expect("ftruncate");
    fd
}

// ---------------------------------------------------------------------------
// CaptureHelper – persistent Wayland connection with background dispatch
// ---------------------------------------------------------------------------

/// A persistent helper that owns a dedicated Wayland connection and dispatch
/// thread for the lifetime of the applet.  All capture sessions are created
/// on this connection.
#[derive(Clone)]
pub struct CaptureHelper {
    inner: Arc<CaptureHelperInner>,
}

struct CaptureHelperInner {
    conn: Connection,
    outputs: Mutex<Vec<wl_output::WlOutput>>,
    output_infos: Mutex<HashMap<wl_output::WlOutput, OutputInfo>>,
    qh: QueueHandle<AppData>,
    capturer: Capturer,
    wl_shm: wl_shm::WlShm,
}

impl CaptureHelper {
    /// Connect to the Wayland compositor, bind globals, discover outputs, and
    /// spawn a persistent dispatch thread.
    pub fn new() -> Self {
        eprintln!(
            "[capture] CaptureHelper::new() — creating persistent Wayland connection"
        );

        // Force a fresh Wayland socket connection by manually connecting to
        // the socket file, ignoring WAYLAND_SOCKET fd inheritance.  When spawned
        // by cosmic-panel, the environment may have WAYLAND_SOCKET set to the
        // panel's own socket fd; reusing that shared connection causes the
        // compositor to delay the first screencopy request by ~6.4 seconds.
        let wayland_display = std::env::var("WAYLAND_DISPLAY")
            .unwrap_or_else(|_| "wayland-1".to_string());
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
        let screencopy_state = ScreencopyState::new(&globals, &qh);
        let shm_state = Shm::bind(&globals, &qh).expect("CaptureHelper: Shm::bind");

        let helper = CaptureHelper {
            inner: Arc::new(CaptureHelperInner {
                conn: conn.clone(),
                outputs: Mutex::new(Vec::new()),
                output_infos: Mutex::new(HashMap::new()),
                qh: qh.clone(),
                capturer: screencopy_state.capturer().clone(),
                wl_shm: shm_state.wl_shm().clone(),
            }),
        };

        let mut data = AppData {
            registry_state,
            screencopy_state,
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
            "[capture] CaptureHelper initialized — {} output(s) found, spawning dispatch thread",
            n_outputs
        );

        // Spawn the persistent dispatch thread (copied from portal).
        thread::spawn(move || loop {
            if event_queue.blocking_dispatch(&mut data).is_err() {
                eprintln!(
                    "[capture] CaptureHelper dispatch thread: connection lost, exiting"
                );
                break;
            }
        });

        helper
    }

    /// List all known outputs.
    pub fn outputs(&self) -> Vec<wl_output::WlOutput> {
        self.inner.outputs.lock().unwrap().clone()
    }

    /// Get the output info for a given output.
    pub fn output_info(&self, output: &wl_output::WlOutput) -> Option<OutputInfo> {
        self.inner
            .output_infos
            .lock()
            .unwrap()
            .get(output)
            .cloned()
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

    /// Create a capture session for the given source (copied from portal).
    pub fn capture_source_session(&self, source: CaptureSource) -> CaptureSession {
        CaptureSession(Arc::new_cyclic(|weak_session| {
            let options = CaptureOptions::empty();
            let ctk_session = self
                .inner
                .capturer
                .create_session(
                    &source,
                    options,
                    &self.inner.qh,
                    SessionData {
                        session: weak_session.clone(),
                        session_data: ScreencopySessionData::default(),
                    },
                )
                .expect(
                    "create_session failed — compositor does not support \
                     ext-image-copy-capture-v1",
                );
            self.inner.conn.flush().unwrap();
            CaptureSessionInner {
                conn: self.inner.conn.clone(),
                capture_session: ctk_session,
                state: Mutex::new(SessionState::default()),
                condvar: Condvar::new(),
            }
        }))
    }

    /// Negotiate a capture session and allocate a buffer for `source`,
    /// without grabbing the actual frame yet.
    ///
    /// This is the slow, round-trip-heavy half of a capture (session
    /// creation + format negotiation + buffer allocation).  It has no
    /// dependency on what is currently on screen, so callers can run it
    /// concurrently with other UI transitions (e.g. closing our popup) and
    /// pay only for the fast [`Self::finish_capture_shm_blocking`] step once
    /// it is actually safe to grab pixels.
    pub fn prepare_source_shm_blocking(&self, source: CaptureSource) -> Option<PreparedCapture> {
        let session = self.capture_source_session(source);

        // Wait for the compositor to send formats (BufferSize + ShmFormat + Done).
        let formats = session.wait_for_formats_blocking()?;
        let (width, height) = formats.buffer_size;

        if width == 0 || height == 0 {
            eprintln!(
                "[capture] prepare_source_shm_blocking: compositor gave zero-sized buffer"
            );
            return None;
        }

        eprintln!(
            "[capture] prepare_source_shm_blocking: {}x{} format=Abgr8888",
            width, height
        );

        // Create memfd and SHM buffer (copied from portal's create_shm_buffer).
        let fd = create_memfd(width, height);
        let buffer =
            self.create_shm_buffer(&fd, width, height, width * 4, wl_shm::Format::Abgr8888);

        Some(PreparedCapture {
            session,
            buffer,
            width,
            height,
            fd,
        })
    }

    /// Finish a capture previously started with
    /// [`Self::prepare_source_shm_blocking`]: grab the actual frame and
    /// block until the compositor signals Ready / Failed.
    ///
    /// This should be fast (roughly one compositor frame) since all the
    /// slow negotiation already happened in the `prepare` step.
    pub fn finish_capture_shm_blocking(&self, prepared: PreparedCapture) -> Option<ShmImage> {
        let PreparedCapture {
            session,
            buffer,
            width,
            height,
            fd,
        } = prepared;

        // Full damage rect (copied from portal — empty damage is incorrect).
        let damage = &[Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        }];

        // Capture and wait for Ready / Failed.
        let res = session.capture_wl_buffer_blocking(&buffer, damage, &self.inner.qh);
        buffer.destroy();

        match res {
            Ok(frame) => {
                let transform = match frame.transform {
                    WEnum::Value(t) => t,
                    WEnum::Unknown(v) => {
                        eprintln!(
                            "[capture] unknown transform code {}, assuming Normal",
                            v
                        );
                        wl_output::Transform::Normal
                    }
                };
                Some(ShmImage {
                    fd,
                    width,
                    height,
                    transform,
                })
            }
            Err(reason) => {
                eprintln!(
                    "[capture] capture_wl_buffer_blocking failed: {:?}",
                    reason
                );
                None
            }
        }
    }

    /// Capture a source to SHM, blocking until the frame is ready.
    /// Equivalent to calling [`Self::prepare_source_shm_blocking`] followed
    /// immediately by [`Self::finish_capture_shm_blocking`].  Kept for
    /// call sites that don't need to overlap negotiation with other work.
    pub fn capture_source_shm_blocking(&self, source: CaptureSource) -> Option<ShmImage> {
        let prepared = self.prepare_source_shm_blocking(source)?;
        self.finish_capture_shm_blocking(prepared)
    }

    /// Create a `wl_buffer` from a memfd (copied from portal's `create_shm_buffer`).
    ///
    /// The pool is destroyed immediately after creating the buffer, matching
    /// the portal's approach.
    pub fn create_shm_buffer(
        &self,
        fd: &OwnedFd,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
    ) -> wl_buffer::WlBuffer {
        let pool = self.inner.wl_shm.create_pool(
            fd.as_fd(),
            (stride * height) as i32,
            &self.inner.qh,
            (),
        );
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            &self.inner.qh,
            (),
        );
        // Portal destroys the pool immediately — the buffer keeps a reference.
        pool.destroy();
        buffer
    }
}

// ---------------------------------------------------------------------------
// PreparedCapture — a negotiated session + allocated buffer, ready for the
// actual (fast) frame-grab step.
// ---------------------------------------------------------------------------

/// The result of [`CaptureHelper::prepare_source_shm_blocking`]: a capture
/// session with formats negotiated and a buffer already allocated.
///
/// Only the (fast) [`CaptureHelper::finish_capture_shm_blocking`] step
/// remains.  Splitting the capture this way lets callers overlap the slow
/// negotiation with other async work (e.g. closing a popup) so that only
/// the minimal, fast "grab the frame" step happens once it's actually safe
/// to capture pixels — minimising any visible gap before the frozen overlay
/// appears.
pub struct PreparedCapture {
    session: CaptureSession,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    fd: OwnedFd,
}

// ---------------------------------------------------------------------------
// CaptureSession — per-capture session with blocking wait (copied from portal)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SessionState {
    formats: Option<Formats>,
    stopped: bool,
    wakers: Vec<std::task::Waker>,
}

struct CaptureSessionInner {
    conn: Connection,
    capture_session: CtkSession,
    state: Mutex<SessionState>,
    condvar: Condvar,
}

/// A single capture session that can block until the compositor responds.
pub struct CaptureSession(Arc<CaptureSessionInner>);

impl CaptureSession {
    fn update<F: FnOnce(&mut SessionState)>(&self, f: F) {
        let mut state = self.0.state.lock().unwrap();
        f(&mut state);
        for waker in std::mem::take(&mut state.wakers) {
            waker.wake();
        }
        self.0.condvar.notify_all();
    }

    fn for_session(session: &CtkSession) -> Option<Self> {
        session.data::<SessionData>()?.session.upgrade().map(Self)
    }

    /// Block until the compositor sends formats (BufferSize + ShmFormat + Done)
    /// for this session.
    fn wait_for_formats_blocking(&self) -> Option<Formats> {
        let mut state = self.0.state.lock().unwrap();
        loop {
            if state.stopped {
                return None;
            }
            if let Some(formats) = &state.formats {
                return Some(formats.clone());
            }
            state = self.0.condvar.wait(state).unwrap();
        }
    }

    /// Capture to a `wl_buffer`, blocking until Ready or Failed.
    fn capture_wl_buffer_blocking(
        &self,
        buffer: &wl_buffer::WlBuffer,
        buffer_damage: &[Rect],
        qh: &QueueHandle<AppData>,
    ) -> Result<Frame, WEnum<FailureReason>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.0.capture_session.capture(
            buffer,
            buffer_damage,
            qh,
            FrameData {
                frame_data: ScreencopyFrameData::default(),
                sender: Mutex::new(Some(sender)),
            },
        );
        self.0.conn.flush().ok();
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(WEnum::Value(FailureReason::Stopped))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(WEnum::Value(FailureReason::Stopped))
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_stopped(&self) -> bool {
        self.0.state.lock().unwrap().stopped
    }
}

// ---------------------------------------------------------------------------
// ShmImage — captured frame data (copied from portal)
// ---------------------------------------------------------------------------

/// A single captured frame, stored as a memfd that can be mmap'd on demand.
/// This exactly matches the portal's `ShmImage` struct.
pub struct ShmImage {
    pub fd: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub transform: wl_output::Transform,
}

impl ShmImage {
    /// Read pixel data from the memfd via mmap and decode as an RGBA image.
    pub fn image(&self) -> anyhow::Result<RgbaImage> {
        let mmap = unsafe { memmap2::Mmap::map(&self.fd.as_fd())? };
        RgbaImage::from_raw(self.width, self.height, mmap.to_vec())
            .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))
    }

    /// Like `image()` but applies the output transform (rotation / flip) so
    /// the returned image always has `Normal` orientation.
    pub fn image_transformed(&self) -> anyhow::Result<RgbaImage> {
        let mut dynamic = image::DynamicImage::from(self.image()?);
        dynamic.apply_orientation(match self.transform {
            wl_output::Transform::Normal => image::metadata::Orientation::NoTransforms,
            wl_output::Transform::_90 => image::metadata::Orientation::Rotate90,
            wl_output::Transform::_180 => image::metadata::Orientation::Rotate180,
            wl_output::Transform::_270 => image::metadata::Orientation::Rotate270,
            wl_output::Transform::Flipped => image::metadata::Orientation::FlipHorizontal,
            wl_output::Transform::Flipped90 => image::metadata::Orientation::Rotate90FlipH,
            wl_output::Transform::Flipped180 => image::metadata::Orientation::FlipVertical,
            wl_output::Transform::Flipped270 => image::metadata::Orientation::Rotate270FlipH,
            _ => unreachable!(),
        });
        match dynamic {
            image::DynamicImage::ImageRgba8(img) => Ok(img),
            _ => unreachable!(
                "image_transformed should always return Rgba8 after apply_orientation"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// User-data types for the dispatch handlers (copied from portal)
// ---------------------------------------------------------------------------

struct SessionData {
    session: Weak<CaptureSessionInner>,
    session_data: ScreencopySessionData,
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

struct FrameData {
    frame_data: ScreencopyFrameData,
    sender: Mutex<
        Option<std::sync::mpsc::Sender<Result<Frame, WEnum<FailureReason>>>>,
    >,
}

impl ScreencopyFrameDataExt for FrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.frame_data
    }
}

// ---------------------------------------------------------------------------
// AppData — implements all dispatch traits (copied from portal)
// ---------------------------------------------------------------------------

struct AppData {
    registry_state: RegistryState,
    screencopy_state: ScreencopyState,
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

impl ScreencopyHandler for AppData {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy_state
    }

    fn init_done(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        session: &CtkSession,
        formats: &Formats,
    ) {
        if let Some(s) = CaptureSession::for_session(session) {
            s.update(|data| {
                data.formats = Some(formats.clone());
            });
        }
    }

    fn stopped(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        session: &CtkSession,
    ) {
        if let Some(s) = CaptureSession::for_session(session) {
            s.update(|data| {
                data.stopped = true;
            });
        }
    }

    fn ready(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        screencopy_frame: &CaptureFrame,
        frame: Frame,
    ) {
        if let Some(sender) = screencopy_frame
            .data::<FrameData>()
            .and_then(|data| data.sender.lock().unwrap().take())
        {
            let _ = sender.send(Ok(frame));
        }
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        screencopy_frame: &CaptureFrame,
        reason: WEnum<FailureReason>,
    ) {
        if let Some(sender) = screencopy_frame
            .data::<FrameData>()
            .and_then(|data| data.sender.lock().unwrap().take())
        {
            let _ = sender.send(Err(reason));
        }
    }
}

/// Required for protocol objects not covered by the delegation macros.
impl Dispatch<wl_buffer::WlBuffer, (), AppData> for AppData {
    fn event(
        _: &mut AppData,
        _: &wl_buffer::WlBuffer,
        _: <wl_buffer::WlBuffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, (), AppData> for AppData {
    fn event(
        _: &mut AppData,
        _: &wl_shm_pool::WlShmPool,
        _: <wl_shm_pool::WlShmPool as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Delegation macros
// ---------------------------------------------------------------------------

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_shm!(AppData);
cosmic::cctk::delegate_screencopy!(AppData);
