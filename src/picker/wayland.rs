// SPDX-License-Identifier: MPL-2.0

//! Wayland screen-capture for the desktop colour picker.
//!
//! This module runs a synchronous capture on a dedicated OS thread with its
//! own Wayland connection.  It captures the visible contents of all outputs
//! via `ext-image-copy-capture-v1` and returns the raw BGRA pixel data.
//!
//! ## Capture flow
//!
//! 1. Connect, bind globals, discover outputs
//! 2. For each output, call `capturer.create_session()` → creates a session
//! 3. Session emits `BufferSize`, `ShmFormat`, `Done` → `init_done()` called
//! 4. In `init_done()`: create SHM pool + `wl_buffer`, call `session.capture()`
//! 5. Frame emits `Transform`, `Damage`, `PresentationTime`, `Ready` → `ready()` called
//! 6. In `ready()`: read pixel data from SHM mmap, store in `CaptureResult`
//! 7. Return captured outputs to caller

use std::sync::{Arc, Mutex};

use cosmic::cctk::screencopy::{
    CaptureFrame, CaptureOptions, CaptureSession, CaptureSource,
    FailureReason, Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt,
    ScreencopyHandler, ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
};
use cosmic::cctk::sctk::output::{OutputHandler, OutputInfo, OutputState};
use cosmic::cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic::cctk::sctk::shm::raw::RawPool;
use cosmic::cctk::sctk::shm::{Shm, ShmHandler};
use cosmic::cctk::sctk::{self};
use cosmic::cctk::wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm},
};

use super::CapturedOutput;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const BPP: usize = 4;

// ---------------------------------------------------------------------------
// SHM format negotiation
// ---------------------------------------------------------------------------

/// Pick a shared-memory pixel format supported by both the compositor and our
/// pixel-reading code.
///
/// The compositor advertises the formats it is willing to write into our
/// `wl_buffer` via the `shm_format` events collected in
/// `Formats::shm_formats`.  We must pick* one of those — picking an
/// unadvertised format (e.g. the hardcoded `Xrgb8888` we used before) causes
/// `FailureReason::BufferConstraints`.
///
/// ## Byte order of the chosen format
///
/// Both `Xbgr8888` and `Abgr8888` store pixels in the same byte order in
/// memory (little-endian):
///
/// | byte 0 | byte 1 | byte 2 | byte 3 |
/// |--------|--------|--------|--------|
/// | R      | G      | B      | X / A  |
///
/// This is an intentional choice — it matches the `RGB` channel layout our
/// [`CapturedOutput::pixel_at`](super::CapturedOutput::pixel_at) expects.
///
/// \* "must" in the protocol sense — the compositor will reject a buffer
/// whose format does not appear in its advertised list.
fn pick_shm_format(formats: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    // 1st choice: 8‑bit, no alpha channel.
    for &fmt in formats {
        if fmt == wl_shm::Format::Xbgr8888 {
            return Some(fmt);
        }
    }
    // 2nd choice: 8‑bit with alpha (same RGB byte layout).
    for &fmt in formats {
        if fmt == wl_shm::Format::Abgr8888 {
            return Some(fmt);
        }
    }
    // Last resort: whatever the compositor listed first.
    formats.first().copied()
}

// ---------------------------------------------------------------------------
// User-data wrappers for session and frame dispatch
// ---------------------------------------------------------------------------

/// Per-output session data that lets us identify which output a
/// `CaptureSession` belongs to when its events fire.
struct SessionUserData {
    inner: ScreencopySessionData,
    output_index: usize,
}

impl ScreencopySessionDataExt for SessionUserData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.inner
    }
}

/// Per-output frame data that lets us identify which output a
/// `CaptureFrame` captured, and remember the buffer dimensions and format.
struct FrameUserData {
    inner: ScreencopyFrameData,
    output_index: usize,
    width: u32,
    height: u32,
    /// The pixel format that was negotiated for this capture.
    format: wl_shm::Format,
}

impl ScreencopyFrameDataExt for FrameUserData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.inner
    }
}

// ---------------------------------------------------------------------------
// AppData – the core struct implementing all Wayland dispatch traits
// ---------------------------------------------------------------------------

