// SPDX-License-Identifier: MPL-2.0

//! Entry point for screen capture.
//!
//! This module provides the async `capture_all_outputs()` function that the
//! event loop calls.  It uses the persistent [`CaptureHelper`] to avoid the
//! overhead of creating a fresh Wayland connection per capture (as we did
//! originally).  The helper is created once and reused.
//!
//! The capture flow follows `xdg-desktop-portal-cosmic` exactly:
//!
//! 1. Create a capture session on the persistent connection.
//! 2. Wait for the compositor to send formats (block on condvar).
//! 3. Create a memfd + SHM buffer (Abgr8888).
//! 4. Call `session.capture()` with a full damage rect.
//! 5. Wait for Ready (block on condvar).
//! 6. Read pixels from the memfd via mmap → `RgbaImage`.
//! 7. Build `Handle::from_rgba()` on the capture thread.
//! 8. Return [`CapturedOutput`] with both the handle and the image data.

use std::sync::OnceLock;

use image::RgbaImage;
use tokio::sync::oneshot;

use crate::picker::capture::{CaptureHelper, CaptureSource, PreparedCapture};
use crate::picker::CapturedOutput;

/// Extract a human-readable message from a `std::thread` panic payload.
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Singleton helper — initialised once and reused for the applet's lifetime.
// ---------------------------------------------------------------------------

fn helper() -> &'static CaptureHelper {
    static HELPER: OnceLock<CaptureHelper> = OnceLock::new();
    HELPER.get_or_init(|| {
        eprintln!("[capture] Initialising persistent CaptureHelper singleton...");
        CaptureHelper::new()
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Capture all connected outputs.
///
/// This is called from the iced event loop via `Task::perform`.  It spawns a
/// dedicated OS thread that uses the persistent [`CaptureHelper`] — no fresh
/// Wayland connection is created per capture.
///
/// Returns the captured outputs with pre-built GPU handles.
pub async fn capture_all_outputs() -> Result<Vec<CapturedOutput>, anyhow::Error> {
    let h = helper();
    let t_start = std::time::Instant::now();
    eprintln!("[capture] === STARTING Wayland screen capture (persistent connection) ===");

    // Read outputs from the helper's state (discovered at init time).
    let wl_outputs = h.outputs();
    let n = wl_outputs.len();
    eprintln!("[capture] {} output(s) from CaptureHelper state", n);

    if n == 0 {
        return Err(anyhow::anyhow!("No Wayland outputs found"));
    }

    // Collect output infos before spawning the thread.
    let output_infos: Vec<_> = wl_outputs
        .iter()
        .map(|o| {
            let info = h.output_info(o);
            (o.clone(), info)
        })
        .collect();

    // Spawn a thread for blocking capture work.
    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let captured_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut results: Vec<CapturedOutput> = Vec::with_capacity(n);
            // Capture each output sequentially (same as portal).
            for (output, info) in &output_infos {
                let Some(info) = info else {
                    eprintln!("[capture]   SKIP: no OutputInfo for an output");
                    continue;
                };
                let name = info.name.clone().unwrap_or_default();
                let (ox, oy) = info.location;
                let logical_size = info.logical_size.unwrap_or((0, 0));

                eprintln!("[capture]   Capturing output '{}' ...", name);

                // --- portal's capture_source_shm flow (blocking) ---
                let shm_img = match h.capture_source_shm_blocking(CaptureSource::Output(output.clone())) {
                    Some(img) => img,
                    None => {
                        eprintln!("[capture]   FAILED: capture_source_shm_blocking returned None for '{}'", name);
                        continue;
                    }
                };

                // --- Read pixels via mmap + transform (portal's ShmImage::image_transformed) ---
                let t_read = std::time::Instant::now();
                let rgba: RgbaImage = match shm_img.image_transformed() {
                    Ok(img) => img,
                    Err(e) => {
                        eprintln!("[capture]   FAILED: image_transformed for '{}': {}", name, e);
                        continue;
                    }
                };
                eprintln!(
                    "[capture]   image_transformed for '{}' took {:?} ({}x{})",
                    name,
                    t_read.elapsed(),
                    rgba.width(),
                    rgba.height(),
                );

                // --- Build GPU handle (portal's ScreenshotImage::new) ---
                let t_handle = std::time::Instant::now();
                let handle = cosmic::widget::image::Handle::from_rgba(
                    rgba.width(),
                    rgba.height(),
                    rgba.clone().into_vec(),
                );
                eprintln!(
                    "[capture]   Handle::from_rgba for '{}' took {:?}",
                    name,
                    t_handle.elapsed(),
                );

                results.push(CapturedOutput {
                    name,
                    rgba,
                    image_handle: handle,
                    width: shm_img.width,
                    height: shm_img.height,
                    logical_width: logical_size.0.max(0) as u32,
                    logical_height: logical_size.1.max(0) as u32,
                    pos_x: ox,
                    pos_y: oy,
                });
            }

            Ok(results) as Result<Vec<CapturedOutput>, anyhow::Error>
        }));

        let result = match captured_result {
            Ok(Ok(outputs)) => Ok(outputs),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                eprintln!("[capture] THREAD PANICKED: {msg}");
                Err(anyhow::anyhow!("Capture thread panicked: {msg}"))
            }
        };
        let _ = tx.send(result);
    });

    let result = rx.await.map_err(|_| anyhow::anyhow!("Capture thread was cancelled"))?;
    let captured = result?;

    eprintln!(
        "[capture] === Capture finished: {} output(s) in {:?} ===",
        captured.len(),
        t_start.elapsed(),
    );

    Ok(captured)
}

