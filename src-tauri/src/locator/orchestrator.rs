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

use super::adapters;
use super::hit_test::{self, HitTestOutcome, RoleHit};
use super::trace::{AdapterTrace, Corroboration, FinalDecision, LocateTrace, OcrTrace, TemplateTrace};
use super::{a11y, ocr, template, LocateResult};
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
    /// The answering model's `ai_bbox` is trusted to *corroborate* (rescue) a borderline
    /// OCR match. Trust is default-on; only models on the configurable distrust list
    /// (default: the managed free-tier chain) get no corroboration vote — "no pointer
    /// beats wrong pointer". See `ai::bbox::bbox_is_decisive` / `BBOX_DISTRUST_MODELS`.
    pub bbox_decisive: bool,
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
    /// Pass-3 icon templates (Workstream B): `(icon_name, png/jpg bytes)` candidates the active
    /// nav-pack supplies for this target. Tried by NCC against the capture only when A11y + OCR
    /// both miss. Empty (the common case) → Pass 3 is skipped entirely.
    pub icon_templates: Vec<(String, Vec<u8>)>,
    /// Pack `element_hints` search region for this target, as a fractional rect `[x0,y0,x1,y1]`
    /// (0..1) within the captured window. Used by template matching only to break ties between
    /// multiple matches (region containment); never restricts the search. `None` → no region prior.
    pub icon_region: Option<[f32; 4]>,
    /// The active pack supplied an icon template for this target — i.e. it's a known icon-only
    /// element. A11y skips its expensive dead-end fallbacks (the `pane_fallback` raw-view walk,
    /// up to 2.5 s, and the empty-candidate `deep_role_match`) on non-Chrome surfaces, since an
    /// icon-only glyph has no accessible name to find; the bounded matcher passes still run, and
    /// template matching is the real path. Halves the locate time on sparse-A11y apps (Blender).
    pub icon_target: bool,
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

    // Pass 0 — app-specific adapters (Excel cells, …). Deterministic local geometry for
    // targets where AI grounding is weakest. An adapter only runs when it recognises the
    // focused app *and* the target shape; otherwise we fall straight through to A11y.
    if let Some(outcome) = adapters::try_locate(opts.target_hwnd, target_text) {
        let hit = outcome.result;
        trace.adapter = Some(AdapterTrace {
            name: outcome.name,
            hit: hit.is_some(),
            detail: outcome.detail,
        });
        if let Some(result) = hit {
            trace.final_decision = FinalDecision::HitAdapter;
            trace.final_bbox = Some(result.bbox);
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((Some(result), trace));
        }
        // Adapter claimed the target but couldn't resolve it (e.g. a scrolled-out cell);
        // the trace records why, and we fall through to the untouched A11y → OCR path.
    }

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
        // E2 — region-cropped upscaled re-OCR to rescue compact text the full-frame OCR mangled.
        // Skipped for icon targets (a glyph has no text — template is their path) and when there's
        // no AI bbox to crop to. A rescued hit sits in the bbox region, so it's corroborated.
        if !opts.icon_target {
            ocr_trace.region_ocr_attempted = ai_bbox_img.is_some();
            if let Some(result) = try_region_ocr(
                &ocr_bytes,
                target_text,
                ai_bbox_img,
                &crop_rect,
                img_w,
                img_h,
                opts.role.as_deref(),
                opts.nearby_text.as_deref(),
            ) {
                trace.ocr = ocr_trace;
                trace.final_decision = FinalDecision::HitOcr;
                trace.final_bbox = Some(result.bbox);
                trace.elapsed_ms = started.elapsed().as_millis() as u32;
                return Ok((Some(result), trace));
            }
        }
        trace.ocr = ocr_trace;
        // Pass 3 — icon template matching (nav-pack icons), the last resort for icon-only
        // controls A11y + OCR can't name. No-op when the pack supplied no candidates.
        let (tmpl_hit, tmpl_trace) =
            try_template_pass(&ocr_bytes, &crop_rect, img_w, img_h, opts.icon_region, ai_bbox_img, &opts.icon_templates);
        trace.template = tmpl_trace;
        if let Some(result) = tmpl_hit {
            trace.final_decision = FinalDecision::HitTemplate;
            trace.final_bbox = Some(result.bbox);
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((Some(result), trace));
        }
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
    // On an interactive hit it also hands back the control's bounding rect, used below to
    // snap the pointer off the OCR text span onto the whole clickable control.
    let (role, role_rect) = hit_test::verify_role(cx, cy);
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
    // (4) AI bbox region proximity (soft) — but only a corroboration vote when the
    // answering model is a strong grounder. A weak model's bbox is too unreliable to
    // *rescue* an otherwise-uncorroborated OCR match (it would let a near-coincidental
    // content word through), so its proximity carries no vote. The raw proximity is
    // still recorded in the trace for debugging.
    let near_ai_bbox_raw = ai_bbox_img
        .map(|ab| {
            let wcx = hit.bbox.0 as f32 + hit.bbox.2 as f32 / 2.0;
            let wcy = hit.bbox.1 as f32 + hit.bbox.3 as f32 / 2.0;
            let acx = ab.0 as f32 + ab.2 as f32 / 2.0;
            let acy = ab.1 as f32 + ab.3 as f32 / 2.0;
            let thresh = ((img_w as f32).powi(2) + (img_h as f32).powi(2)).sqrt() * 0.20;
            ((acx - wcx).powi(2) + (acy - wcy).powi(2)).sqrt() <= thresh
        })
        .unwrap_or(false);
    let near_ai_bbox = near_ai_bbox_raw && opts.bbox_decisive;

    // A fuzzy (approximate) OCR match is a guess about WHICH word — so it must NOT win on
    // isolation alone; it needs spatial agreement (the nearby-text anchor or a trusted AI
    // bbox). Without this, "Move"→"Mode" (75%) wins on an isolated label far from where the
    // model grounded the target (live-observed in Blender), preempting template matching.
    // Exact/substring matches are exact-text and keep the isolation path.
    let is_fuzzy = ocr_trace
        .strategy_used
        .as_deref()
        .map(|s| s.contains("fuzzy"))
        .unwrap_or(false);
    let spatially_corroborated = near_anchor || near_ai_bbox;

    let role_kind = match &role {
        RoleHit::Interactive(_) => RoleKind::Interactive,
        RoleHit::Content(_) => RoleKind::Content,
        RoleHit::Unknown => RoleKind::Unknown,
    };
    let accept = corroboration_accept(role_kind, is_fuzzy, isolation_ok, spatially_corroborated);
    ocr_trace.corroboration = Some(Corroboration {
        uia_control_type,
        uia_interactive,
        isolation,
        isolation_line_len: line_len,
        isolation_ok,
        near_anchor,
        near_ai_bbox: near_ai_bbox_raw,
        bbox_decisive: opts.bbox_decisive,
        accepted: accept,
        snapped_to_uia: false,
    });

    // Confidence order: A11y → exact/substring OCR → **template** → fuzzy OCR. A deterministic
    // NCC icon match is more reliable than an approximate text guess of a *different* word
    // (live-observed: "Move"→"Mode" 75% beating the real Move icon). So whenever the OCR winner
    // is fuzzy OR uncorroborated, try the pack's icon templates first and prefer a hit. An
    // exact/substring match that passed corroboration is authoritative text and skips this.
    // No-op when the active pack supplied no icon candidates (the common case).
    if is_fuzzy || !accept {
        let (tmpl_hit, tmpl_trace) =
            try_template_pass(&ocr_bytes, &crop_rect, img_w, img_h, opts.icon_region, ai_bbox_img, &opts.icon_templates);
        trace.template = tmpl_trace;
        if let Some(result) = tmpl_hit {
            trace.ocr = ocr_trace;
            trace.final_decision = FinalDecision::HitTemplate;
            trace.final_bbox = Some(result.bbox);
            trace.elapsed_ms = started.elapsed().as_millis() as u32;
            return Ok((Some(result), trace));
        }
    }

    if !accept {
        let role_label = match &role {
            RoleHit::Interactive(_) => "interactive",
            RoleHit::Content(_) => "content",
            RoleHit::Unknown => "unknown",
        };
        trace.ocr = ocr_trace;
        trace.final_decision = FinalDecision::RejectedUncorroborated {
            detail: format!(
                "uia={role_label} fuzzy={is_fuzzy} isolation={isolation:.2}/{line_len} anchor={near_anchor} ai_bbox={near_ai_bbox}"
            ),
        };
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    // Pointer snap: OCR's `bbox` is only the matched *text* span — a single word (so a
    // multi-word link/title pointer covers just one word) or a whole long line. When the
    // UIA role hit-test resolved an interactive control under the match, its rect is the
    // real clickable element, so snap the pointer to it. This tightens OCR pointers to the
    // same precision as A11y hits and fixes the "short bbox on substring match" gap.
    let pointer_bbox = match role_rect {
        Some(er) if uia_snap_plausible(&er, &bbox, &crop_rect) => {
            if let Some(c) = ocr_trace.corroboration.as_mut() {
                c.snapped_to_uia = true;
            }
            er
        }
        _ => bbox,
    };

    let result = LocateResult {
        bbox: pointer_bbox,
        name: hit.text.clone(),
        role: "Ocr".to_string(),
        confidence: hit.confidence,
    };
    trace.ocr = ocr_trace;
    trace.final_decision = FinalDecision::HitOcr;
    trace.final_bbox = Some(pointer_bbox);
    trace.elapsed_ms = started.elapsed().as_millis() as u32;
    Ok((Some(result), trace))
}