struct AppData {
    // sctk-managed state
    registry_state: RegistryState,
    output_state: OutputState,
    shm_state: Shm,
    screencopy_state: ScreencopyState,

    // Tracked outputs
    outputs: Vec<wl_output::WlOutput>,
    output_infos: Vec<OutputInfo>,
    output_names: Vec<String>,

    // Per-output capture state
    sessions: Vec<Option<CaptureSession>>,
    pools: Vec<Option<RawPool>>,
    buffers: Vec<Option<wl_buffer::WlBuffer>>,

    // Synchronisation
    pending_captures: usize,

    // Shared capture result (read by the main thread loop after dispatch)
    result: Arc<Mutex<Vec<CapturedOutput>>>,
}

impl AppData {
    fn output_idx(&self, output: &wl_output::WlOutput) -> Option<usize> {
        self.outputs.iter().position(|o| o == output)
    }
}

// ---------------------------------------------------------------------------
// Synchronous capture entry-point – called from a spawned OS thread
// ---------------------------------------------------------------------------

/// Capture all outputs synchronously.
///
/// This function blocks until all output captures complete or fail.  It must
/// be called from a dedicated OS thread (not from the iced event loop).
pub(super) fn capture_all_outputs_sync() -> Result<Vec<CapturedOutput>, anyhow::Error> {
    eprintln!("[capture] === STARTING Wayland screen capture ===");

    // ── 1. Connect to Wayland compositor ────────────────────────────────
    eprintln!("[capture] Connecting to Wayland compositor...");
    let conn = Connection::connect_to_env().map_err(|e| {
        eprintln!("[capture] FAILED: Wayland connection: {e:?}");
        anyhow::anyhow!("Wayland connection: {e:?}")
    })?;
    eprintln!("[capture] Wayland connected.");

    let (globals, mut event_queue) = registry_queue_init::<AppData>(&conn).map_err(|e| {
        eprintln!("[capture] FAILED: registry_queue_init: {e:?}");
        anyhow::anyhow!("registry_queue_init: {e:?}")
    })?;
    let qh = event_queue.handle();
    eprintln!("[capture] Registry queue initialized.");

    // ── Diagnose: list all available Wayland globals ────────────────────
    eprintln!("[capture] Available Wayland globals from compositor:");
    let global_list = globals.contents();
    global_list.with_list(|globals| {
        for g in globals {
            eprintln!(
                "[capture]   name={} interface={} v{}",
                g.name, g.interface, g.version
            );
        }
    });
    eprintln!("[capture] End of global list.");

    // Check specifically for the protocols we need
    let mut has_image_copy_capture = false;
    let mut has_output_capture_source = false;
    global_list.with_list(|globals| {
        for g in globals {
            if g.interface == "ext_image_copy_capture_manager_v1" {
                has_image_copy_capture = true;
            }
            if g.interface == "ext_output_image_capture_source_manager_v1" {
                has_output_capture_source = true;
            }
        }
    });
    eprintln!(
        "[capture] Required protocols: ext_image_copy_capture_manager_v1={}, ext_output_image_capture_source_manager_v1={}",
        has_image_copy_capture,
        has_output_capture_source,
    );
    if !has_image_copy_capture || !has_output_capture_source {
        eprintln!("[capture] WARNING: Compositor is missing required capture protocols!");
    }

    let result: Arc<Mutex<Vec<CapturedOutput>>> = Arc::new(Mutex::new(Vec::new()));

    let mut data = AppData {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm_state: match Shm::bind(&globals, &qh) {
            Ok(s) => {
                eprintln!("[capture] SHM bound.");
                s
            }
            Err(e) => {
                eprintln!("[capture] FAILED: Shm::bind: {e:?}");
                return Err(anyhow::anyhow!("Shm::bind: {e:?}"));
            }
        },
        screencopy_state: {
            let state = ScreencopyState::new(&globals, &qh);
            // We cannot directly inspect CapturerInner's private fields, but
            // the global listing above tells us whether the protocol was advertised.
            eprintln!(
                "[capture] ScreencopyState created (image_copy_capture={}, output_source={}).",
                has_image_copy_capture,
                has_output_capture_source,
            );
            state
        },
        outputs: Vec::new(),
        output_infos: Vec::new(),
        output_names: Vec::new(),
        sessions: Vec::new(),
        pools: Vec::new(),
        buffers: Vec::new(),
        pending_captures: 0,
        result: result.clone(),
    };

    // ── 2. Initial roundtrip to discover outputs ────────────────────────
    eprintln!("[capture] Roundtrip to discover outputs...");
    let roundtrip_result = event_queue.roundtrip(&mut data);
    eprintln!("[capture] Roundtrip returned.");
    roundtrip_result.map_err(|e| {
        eprintln!("[capture] FAILED: roundtrip: {e:?}");
        anyhow::anyhow!("roundtrip: {e:?}")
    })?;

    eprintln!("[capture] Outputs in AppData: {}", data.outputs.len());
    eprintln!("[capture] Outputs in OutputState: {}", data.output_state.outputs().count());
    for wl_output in data.output_state.outputs() {
        let info = data.output_state.info(&wl_output);
        eprintln!(
            "[capture]   OutputState output: info={:?}",
            info.as_ref().map(|i| (
                i.name.clone(),
                i.location,
                i.physical_size,
                i.logical_size,
            ))
        );
    }
    for (i, name) in data.output_names.iter().enumerate() {
        let info = &data.output_infos[i];
        eprintln!(
            "[capture]   AppData output[{}]: {} at ({},{}), logical {:?}, physical {}x{}",
            i, name,
            info.location.0, info.location.1,
            info.logical_size,
            info.physical_size.0, info.physical_size.1,
        );
    }

    if data.outputs.is_empty() {
        eprintln!("[capture] FAILED: No outputs found after roundtrip.");
        eprintln!("[capture] FAILED: This likely means wl_output events were not dispatched.");
        eprintln!("[capture] FAILED: The sctk delegate_output! macro may be missing.");
        return Err(anyhow::anyhow!("No outputs found"));
    }

    let n = data.outputs.len();
    data.sessions = (0..n).map(|_| None).collect();
    data.pools = (0..n).map(|_| None).collect();
    data.buffers = (0..n).map(|_| None).collect();
    data.pending_captures = n;

    eprintln!("[capture] Creating capture sessions for {n} outputs...");

    // ── 3. Create capture sessions for each output ──────────────────────
    for idx in 0..n {
        let name = &data.output_names[idx];
        eprintln!("[capture]   session for output[{idx}] ({name})...");
        let source = CaptureSource::Output(data.outputs[idx].clone());
        let session_data = SessionUserData {
            inner: ScreencopySessionData::default(),
            output_index: idx,
        };

        match data
            .screencopy_state
            .capturer()
            .create_session(&source, CaptureOptions::empty(), &qh, session_data)
        {
            Ok(session) => {
                eprintln!("[capture]   session[{idx}] created successfully.");
                data.sessions[idx] = Some(session);
            }
            Err(e) => {
                eprintln!(
                    "[capture]   FAILED: session[{idx}] ({name}) create_session: {e}"
                );
                eprintln!(
                    "[capture]   FAILED: This means the compositor does not support \"ext-image-copy-capture-v1\" for Output sources."
                );
                data.pending_captures = data.pending_captures.saturating_sub(1);
            }
        }
    }

    eprintln!(
        "[capture] Pending captures after session creation: {}",
        data.pending_captures
    );

    // ── 4. Dispatch loop – wait for all captures to complete ────────────
    //
    // Sessions emit BufferSize ↔ ShmFormat ↔ Done events which trigger
    // `init_done()` → we create a SHM pool + buffer and call
    // `session.capture()`.  Frames then emit Transform ↔ Damage ↔ Ready
    // events which trigger `ready()` → we read pixel data.
    eprintln!("[capture] Entering dispatch loop (pending={})...", data.pending_captures);
    while data.pending_captures > 0 {
        if event_queue.blocking_dispatch(&mut data).is_err() {
            eprintln!("[capture] FAILED: Wayland connection lost during dispatch");
            return Err(anyhow::anyhow!("Wayland connection lost"));
        }
    }
    eprintln!("[capture] Dispatch loop finished.");

    // ── 5. Return captured data ─────────────────────────────────────────
    let captured = result.lock().unwrap().clone();
    eprintln!(
        "[capture] Captured outputs: {} (returning {} to caller)",
        captured.len(),
        if captured.is_empty() { "ERROR" } else { "OK" }
    );
    if captured.is_empty() {
        eprintln!("[capture] FAILED: All captures failed — no outputs captured.");
        return Err(anyhow::anyhow!("All captures failed"));
    }

    Ok(captured)
}

