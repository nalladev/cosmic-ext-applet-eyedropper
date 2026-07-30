// SPDX-License-Identifier: MPL-2.0

//! Persistent Wayland connection helper for output discovery.
//!
//! Owns a dedicated Wayland connection and dispatch thread that tracks
//! monitor geometry.  Used by the portal Screenshot capture path to
//! crop the full-desktop image per-output.
//!
//! The [`CaptureHelper`] singleton is initialized once and reused for
//! the applet's lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use cosmic::cctk::sctk;
use cosmic::cctk::sctk::output::{OutputHandler, OutputInfo, OutputState};
use cosmic::cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic::cctk::sctk::shm::{Shm, ShmHandler};
use cosmic::cctk::wayland_client::{
    Connection, QueueHandle, globals::registry_queue_init, protocol::wl_output,
};

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
// Portal output metadata
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
// Wayland output tracking — used by CaptureHelper
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
