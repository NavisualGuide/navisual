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

use super::hit_test::{self, HitTestOutcome};
use super::trace::{FinalDecision, LocateTrace, OcrTrace};
use super::{a11y, ocr, LocateResult};
use crate::capture::{self, Rect};
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct LocateOptions {
    pub role: Option<String>,
    pub nearby_text: Option<String>,
    /// AI-predicted target bounding box in **virtual-desktop physical pixels**.
    /// Used by A11y (proximity sort) and OCR (overlap filter with ±300%
    /// expansion). When `None`, both tiers run unfiltered.
    pub ai_bbox: Option<Rect>,
    pub a11y_timeout_ms: u64,
    pub min_confidence: f32,
    /// Raw HWND captured at AI-call time. When set, A11y searches this HWND
    /// directly (not GetForegroundWindow) and OCR captures this HWND's rect —
    /// so a focus change between AI capture and locate can't redirect us to
    /// the wrong window.
    pub target_hwnd: Option<usize>,
    /// When set, the orchestrator writes the lossless PNG sent to OCR to this
    /// path. Useful for diagnosing why OCR misses specific UI elements.
    pub debug_ocr_image_path: Option<std::path::PathBuf>,
}

pub fn locate(
    target_text: &str,
    opts: &LocateOptions,
) -> Result<(Option<LocateResult>, LocateTrace)> {
    let started = Instant::now();
    let mut trace = LocateTrace::new(target_text);
    trace.target_role = opts.role.clone();
    trace.nearby_text = opts.nearby_text.clone();
    trace.ai_bbox = opts.ai_bbox;

    // Pass 1 — A11y.
    let mut a11y_opts = opts.clone();
    if a11y_opts.a11y_timeout_ms == 0 {
        a11y_opts.a11y_timeout_ms = 150;
    }
    let (a11y_hit, a11y_trace) = match a11y::find_element(target_text, &a11y_opts) {
        Ok(v) => v,
        Err(e) => {
            trace.final_decision = FinalDecision::Error {
                message: e.to_string(),
            };
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((None, trace));
        }
    };
    trace.a11y = a11y_trace;
    if let Some(hit) = a11y_hit {
        trace.final_decision = FinalDecision::HitA11y;
        trace.final_bbox = Some(hit.bbox);
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((Some(hit), trace));
    }

    // Pass 2 — OCR fallback on the active window.
    //
    // A1: Capture at native resolution (no cap_size, no JPEG encode) so OCR
    //     sees clean pixels instead of the downscaled+compressed AI image.
    let ocr_started = Instant::now();
    let exclude = capture::get_panel_rects();

    // Prefer the pinned HWND (the one the AI saw) so a focus change between
    // AI capture and locate can't send us to the wrong window. Falls through
    // to GetForegroundWindow if no HWND is pinned or the window is gone.
    let pinned_capture = opts
        .target_hwnd
        .and_then(|h| capture::recapture_window_raw(h, &exclude).ok());

    let (ocr_bytes, crop_rect, img_w, img_h) = match pinned_capture {
        Some((raw_img, rect)) => {
            let (iw, ih) = (raw_img.width(), raw_img.height());
            match capture::encode_png_for_ocr(&raw_img) {
                Ok(bytes) => (bytes, rect, iw, ih),
                Err(e) => {
                    trace.final_decision = FinalDecision::Error {
                        message: e.to_string(),
                    };
                    trace.elapsed_ms = started.elapsed().as_millis() as u32;
                    return Ok((None, trace));
                }
            }
        }
        None => match capture::capture_active_window_raw(&exclude) {
            Ok((raw_img, rect, _hwnd)) => {
                let (iw, ih) = (raw_img.width(), raw_img.height());
                match capture::encode_png_for_ocr(&raw_img) {
                    Ok(bytes) => (bytes, rect, iw, ih),
                    Err(e) => {
                        trace.final_decision = FinalDecision::Error {
                            message: e.to_string(),
                        };
                        trace.elapsed_ms = started.elapsed().as_millis() as u32;
                        return Ok((None, trace));
                    }
                }
            }
            Err(_) => {
                // Fallback: primary monitor via JPEG (panel is foreground or no app found).
                let jpeg = match capture::capture_primary_monitor_jpeg(80) {
                    Ok(b) => b,
                    Err(e) => {
                        trace.final_decision = FinalDecision::Error {
                            message: e.to_string(),
                        };
                        trace.elapsed_ms = started.elapsed().as_millis() as u32;
                        return Ok((None, trace));
                    }
                };
                let (iw, ih) = image::load_from_memory(&jpeg)
                    .map(|img| (img.width(), img.height()))
                    .unwrap_or((0, 0));
                (
                    jpeg,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    },
                    iw,
                    ih,
                )
            }
        },
    };

    // When debug screenshots are enabled, save the exact PNG sent to OCR so it
    // can be inspected. This is the lossless native-resolution image — what
    // OCR actually sees, not the downscaled JPEG sent to the AI.
    if let Some(ref path) = opts.debug_ocr_image_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, &ocr_bytes) {
            log::warn!("debug_ocr_image_path write failed: {e}");
        }
    }

    let results = match ocr::run_ocr(&ocr_bytes) {
        Ok(r) => r,
        Err(e) => {
            trace.final_decision = FinalDecision::Error {
                message: e.to_string(),
            };
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((None, trace));
        }
    };
    let mut ocr_trace = OcrTrace {
        ran: true,
        line_count: results.iter().filter(|r| r.confidence >= 1.0).count(),
        word_count: results.iter().filter(|r| r.confidence < 1.0).count(),
        sample_texts: results.iter().take(30).map(|r| r.text.clone()).collect(),
        ..Default::default()
    };

    // Convert the VD-space AI bbox into OCR-image-pixel space so the matcher
    // can filter candidates by overlap. Reverses the post-OCR scale-and-offset
    // step below: img = (vd - crop_origin) * (img / crop). When `crop_rect.width`
    // is 0 (full-screen JPEG fallback) we keep the bbox in image space as-is.
    let ai_bbox_img: Option<(i32, i32, u32, u32)> = opts.ai_bbox.map(|b| {
        if crop_rect.width == 0 || crop_rect.height == 0 || img_w == 0 || img_h == 0 {
            return (b.x, b.y, b.width, b.height);
        }
        let inv_sx = img_w as f32 / crop_rect.width as f32;
        let inv_sy = img_h as f32 / crop_rect.height as f32;
        let x = ((b.x - crop_rect.x) as f32 * inv_sx).round() as i32;
        let y = ((b.y - crop_rect.y) as f32 * inv_sy).round() as i32;
        let w = (b.width as f32 * inv_sx).round() as u32;
        let h = (b.height as f32 * inv_sy).round() as u32;
        (x, y, w, h)
    });

    let find_opts = ocr::FindOptions {
        role: opts.role.as_deref(),
        nearby_text: opts.nearby_text.as_deref(),
        screen_width: img_w,
        screen_height: img_h,
        ai_bbox: ai_bbox_img,
        min_confidence: opts.min_confidence,
    };

    let outcome = ocr::find_text(target_text, &results, &find_opts);
    ocr_trace.candidates = outcome.candidates;
    ocr_trace.strategy_used = outcome.strategy_used;
    ocr_trace.elapsed_ms = ocr_started.elapsed().as_millis() as u32;
    let Some(hit) = outcome.winner else {
        trace.ocr = ocr_trace;
        trace.final_decision = FinalDecision::Miss;
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    };
    let hit = hit.clone();

    // Translate image-pixel coords back to virtual-desktop coords.
    // img_w/img_h are the OCR image dims (native or 2× upscaled).
    // sx/sy converts OCR-space pixels back to native screen pixels, then the
    // crop origin is added to get virtual-desktop absolute coordinates.
    let (sx, sy) = if img_w > 0 && img_h > 0 && crop_rect.width > 0 && crop_rect.height > 0 {
        (
            crop_rect.width as f32 / img_w as f32,
            crop_rect.height as f32 / img_h as f32,
        )
    } else {
        (1.0, 1.0)
    };
    let bbox = Rect {
        x: (hit.bbox.0 as f32 * sx).round() as i32 + crop_rect.x,
        y: (hit.bbox.1 as f32 * sy).round() as i32 + crop_rect.y,
        width: (hit.bbox.2 as f32 * sx).round() as u32,
        height: (hit.bbox.3 as f32 * sy).round() as u32,
    };

    // C5 — WindowFromPoint hit-test: reject if the leaf HWND under the bbox
    // centre belongs to a non-interactive Win32 class (label, header, etc.).
    let cx = bbox.x + (bbox.width as i32 / 2);
    let cy = bbox.y + (bbox.height as i32 / 2);
    match hit_test::verify_hit(cx, cy) {
        HitTestOutcome::Rejected { leaf_class } => {
            trace.ocr = ocr_trace;
            trace.final_decision = FinalDecision::RejectedByHitTest { leaf_class };
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((None, trace));
        }
        HitTestOutcome::Pass | HitTestOutcome::WebRenderer => {}
    }

    let result = LocateResult {
        bbox,
        name: hit.text.clone(),
        role: "Ocr".to_string(),
        confidence: hit.confidence,
    };
    trace.ocr = ocr_trace;
    trace.final_decision = FinalDecision::HitOcr;
    trace.final_bbox = Some(bbox);
    trace.elapsed_ms = started.elapsed().as_millis() as u32;
    Ok((Some(result), trace))
}