// ---------------------------------------------------------------------------
// Two-phase capture — negotiate ahead of time, grab the frame at the last
// possible moment.
// ---------------------------------------------------------------------------
//
// `capture_all_outputs()` above does session negotiation *and* the actual
// frame grab in one shot.  When entering picker mode we first have to close
// our own popup (so it isn't included in the capture) and wait for the
// compositor to confirm it's gone before it's safe to grab pixels.  If we
// only started capturing *after* that confirmation, the (fairly slow)
// session/format negotiation would run entirely inside the user-visible gap
// between "popup gone" and "frozen overlay shown", which is perceived as a
// flicker of the live desktop.
//
// Splitting the pipeline lets the caller run `prepare_all_outputs()`
// concurrently with closing the popup, and call `finish_all_outputs()` —
// just the fast, already-negotiated frame grab — the instant the popup is
// confirmed closed.

/// Metadata plus a negotiated-but-not-yet-captured session for one output.
/// Produced by [`prepare_all_outputs`], consumed by [`finish_all_outputs`].
pub struct PreparedOutputCapture {
    name: String,
    pos_x: i32,
    pos_y: i32,
    logical_width: u32,
    logical_height: u32,
    prepared: PreparedCapture,
}

/// Negotiate capture sessions (session creation, format negotiation, and
/// buffer allocation) for all outputs, without grabbing any frames yet.
///
/// This is the slow, round-trip-heavy part of a capture.  It does not
/// depend on what is currently visible on screen, so it is safe to run
/// concurrently with other UI transitions (e.g. closing a popup).  Call
/// [`finish_all_outputs`] once it is actually safe to capture pixels.
pub async fn prepare_all_outputs() -> Result<Vec<PreparedOutputCapture>, anyhow::Error> {
    let h = helper();
    let t_start = std::time::Instant::now();
    eprintln!("[capture] === Preparing capture sessions (persistent connection) ===");

    let wl_outputs = h.outputs();
    let n = wl_outputs.len();
    eprintln!("[capture] {} output(s) from CaptureHelper state", n);

    if n == 0 {
        return Err(anyhow::anyhow!("No Wayland outputs found"));
    }

    let output_infos: Vec<_> = wl_outputs
        .iter()
        .map(|o| {
            let info = h.output_info(o);
            (o.clone(), info)
        })
        .collect();

    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let prepared_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut results: Vec<PreparedOutputCapture> = Vec::with_capacity(n);
            for (output, info) in &output_infos {
                let Some(info) = info else {
                    eprintln!("[capture]   SKIP (prepare): no OutputInfo for an output");
                    continue;
                };
                let name = info.name.clone().unwrap_or_default();
                let (ox, oy) = info.location;
                let logical_size = info.logical_size.unwrap_or((0, 0));

                eprintln!("[capture]   Preparing output '{}' ...", name);

                let Some(prepared) =
                    h.prepare_source_shm_blocking(CaptureSource::Output(output.clone()))
                else {
                    eprintln!("[capture]   FAILED: prepare_source_shm_blocking for '{}'", name);
                    continue;
                };

                results.push(PreparedOutputCapture {
                    name,
                    pos_x: ox,
                    pos_y: oy,
                    logical_width: logical_size.0.max(0) as u32,
                    logical_height: logical_size.1.max(0) as u32,
                    prepared,
                });
            }

            Ok(results) as Result<Vec<PreparedOutputCapture>, anyhow::Error>
        }));

        let result = match prepared_result {
            Ok(Ok(outputs)) => Ok(outputs),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = panic_message(panic);
                eprintln!("[capture] PREPARE THREAD PANICKED: {msg}");
                Err(anyhow::anyhow!("Prepare thread panicked: {msg}"))
            }
        };
        let _ = tx.send(result);
    });

    let result = rx.await.map_err(|_| anyhow::anyhow!("Prepare thread was cancelled"))?;
    let prepared = result?;

    eprintln!(
        "[capture] === Prepare finished: {} output(s) in {:?} ===",
        prepared.len(),
        t_start.elapsed(),
    );

    Ok(prepared)
}