/// UIA role family under the OCR match, for the corroboration decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoleKind {
    Interactive,
    Content,
    Unknown,
}

/// Decide whether an OCR text match is trustworthy enough to point at, given the UIA role under
/// it and the available corroborators. The key precision rule: a **fuzzy** (approximate) match
/// is a guess about *which* word, so on any non-interactive surface it requires **spatial**
/// corroboration (the nearby-text anchor or a trusted AI bbox) — it never wins on label
/// isolation alone. Exact/substring matches keep the isolation path. ("No pointer beats wrong
/// pointer".)
fn corroboration_accept(
    role: RoleKind,
    is_fuzzy: bool,
    isolation_ok: bool,
    spatially_corroborated: bool,
) -> bool {
    match role {
        // UIA confirms a real interactive control under the point — authoritative.
        RoleKind::Interactive => true,
        // Content (Document/Text/terminal): an isolated label only; a fuzzy guess there is
        // almost certainly content text unless it also agrees spatially.
        RoleKind::Content => isolation_ok && (!is_fuzzy || spatially_corroborated),
        // Unknown (cold tree / non-UIA surface like an OpenGL app): exact/substring can pass on
        // isolation; a fuzzy guess must agree spatially.
        RoleKind::Unknown => {
            if is_fuzzy {
                spatially_corroborated
            } else {
                isolation_ok || spatially_corroborated
            }
        }
    }
}

