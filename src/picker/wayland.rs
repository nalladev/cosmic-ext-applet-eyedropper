// SPDX-License-Identifier: MPL-2.0

//! Entry point for screen capture via the XDG Desktop Portal Screenshot API.
//!
//! This uses the same approach as Flameshot: `org.freedesktop.portal.Screenshot`
//! with `interactive=false` for a one-shot full-desktop capture with no
//! per-call permission prompts (uses portal's "Remember" checkbox).
//!
//! The flow:
//!
//! 1. Call `Screenshot::request().interactive(false)` → portal returns image
//!    file URI (first time shows permission dialog with "Remember" checkbox).
//! 2. Read the image file, crop per-output using Wayland output geometry.
//! 3. Create per-output `CapturedOutput` with GPU handles.
//!
//! This completely avoids `PipeWire`, `ScreenCast` sessions, stream management,
//! and restore tokens — it's a single portal call that returns the screenshot
//! directly.

use std::sync::OnceLock;

use image::RgbaImage;
use tokio::sync::oneshot;

use crate::picker::CapturedOutput;
use crate::picker::capture::CaptureHelper;

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
// Screenshot portal capture – the Flameshot approach.
// ---------------------------------------------------------------------------

/// Capture all outputs using the XDG Desktop Portal Screenshot API.
///
/// This is exactly how Flameshot works on Wayland:
/// `org.freedesktop.portal.Screenshot` with `interactive=false` gives
/// a one-shot full-desktop screenshot. The portal handles permission
/// persistence automatically (first call shows "Allow" dialog with
/// "Remember" checkbox; subsequent calls are silent).
///
/// The image is cropped per-output using Wayland output geometry from
/// the [`CaptureHelper`] singleton. Each output gets its own
/// [`CapturedOutput`] with a GPU texture handle ready for overlay
/// rendering.
///
/// Compared to the old `ScreenCast` + `PipeWire` approach this is:
///
/// * **No `PipeWire`** – no streams, no buffers, no fd, no frame sync.
/// * **No session management** – no `create_session` / `select_sources` /
///   `start` / `open_pipe_wire_remote` round-trips.
/// * **No restore tokens** – the portal handles persistence internally.
/// * **Single D-Bus call** – request → response with image URI.
#[allow(
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::items_after_statements
)]
#[allow(clippy::missing_errors_doc)]
pub async fn capture_outputs() -> Result<Vec<CapturedOutput>, anyhow::Error> {
    use ashpd::desktop::screenshot::Screenshot;

    let h = helper();
    let t_start = std::time::Instant::now();
    eprintln!("[capture] === Starting Screenshot portal capture ===");

    // ── Phase 1: portal call (async D-Bus) ────────────────────────
    let response = Screenshot::request()
        .interactive(false) // one-shot, no region selection UI
        .modal(false) // not a modal dialog
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Screenshot request failed: {e}"))?
        .response()
        .map_err(|e| anyhow::anyhow!("Screenshot response error: {e}"))?;

    let uri = response.uri();
    let path = uri
        .to_file_path()
        .map_err(|()| anyhow::anyhow!("Cannot convert screenshot URI to file path: {uri}"))?;
    eprintln!("[capture]   screenshot saved to: {}", path.display());

    // ── Phase 2: load, crop, build handles (blocking thread) ──────
    let (tx, rx) = oneshot::channel();

    std::thread::spawn(move || {
        let captured_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let t_load = std::time::Instant::now();
            let full_img = image::open(&path)
                .map_err(|e| anyhow::anyhow!("Failed to open screenshot image: {e}"))?;
            let full_rgba = full_img.to_rgba8();
            let full_width = full_rgba.width();
            let full_height = full_rgba.height();
            eprintln!(
                "[capture]   loaded screenshot {}x{} in {:?}",
                full_width,
                full_height,
                t_load.elapsed(),
            );

            // Clean up the temp file the portal left behind.
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("[capture]   warning: failed to remove temp file: {e}");
            }

            // Compute the total logical desktop area from all known outputs.
            let wl_outputs = h.outputs();
            let n = wl_outputs.len();
            eprintln!("[capture]   {n} Wayland output(s) for cropping");

            if n == 0 {
                eprintln!("[capture]   WARNING: no outputs discovered, using whole screenshot");
                // Fallback: treat the whole screenshot as a single output.
                let handle = cosmic::widget::image::Handle::from_rgba(
                    full_width,
                    full_height,
                    full_rgba.clone().into_vec(),
                );
                return Ok(vec![CapturedOutput {
                    name: "screenshot".to_string(),
                    rgba: full_rgba,
                    image_handle: handle,
                    width: full_width,
                    height: full_height,
                    logical_width: full_width,
                    logical_height: full_height,
                    pos_x: 0,
                    pos_y: 0,
                }]);
            }

            // Find min/max coordinates to compute total logical desktop size.
            struct OutputGeom {
                pos_x: i32,
                pos_y: i32,
                log_w: u32,
                log_h: u32,
                name: String,
            }
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            let mut geoms: Vec<OutputGeom> = Vec::with_capacity(n);

            for output in &wl_outputs {
                if let Some(info) = h.output_info(output) {
                    let (px, py) = info.location;
                    let (lw, lh) = info.logical_size.unwrap_or((px + 1920, py + 1080)); // fallback guess
                    let (luw, luh) = (lw.max(0).cast_unsigned(), lh.max(0).cast_unsigned());

                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px + lw);
                    max_y = max_y.max(py + lh);

                    geoms.push(OutputGeom {
                        pos_x: px,
                        pos_y: py,
                        log_w: luw,
                        log_h: luh,
                        name: info
                            .name
                            .unwrap_or_else(|| format!("monitor-{}", geoms.len())),
                    });
                }
            }

            let total_logical_w = (max_x - min_x).max(1).cast_unsigned();
            let total_logical_h = (max_y - min_y).max(1).cast_unsigned();

            // Scale factor: screenshot pixels per logical coordinate.
            let scale_x = f64::from(full_width) / f64::from(total_logical_w);
            let scale_y = f64::from(full_height) / f64::from(total_logical_h);

            eprintln!(
                "[capture]   desktop logical {}x{} (min {}/{}) screenshot {}x{} → scale {:.3}x{:.3}",
                total_logical_w,
                total_logical_h,
                min_x,
                min_y,
                full_width,
                full_height,
                scale_x,
                scale_y,
            );

            let mut results: Vec<CapturedOutput> = Vec::with_capacity(geoms.len());

            for g in &geoms {
                // Crop region in screenshot pixel coordinates.
                let crop_x = (f64::from(g.pos_x - min_x) * scale_x).round() as u32;
                let crop_y = (f64::from(g.pos_y - min_y) * scale_y).round() as u32;
                let crop_w = (f64::from(g.log_w) * scale_x).round() as u32;
                let crop_h = (f64::from(g.log_h) * scale_y).round() as u32;

                // Clamp to image bounds.
                let crop_w = crop_w.min(full_width.saturating_sub(crop_x));
                let crop_h = crop_h.min(full_height.saturating_sub(crop_y));

                if crop_w == 0 || crop_h == 0 {
                    eprintln!(
                        "[capture]   SKIP {}: zero-sized crop {}x{}",
                        g.name, crop_w, crop_h
                    );
                    continue;
                }

                eprintln!(
                    "[capture]   crop {}: logical={}x{} @({},{}) → pixel={}x{} @({},{})",
                    g.name, g.log_w, g.log_h, g.pos_x, g.pos_y, crop_w, crop_h, crop_x, crop_y,
                );

                let cropped: RgbaImage =
                    image::imageops::crop_imm(&full_rgba, crop_x, crop_y, crop_w, crop_h)
                        .to_image();

                let handle = cosmic::widget::image::Handle::from_rgba(
                    crop_w,
                    crop_h,
                    cropped.clone().into_vec(),
                );

                results.push(CapturedOutput {
                    name: g.name.clone(),
                    rgba: cropped,
                    image_handle: handle,
                    width: crop_w,
                    height: crop_h,
                    logical_width: g.log_w,
                    logical_height: g.log_h,
                    pos_x: g.pos_x,
                    pos_y: g.pos_y,
                });
            }

            eprintln!(
                "[capture]   produced {} per-output CapturedOutput(s)",
                results.len()
            );
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

    let result = rx
        .await
        .map_err(|_| anyhow::anyhow!("Capture thread was cancelled"))?;
    let captured = result?;

    eprintln!(
        "[capture] === Screenshot capture finished: {} output(s) in {:?} ===",
        captured.len(),
        t_start.elapsed(),
    );

    Ok(captured)
}