/// Finish captures previously started with [`prepare_all_outputs`]: grab the
/// actual frame for each output and build the RGBA image + GPU handle.
///
/// This should be fast (roughly one compositor frame per output) since all
/// session negotiation already happened in [`prepare_all_outputs`].
pub async fn finish_all_outputs(
    prepared: Vec<PreparedOutputCapture>,
) -> Result<Vec<CapturedOutput>, anyhow::Error> {
    let h = helper();
    let t_start = std::time::Instant::now();
    let n = prepared.len();
    eprintln!("[capture] === Finishing capture for {} prepared output(s) ===", n);

    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let captured_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut results: Vec<CapturedOutput> = Vec::with_capacity(n);
            for p in prepared {
                let PreparedOutputCapture {
                    name,
                    pos_x,
                    pos_y,
                    logical_width,
                    logical_height,
                    prepared,
                } = p;

                let shm_img = match h.finish_capture_shm_blocking(prepared) {
                    Some(img) => img,
                    None => {
                        eprintln!("[capture]   FAILED: finish_capture_shm_blocking for '{}'", name);
                        continue;
                    }
                };

                let rgba: RgbaImage = match shm_img.image_transformed() {
                    Ok(img) => img,
                    Err(e) => {
                        eprintln!("[capture]   FAILED: image_transformed for '{}': {}", name, e);
                        continue;
                    }
                };

                let handle = cosmic::widget::image::Handle::from_rgba(
                    rgba.width(),
                    rgba.height(),
                    rgba.clone().into_vec(),
                );

                results.push(CapturedOutput {
                    name,
                    rgba,
                    image_handle: handle,
                    width: shm_img.width,
                    height: shm_img.height,
                    logical_width,
                    logical_height,
                    pos_x,
                    pos_y,
                });
            }

            Ok(results) as Result<Vec<CapturedOutput>, anyhow::Error>
        }));

        let result = match captured_result {
            Ok(Ok(outputs)) => Ok(outputs),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = panic_message(panic);
                eprintln!("[capture] FINISH THREAD PANICKED: {msg}");
                Err(anyhow::anyhow!("Finish thread panicked: {msg}"))
            }
        };
        let _ = tx.send(result);
    });

    let result = rx.await.map_err(|_| anyhow::anyhow!("Finish thread was cancelled"))?;
    let captured = result?;

    eprintln!(
        "[capture] === Finish complete: {} output(s) in {:?} ===",
        captured.len(),
        t_start.elapsed(),
    );

    Ok(captured)
}