/// E2 — region-cropped OCR rescue. Full-frame OCR missed: compact text below the engine's
/// ~30 px floor gets mangled (Photoshop options bar ~11 px, VS Code at a reduced font). When the
/// model gave a bbox, crop to it (+ a generous margin), **upscale ~3×**, and re-OCR just that
/// small region so the text clears the floor. Bounded cost (~tens of ms on a ~few-hundred-px
/// crop, vs the reverted whole-image upscale's ~837 ms / pathological 12 s under GPU load — you
/// only upscale the crop). A hit is corroborated by sitting in the AI bbox (the model said the
/// target is here *and* upscaled OCR reads it there). Returns the result in VD coords.
#[allow(clippy::too_many_arguments)]
fn try_region_ocr(
    haystack_png: &[u8],
    target_text: &str,
    ai_bbox_img: Option<(i32, i32, u32, u32)>,
    crop_rect: &Rect,
    img_w: u32,
    img_h: u32,
    role: Option<&str>,
    nearby_text: Option<&str>,
) -> Option<LocateResult> {
    const UP: u32 = 3;
    let (bx, by, bw, bh) = ai_bbox_img?;
    let full = image::load_from_memory(haystack_png).ok()?.to_rgba8();
    // Window = a margin around the bbox centre, **capped** so a loosely-grounded (large) bbox
    // can't balloon the upscaled OCR cost — half-extents are bbox-half + slack, clamped to a max,
    // then clamped to the image. Keeps the worst case ~3 MP at 3× (~2 s); a tight word/line bbox
    // stays ~0.2-0.8 MP (~150-400 ms).
    let (cx, cy) = (bx + bw as i32 / 2, by + bh as i32 / 2);
    let halfw = (bw as i32 / 2 + 100).min(350);
    let halfh = (bh as i32 / 2 + 70).min(250);
    let rx0 = (cx - halfw).max(0);
    let ry0 = (cy - halfh).max(0);
    let rx1 = (cx + halfw).min(img_w as i32);
    let ry1 = (cy + halfh).min(img_h as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return None;
    }
    let (rw, rh) = ((rx1 - rx0) as u32, (ry1 - ry0) as u32);
    let region = image::imageops::crop_imm(&full, rx0 as u32, ry0 as u32, rw, rh).to_image();
    let up = image::imageops::resize(&region, rw * UP, rh * UP, image::imageops::FilterType::Lanczos3);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(up)
        .write_to(&mut png, image::ImageFormat::Png)
        .ok()?;
    let results = ocr::run_ocr(png.get_ref()).ok()?;
    // The whole window is the AI-bbox area, so no overlap filter is needed here.
    let find_opts = ocr::FindOptions {
        role,
        nearby_text,
        screen_width: rw * UP,
        screen_height: rh * UP,
        ai_bbox: None,
        avoid_bbox: None,
        min_confidence: 0.5,
    };
    let outcome = ocr::find_text(target_text, &results, &find_opts);
    let hit = outcome.winner?;
    // upscaled-window-local → image px (÷UP + window origin) → virtual-desktop.
    let (sx, sy) = if img_w > 0 && img_h > 0 && crop_rect.width > 0 && crop_rect.height > 0 {
        (
            crop_rect.width as f32 / img_w as f32,
            crop_rect.height as f32 / img_h as f32,
        )
    } else {
        (1.0, 1.0)
    };
    let ix = rx0 as f32 + hit.bbox.0 as f32 / UP as f32;
    let iy = ry0 as f32 + hit.bbox.1 as f32 / UP as f32;
    let bbox = Rect {
        x: (ix * sx).round() as i32 + crop_rect.x,
        y: (iy * sy).round() as i32 + crop_rect.y,
        width: (hit.bbox.2 as f32 / UP as f32 * sx).round() as u32,
        height: (hit.bbox.3 as f32 / UP as f32 * sy).round() as u32,
    };
    Some(LocateResult {
        bbox,
        name: hit.text.clone(),
        role: "OcrRegion".to_string(),
        confidence: hit.confidence,
    })
}

