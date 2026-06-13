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

use super::hit_test::{self, HitTestOutcome, RoleHit};
use super::trace::{Corroboration, FinalDecision, LocateTrace, OcrTrace};
use super::{a11y, ocr, LocateResult};
use crate::capture::{self, Rect};
use anyhow::Result;
use std::time::Instant;

/// Corroboration-gate isolation thresholds: a match is treated as *content* only when it
/// occupies < `ISOLATION_MIN` of a line longer than `ISOLATION_LINE_FLOOR` chars — so
/// packed menu/tab strips (short lines) are never falsely rejected.
const ISOLATION_LINE_FLOOR: usize = 40;
const ISOLATION_MIN: f32 = 0.4;

#[derive(Debug, Clone, Default)]
pub struct LocateOptions {
    pub role: Option<String>,
    pub nearby_text: Option<String>,
    /// AI-predicted target bounding box in **virtual-desktop physical pixels**.
    /// Used by A11y (proximity sort) and OCR (overlap filter with ±300%
    /// expansion). When `None`, both tiers run unfiltered.
    pub ai_bbox: Option<Rect>,
    /// "Wrong spot" memory in **virtual-desktop physical pixels**: the bbox the
    /// locator pointed at before the user pressed Wrong → Wrong spot. Candidates
    /// whose centre falls inside it are excluded in both the A11y and OCR tiers,
    /// so the correction retry surfaces the second-best match instead of
    /// deterministically repeating the same wrong pick.
    pub avoid_bbox: Option<Rect>,
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
    pre_ocr: Option<(&[u8], Rect)>,
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

