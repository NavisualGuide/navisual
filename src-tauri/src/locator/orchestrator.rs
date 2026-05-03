//! Element-locator orchestrator — A11y first, OCR fallback.
//!
//! Matches the v0.3 Python `element_locator.py` ordering:
//!   1. Try Windows UIA (< 5ms for browser tasks).
//!   2. On miss, capture the active window, run Windows.Media.Ocr, run
//!      `find_text` against the results.
//!
//! Returns a `LocateResult` whose bbox is in **virtual-desktop physical
//! pixels** — the same coordinate system as `capture::Rect` so the
//! eventual overlay renderer can consume it directly without further
//! translation.

use super::{a11y, ocr, LocateResult};
use crate::capture::{self, Rect};
use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct LocateOptions {
    pub role: Option<String>,
    pub nearby_text: Option<String>,
    pub zone: Option<(u32, u32)>,
    pub a11y_timeout_ms: u64,
    pub min_confidence: f32,
}

pub fn locate(target_text: &str, opts: &LocateOptions) -> Result<Option<LocateResult>> {
    // Pass 1 — A11y.
    let mut a11y_opts = opts.clone();
    if a11y_opts.a11y_timeout_ms == 0 {
        a11y_opts.a11y_timeout_ms = 150;
    }
    if let Some(hit) = a11y::find_element(target_text, &a11y_opts)? {
        return Ok(Some(hit));
    }

    // Pass 2 — OCR fallback on the active window.
    let (jpeg, crop_rect, _hwnd) = match capture::capture_active_window_jpeg(80) {
        Ok(v) => v,
        Err(_) => {
            // If active-window capture fails (e.g. our own panel is
            // foreground), fall back to the full primary monitor.
            let bytes = capture::capture_primary_monitor_jpeg(80)?;
            // Primary monitor rect at (0,0) — not strictly correct on
            // multi-monitor, but good enough for the fallback of a fallback.
            (
                bytes,
                Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                0usize,
            )
        }
    };

    // Decode dims from the JPEG so find_text's zone-filter gets correct
    // screen_width/height (the OCR coords live in image space).
    let (img_w, img_h) = image::load_from_memory(&jpeg)
        .map(|img| (img.width(), img.height()))
        .unwrap_or((0, 0));

    let results = ocr::run_ocr(&jpeg)?;
    let find_opts = ocr::FindOptions {
        role: opts.role.as_deref(),
        nearby_text: opts.nearby_text.as_deref(),
        screen_width: img_w,
        screen_height: img_h,
        zone: opts.zone,
        min_confidence: opts.min_confidence,
    };

    let Some(hit) = ocr::find_text(target_text, &results, &find_opts) else {
        return Ok(None);
    };

    // Translate image-pixel coords back to virtual-desktop coords.
    // The capture pipeline downscales to 1536×768 max before JPEG encode, so
    // img_w/img_h may be smaller than crop_rect.width/height. Scale back up
    // first, then add the crop origin.
    let (sx, sy) = if img_w > 0 && img_h > 0 && crop_rect.width > 0 && crop_rect.height > 0 {
        (
            crop_rect.width  as f32 / img_w as f32,
            crop_rect.height as f32 / img_h as f32,
        )
    } else {
        (1.0, 1.0)
    };
    let bbox = Rect {
        x:      (hit.bbox.0 as f32 * sx).round() as i32 + crop_rect.x,
        y:      (hit.bbox.1 as f32 * sy).round() as i32 + crop_rect.y,
        width:  (hit.bbox.2 as f32 * sx).round() as u32,
        height: (hit.bbox.3 as f32 * sy).round() as u32,
    };

    Ok(Some(LocateResult {
        bbox,
        name: hit.text.clone(),
        role: "Ocr".to_string(),
        confidence: hit.confidence,
    }))
}