// ---------------------------------------------------------------------------
// sctk trait implementations
// ---------------------------------------------------------------------------

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
        if let Some(info) = self.output_state.info(&output) {
            let name = info.name.clone().unwrap_or_default();
            eprintln!(
                "[capture] new_output: {} at ({},{}), logical {:?}, physical {}x{}",
                name,
                info.location.0, info.location.1,
                info.logical_size,
                info.physical_size.0, info.physical_size.1,
            );
            self.outputs.push(output);
            self.output_infos.push(info);
            self.output_names.push(name);
        } else {
            eprintln!("[capture] new_output: no OutputInfo available (output_state.info returned None)");
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.output_state.info(&output) {
            if let Some(idx) = self.output_idx(&output) {
                self.output_infos[idx] = info;
            }
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        output: wl_output::WlOutput,
    ) {
        if let Some(idx) = self.output_idx(&output) {
            self.outputs.remove(idx);
            self.output_infos.remove(idx);
            self.output_names.remove(idx);
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
        qh: &QueueHandle<AppData>,
        session: &CaptureSession,
        formats: &Formats,
    ) {
        // Determine which output this session belongs to.
        let session_data: &SessionUserData = match session.data() {
            Some(d) => d,
            None => {
                eprintln!("[capture] init_done: FAILED — session has no user-data; cannot identify output");
                self.pending_captures = self.pending_captures.saturating_sub(1);
                return;
            }
        };

        let idx = session_data.output_index;
        let name = self.output_names.get(idx).cloned().unwrap_or_default();
        let (width, height) = formats.buffer_size;

        eprintln!(
            "[capture] init_done for output[{idx}] ({name}): buffer {}x{}, shm_formats={:?}, dmabuf={:?}",
            width, height,
            formats.shm_formats,
            formats.dmabuf_device,
        );

        if width == 0 || height == 0 {
            eprintln!("[capture] init_done: FAILED — compositor gave zero-sized buffer for output {idx} ({name})");
            self.pending_captures = self.pending_captures.saturating_sub(1);
            return;
        }

        let stride = width as i32 * BPP as i32;
        let pool_size = (stride * height as i32) as usize;
        eprintln!("[capture] init_done: stride={stride}, pool_size={pool_size}");

        // Create a shared-memory pool that the compositor will write pixels into.
        let mut pool = match RawPool::new(pool_size, &self.shm_state) {
            Ok(p) => {
                eprintln!("[capture] init_done: SHM pool created ({} bytes).", pool_size);
                p
            }
            Err(e) => {
                eprintln!("[capture] init_done: FAILED — SHM pool of {pool_size} bytes: {e}");
                self.pending_captures = self.pending_captures.saturating_sub(1);
                return;
            }
        };

        // Pick a pixel format advertised by the compositor.
        let format = pick_shm_format(&formats.shm_formats).unwrap_or(wl_shm::Format::Abgr8888);
        eprintln!("[capture] init_done: creating wl_buffer (width={width}, height={height}, stride={stride}, format={format:?})...");
        let buffer = pool.create_buffer(
            0,             // offset (bytes from pool start)
            width as i32,  // buffer width  (pixels)
            height as i32, // buffer height (pixels)
            stride,        // bytes per row
            format,
            (), // buffer user-data (unit type – we don't need it)
            qh,
        );
        eprintln!("[capture] init_done: wl_buffer created (format={format:?}).");

        // User-data for the upcoming frame – remembers the output index
        // and format so `ready()` can read the pixels correctly.
        let frame_data = FrameUserData {
            inner: ScreencopyFrameData::default(),
            output_index: idx,
            width,
            height,
            format,
        };

        // Start the capture! The compositor will write pixel data into the
        // SHM buffer and fire `ready()` once done.
        eprintln!("[capture] init_done: calling session.capture() for output[{idx}] ({name})...");
        session.capture(&buffer, &[], qh, frame_data);
        eprintln!("[capture] init_done: session.capture() returned for output[{idx}].");

        self.pools[idx] = Some(pool);
        self.buffers[idx] = Some(buffer);
    }

    fn stopped(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        _session: &CaptureSession,
    ) {
        eprintln!("[capture] stopped: session stopped (decrementing pending).");
        // Capture session stopped (normal completion)
        self.pending_captures = self.pending_captures.saturating_sub(1);
    }

    fn ready(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        screencopy_frame: &CaptureFrame,
        frame: Frame,
    ) {
        // Find the output index from the frame user-data.
        let frame_data: &FrameUserData = match screencopy_frame.data() {
            Some(d) => d,
            None => {
                eprintln!("[capture] ready: FAILED — frame has no user-data");
                self.pending_captures = self.pending_captures.saturating_sub(1);
                return;
            }
        };
        let idx = frame_data.output_index;
        let name = self.output_names.get(idx).cloned().unwrap_or_default();

        eprintln!(
            "[capture] ready for output[{idx}] ({name}): {}x{} format={:?} transform={:?} damage={} rects",
            frame_data.width, frame_data.height,
            frame_data.format,
            frame.transform,
            frame.damage.len(),
        );

        // Read pixel data from the SHM mmap held by the pool.
        let pixel_data = match self.pools.get_mut(idx) {
            Some(Some(pool)) => {
                let mmap = pool.mmap();
                let data_len = frame_data.width as usize * frame_data.height as usize * BPP;
                let available = mmap.len();
                if data_len > available {
                    eprintln!(
                        "[capture] ready: WARNING — mmap too small: need {data_len}, have {available}"
                    );
                }
                mmap[..data_len.min(available)].to_vec()
            }
            _ => {
                eprintln!("[capture] ready: FAILED — no SHM pool for output {idx}; cannot read pixels");
                self.pending_captures = self.pending_captures.saturating_sub(1);
                return;
            }
        };
        eprintln!("[capture] ready: read {} bytes of pixel data.", pixel_data.len());

        let loc = self
            .output_infos
            .get(idx)
            .map(|info| info.location)
            .unwrap_or((0, 0));

        let logical_size = self
            .output_infos
            .get(idx)
            .and_then(|info| info.logical_size)
            .unwrap_or((frame_data.width as i32, frame_data.height as i32));

        let mut result = self.result.lock().unwrap();
        result.push(CapturedOutput {
            name: self.output_names.get(idx).cloned().unwrap_or_default(),
            data: pixel_data,
            width: frame_data.width,
            height: frame_data.height,
            logical_width: logical_size.0.max(0) as u32,
            logical_height: logical_size.1.max(0) as u32,
            pos_x: loc.0,
            pos_y: loc.1,
        });
        eprintln!("[capture] ready: output[{idx}] ({name}) captured successfully.");

        self.pending_captures = self.pending_captures.saturating_sub(1);
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<AppData>,
        screencopy_frame: &CaptureFrame,
        reason: WEnum<FailureReason>,
    ) {
        let reason_str = match &reason {
            WEnum::Value(v) => format!("{v:?}"),
            WEnum::Unknown(u) => format!("unknown code {u}"),
        };
        let frame_data: Option<&FrameUserData> = screencopy_frame.data();
        if let Some(fd) = frame_data {
            let name = self.output_names.get(fd.output_index).cloned().unwrap_or_default();
            eprintln!(
                "[capture] FAILED: output[{}] ({}) capture failed: reason={}",
                fd.output_index, name, reason_str
            );
        } else {
            eprintln!("[capture] FAILED: capture failed (unknown output): reason={}", reason_str);
        }
        self.pending_captures = self.pending_captures.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// Dispatch implementations – required for protocol objects not covered by
// the delegation macros.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Delegation macros
// ---------------------------------------------------------------------------

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_shm!(AppData);
cosmic::cctk::delegate_screencopy!(AppData);
