// SPDX-License-Identifier: MPL-2.0

//! Entry point for screen capture.
//!
//! This module provides the async capture functions that the event loop calls.
//! It uses the XDG Desktop Portal `ScreenCast` API + `PipeWire` for capture,
//! which works in both native and Flatpak builds.
//!
//! The capture flow:
//!
//! 1. Create a portal `ScreenCast` session.
//! 2. Select all monitors as sources.
//! 3. Start the session → get `PipeWire` node IDs + fd.
//! 4. Connect to `PipeWire`, create streams, wait for frames.
//! 5. Read pixels from `PipeWire` buffers → `RgbaImage`.

use std::sync::OnceLock;

use image::RgbaImage;
use tokio::sync::oneshot;

use crate::picker::capture::{
    self, CaptureHelper, PortalOutputInfo, PreparedCapture,
};
use crate::picker::CapturedOutput;

/// Extract a human-readable message from a `std::thread` panic payload.
#[allow(clippy::needless_pass_by_value)]
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
// Single-shot capture (one portal session → all outputs)
// ---------------------------------------------------------------------------

/// Capture all connected outputs using a single portal session.
///
/// Creates a portal session, connects `PipeWire`, and grabs frames
/// from all monitors in one shot.
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub async fn capture_all_outputs() -> Result<(Vec<CapturedOutput>, Option<String>), anyhow::Error> {
    let h = helper();
    let t_start = std::time::Instant::now();
    eprintln!("[capture] === STARTING portal screen capture ===");

    let wl_outputs = h.outputs();
    let n = wl_outputs.len();
    eprintln!("[capture] {n} output(s) from CaptureHelper state");

    if n == 0 {
        return Err(anyhow::anyhow!("No Wayland outputs found"));
    }

    // Phase 1: portal D-Bus calls (async).
        let (prepared, new_restore_token) = capture::portal_prepare_all(h, None).await?;

        if prepared.is_empty() {
        return Err(anyhow::anyhow!("Portal returned no streams"));
    }

    // Phase 2: PipeWire frame grab (blocking, on a thread).
    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let captured_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let shm_images = capture::pipewire_finish_all(&prepared)
                .map_err(|e| anyhow::anyhow!("PipeWire capture failed: {e}"))?;

            let mut results: Vec<CapturedOutput> = Vec::with_capacity(shm_images.len());

            for (shm_img, info) in shm_images {
                            let t_read = std::time::Instant::now();
                            let rgba: RgbaImage = shm_img.image_transformed()
                    .map_err(|e| anyhow::anyhow!("image_transformed failed: {e}"))?;
                eprintln!(
                    "[capture]   image_transformed for '{}` took {:?} ({}x{})",
                    info.name,
                    t_read.elapsed(),
                    rgba.width(),
                    rgba.height(),
                );

                let t_handle = std::time::Instant::now();
                let handle = cosmic::widget::image::Handle::from_rgba(
                    rgba.width(),
                    rgba.height(),
                    rgba.clone().into_vec(),
                );
                eprintln!(
                    "[capture]   Handle::from_rgba for '{}` took {:?}",
                    info.name,
                    t_handle.elapsed(),
                );

                results.push(CapturedOutput {
                    name: info.name.clone(),
                    rgba,
                    image_handle: handle,
                    width: shm_img.width,
                    height: shm_img.height,
                    logical_width: info.logical_width,
                    logical_height: info.logical_height,
                    pos_x: info.pos_x,
                    pos_y: info.pos_y,
                });
            }

            Ok(results) as Result<Vec<CapturedOutput>, anyhow::Error>
        }));

        let result = match captured_result {
            Ok(Ok(outputs)) => Ok(outputs),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = panic_message(panic);
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

        Ok((captured, new_restore_token))
}

// ---------------------------------------------------------------------------
// Two-phase capture — negotiate portal session ahead of time, grab frames
// later when it's safe to do so.
// ---------------------------------------------------------------------------

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