    // Prefer a pre-captured OCR image (taken at AI-capture time, overlay cleared, BEFORE the
    // streamed subtitle appeared) so OCR never reads our own caption — and there's no clear/
    // redraw flicker. Fall back to a fresh re-capture when none was supplied (e.g. next_step).
    let (ocr_bytes, crop_rect, img_w, img_h) = if let Some((png, rect)) = pre_ocr {
        let (iw, ih) = image::load_from_memory(png)
            .map(|i| (i.width(), i.height()))
            .unwrap_or((0, 0));
        (png.to_vec(), rect, iw, ih)
    } else {
        let exclude = capture::get_panel_rects();
        // Prefer the pinned HWND (the one the AI saw) so a focus change between AI capture and
        // locate can't send us to the wrong window. Falls through to GetForegroundWindow.
        let pinned_capture = opts
            .target_hwnd
            .and_then(|h| capture::recapture_window_raw(h, &exclude).ok());
        match pinned_capture {
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
        }
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
    let to_img_space = |b: Rect| -> (i32, i32, u32, u32) {
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
    };
    let ai_bbox_img: Option<(i32, i32, u32, u32)> = opts.ai_bbox.map(to_img_space);
    let avoid_bbox_img: Option<(i32, i32, u32, u32)> = opts.avoid_bbox.map(to_img_space);

    let find_opts = ocr::FindOptions {
        role: opts.role.as_deref(),
        nearby_text: opts.nearby_text.as_deref(),
        screen_width: img_w,
        screen_height: img_h,
        ai_bbox: ai_bbox_img,
        avoid_bbox: avoid_bbox_img,
        min_confidence: opts.min_confidence,
    };

    let outcome = ocr::find_text(target_text, &results, &find_opts);
    ocr_trace.candidates = outcome.candidates;
    ocr_trace.strategy_used = outcome.strategy_used;
    ocr_trace.elapsed_ms = ocr_started.elapsed().as_millis() as u32;
    // NOTE: a 2× whole-image upscale retry lived here briefly (2026-06-12) and
    // was reverted the next day. A measured scale sweep (the ignored
    // `ocr_scale_sweep` test) showed it rescued only 1 of 2 genuinely-present
    // hard targets (Photoshop "Select and Mask" yes; "Object Selection" no at
    // any scale), and the live cost was pathological — 12 s on a 2-MP window vs.
    // <300 ms offline, i.e. GPU/resource contention in the loaded app, not an
    // inherent OCR cost. Net negative: a guaranteed multi-second stall on every
    // hard miss for an unreliable rescue. The pixel-size floor is a property of
    // the input shared by all OCR engines (ocr-improvements-plan.md) — the real
    // fix is a better engine (ocrs spike) or letting the vision AI read it (v0.7),
    // not upscaling. The `ocr_scale_sweep` harness is kept for that evaluation.
    let Some(hit) = outcome.winner.cloned() else {
        trace.ocr = ocr_trace;
        trace.final_decision = FinalDecision::Miss;
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    };

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

    // Corroboration gate. A11y already missed (this is the OCR fallback), so an OCR text
    // match by name is only trusted when corroborated — otherwise it's likely the same word
    // appearing as content (terminal/document), not the control ("no pointer beats wrong
    // pointer"). Accept if ANY corroborator holds, else hard-reject.
    let cx = bbox.x + (bbox.width as i32 / 2);
    let cy = bbox.y + (bbox.height as i32 / 2);

    // Cheap native pre-filter: reject obviously inert Win32 leaf classes (scrollbar, static…).
    if let HitTestOutcome::Rejected { leaf_class } = hit_test::verify_hit(cx, cy) {
        trace.ocr = ocr_trace;
        trace.final_decision = FinalDecision::RejectedByHitTest { leaf_class };
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    // (1) UIA role hit-test — primary; works native AND primed-Electron.
    let role = hit_test::verify_role(cx, cy);
    let (uia_control_type, uia_interactive) = match &role {
        RoleHit::Interactive(ct) => (Some(ct.clone()), true),
        RoleHit::Content(ct) => (Some(ct.clone()), false),
        RoleHit::Unknown => (None, false),
    };
    // (2) Label isolation (image-pixel space, same as `results`).
    let (isolation, line_len) = ocr::isolation_for(&hit.bbox, target_text, &results);
    let isolation_ok = !(line_len > ISOLATION_LINE_FLOOR && isolation < ISOLATION_MIN);
    // (3) nearby_text anchor proximity (soft). A nearby_text identical to the
    // target carries no independent signal (self-anchor) — treat as absent.
    let near_anchor = opts
        .nearby_text
        .as_deref()
        .filter(|a| !a.trim().eq_ignore_ascii_case(target_text.trim()))
        .map(|a| ocr::anchor_near(&hit.bbox, a, &results, img_w, img_h))
        .unwrap_or(false);
    // (4) AI bbox region proximity (soft).
    let near_ai_bbox = ai_bbox_img
        .map(|ab| {
            let wcx = hit.bbox.0 as f32 + hit.bbox.2 as f32 / 2.0;
            let wcy = hit.bbox.1 as f32 + hit.bbox.3 as f32 / 2.0;
            let acx = ab.0 as f32 + ab.2 as f32 / 2.0;
            let acy = ab.1 as f32 + ab.3 as f32 / 2.0;
            let thresh = ((img_w as f32).powi(2) + (img_h as f32).powi(2)).sqrt() * 0.20;
            ((acx - wcx).powi(2) + (acy - wcy).powi(2)).sqrt() <= thresh
        })
        .unwrap_or(false);

    // UIA role is authoritative when it has an opinion. Interactive → accept. Content
    // (Document/Text/terminal) is a hard negative: the SOFT corroborators (anchor/bbox)
    // must NOT override it (a nearby word can coincide with a content match) — only a
    // genuinely isolated label still rescues it. Unknown (cold tree / non-UIA surface) →
    // fall to all corroborators.
    let accept = match &role {
        RoleHit::Interactive(_) => true,
        RoleHit::Content(_) => isolation_ok,
        RoleHit::Unknown => isolation_ok || near_anchor || near_ai_bbox,
    };
    ocr_trace.corroboration = Some(Corroboration {
        uia_control_type,
        uia_interactive,
        isolation,
        isolation_line_len: line_len,
        isolation_ok,
        near_anchor,
        near_ai_bbox,
        accepted: accept,
    });

    if !accept {
        let role_label = match &role {
            RoleHit::Interactive(_) => "interactive",
            RoleHit::Content(_) => "content",
            RoleHit::Unknown => "unknown",
        };
        trace.ocr = ocr_trace;
        trace.final_decision = FinalDecision::RejectedUncorroborated {
            detail: format!(
                "uia={role_label} isolation={isolation:.2}/{line_len} anchor={near_anchor} ai_bbox={near_ai_bbox}"
            ),
        };
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
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
