//! Screen change detection — Phase E.3.
//!
//! A background thread captures the active window every `POLL_MS` milliseconds
//! and computes a lightweight perceptual hash (average hash on an 8×8 greyscale
//! thumbnail).  When the Hamming distance between successive hashes exceeds
//! `CHANGE_THRESHOLD`, a `screen_changed` Tauri event is emitted so the
//! frontend can auto-advance non-checkpoint steps without user interaction.

use crate::capture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// Polling interval in milliseconds.
const POLL_MS: u64 = 500;

/// Hamming distance threshold (0–64).  A value of 6 means roughly 10 % of the
/// 64 hash bits differ — enough to catch a page navigation or dialog
/// appearing, but not a cursor blink or subtle animation.
const CHANGE_THRESHOLD: u32 = 6;

/// Payload emitted to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct ScreenChangedPayload {
    pub distance: u32,
}

/// Handle returned from `start` — dropping it (or calling `stop`) halts the
/// background polling thread.
pub struct ScreenWatcher {
    running: Arc<AtomicBool>,
}

impl ScreenWatcher {
    /// Spawn the background watcher.  It will immediately start polling.
    pub fn start(app: AppHandle) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();

        std::thread::Builder::new()
            .name("screen-watcher".into())
            .spawn(move || {
                let mut prev_hash: Option<u64> = None;
                while flag.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
                    if !flag.load(Ordering::Relaxed) {
                        break;
                    }

                    let hash = match compute_screen_hash(&app) {
                        Some(h) => h,
                        None => continue,
                    };

                    if let Some(prev) = prev_hash {
                        let dist = hamming(prev, hash);
                        if dist >= CHANGE_THRESHOLD {
                            let _ = app.emit(
                                "screen_changed",
                                ScreenChangedPayload { distance: dist },
                            );
                            log::debug!("screen_changed: distance={dist}");
                        }
                    }
                    prev_hash = Some(hash);
                }
                log::info!("screen-watcher stopped");
            })
            .expect("spawn screen-watcher thread");

        Self { running }
    }

    /// Stop the background polling thread.
    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for ScreenWatcher {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Capture the active window at low quality and compute the average hash.
/// The panel rect is excluded so its own UI updates (streaming text, etc.)
/// do not trigger false screen-change events.
fn compute_screen_hash(app: &AppHandle) -> Option<u64> {
    let exclude = capture::get_panel_rects();
    // Use low JPEG quality — we only need a rough thumbnail.
    let (jpeg, _rect, _hwnd) = capture::capture_active_window_jpeg(30, &exclude).ok()?;
    let img = image::load_from_memory(&jpeg).ok()?;

    // Resize to 8×8 greyscale.
    let thumb = image::imageops::resize(
        &img.to_luma8(),
        8,
        8,
        image::imageops::FilterType::Triangle,
    );

    // Average hash: compute mean pixel value, then set each bit to 1 if the
    // pixel is above the mean.
    let pixels: Vec<u8> = thumb.pixels().map(|p| p.0[0]).collect();
    let mean: u64 = pixels.iter().map(|&v| v as u64).sum::<u64>() / pixels.len().max(1) as u64;
    let mut hash: u64 = 0;
    for (i, &v) in pixels.iter().enumerate() {
        if (v as u64) > mean {
            hash |= 1u64 << i;
        }
    }
    Some(hash)
}

/// Hamming distance between two 64-bit hashes.
fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}