/// Negotiate a portal `ScreenCast` session for all outputs, without grabbing
/// any frames yet.
///
/// This is the slow, round-trip-heavy part of a capture (D-Bus calls).  It
/// does not depend on what is currently visible on screen, so it is safe to
/// run concurrently with other UI transitions (e.g. closing a popup).  Call
/// [`finish_all_outputs`] once it is actually safe to capture pixels.
#[allow(clippy::missing_errors_doc)]
pub async fn prepare_all_outputs() -> Result<(Vec<PreparedOutputCapture>, Option<String>), anyhow::Error> {
    let h = helper();
    let t_start = std::time::Instant::now();
    eprintln!("[capture] === Preparing portal capture sessions ===");

    let wl_outputs = h.outputs();
    let n = wl_outputs.len();
    eprintln!("[capture] {n} output(s) from CaptureHelper state");

    if n == 0 {
        return Err(anyhow::anyhow!("No Wayland outputs found"));
    }

    let (prepared, new_restore_token) = capture::portal_prepare_all(h, None).await?;

        let results: Vec<PreparedOutputCapture> = prepared
        .into_iter()
        .map(|(prep, info)| {
            eprintln!("[capture]   Prepared output '{}' node={}", info.name, prep.node_id);

            PreparedOutputCapture {
                name: info.name,
                pos_x: info.pos_x,
                pos_y: info.pos_y,
                logical_width: info.logical_width,
                logical_height: info.logical_height,
                prepared: prep,
            }
        })
        .collect();

    eprintln!(
            "[capture] === Prepare finished: {} output(s) in {:?} ===",
            results.len(),
            t_start.elapsed(),
        );

        Ok((results, new_restore_token))
}

/// Finish captures previously started with [`prepare_all_outputs`]: connect
/// to `PipeWire` and grab the actual frame for each output.
///
/// This should be fast since all portal D-Bus negotiation already happened
/// in [`prepare_all_outputs`].
#[allow(clippy::missing_errors_doc)]
pub async fn finish_all_outputs(
    prepared: Vec<PreparedOutputCapture>,
) -> Result<Vec<CapturedOutput>, anyhow::Error> {
    let t_start = std::time::Instant::now();
    let n = prepared.len();
    eprintln!("[capture] === Finishing capture for {n} prepared output(s) ===");

    // Convert to the format pipewire_finish_all expects.
    let pw_prepared: Vec<(PreparedCapture, PortalOutputInfo)> = prepared
        .iter()
        .map(|p| {
            let info = PortalOutputInfo {
                name: p.name.clone(),
                pos_x: p.pos_x,
                pos_y: p.pos_y,
                logical_width: p.logical_width,
                logical_height: p.logical_height,
            };
            // Clone the PreparedCapture (Arc clone for session).
            let prep = PreparedCapture {
                session: p.prepared.session.clone(),
                node_id: p.prepared.node_id,
                width: p.prepared.width,
                height: p.prepared.height,
            };
            (prep, info)
        })
        .collect();

    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let captured_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let shm_images = capture::pipewire_finish_all(&pw_prepared)
                .map_err(|e| anyhow::anyhow!("PipeWire capture failed: {e}"))?;

            let mut results: Vec<CapturedOutput> = Vec::with_capacity(shm_images.len());
                        for (p, (shm_img, info)) in
                            prepared.iter().zip(shm_images) {
                            let rgba: RgbaImage = shm_img.image_transformed()
                    .map_err(|e| anyhow::anyhow!("image_transformed failed: {e}"))?;

                let handle = cosmic::widget::image::Handle::from_rgba(
                    rgba.width(),
                    rgba.height(),
                    rgba.clone().into_vec(),
                );

                results.push(CapturedOutput {
                    name: info.name.clone(),
                    rgba,
                    image_handle: handle,
                    width: shm_img.width,
                    height: shm_img.height,
                    logical_width: p.logical_width,
                    logical_height: p.logical_height,
                    pos_x: p.pos_x,
                    pos_y: p.pos_y,
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