/// Disambiguate when full-screen matching found **more than one** instance of an icon (similar
/// or repeated glyphs). The spatial priors only **break ties**, never restrict the search, so a
/// stale/wrong prior can't cause a miss. In order: (1) region containment — keep matches whose
/// centre is inside the pack's region hint, but if that leaves none (stale hint on moved UI) the
/// region is ignored; (2) AI-bbox proximity — among the rest, pick the one nearest the model's
/// predicted point; (3) highest score — last resort, and the only step when there are no priors.
/// `cands` is non-empty; coords are image px.
fn pick_match(
    mut cands: Vec<(template::TemplateMatch, String)>,
    icon_region: Option<[f32; 4]>,
    ai_bbox_img: Option<(i32, i32, u32, u32)>,
    img_w: u32,
    img_h: u32,
) -> (template::TemplateMatch, String) {
    let centre = |m: &template::TemplateMatch| {
        (
            m.x as f32 + m.width as f32 / 2.0,
            m.y as f32 + m.height as f32 / 2.0,
        )
    };
    if cands.len() > 1 {
        if let Some([fx0, fy0, fx1, fy1]) = icon_region {
            let (rx0, ry0) = (fx0 * img_w as f32, fy0 * img_h as f32);
            let (rx1, ry1) = (fx1 * img_w as f32, fy1 * img_h as f32);
            let inside: Vec<_> = cands
                .iter()
                .filter(|(m, _)| {
                    let (cx, cy) = centre(m);
                    cx >= rx0 && cx < rx1 && cy >= ry0 && cy < ry1
                })
                .cloned()
                .collect();
            if !inside.is_empty() {
                cands = inside;
            }
        }
    }
    if cands.len() > 1 {
        if let Some((bx, by, bw, bh)) = ai_bbox_img {
            let (acx, acy) = (bx as f32 + bw as f32 / 2.0, by as f32 + bh as f32 / 2.0);
            let d2 = |m: &template::TemplateMatch| {
                let (cx, cy) = centre(m);
                (cx - acx).powi(2) + (cy - acy).powi(2)
            };
            cands.sort_by(|(a, _), (b, _)| {
                d2(a).partial_cmp(&d2(b)).unwrap_or(std::cmp::Ordering::Equal)
            });
            return cands.into_iter().next().unwrap();
        }
    }
    // Highest score (cands already roughly score-sorted; make it explicit).
    cands.sort_by(|(a, _), (b, _)| {
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    cands.into_iter().next().unwrap()
}

/// Pass 3 — match the active nav-pack's candidate icon crops against the capture by NCC.
///
/// **Full-screen** coarse-to-fine ([`template::match_icon_pyramid`]) finds *every* on-screen
/// instance of each candidate icon (top-K), so the search never depends on a possibly-wrong AI
/// bbox or region hint. When more than one instance matches (similar/repeated icons), the priors
/// only **break the tie** ([`pick_match`]). Accepts only above `DEFAULT_MIN_SCORE` — "no pointer
/// beats wrong pointer"; the pyramid returns raw scores so the trace records the best even on a
/// reject. Matches are mapped image px → virtual-desktop (the same transform as OCR hits).
fn try_template_pass(
    haystack_png: &[u8],
    crop_rect: &Rect,
    img_w: u32,
    img_h: u32,
    icon_region: Option<[f32; 4]>,
    ai_bbox_img: Option<(i32, i32, u32, u32)>,
    templates: &[(String, Vec<u8>)],
) -> (Option<LocateResult>, Option<TemplateTrace>) {
    if templates.is_empty() {
        return (None, None);
    }
    let Ok(full) = template::load_gray_from_bytes(haystack_png) else {
        return (None, Some(TemplateTrace::default()));
    };
    let needles: Vec<(String, image::GrayImage)> = templates
        .iter()
        .filter_map(|(name, bytes)| {
            template::load_gray_from_bytes(bytes).ok().map(|g| (name.clone(), g))
        })
        .collect();

    let mut tr = TemplateTrace {
        templates_tried: needles.len(),
        best_score: -1.0,
        ..Default::default()
    };
    // Full-screen top-K per icon (min_score -1.0 → raw, so the trace's best_score is recorded
    // even on a reject), pooled across icons.
    let mut cands: Vec<(template::TemplateMatch, String)> = Vec::new();
    for (name, needle) in &needles {
        for m in template::match_icon_pyramid(&full, needle, -1.0) {
            if m.score > tr.best_score {
                tr.best_score = m.score;
                tr.best_scale = m.scale;
                tr.best_icon = Some(name.clone());
            }
            cands.push((m, name.clone()));
        }
    }
    cands.retain(|(m, _)| m.score >= template::DEFAULT_MIN_SCORE);
    if cands.is_empty() {
        return (None, Some(tr));
    }
    let (m, name) = pick_match(cands, icon_region, ai_bbox_img, img_w, img_h);
    tr.accepted = true;
    tr.best_icon = Some(name.clone());

    let (sx, sy) = if img_w > 0 && img_h > 0 && crop_rect.width > 0 && crop_rect.height > 0 {
        (
            crop_rect.width as f32 / img_w as f32,
            crop_rect.height as f32 / img_h as f32,
        )
    } else {
        (1.0, 1.0)
    };
    let bbox = Rect {
        x: (m.x as f32 * sx).round() as i32 + crop_rect.x,
        y: (m.y as f32 * sy).round() as i32 + crop_rect.y,
        width: (m.width as f32 * sx).round() as u32,
        height: (m.height as f32 * sy).round() as u32,
    };
    let result = LocateResult {
        bbox,
        name,
        role: "Template".to_string(),
        confidence: m.score,
    };
    (Some(result), Some(tr))
}

/// Whether the UIA element rect under the OCR match is a sound pointer target. Requires the
/// element to actually sit under the match (its rect contains the OCR winner's centre) and
/// rejects container-sized rects (a whole pane / list / window), which would make the
/// pointer vague rather than precise. The size cap only applies when the captured-window
/// size (`crop`) is known.
fn uia_snap_plausible(er: &Rect, ocr: &Rect, crop: &Rect) -> bool {
    let cx = ocr.x + ocr.width as i32 / 2;
    let cy = ocr.y + ocr.height as i32 / 2;
    let contains =
        cx >= er.x && cx < er.x + er.width as i32 && cy >= er.y && cy < er.y + er.height as i32;
    if !contains {
        return false;
    }
    if crop.width > 0
        && crop.height > 0
        && (er.width as f32 > crop.width as f32 * 0.9
            || er.height as f32 > crop.height as f32 * 0.6)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    // Live: drive the E2 region-OCR rescue (try_region_ocr) against a real capture with a
    // synthetic AI bbox, to confirm it reads compact text + maps coords. With the whole image as
    // the crop_rect (origin 0, sx=1) the result bbox is in image px, so it should sit inside the
    // given BBOX region. IN=capture.png; BBOX=x,y,w,h (around the target text); TARGET=word.
    //   $env:IN="vscode_cap.png"; $env:BBOX="450,180,200,150"; $env:TARGET="user";
    //   cargo test --lib region_ocr_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn region_ocr_live() {
        let bytes = std::fs::read(std::env::var("IN").unwrap()).unwrap();
        let (w, h) = image::load_from_memory(&bytes).map(|i| (i.width(), i.height())).unwrap();
        let p: Vec<i32> = std::env::var("BBOX")
            .unwrap()
            .split(',')
            .map(|s| s.trim().parse().unwrap())
            .collect();
        let bbox = (p[0], p[1], p[2] as u32, p[3] as u32);
        let target = std::env::var("TARGET").unwrap();
        let crop = Rect { x: 0, y: 0, width: w, height: h };
        let t = std::time::Instant::now();
        let res = try_region_ocr(&bytes, &target, Some(bbox), &crop, w, h, None, None);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match res {
            Some(r) => eprintln!("FOUND '{}' role={} at {:?} in {ms:.0}ms (bbox region {:?})", r.name, r.role, r.bbox, bbox),
            None => eprintln!("not found in {ms:.0}ms (bbox region {:?})", bbox),
        }
    }

    #[test]
    fn snap_accepts_control_sized_element_over_word() {
        // OCR matched the word "mini" (tight span); UIA resolves the whole link/title.
        let ocr = r(300, 200, 40, 16);
        let element = r(120, 196, 380, 24); // the full clickable title, contains the word
        let crop = r(0, 0, 1280, 1024);
        assert!(uia_snap_plausible(&element, &ocr, &crop));
    }

    #[test]
    fn snap_rejects_element_not_under_match() {
        // ElementFromPoint resolved something whose rect doesn't cover the OCR centre —
        // never snap there (would move the pointer off the matched text).
        let ocr = r(300, 200, 40, 16);
        let element = r(600, 600, 80, 24);
        let crop = r(0, 0, 1280, 1024);
        assert!(!uia_snap_plausible(&element, &ocr, &crop));
    }

    #[test]
    fn snap_rejects_container_sized_rect() {
        let ocr = r(300, 200, 40, 16);
        let crop = r(0, 0, 1280, 1024);
        // Whole-pane width → vague pointer, reject.
        let too_wide = r(0, 190, 1200, 30);
        assert!(!uia_snap_plausible(&too_wide, &ocr, &crop));
        // Whole-column height (a list/tree pane) → reject.
        let too_tall = r(280, 0, 120, 800);
        assert!(!uia_snap_plausible(&too_tall, &ocr, &crop));
    }

    #[test]
    fn pick_match_uses_priors_only_to_break_ties() {
        use super::pick_match;
        let tm = |x, y, score| {
            (
                template::TemplateMatch { x, y, width: 20, height: 20, score, scale: 1.0 },
                "move".to_string(),
            )
        };
        // Single candidate → returned regardless of priors.
        let (m, _) = pick_match(vec![tm(500, 500, 0.95)], None, None, 1000, 800);
        assert_eq!((m.x, m.y), (500, 500));
        // Two candidates + "left" region [0,0,0.2,1] → keep the one inside, even though the
        // outside one scored higher (region containment disambiguates).
        let (m, _) = pick_match(
            vec![tm(800, 100, 0.99), tm(40, 100, 0.92)],
            Some([0.0, 0.0, 0.2, 1.0]),
            None,
            1000,
            800,
        );
        assert_eq!(m.x, 40);
        // No region, bbox near the right one → pick nearest the bbox centre.
        let (m, _) = pick_match(
            vec![tm(40, 100, 0.99), tm(800, 110, 0.92)],
            None,
            Some((790, 100, 20, 20)),
            1000,
            800,
        );
        assert_eq!(m.x, 800);
        // No priors → highest score.
        let (m, _) = pick_match(vec![tm(40, 100, 0.92), tm(800, 100, 0.99)], None, None, 1000, 800);
        assert_eq!(m.x, 800);
        // Region matches none (stale hint) → region ignored, fall back to score.
        let (m, _) = pick_match(
            vec![tm(800, 100, 0.99), tm(900, 100, 0.92)],
            Some([0.0, 0.0, 0.2, 1.0]),
            None,
            1000,
            800,
        );
        assert_eq!(m.x, 800);
    }

    #[test]
    fn fuzzy_match_needs_spatial_corroboration() {
        use super::{corroboration_accept, RoleKind};
        // The live Blender regression: target "Move" fuzzy-matched "Mode" (isolated label) on a
        // non-UIA (Unknown) surface, far from the AI bbox and with no anchor → must REJECT so
        // template matching gets a turn.
        assert!(!corroboration_accept(RoleKind::Unknown, true, true, false));
        // A fuzzy match that DOES agree spatially is accepted.
        assert!(corroboration_accept(RoleKind::Unknown, true, true, true));
        // Exact/substring (not fuzzy) still passes on isolation alone — no regression.
        assert!(corroboration_accept(RoleKind::Unknown, false, true, false));
        // Content surface: a fuzzy guess without spatial agreement is rejected even if isolated.
        assert!(!corroboration_accept(RoleKind::Content, true, true, false));
        assert!(corroboration_accept(RoleKind::Content, true, true, true));
        // Interactive UIA hit is authoritative regardless.
        assert!(corroboration_accept(RoleKind::Interactive, true, false, false));
        // Nothing corroborates a non-fuzzy Unknown match with no isolation → reject.
        assert!(!corroboration_accept(RoleKind::Unknown, false, false, false));
    }

    #[test]
    fn snap_size_cap_skipped_when_crop_unknown() {
        // Full-screen JPEG fallback path: crop is degenerate, so only containment gates.
        let ocr = r(300, 200, 40, 16);
        let crop = r(0, 0, 0, 0);
        let big = r(0, 190, 1200, 30);
        assert!(uia_snap_plausible(&big, &ocr, &crop));
    }
}
