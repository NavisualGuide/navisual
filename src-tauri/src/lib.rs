// Copyright (c) 2024-2026 Jin Fu
// Licensed under the Functional Source License, Version 1.1 (Apache 2.0).
// See the LICENSE file in the root of this repository for complete details.

//! Navisual — Rust/Tauri backend entry point.

mod ai;
mod capture;
mod credvault;
mod jsonl_log;
mod locator;
mod overlay;
mod packs;
mod prompt_log;
mod server;
mod track;
mod tts;

use ai::config::Config;
use ai::cost_tracker::CostTracker;
use ai::router::AiRouter;
use ai::session::SessionManager;
use ai::types::{GuidanceStep, OverlayType};

use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use tokio::time::Duration;

pub static APP_HANDLE: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

#[derive(serde::Serialize)]
struct CaptureResult {
    jpeg_base64: String,
    width: u32,
    height: u32,
    crop_rect: Option<capture::Rect>,
    bytes: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Default)]
struct GuidanceState {
    session_id: Option<String>,
    steps: Vec<GuidanceStep>,
    state_summary: String,
    needs_input: bool,
    provider: String,
    /// Capture rect from the most recent guide() call — stored so next_step()
    /// can confine the visual grid overlay to the same app window.
    capture_rect: Option<capture::Rect>,
    /// Raw HWND (as usize) of the target app window discovered on first guide().
    /// Reused on every subsequent call so the program never loses track of the
    /// target even after git dialogs, credential prompts, or other transient
    /// windows pop above it in z-order.
    target_hwnd: Option<usize>,
    /// User-explicitly pinned window (via the target-picker dropdown). Survives
    /// new tasks; only cleared by `unpin_target_window` or when the window closes.
    pinned_hwnd: Option<usize>,
    /// The hwnd most recently announced with a boundary flash, from ANY of the
    /// four call sites (auto-detect refresh, guide, correction, explicit pin) —
    /// lets `refresh_active_window` flash only when the auto-followed target
    /// actually changes, not on every passive z-order/foreground event.
    last_announced_hwnd: Option<usize>,
    /// User-selected "Entire desktop" target (via the target-picker dropdown).
    /// When true, every capture path (guide / correction) grabs the whole virtual
    /// desktop instead of a single window. A deliberate, sticky user choice —
    /// the AI can no longer request full-screen on its own. Mutually exclusive
    /// with `pinned_hwnd`; survives new tasks like a pin.
    full_screen_mode: bool,
    /// When `full_screen_mode` is set, the specific monitor the user chose to share
    /// (its virtual-desktop rect). `None` = the whole stitched virtual desktop — used
    /// only on single-monitor systems; with 2+ monitors the picker requires choosing a
    /// single screen (a stitched multi-monitor capture is downscaled to uselessness).
    full_screen_monitor: Option<capture::Rect>,
    /// v0.7 Workstream S — the Structured-Context element snapshot enumerated at the
    /// most recent AI-capture (the [Screen Elements] list the AI saw). Stored beside
    /// the capture state so `next_step`'s locates resolve `target_element_id` against
    /// the SAME list; replaced (or cleared) on every guide/correction capture.
    context_elements: Option<std::sync::Arc<Vec<locator::ContextElement>>>,
    /// Training-data join key (llm-finetuning-eval.md §5b): the UUID of the most
    /// recent AI request. `next_step` attributes its locate traces to this (the
    /// request whose response produced the steps it's advancing through); each new
    /// guide/correction request reads it as `prev_request_id` (chaining
    /// rejected→chosen correction pairs) before replacing it with its own.
    request_id: Option<String>,
    /// Flow A: candidate boxes currently on screen awaiting state-readback
    /// resolution (the user's next click in the app is the answer). Armed by
    /// `retry_locate` when it shows 2+ candidates; resolved (label banked to the
    /// local training mirror) or expired at the next backend event.
    pending_candidates: Option<locator::candidates::PendingCandidates>,
    /// Session-sticky language SAMPLE — the user's last substantial message (see
    /// `prompts::is_language_sample`), persisted across machine `[User completed:]` turns so the
    /// reply language follows what the user last wrote, in EITHER direction: into a non-Latin
    /// language ("请说中文") and back out again ("please speak English."). Recomputed on each new
    /// task; updated on any reply that is itself a language sample; a short ack ("ok") leaves it
    /// unchanged. `None` → defer to the current request text (`prompts::reply_language_directive`).
    reply_lang_sample: Option<String>,
}

/// S.1 — enumerate the Structured-Context element snapshot for the captured window.
/// Runs on a blocking thread right after the AI capture (same freshness contract as
/// the screenshot); every skip (framework `Other`, over-cap, over-budget, zero
/// elements) is logged with its reason and yields `None` → no prompt block, no
/// Pass 0.5 (Decision 4: skip the whole block, never truncate).
#[cfg(windows)]
fn enumerate_context_snapshot(hwnd: usize) -> Option<Vec<locator::ContextElement>> {
    let started = std::time::Instant::now();
    match locator::a11y::enumerate_context_elements(hwnd) {
        Ok(els) if els.is_empty() => {
            log::info!("[context] skipped: zero elements");
            None
        }
        Ok(els) => {
            log::info!(
                "[context] {} elements enumerated in {} ms",
                els.len(),
                started.elapsed().as_millis()
            );
            Some(els)
        }
        Err(reason) => {
            log::info!("[context] skipped: {reason}");
            None
        }
    }
}

#[cfg(not(windows))]
fn enumerate_context_snapshot(_hwnd: usize) -> Option<Vec<locator::ContextElement>> {
    None
}

/// S.1 adaptive skip (2026-07-07) — wraps `enumerate_context_snapshot` with a real
/// wall-clock bound. `CONTEXT_BUDGET_MS` alone can't do this: it's checked only after the
/// blocking `find_all_build_cache` call already returned, so on a window like Lightroom
/// Classic (measured 2.2-5.7s per call — see `locator-testing.md` §0) every request pays
/// the full cost regardless of what that constant is set to. This wraps the call in a
/// timeout matching the same budget, so the *caller* never waits longer than that; if a
/// window trips the timeout enough times in a row, `context_window_is_slow` skips it
/// outright on later calls — no window class, no app identity, purely behavioural, so it
/// adapts to any app with this pathology instead of needing a per-app hardcoded check.
/// The abandoned blocking task keeps running to completion on its own thread regardless
/// (COM calls can't be cancelled) — accepted cost, paid once or twice per window per
/// session rather than on every single request.
#[cfg(windows)]
async fn enumerate_context_snapshot_bounded(hwnd: usize) -> Option<Vec<locator::ContextElement>> {
    if locator::a11y::context_window_is_slow(hwnd) {
        log::info!("[context] skipped: window already proven slow this session");
        return None;
    }
    // This outer timeout is only the uninterruptible-COM safety net — the inner
    // `enumerate_context_elements` enforces its own, tighter, per-window budget
    // (CONTEXT_BUDGET_MS for the bulk path, the larger EXCEL_CONTEXT_BUDGET_MS for
    // Excel's pruned walk). So the net must be the LARGER of the two, or it would
    // kill Excel ~500 ms before Excel's own budget even fires — silently defeating
    // EXCEL_CONTEXT_BUDGET_MS and making the adaptive-skip trip harder for Excel than
    // designed (found 2026-07-13). The wrapper stays app-identity-agnostic; the inner
    // code owns the per-window value.
    let net_ms = locator::a11y::CONTEXT_BUDGET_MS.max(locator::a11y::EXCEL_CONTEXT_BUDGET_MS);
    let budget = std::time::Duration::from_millis(net_ms as u64);
    match tokio::time::timeout(
        budget,
        tokio::task::spawn_blocking(move || enumerate_context_snapshot(hwnd)),
    )
    .await
    {
        Ok(Ok(result)) => {
            locator::a11y::context_window_mark_fast(hwnd);
            result
        }
        Ok(Err(_join_error)) => None,
        Err(_timed_out) => {
            log::info!(
                "[context] skipped: enumeration exceeded {} ms wall-clock, abandoning (still finishing in the background)",
                budget.as_millis()
            );
            locator::a11y::context_window_mark_slow(hwnd);
            None
        }
    }
}

#[cfg(not(windows))]
async fn enumerate_context_snapshot_bounded(
    _hwnd: usize,
) -> Option<Vec<locator::ContextElement>> {
    None
}

/// Shared app state.
struct AppState {
    ai_router: Mutex<AiRouter>,
    guidance: parking_lot::Mutex<GuidanceState>,
    tts: tts::TtsEngine,
    tracker: track::WindowTracker,
    /// Last non-None overlay emitted — used by restore_overlay to bring it back after Clear.
    last_overlay: parking_lot::Mutex<Option<LastOverlay>>,
    /// Resolved path to the .env settings file — always writable (app data dir).
    env_path: PathBuf,
    /// Path to the Supabase session JSON file.
    supabase_session_path: PathBuf,
    /// Provider-independent Supabase session for account management (sign in,
    /// sign out, get_account_info, etc.). Lives here — not inside the AI
    /// client — so account commands work regardless of which `API_PROVIDER`
    /// is active.  The `ManagedClient`'s own session is kept in sync for
    /// relay requests.
    supabase_session: Mutex<Option<server::SupabaseSession>>,
    /// Previous aHash for Autopilot on-demand screen-change polling.
    screen_hash: parking_lot::Mutex<Option<u64>>,
    /// Most-recent AI-image JPEG bytes (the one sent to the AI on the latest
    /// `guide`/`next_step`/`send_correction`). Held in RAM only — never
    /// written to disk — so the lightbox can re-open it without persisting
    /// the user's screen content to storage. Cleared on new task / on quit.
    chat_full_jpeg: parking_lot::Mutex<Option<Vec<u8>>>,
    /// Nav-Packs loaded at startup (bundled + user). Read-only after load; the active pack
    /// for the focused window injects app-specific guidance + shortcuts into the prompt.
    packs: packs::PackRegistry,
}

/// Snapshot of the most recent non-clear overlay. Stored so `restore_overlay`
/// can re-emit after the user clears the screen guide.
#[derive(Clone)]
struct LastOverlay {
    kind: overlay::OverlayKind,
    bbox: Option<capture::Rect>,
    text: Option<String>,
    ai_bbox: Option<capture::Rect>,
    /// Target app window — so restore_overlay can re-arm the tracker (anchored to the
    /// right app) and auto-hide/redraw keeps working after Clear → Show.
    target_hwnd: Option<usize>,
}

/// Return true when `text` looks like a keyboard shortcut (e.g. "Ctrl+A",
/// "Alt+Tab", "Win+D"). These are button combos — pasting them does nothing.
fn looks_like_shortcut(text: &str) -> bool {
    let t = text.trim();
    // Any token sequence joined by '+' where at least one token is a known
    // modifier key is almost certainly a keyboard shortcut.
    let modifier_keys = [
        "ctrl", "control", "alt", "shift", "win", "cmd", "super", "meta", "fn", "hyper",
    ];
    let parts: Vec<&str> = t.split('+').map(str::trim).collect();
    if parts.len() < 2 {
        return false;
    }
    parts
        .iter()
        .any(|p| modifier_keys.contains(&p.to_ascii_lowercase().as_str()))
}

/// Slightly enlarge the AI bbox so the hint pointer reads as a thin collar
/// around the element, not a tight fit. The "approximate" feel is conveyed by
/// the dashed bracket styling in `drawHint`, not by size — so this is just a
/// small 1.1× collar with a 60 px minimum to ensure very small bboxes still
/// have visible brackets. Result is clamped to `capture_rect`.
fn inflate_hint_bbox(
    ai_bbox: capture::Rect,
    capture_rect: Option<capture::Rect>,
) -> Option<capture::Rect> {
    let rect = capture_rect?;
    let scale = 1.1f32;
    let new_w = (ai_bbox.width as f32 * scale).max(60.0) as i32;
    let new_h = (ai_bbox.height as f32 * scale).max(60.0) as i32;
    let cx = ai_bbox.x + ai_bbox.width as i32 / 2;
    let cy = ai_bbox.y + ai_bbox.height as i32 / 2;
    let mut x = cx - new_w / 2;
    let mut y = cy - new_h / 2;
    let max_x = rect.x + rect.width as i32;
    let max_y = rect.y + rect.height as i32;
    x = x.max(rect.x).min(max_x.saturating_sub(1));
    y = y.max(rect.y).min(max_y.saturating_sub(1));
    let w = (new_w).min(max_x - x).max(1) as u32;
    let h = (new_h).min(max_y - y).max(1) as u32;
    log::info!(
        "hint fallback: ai_bbox={:?}, inflated to ({}, {}, {}, {})",
        ai_bbox,
        x,
        y,
        w,
        h
    );
    Some(capture::Rect {
        x,
        y,
        width: w,
        height: h,
    })
}

/// Convert the AI's raw `target_bbox` from a step into a screen-coord Rect,
/// applying the per-provider coordinate-system conversion. Returns `None`
/// if the AI didn't return a bbox or we don't have a capture rect.
fn compute_ai_bbox_for_step(
    step: &GuidanceStep,
    capture_rect: Option<capture::Rect>,
    provider: &str,
) -> Option<capture::Rect> {
    let raw = step.target_bbox?;
    let rect = capture_rect?;
    let (ai_w, ai_h) = capture::ai_image_dims(rect.width, rect.height);
    let format = ai::bbox::bbox_format_for_provider(provider);
    // Boundary unwrap: the VdRect (virtual-desktop physical pixels) feeds the
    // overlay + locator options, which consume exactly that space.
    ai::bbox::ai_bbox_to_screen_rect(raw, format, ai_w, ai_h, rect).map(|vd| vd.into_inner())
}

fn overlay_kind_for_step(overlay_type: &OverlayType) -> overlay::OverlayKind {
    match overlay_type {
        OverlayType::Arrow => overlay::OverlayKind::Arrow,
        OverlayType::Highlight | OverlayType::Circle => overlay::OverlayKind::Box,
        OverlayType::Subtitle => overlay::OverlayKind::Subtitle,
        OverlayType::None => overlay::OverlayKind::None,
    }
}

/// The locate half of `execute_step` — target-text validation, pack hints, option
/// building, and the orchestrator call, with no drawing. Extracted (Flow A) so
/// `retry_locate`'s candidate collection can run additional locates without
/// re-drawing or duplicating the primary's work.
#[allow(clippy::too_many_arguments)]
fn locate_for_step(
    step: &GuidanceStep,
    target_hwnd: Option<usize>,
    debug_ocr_path: Option<std::path::PathBuf>,
    ai_bbox: Option<capture::Rect>,
    bbox_decisive: bool,
    avoid_bboxes: Vec<capture::Rect>,
    packs: &packs::PackRegistry,
    context_elements: Option<std::sync::Arc<Vec<locator::ContextElement>>>,
    pre_ocr: Option<(&[u8], capture::Rect)>,
) -> (
    Option<locator::LocateResult>,
    Option<locator::trace::LocateTrace>,
) {
    // Treat an empty/whitespace target_text as "no target". The Ollama schema now
    // *requires* target_text (so small local models can't silently omit it and leave
    // the locator with nothing), and genuine no-target steps (scroll, press a key)
    // emit an empty string — those must not trigger a bogus locate.
    if let Some(text) = step.target_text.as_ref().filter(|t| !t.trim().is_empty()) {
        #[cfg(windows)]
        {
            let (icon_templates, icon_region, icon_authoring_scale) =
                pack_locate_hints(packs, target_hwnd, text);
            // A pack icon for this target ⇒ a known icon-only element → A11y skips its
            // expensive dead-end fallbacks + the bbox probe (it can't name a glyph), runs only
            // a tight matcher pass, then template matching takes over. 150 ms vs 500 ms.
            let icon_target = !icon_templates.is_empty();
            // S.4 skip condition: an icon target with a pack template never uses
            // selection — the template is already the authority for glyphs.
            let selected_element_id = if icon_target {
                None
            } else {
                step.target_element_id
            };
            let opts = locator::orchestrator::LocateOptions {
                role: step
                    .target_role
                    .as_ref()
                    .map(|r| format!("{:?}", r).to_lowercase()),
                nearby_text: step.target_nearby_text.clone(),
                ai_bbox,
                bbox_decisive,
                avoid_bboxes,
                a11y_timeout_ms: if icon_target { 150 } else { 500 },
                min_confidence: 0.5,
                target_hwnd,
                debug_ocr_image_path: debug_ocr_path,
                icon_templates,
                icon_region,
                icon_target,
                icon_authoring_scale,
                context_elements: context_elements.clone(),
                selected_element_id,
            };
            let text_owned = text.clone();
            match locator::orchestrator::locate(&text_owned, &opts, pre_ocr) {
                Ok((result, trace)) => (result, Some(trace)),
                Err(e) => {
                    log::warn!("locate failed for {:?}: {e}", text);
                    (None, None)
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (
                text,
                target_hwnd,
                debug_ocr_path,
                avoid_bboxes,
                &pre_ocr,
                &context_elements,
            );
            (None, None)
        }
    } else {
        (None, None)
    }
}

/// What `execute_step` produced, in order: the verified locate result (None on a
/// miss), the trace, whether the diffuse AI-bbox hint ring was drawn (the frontend's
/// third ✗ Wrong picker state — a visible hint IS rejectable), and the candidate boxes
/// actually drawn (Flow A collection, or a Flow B ambiguity set on a miss — callers
/// populate `GuideResponse.candidates` and arm the state-readback from it; empty when
/// a single pointer was drawn).
type StepOutcome = (
    Option<locator::LocateResult>,
    Option<locator::trace::LocateTrace>,
    bool,
    Vec<capture::Rect>,
);

#[allow(clippy::too_many_arguments)]
fn execute_step(
    app: &AppHandle,
    step: &GuidanceStep,
    target_hwnd: Option<usize>,
    debug_ocr_path: Option<std::path::PathBuf>,
    tracker: &track::WindowTracker,
    last_overlay: &parking_lot::Mutex<Option<LastOverlay>>,
    ai_bbox: Option<capture::Rect>,
    // Whether the answering model is a strong grounder, so its ai_bbox may corroborate
    // a borderline OCR match (`ai::bbox::bbox_is_decisive`). Weak grounders' bboxes get
    // no corroboration vote.
    bbox_decisive: bool,
    // "Wrong spot" memory: every bbox a pointer occupied that the user rejected
    // this step (accumulates across B5 local retries). The locator excludes
    // candidates there so a retry can't repeat a rejected pick. Empty for
    // guide()/next_step; set by retry_locate and the correction path.
    avoid_bboxes: Vec<capture::Rect>,
    capture_rect: Option<capture::Rect>,
    // Native-res OCR image captured at AI-capture time (overlay cleared, before the streamed
    // subtitle). When present the locator's OCR uses it instead of re-capturing — so it never
    // reads our own caption and there's no clear/redraw flicker. None → locator re-captures.
    pre_ocr: Option<(Vec<u8>, capture::Rect)>,
    // Loaded nav-packs — when the focused window matches a pack with icon crops for this
    // target, those crops feed the locator's Pass-3 template matching (Workstream B).
    packs: &packs::PackRegistry,
    // v0.7 Workstream S — the Structured-Context snapshot from the most recent AI
    // capture (the [Screen Elements] list the AI saw). With the step's
    // target_element_id it drives Pass 0.5; None → byte-identical v0.6 behaviour.
    context_elements: Option<std::sync::Arc<Vec<locator::ContextElement>>>,
    // Flow A: a locate result the caller already computed (retry_locate's candidate
    // collection runs the locator itself) — Some(..) skips the locate here entirely.
    precomputed: Option<(
        Option<locator::LocateResult>,
        Option<locator::trace::LocateTrace>,
    )>,
    // Flow A: ranked candidate boxes to draw INSTEAD of the single pointer when 2+
    // (the primary must equal `located`'s bbox). Empty for every normal call.
    candidates_abs: &[capture::Rect],
) -> Result<StepOutcome, String> {
    let (located, trace) = match precomputed {
        Some(pre) => pre,
        None => locate_for_step(
            step,
            target_hwnd,
            debug_ocr_path,
            ai_bbox,
            bbox_decisive,
            avoid_bboxes,
            packs,
            context_elements,
            pre_ocr.as_ref().map(|(p, r)| (p.as_slice(), *r)),
        ),
    };

    let mut kind = overlay_kind_for_step(&step.overlay_type);
    let mut bbox = located.as_ref().map(|r| r.bbox);

    // When the locator found a target, always show at least an arrow — never
    // suppress the pointer just because the model returned overlay_type:none.
    if located.is_some() && matches!(kind, overlay::OverlayKind::None) {
        kind = overlay::OverlayKind::Arrow;
    }

    // Hint fallback: when A11y *and* OCR both missed but the AI returned a
    // target_bbox, emit a diffuse highlight at the inflated AI bbox so the
    // user gets a "search this region" cue instead of nothing. The
    // "pointer unavailable" caption in the panel still tells them it's
    // approximate.
    //
    // Gated on bbox_decisive (audit 2026-07-12 C6): a distrusted model's bbox is,
    // by the same definition that keeps it from corroborating an OCR match, an
    // unreliable region — drawing "look here" at it points the user at a spot the
    // model likely got wrong, which is worse than an honest "pointer unavailable"
    // with no region. Trusted grounders (the default for all but the weak free
    // chain) still get the hint.
    // Flow B: a pass recorded a KNOWN tie during the run. The firing rule
    // (candidates::flow_b_boxes) is information-based, not miss-based: the set fires
    // on a miss, AND on an OCR hit that lands INSIDE a set member — such a hit isn't
    // new information, it merely resolved the recorded ground-truth tie with weaker
    // evidence (live: PPT's two "Click to add text" placeholders — OCR picked one by
    // AI-bbox proximity; the honest outcome is both boxes with that pick as ①). A hit
    // OUTSIDE the set, or from any stronger pass, is new information and stands alone
    // (the Save/QAT case — A11y's button beat the adapter's 4 prose occurrences).
    let flow_b_set: Vec<capture::Rect> = if candidates_abs.is_empty() {
        let final_is_hit_ocr = trace
            .as_ref()
            .map(|t| {
                matches!(
                    t.final_decision,
                    locator::trace::FinalDecision::HitOcr
                )
            })
            .unwrap_or(false);
        locator::candidates::flow_b_boxes(
            located.as_ref().map(|r| &r.bbox),
            final_is_hit_ocr,
            trace
                .as_ref()
                .and_then(|t| t.ambiguity_set.as_ref())
                .map(|s| s.boxes.as_slice()),
        )
    } else {
        Vec::new()
    };
    // Flow A (explicit collection from retry_locate) takes precedence; either way the
    // user is never asked to choose — their next real click resolves it (readback).
    let shown_candidates: Vec<capture::Rect> = if candidates_abs.len() >= 2 {
        candidates_abs.to_vec()
    } else {
        flow_b_set
    };
    let candidate_mode = shown_candidates.len() >= 2;
    if candidate_mode {
        kind = overlay::OverlayKind::Candidates;
        // Flow B has no verified primary — ① anchors the overlay/tracker instead.
        if bbox.is_none() {
            bbox = Some(shown_candidates[0]);
        }
    }

    let mut hint_shown = false;
    if !candidate_mode
        && located.is_none()
        && bbox_decisive
        && step
            .target_text
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty())
    {
        if let Some(ai) = ai_bbox {
            if let Some(hint) = inflate_hint_bbox(ai, capture_rect) {
                kind = overlay::OverlayKind::Hint;
                bbox = Some(hint);
                hint_shown = true;
            }
        }
    }

    let text_for_overlay = Some(step.instruction.clone());

    // Is the target area visible right now? Drives the initial draw; the window tracker
    // (started below) then keeps it in sync — auto-hiding the pointer if the target gets
    // covered by another app and auto-redrawing it when the target is visible again.
    // When hidden we don't draw the pointer onto the wrong window, and we tell the UI so
    // it can offer a re-analyse.
    let visible = match (target_hwnd, bbox) {
        (Some(th), Some(b)) => {
            capture::target_visible_in_rect(b.x, b.y, b.width as i32, b.height as i32, th)
        }
        _ => true,
    };

    // Persist for restore_overlay — the AI bbox alone is a valid state too. Candidate
    // boxes are transient by design (resolved by the user's next click), so a later
    // restore brings back only the primary as a plain box, not the whole set.
    if !matches!(kind, overlay::OverlayKind::None) || ai_bbox.is_some() {
        *last_overlay.lock() = Some(LastOverlay {
            kind: if candidate_mode {
                overlay::OverlayKind::Box
            } else {
                kind
            },
            bbox,
            text: text_for_overlay.clone(),
            ai_bbox,
            target_hwnd,
        });
    }
    if visible {
        match overlay::make_update_full(
            kind,
            bbox,
            text_for_overlay.clone(),
            ai_bbox,
            if candidate_mode {
                shown_candidates.clone()
            } else {
                Vec::new()
            },
        ) {
            Ok(update) => {
                if let Err(e) = overlay::emit_update(app, update) {
                    log::warn!("overlay emit failed: {e}");
                }
            }
            Err(e) => log::warn!("overlay make_update failed: {e}"),
        }
    } else {
        // Target covered by another app — hide the pointer and tell the UI so it can
        // offer a re-analyse. The tracker auto-redraws it the moment the target shows.
        if let Ok(update) =
            overlay::make_update_with_ai_bbox(overlay::OverlayKind::None, None, None, None)
        {
            let _ = overlay::emit_update(app, update);
        }
        let _ = app.emit("pointer_occluded", ());
    }

    // E.4 — Clipboard: if the AI supplied text to copy, write it now so
    // it's in the clipboard before the user acts on the instruction.
    // Guard: skip values that look like keyboard shortcuts (e.g. "Ctrl+A",
    // "Alt+Tab") — pressing a shortcut cannot be assisted by clipboard paste.
    if let Some(ref clip_text) = step.clipboard {
        if !looks_like_shortcut(clip_text) {
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(clip_text.clone())) {
                Ok(()) => log::info!("clipboard: wrote {} chars", clip_text.len()),
                Err(e) => log::warn!("clipboard write failed: {e}"),
            }
        } else {
            log::info!("clipboard: skipped shortcut-like value '{clip_text}'");
        }
    }

    // Start tracking the window so the overlay follows it and auto-hides/redraws with
    // the target's visibility — anchored to the target app (target_hwnd) so the pointer
    // only ever moves with the right window, never another app overlapping the spot.
    if let Some(ref b) = bbox {
        tracker.start_with_candidates(
            b,
            kind,
            text_for_overlay,
            app.clone(),
            target_hwnd,
            visible,
            if candidate_mode { &shown_candidates } else { &[] },
        );
    } else {
        tracker.clear();
    }

    Ok((located, trace, hint_shown, shown_candidates))
}

// ---------- Autopilot screen-change polling + stale-response detection ----------

/// Hamming distance (out of 64) at which Autopilot considers the screen to have
/// "changed" relative to the state the AI last gave guidance for.
/// 10/64 ≈ 16% — high enough to ignore JPEG noise, blinking carets, small live
/// content (Slack typing indicators, etc.); low enough to catch a dialog
/// opening, page navigation, or a new view.
const AUTOPILOT_CHANGE_THRESHOLD: u32 = 10;

/// Hamming distance at which an AI response is considered "stale" — i.e. the
/// screen drifted enough during the 5–90 s of AI thinking that the rendered
/// guidance may no longer apply. Set higher than the autopilot threshold so
/// the interruptive banner only appears on clearly substantial drift.
const STALE_RESPONSE_THRESHOLD: u32 = 13;

fn ahash_from_luma8(luma: &image::ImageBuffer<image::Luma<u8>, Vec<u8>>) -> u64 {
    let thumb = image::imageops::resize(luma, 8, 8, image::imageops::FilterType::Triangle);
    let pixels: Vec<u8> = thumb.pixels().map(|p| p.0[0]).collect();
    let mean: u64 = pixels.iter().map(|&v| v as u64).sum::<u64>() / pixels.len().max(1) as u64;
    let mut hash: u64 = 0;
    for (i, &v) in pixels.iter().enumerate() {
        if (v as u64) > mean {
            hash |= 1u64 << i;
        }
    }
    hash
}

fn ahash_of_jpeg(jpeg: &[u8]) -> Option<u64> {
    let img = image::load_from_memory(jpeg).ok()?;
    Some(ahash_from_luma8(&img.to_luma8()))
}

/// Capture the guidance target raw + compute aHash in one step. Used by the autopilot
/// polling loop, the post-AI-call baseline anchor, and stale-response detection. Skipping
/// the JPEG roundtrip used elsewhere saves ~10 ms per call — meaningful at 2 captures/sec
/// while autopilot is on.
///
/// `target_hwnd` (audit 2026-07-12 C5): the window guidance is anchored to (a pin, or the
/// last-guided foreground window). When set, hash THAT window — so autopilot reacts to
/// changes in the app being guided, not whatever the user alt-tabbed to, and the
/// stale-response check compares like-for-like against `guide()`'s own target capture. A
/// closed/minimized target (or `None`, e.g. full-screen mode) falls back to the foreground
/// window, matching the pre-C5 behaviour.
fn ahash_of_screen(target_hwnd: Option<usize>) -> Option<u64> {
    let exclude = capture::get_panel_rects();
    let img = match target_hwnd {
        Some(h) => capture::recapture_window_raw(h, &exclude)
            .map(|(img, _rect)| img)
            .or_else(|_| capture::capture_active_window_raw(&exclude).map(|(img, _, _)| img))
            .ok()?,
        None => capture::capture_active_window_raw(&exclude).map(|(img, _, _)| img).ok()?,
    };
    let luma = image::imageops::grayscale(&img);
    Some(ahash_from_luma8(&luma))
}

/// The window guidance is currently anchored to — a pin if set, else the last-guided
/// target window. `None` in full-screen mode (no single window) so hashing falls back
/// to the foreground. This is the reference `ahash_of_screen` should hash so autopilot
/// and stale-detection watch the guided app, not whatever is foreground (C5).
fn guidance_target_hwnd(state: &AppState) -> Option<usize> {
    let g = state.guidance.lock();
    if g.full_screen_mode {
        return None;
    }
    g.pinned_hwnd.or(g.target_hwnd)
}

fn hamming64(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Stale-response check: compare the pre-AI-call screen hash against the
/// post-response one and emit `ai_response_stale` when the drift crosses the
/// threshold. The drift is ALWAYS logged (hits and near-misses) so a "banner
/// shows every time" report is diagnosable from the log — the 2026-07-17
/// PowerPoint session needed offline image forensics because nothing recorded
/// the measured drift.
fn emit_stale_if_drifted(app: &tauri::AppHandle, pre_hash: Option<u64>, post_hash: Option<u64>) {
    if let (Some(p), Some(q)) = (pre_hash, post_hash) {
        let drift = hamming64(p, q);
        let stale = drift >= STALE_RESPONSE_THRESHOLD;
        log::info!("[stale] drift={drift}/64 threshold={STALE_RESPONSE_THRESHOLD} emitting={stale}");
        if stale {
            let _ = app.emit("ai_response_stale", serde_json::json!({ "drift": drift }));
        }
    }
}

/// Capture a fresh active-window hash off the blocking pool and store it as
/// the Autopilot baseline. Called at the end of every AI/local guidance event
/// (`guide`, `next_step`, `send_correction`) so the baseline always reflects
/// the screen the user is being directed against, not a drifting 500 ms-old
/// sample. Returns the captured hash for the caller (used by stale detection).
async fn anchor_autopilot_baseline(state: &AppState) -> Option<u64> {
    let target = guidance_target_hwnd(state);
    let h = tokio::task::spawn_blocking(move || ahash_of_screen(target))
        .await
        .ok()
        .flatten();
    *state.screen_hash.lock() = h;
    h
}

/// Called by the frontend Autopilot polling loop every 500 ms while autopilot
/// is on. Compares the current screen against the *anchored* baseline (set
/// when the AI last gave guidance) — does NOT update the baseline. Without
/// the anchor, the previous design compared each poll against the previous
/// poll, which made the baseline drift with the screen and only caught sudden
/// changes within a 500 ms window.
///
/// The screen capture happens on the blocking pool so it never ties up a tokio
/// async worker — important because this fires twice per second and an in-flight
/// AI streaming request shares those workers.
#[tauri::command]
async fn check_screen_changed(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let target = guidance_target_hwnd(&state);
    let hash = tokio::task::spawn_blocking(move || ahash_of_screen(target))
        .await
        .ok()
        .flatten();
    let prev = *state.screen_hash.lock();
    let changed = match (hash, prev) {
        (Some(h), Some(p)) => hamming64(p, h) >= AUTOPILOT_CHANGE_THRESHOLD,
        _ => false,
    };
    Ok(serde_json::json!({ "changed": changed }))
}

/// Append one AI-call timing row to `%APPDATA%\com.navisual.app\model_timings.csv`
/// so per-model latency can be pulled into a spreadsheet for comparison. Records
/// the pure AI round-trip (capture + locate excluded). Best-effort — write
/// failures are logged and ignored. `elapsed_ms` is the wall-clock AI time;
/// `model` for the managed provider is the client-sent hint (the relay may
/// override server-side).
fn log_model_timing(
    app: &AppHandle,
    provider: &str,
    model: &str,
    elapsed_ms: u128,
    ok: bool,
    steps: usize,
) {
    use std::io::Write;
    let Ok(dir) = app.path().app_local_data_dir() else {
        return;
    };
    let path = dir.join("model_timings.csv");
    let new_file = !path.exists();
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let status = if ok { "ok" } else { "error" };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if new_file {
                let _ = writeln!(f, "timestamp,provider,model,elapsed_ms,status,steps");
            }
            let _ = writeln!(f, "{ts},{provider},{model},{elapsed_ms},{status},{steps}");
        }
        Err(e) => log::warn!("model_timings.csv write failed: {e}"),
    }
}

/// Optionally append a trace to the rolling JSONL log. Written when the locate-log
/// toggle OR training capture is on (a training triple needs the verified locate
/// outcome — it's the label); `training` additionally switches rotation to
/// archive-not-delete (see `jsonl_log`).
fn maybe_log_trace(
    app: &AppHandle,
    trace: &locator::trace::LocateTrace,
    log_enabled: bool,
    training: bool,
) {
    if !log_enabled && !training {
        return;
    }
    if let Ok(dir) = app.path().app_local_data_dir() {
        let path = dir.join("locate_log.jsonl");
        if let Err(e) = locator::trace::append_jsonl(&path, trace, training) {
            log::warn!("locate_log.jsonl write failed: {e}");
        }
    }
}

/// Flow A — resolve any pending candidate boxes via state readback: which candidate
/// did the user's real click land on? Called at the start of every backend guidance
/// entry (guide / next_step / send_correction / retry_locate) — by then the user has
/// acted, and the app's own state (Word caret, PowerPoint shape selection, UIA focus)
/// still carries the answer. The resolved pick is banked to the local training mirror
/// as a ground-truth label; an unresolvable or expired pending is dropped honestly.
/// `escalation`: the resolving event is itself another ✗ Wrong (correction /
/// retry_locate) — an explicit rejection of the shown set, so the readback is never
/// trusted as a "chosen" label (the caret may sit in a candidate the user clicked
/// BEFORE deciding it was wrong). Neutral progress events (guide / next_step) pass
/// false and let the baseline comparison separate a real pick from stale state.
async fn resolve_pending_candidates(app: &AppHandle, state: &AppState, escalation: bool) {
    let Some(pending) = state.guidance.lock().pending_candidates.take() else {
        return;
    };
    let age_ms = chrono::Utc::now().timestamp_millis() - pending.armed_ms;
    if age_ms > locator::candidates::PENDING_EXPIRY_MS {
        log::info!("[candidates] pending expired unresolved ({age_ms} ms old)");
        return;
    }
    let training_enabled = state
        .ai_router
        .lock()
        .await
        .config
        .training_capture_enabled;
    let hwnd = pending.target_hwnd;
    let resolved = tokio::task::spawn_blocking(move || {
        locator::candidates::read_acted_rect(hwnd)
    })
    .await
    .ok()
    .flatten();
    let (resolved_bbox, method) = match &resolved {
        Some((rect, method)) => (Some(*rect), *method),
        None => (None, "no-readback"),
    };
    let outcome = locator::candidates::resolution_outcome(
        escalation,
        pending.baseline.as_ref(),
        resolved_bbox.as_ref(),
        &pending.boxes,
    );
    let chosen = match outcome {
        locator::candidates::Outcome::Chosen(i) => Some(i),
        _ => None,
    };
    log::info!(
        "[candidates] resolution: shown={} outcome={} chosen={:?} method={} age={}ms",
        pending.boxes.len(),
        outcome.as_str(),
        chosen,
        method,
        age_ms
    );
    if training_enabled {
        if let Ok(dir) = app.path().app_local_data_dir() {
            let row = serde_json::json!({
                "kind": "candidate_resolved",
                "ts_ms": chrono::Utc::now().timestamp_millis(),
                "request_id": pending.request_id,
                "target_text": pending.target_text,
                "shown": pending.boxes.len(),
                "boxes": pending.boxes,
                "outcome": outcome.as_str(),
                "chosen_index": chosen,
                "method": method,
                "baseline_bbox": pending.baseline,
                "resolved_bbox": resolved_bbox,
            });
            let path = dir.join("training").join("feedback.jsonl");
            if let Err(e) = jsonl_log::append_line(&path, &row.to_string(), true) {
                log::warn!("training candidate row write failed: {e}");
            }
        }
    }
}

/// Arm the candidate state-readback after boxes were shown (Flow A collection or a
/// Flow B ambiguity set): take the app-state baseline NOW so resolution can tell a
/// genuine post-arm click from stale state. No-op under 2 boxes.
async fn arm_candidates_if_shown(
    state: &AppState,
    request_id: Option<String>,
    target_text: Option<&str>,
    boxes: &[capture::Rect],
    hwnd: Option<usize>,
) {
    if boxes.len() < 2 {
        return;
    }
    let baseline_hwnd = hwnd;
    let baseline = tokio::task::spawn_blocking(move || {
        locator::candidates::read_acted_rect(baseline_hwnd).map(|(r, _)| r)
    })
    .await
    .ok()
    .flatten();
    let mut g = state.guidance.lock();
    g.pending_candidates = Some(locator::candidates::PendingCandidates {
        request_id,
        target_text: target_text.unwrap_or_default().to_string(),
        boxes: boxes.to_vec(),
        target_hwnd: hwnd,
        armed_ms: chrono::Utc::now().timestamp_millis(),
        baseline,
    });
}

/// L1 app-state block for the prompt, bounded so a wedged script channel can never
/// stall a capture (the channel's own connect/read timeouts are ~200/700 ms; this is
/// the outer safety net, mirroring `enumerate_context_snapshot_bounded`'s contract).
fn app_state_snapshot(hwnd: Option<usize>) -> Option<String> {
    let started = std::time::Instant::now();
    let block = locator::adapters::app_state_block(hwnd)?;
    log::info!(
        "[app_state] block collected in {} ms ({} chars)",
        started.elapsed().as_millis(),
        block.len()
    );
    Some(block)
}

/// Exe filename stem for `LocateTrace.app_name` — PII-free app identity (see the field's
/// doc comment for why this is the exe stem and not the resolved display title).
fn trace_app_name(hwnd_opt: Option<usize>) -> Option<String> {
    let info = capture::get_window_info_for_hwnd(hwnd_opt?)?;
    let stem = std::path::Path::new(&info.exe_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&info.exe_name)
        .to_string();
    Some(stem)
}

/// Developer option — append the exact prompt sent to the AI to a single running
/// `prompt_log.jsonl`. Written when the prompt-log toggle OR training capture is on
/// (training triples need the prompt; the toggle alone shouldn't have to be
/// remembered separately). Covers every call site (guide/reply/requery/correction),
/// unlike `debug_screenshot_enabled`'s per-call `prompt_<ts>.txt` dumps (guide()
/// only). System prompt is static and never logged — see `ai/prompts.rs`. With
/// training capture on, the entry also carries the parsed response, the
/// request-id join keys, and archive-not-delete rotation (prompt_log.rs docs).
fn maybe_log_prompt(
    app: &AppHandle,
    log_enabled: bool,
    training: bool,
    fields: prompt_log::PromptLogFields<'_>,
) {
    if !log_enabled && !training {
        return;
    }
    let entry = prompt_log::PromptLogEntry::new(fields);
    if let Ok(dir) = app.path().app_local_data_dir() {
        let path = dir.join("prompt_log.jsonl");
        if let Err(e) = prompt_log::append_jsonl(&path, &entry, training) {
            log::warn!("prompt_log.jsonl write failed: {e}");
        }
    }
}

/// Training capture (llm-finetuning-eval.md §5b): persist the exact AI-sent JPEG as
/// `training/shot_<request_id>.jpg`. Returns the app-data-relative filename for the
/// prompt-log entry's `screenshot_file`. The `training/` dir is deliberately exempt
/// from `cleanup_old_debug_artifacts` — it only exists when the toggle is on, and
/// accumulation is its purpose.
fn save_training_shot(base_dir: Option<&std::path::Path>, request_id: &str, jpeg: &[u8]) -> Option<String> {
    let dir = base_dir?.join("training");
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!("shot_{request_id}.jpg");
    std::fs::write(dir.join(&name), jpeg).ok()?;
    Some(format!("training/{name}"))
}

/// Nav-Pack prompt injection (Workstream C): if a loaded pack's `window_title_pattern`
/// matches the focused window, return its formatted guidance + shortcut block to append to
/// the prompt; empty string otherwise. Keeps the lookup out of the two guidance call sites.
fn active_pack_context(registry: &packs::PackRegistry, hwnd: usize) -> String {
    if registry.is_empty() {
        return String::new();
    }
    let title = capture::get_window_title(hwnd);
    match registry.get_active_pack(&title) {
        Some(pack) => {
            log::debug!("nav-pack '{}' active for '{}'", pack.manifest.id, title);
            ai::prompts::pack_context_block(
                &pack.manifest.target_app,
                &pack.manifest.system_prompt_injection,
                &pack.manifest.shortcuts,
            )
        }
        None => String::new(),
    }
}

/// Pass-3 icon templates: `(icon_name, image bytes)` pairs.
type IconTemplates = Vec<(String, Vec<u8>)>;

/// Workstream B: pack-derived locate hints for `target_text` on the focused window —
/// candidate icon crops `(name, bytes)` for Pass-3 template matching, plus the `element_hints`
/// search region (fractional rect) that makes matching independent of the AI bbox. All empty/
/// None (the common case) when there's no focused window, no active pack, or no match — so
/// Pass 3 stays a no-op. Reads icon files lazily here; only reached on a real locate.
fn pack_locate_hints(
    registry: &packs::PackRegistry,
    hwnd: Option<usize>,
    target_text: &str,
) -> (IconTemplates, Option<[f32; 4]>, f32) {
    let Some(hwnd) = hwnd.filter(|h| *h != 0) else {
        return (Vec::new(), None, 1.0);
    };
    if registry.is_empty() {
        return (Vec::new(), None, 1.0);
    }
    let title = capture::get_window_title(hwnd);
    let Some(pack) = registry.get_active_pack(&title) else {
        return (Vec::new(), None, 1.0);
    };
    let icons = pack
        .candidate_icons(target_text)
        .into_iter()
        .filter_map(|a| std::fs::read(&a.path).ok().map(|bytes| (a.stem.clone(), bytes)))
        .collect();
    (icons, pack.region_hint_for(target_text), pack.authoring_scale())
}

/// Workstream P (v0.7) — curated starter tasks from the nav-pack matching `hwnd`'s
/// window, for the cold-start prefill dropdown. Empty when no pack matches or the
/// pack has none; the frontend falls back to its generic "Show me around {app}".
#[tauri::command]
fn get_pack_starters(state: State<'_, AppState>, hwnd: u64) -> Vec<String> {
    #[cfg(windows)]
    {
        if hwnd == 0 || state.packs.is_empty() {
            return Vec::new();
        }
        let title = capture::get_window_title(hwnd as usize);
        state
            .packs
            .get_active_pack(&title)
            .map(|pack| pack.manifest.starter_tasks.iter().take(3).cloned().collect())
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let _ = (state, hwnd);
        Vec::new()
    }
}

/// Deployment status of the pack-shipped Blender add-on (the script-channel bridge):
/// which Blender installs exist, whether each has the add-on, and whether any is older
/// than the version the pack ships. Drives the panel's install prompt.
#[tauri::command]
fn blender_addon_status(
    state: State<'_, AppState>,
    hwnd: u64,
) -> packs::addon_install::AddonStatus {
    let dir = state.packs.get_by_id("blender").map(|p| p.dir.clone());
    let version = blender_target_version(hwnd);
    packs::addon_install::status(dir.as_deref(), version.as_deref())
}

/// Config-folder version ("5.1") of the Blender the prompt is about. Scoping to the
/// TARGET is what keeps the offer about the Blender in front of the user.
///
/// Two sources, because neither alone covers every version: the window title carries
/// it on 4.x+ ("… - Blender 5.1.2"), while **Blender ≤3.x titles its window just
/// "Blender"** — for those, the `<major>.<minor>` resource folder beside `blender.exe`
/// is authoritative (live 2026-07-19: title-only made 3.6 permanently silent).
fn blender_target_version(hwnd: u64) -> Option<String> {
    #[cfg(windows)]
    {
        if hwnd == 0 {
            return None;
        }
        let hwnd = hwnd as usize;
        let from_title =
            packs::addon_install::config_version_from_title(&capture::get_window_title(hwnd));
        from_title.or_else(|| {
            locator::adapters::window_exe_path(hwnd)
                .as_deref()
                .and_then(packs::addon_install::config_version_from_exe)
        })
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

/// Copy the pack's add-on into every detected Blender config directory. Explicitly
/// user-initiated (writing into another application's config dir is not something to do
/// silently), and deliberately does NOT enable it — the Add-ons checkbox stays the
/// consent gate. Returns what the user must do next.
#[tauri::command]
fn install_blender_addon(
    state: State<'_, AppState>,
    hwnd: u64,
) -> packs::addon_install::InstallResult {
    let dir = state.packs.get_by_id("blender").map(|p| p.dir.clone());
    let version = blender_target_version(hwnd);
    let result = packs::addon_install::install(dir.as_deref(), version.as_deref());
    log::info!(
        "[blender-addon] install (target {version:?}) → {:?} (errors: {:?}, needs_enable={})",
        result.installed,
        result.errors,
        result.needs_enable
    );
    result
}

/// Must match Overlay.svelte's `APP_BOUNDARY_DURATION_MS` — no constant is
/// shared across the Rust/Svelte boundary, so keep the two in sync by hand.
#[cfg(windows)]
const APP_BOUNDARY_DURATION_MS: u64 = 3_000;

/// Phase 0.2: emit the animated "shared app boundary" overlay and the
/// `app_changed` event so the panel chip stays in sync with what's captured.
#[cfg(windows)]
fn announce_shared_app(app: &AppHandle, hwnd_raw: Option<usize>, draw_boundary: bool) {
    let info = hwnd_raw.and_then(capture::get_window_info_for_hwnd);
    if let Some(info) = info {
        let target_hwnd = info.hwnd;
        let payload = SharedAppInfoPayload {
            hwnd: info.hwnd as u64,
            rect: info.rect,
            app_name: info.app_name.clone(),
            exe_name: info.exe_name.clone(),
        };
        let _ = app.emit("app_changed", Some(&payload));

        if draw_boundary {
            // Animated boundary box.
            if let Ok(update) = overlay::make_update(
                overlay::OverlayKind::AppBoundary,
                Some(info.rect),
                Some(info.app_name),
            ) {
                if let Err(e) = overlay::emit_update(app, update) {
                    log::debug!("app_boundary emit failed: {e}");
                }
            }
            track::watch_boundary(
                target_hwnd,
                app.clone(),
                Duration::from_millis(APP_BOUNDARY_DURATION_MS),
            );
        }
    } else {
        let _ = app.emit("app_changed", Option::<SharedAppInfoPayload>::None);
        if draw_boundary {
            if let Ok(update) = overlay::make_update(
                overlay::OverlayKind::AppBoundary,
                None,
                None,
            ) {
                if let Err(e) = overlay::emit_update(app, update) {
                    log::debug!("app_boundary emit failed: {e}");
                }
            }
        }
    }
}

#[cfg(windows)]
pub fn refresh_active_window(app: &AppHandle) {
    let state = app.state::<AppState>();
    let active_info = capture::get_active_window_info();
    let (announce_hwnd, changed) = {
        let mut g = state.guidance.lock();
        if g.pinned_hwnd.is_none() {
            g.target_hwnd = active_info.as_ref().map(|info| info.hwnd);
        }
        let hwnd = g.pinned_hwnd.or(g.target_hwnd);
        let changed = hwnd != g.last_announced_hwnd;
        g.last_announced_hwnd = hwnd;
        (hwnd, changed)
    };
    // Flash only when the followed target actually changed (a new app took focus),
    // not on every passive refresh (z-order shuffle, object update, same app
    // re-settling) — see commit 1601f40 for why a blanket flash-on-every-refresh
    // was reverted.
    announce_shared_app(app, announce_hwnd, changed);
}

#[cfg(not(windows))]
fn announce_shared_app(_app: &AppHandle, _hwnd_raw: Option<usize>, _draw_boundary: bool) {}

#[cfg(not(windows))]
pub fn refresh_active_window(_app: &AppHandle) {}

#[derive(serde::Serialize)]
struct GuideResponse {
    ok: bool,
    session_id: String,
    /// Training-data join key for THIS AI request (llm-finetuning-eval.md §5b) —
    /// the frontend echoes it back on feedback rows so worked/wrong signals join
    /// the request's prompt/response/screenshot/trace records. `None` when no AI
    /// call happened (local advance, pre-capture errors).
    request_id: Option<String>,
    steps: Vec<GuidanceStep>,
    step_index: usize,
    instruction: String,
    located: Option<locator::LocateResult>,
    needs_input: bool,
    provider: String,
    /// The model that actually handled this request. For managed this is the concrete
    /// model OpenRouter routed to (the relay sends the `openrouter/free` router); for
    /// other providers it's the configured model. Surfaced in the debug drawer + logged.
    model: Option<String>,
    /// Input / output token counts for this AI call (None on local-advance / on errors
    /// with no AI call). Shown in the debug Response-info drawer.
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    error: Option<String>,
    /// Path to the debug screenshot saved for this request (None when disabled).
    debug_screenshot_path: Option<String>,
    /// Tiny thumbnail (160×90, JPEG q=40, base64) of the screenshot sent to AI.
    /// Separate from debug screenshots — shown in the chat history bubble.
    chat_thumb_b64: Option<String>,
    /// Locator trace for the current step (Phase 0.1).
    /// `None` when the step has no target_text or when the locator wasn't run.
    locate_trace: Option<locator::trace::LocateTrace>,
    /// AI-returned bounding box in screen (virtual desktop) coordinates.
    /// Developer "Show AI bbox" overlay reads this.
    ai_bbox: Option<capture::Rect>,
    /// Workstream P (v0.7): up to 3 AI-suggested next tasks (already lax-parsed and
    /// capped). Empty when the model offered none, on local advances/errors, or when
    /// the `task_suggestions` setting is off. Display-only — the frontend prefills
    /// the task box (selected) and never auto-submits.
    suggested_tasks: Vec<String>,
    /// The diffuse AI-bbox hint ring was drawn for this step (locator missed, trusted
    /// bbox present). Third picker state: a visible hint is rejectable — "Wrong spot"
    /// on it is a model-grounding-fault label (located=false on the feedback row
    /// distinguishes it from a real-pointer rejection), routed straight to the AI
    /// (the locator already ran everything and missed — nothing local to retry).
    hint_shown: bool,
    /// Flow A (candidate hints): the ranked candidate boxes shown after a "Wrong
    /// spot" local retry found 2+ distinct possibilities (virtual-desktop pixels,
    /// strongest first; `located` is the primary). Empty everywhere else. The
    /// frontend adjusts its copy and adds ALL boxes to the rejected-spot memory
    /// so a further "Wrong spot" escalates to the AI avoiding every shown box.
    candidates: Vec<capture::Rect>,
}

#[derive(serde::Serialize, Clone)]
struct StreamChunkPayload {
    delta: String,
    /// How many steps have STARTED streaming in the partial response so far
    /// (streaming::count_streamed_steps). The panel shows "Step 1 of ~N" live while
    /// the response is still forming, instead of discarding that signal until
    /// completion. Monotonic within one response.
    steps_seen: usize,
}

/// Phase 0.2: payload for "Shared: <App>" header chip and `app_changed` event.
#[derive(serde::Serialize, Clone)]
struct SharedAppInfoPayload {
    hwnd: u64,
    rect: capture::Rect,
    app_name: String,
    exe_name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct SettingsPayload {
    api_provider: String,
    anthropic_api_key: String,
    anthropic_model: String,
    anthropic_fast_model: String,
    gemini_api_key: String,
    gemini_model: String,
    gemini_fast_model: String,
    ollama_base_url: String,
    ollama_model: String,
    openai_api_key: String,
    openai_model: String,
    deepseek_api_key: String,
    deepseek_model: String,
    qwen_api_key: String,
    qwen_model: String,
    qwen_base_url: String,
    custom_api_key: String,
    custom_model: String,
    custom_base_url: String,
    #[serde(default)]
    managed_tier: String,
    overlay_color: String,
    overlay_thickness: u32,
    subtitle_enabled: bool,
    auto_advance: bool,
    tts_enabled: bool,
    tts_voice: String,
    voice_input_enabled: bool,
    voice_language: String,
    hotkey_next: String,
    hotkey_wrong: String,
    hotkey_pause: String,
    hotkey_icon: String,
    hotkey_talk: String,
    debug_screenshot_enabled: bool,
    debug_show_response_info: bool,
    debug_locate_trace_enabled: bool,
    debug_locate_log_file_enabled: bool,
    debug_prompt_log_file_enabled: bool,
    /// Training-data banking (llm-finetuning-eval.md §5b) — see Config field docs.
    #[serde(default)]
    training_capture_enabled: bool,
    /// v0.7 Workstream P — prefilled task suggestions (cold-start prefill + AI
    /// suggested_tasks). Screen Guide toggle; default on (display-only, no risk).
    #[serde(default = "default_true_setting")]
    task_suggestions: bool,
    /// Draw the AI-returned target_bbox on the overlay (developer / comparison).
    /// Front-end only — backend always emits ai_bbox in OverlayUpdate; the
    /// overlay renderer reads this flag (from `overlay:theme`) to decide
    /// whether to draw the cyan dashed box.
    #[serde(default)]
    debug_show_ai_bbox: bool,
    /// Read-only — true when the process was launched with NAVISUAL_DEV=true.
    /// Frontend uses this to show/hide the Developer settings tab. Never
    /// written by save_settings (it's deserialized but ignored on the way in).
    #[serde(default)]
    developer_mode: bool,
}

/// Serde default for settings that are ON unless explicitly disabled.
fn default_true_setting() -> bool {
    true
}

#[derive(serde::Serialize, Clone)]
struct OverlayThemePayload {
    color: String,
    thickness: u32,
    subtitle_enabled: bool,
}

fn update_env_file(path: &std::path::Path, updates: &[(&str, &str)]) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(|s| s.to_string()).collect();

    'outer: for (key, value) in updates {
        let prefix = format!("{}=", key);
        for line in &mut lines {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if !trimmed.starts_with('#') && trimmed.starts_with(&prefix) {
                *line = format!("{}={}", key, value);
                continue 'outer;
            }
        }
        lines.push(format!("{}={}", key, value));
    }

    let mut content = lines.join("\n");
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &content).map_err(|e| format!(".env write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!(".env rename: {e}"))?;
    Ok(())
}

/// Encode a 160×90 thumbnail of the AI image as base64 — for the inline chat
/// bubble in the panel. Pure in-memory: nothing is written to disk. The full
/// JPEG is held in `AppState::chat_full_jpeg` for the lightbox.
fn make_chat_thumbnail(jpeg_bytes: &[u8]) -> Option<String> {
    let img = image::load_from_memory(jpeg_bytes).ok()?;
    let thumb = img.resize(160, 90, image::imageops::FilterType::Nearest);
    let mut buf = Vec::new();
    {
        use image::codecs::jpeg::JpegEncoder;
        let mut enc = JpegEncoder::new_with_quality(&mut buf, 40);
        enc.encode_image(&thumb).ok()?;
    }
    Some(capture::to_base64(&buf))
}

/// Return the full-resolution chat screenshot as base64 (for the lightbox).
/// Read from in-memory state — never touched disk. Returns None if no
/// screenshot has been captured yet this session.
#[tauri::command]
fn get_chat_full_screenshot(state: State<'_, AppState>) -> Option<String> {
    let bytes = state.chat_full_jpeg.lock().clone()?;
    Some(capture::to_base64(&bytes))
}

/// Move any plaintext BYOK API key still sitting in `.env` into the Windows
/// Credential Manager, replacing its line with the sentinel (see credvault.rs).
/// Idempotent: sentinel/empty lines are skipped, so this is a no-op after the
/// first migration; a key the user hand-pastes into `.env` later simply gets
/// migrated on the following launch. A failed vault write leaves the plaintext
/// line untouched (the key must never be lost).
fn migrate_env_secrets_to_credvault(env_path: &std::path::Path) {
    let Ok(existing) = std::fs::read_to_string(env_path) else {
        return; // no .env yet — nothing to migrate
    };
    let mut sentinel_updates: Vec<(&str, &str)> = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        for &key in credvault::SECRET_KEYS {
            let prefix = format!("{key}=");
            if let Some(value) = trimmed.strip_prefix(&prefix) {
                let value = value.trim();
                if !value.is_empty()
                    && value != credvault::SENTINEL
                    && credvault::store(key, value)
                {
                    sentinel_updates.push((key, credvault::SENTINEL));
                    log::info!("[credvault] migrated {key} from .env to the Credential Manager");
                }
            }
        }
    }
    if !sentinel_updates.is_empty() {
        if let Err(e) = update_env_file(env_path, &sentinel_updates) {
            log::warn!("[credvault] .env rewrite after migration failed: {e}");
        }
    }
}

/// On first launch after the Roaming→Local migration (v0.5.24+), move any
/// files written to `%APPDATA%\com.navisual.app` to `%LOCALAPPDATA%\com.navisual.app`.
/// API keys, auth tokens, sessions, and logs are machine-specific and must not
/// sync across devices via roaming profiles.
fn migrate_roaming_to_local(old_dir: &std::path::Path, new_dir: &std::path::Path) {
    if old_dir == new_dir || !old_dir.exists() {
        return;
    }
    const FILES: &[&str] = &[
        ".env",
        "usage.json",
        "supabase_session.json",
        "locate_log.jsonl",
        "locate_log.jsonl.1",
        "model_timings.csv",
    ];
    const DIRS: &[&str] = &["sessions", "debug"];
    let mut moved = 0usize;
    for name in FILES {
        let src = old_dir.join(name);
        let dst = new_dir.join(name);
        if src.exists() && !dst.exists() {
            if std::fs::rename(&src, &dst).is_ok() {
                moved += 1;
            } else {
                log::warn!("migrate {name}: rename failed");
            }
        }
    }
    for name in DIRS {
        let src = old_dir.join(name);
        let dst = new_dir.join(name);
        if src.exists() && !dst.exists() {
            if std::fs::rename(&src, &dst).is_ok() {
                moved += 1;
            } else {
                log::warn!("migrate dir {name}: rename failed");
            }
        }
    }
    if moved > 0 {
        log::info!("migrated {moved} item(s) from Roaming to Local AppData");
        std::fs::remove_dir(old_dir).ok(); // clean up if now empty
    }
}

/// On startup, delete debug-mode artifacts older than 7 days. Both flags
/// (`DEBUG_SCREENSHOT_ENABLED`, `DEBUG_LOCATE_LOG_FILE_ENABLED`) are off
/// by default — this is a safety net for developers who turn them on,
/// forget, and accumulate window-title / OCR-text PII indefinitely.
///
/// Targets: `<app_data>/debug/*` and `<app_data>/locate_log.jsonl{,.1}`.
/// Deliberately does NOT touch `<app_data>/training/` — that dir only exists
/// when `TRAINING_CAPTURE_ENABLED` is deliberately on, and accumulation is its
/// entire purpose (llm-finetuning-eval.md §5b).
fn cleanup_old_debug_artifacts(app_data_dir: &std::path::Path) {
    use std::time::{Duration, SystemTime};
    const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    let now = SystemTime::now();
    let mut removed = 0usize;

    let mut try_remove = |p: &std::path::Path| {
        if let Ok(meta) = std::fs::metadata(p) {
            if let Ok(modified) = meta.modified() {
                if now
                    .duration_since(modified)
                    .map(|d| d > MAX_AGE)
                    .unwrap_or(false)
                    && std::fs::remove_file(p).is_ok()
                {
                    removed += 1;
                }
            }
        }
    };

    let debug_dir = app_data_dir.join("debug");
    if let Ok(entries) = std::fs::read_dir(&debug_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                try_remove(&p);
            }
        }
    }
    try_remove(&app_data_dir.join("locate_log.jsonl"));
    try_remove(&app_data_dir.join("locate_log.jsonl.1"));

    if removed > 0 {
        log::info!("debug cleanup: removed {removed} file(s) older than 7 days");
    }
}

#[tauri::command]
async fn guide(
    app: AppHandle,
    state: State<'_, AppState>,
    task: String,
    is_reply: bool,
) -> Result<GuideResponse, String> {
    // Flow A: any candidate boxes on screen are resolved by the state the user's
    // click left behind — read it before this request changes anything.
    resolve_pending_candidates(&app, &state, false).await;
    let is_next_requery = task.starts_with("[User completed:");
    // Only reset session state for a genuine new task (not resume, not reply, not next-requery).
    // target_hwnd is intentionally NOT reset here — it persists across new sub-tasks in the same
    // working context so recording tools in the foreground (OBS, ScreenToGif) can't steal the
    // target. The "＋ New task" button calls `new_session` to reset target_hwnd explicitly.
    if !task.is_empty() && !is_reply && !is_next_requery {
        let mut g = state.guidance.lock();
        g.session_id = None;
        g.steps = vec![];
        g.state_summary = String::new();
        // A new task governs its own reply language — store it as the language sample when it's
        // substantial enough to read (else clear, and the directive falls back to the task text).
        g.reply_lang_sample = crate::ai::prompts::is_language_sample(&task).then(|| task.clone());
        // Drop the previous screenshot from RAM — new task starts fresh.
        *state.chat_full_jpeg.lock() = None;
    } else if is_reply && crate::ai::prompts::is_language_sample(&task) {
        // A substantial reply updates the sticky language sample, so a mid-session switch takes
        // effect and survives the next machine `[User completed:]` turn — in BOTH directions
        // (into "请说中文", and back out with "please speak English."). A short ack ("ok") is not a
        // sample, so it leaves the current session language untouched.
        state.guidance.lock().reply_lang_sample = Some(task.clone());
    }

    let session_id = {
        let mut router = state.ai_router.lock().await;
        if task.is_empty() || is_reply || is_next_requery {
            if let Some(session) = &router.session_manager.current_session {
                session.id.to_string()
            } else {
                return Err("No active session to continue".to_string());
            }
        } else {
            let session = router.session_manager.create_session(task.clone());
            session.id.to_string()
        }
    };

    let (debug_screenshot_enabled, training_enabled) = {
        let router = state.ai_router.lock().await;
        (
            router.config.debug_screenshot_enabled,
            router.config.training_capture_enabled,
        )
    };

    // Training-data join key (llm-finetuning-eval.md §5b): one UUID per AI request,
    // shared by the prompt-log entry, the locate trace, the saved screenshot, and
    // any feedback row. prev_request_id chains this session's requests (a correction
    // entry's prev is the response it rejects — a natural preference pair). Read
    // before the state stores the new id; stored only once the AI call succeeds.
    let request_id = uuid::Uuid::new_v4().to_string();
    let prev_request_id = state.guidance.lock().request_id.clone();

    // `is_fs` is the user's sticky "Entire desktop" choice from the target picker
    // (GuidanceState.full_screen_mode). The AI no longer decides this — full-screen
    // is now an explicit, user-initiated capture scope. When set, pinned/target HWNDs
    // are ignored and the whole virtual desktop is captured.
    let (stored_hwnd, is_pinned, is_fs, fs_monitor) = {
        let g = state.guidance.lock();
        (
            g.pinned_hwnd.or(g.target_hwnd),
            g.pinned_hwnd.is_some(),
            g.full_screen_mode,
            g.full_screen_monitor,
        )
    };

    // Data-leak guard: a screen BitBlt of a window's rect grabs whatever is
    // *visually* there, so a PINNED target that's fully hidden behind another app
    // would be captured as the OCCLUDING app's pixels and sent to the AI. The user
    // chose "refuse + prompt" over auto-raising (which steals focus / is unreliable),
    // so bail before capturing and tell them to bring it forward. (Scoped to pinned:
    // in auto-detect the tracker keeps the target on the foreground, and the capture
    // mask now fails safe — greys, never leaks — if a stale target is ever occluded.)
    #[cfg(windows)]
    if !is_fs && is_pinned {
        if let Some(hwnd) = stored_hwnd {
            if capture::window_fully_occluded(hwnd) {
                let app = capture::get_window_info_for_hwnd(hwnd)
                    .map(|i| i.app_name)
                    .unwrap_or_else(|| "The pinned app".to_string());
                return Ok(GuideResponse {
                    ok: false,
                    session_id,
                    request_id: None,
                    steps: vec![],
                    step_index: 0,
                    instruction: String::new(),
                    located: None,
                    needs_input: false,
                    provider: String::new(),
                    model: None,
                    input_tokens: None,
                    output_tokens: None,
                    error: Some(format!(
                        "{app} is hidden behind another window, so Navisual can't see it. \
                         Bring it to the front (click it in the taskbar), or pick a different \
                         app with the target selector (🎯), then try again."
                    )),
                    debug_screenshot_path: None,
                    chat_thumb_b64: None,
                    locate_trace: None,
                    ai_bbox: None,
                    suggested_tasks: Vec::new(),
        hint_shown: false,
        candidates: Vec::new(),
                });
            }
        }
    }

    // Get the panel rect before entering spawn_blocking — blanked from the
    // capture so the AI never sees our own UI chrome in screenshots.
    let exclude = capture::get_panel_rects();

    // Debug folder is a sub-directory of the app data dir.
    let debug_dir = app.path().app_local_data_dir().map(|p| p.join("debug")).ok();

    // Clear the previous step's pointer before capture — prevents it from
    // appearing in the AI's screenshot. Stop the tracker first so it can't
    // re-emit the old overlay during the 33 ms DWM composite wait.
    state.tracker.clear();
    if let Ok(update) = overlay::make_update(overlay::OverlayKind::None, None, None) {
        let _ = overlay::emit_update(&app, update);
    }
    tokio::time::sleep(std::time::Duration::from_millis(33)).await;

    #[allow(clippy::type_complexity)]
    let capture_result = tokio::task::spawn_blocking(move || -> Result<(String, Option<capture::Rect>, Option<usize>, Option<String>, Option<String>, Vec<u8>, Option<u64>, Option<Vec<u8>>, Option<capture::Rect>), ()> {
        let (bytes, rect_opt, hwnd_opt) = if is_fs {
            // A chosen single monitor, else (single-monitor systems) the whole desktop.
            let cap = match fs_monitor {
                Some(r) => capture::capture_region_jpeg(r, 75, &exclude),
                None => capture::capture_virtual_desktop_jpeg(75, &exclude),
            };
            match cap {
                Ok((bytes, rect)) => (bytes, Some(rect), None),
                Err(_) => return Err(()),
            }
        } else if let Some(hwnd_raw) = stored_hwnd {
            // Reuse the HWND we already discovered — skip z-order walk entirely.
            match capture::recapture_window_jpeg(hwnd_raw, 75, &exclude) {
                Ok((bytes, rect)) => (bytes, Some(rect), Some(hwnd_raw)),
                Err(_) => {
                    // Window was closed/minimised — rediscover.
                    match capture::capture_active_window_jpeg(75, &exclude) {
                        Ok((bytes, rect, hwnd)) => (bytes, Some(rect), Some(hwnd)),
                        Err(_) => return Err(()),
                    }
                }
            }
        } else {
            // First call for this task — discover the target window.
            match capture::capture_active_window_jpeg(75, &exclude) {
                Ok((bytes, rect, hwnd)) => (bytes, Some(rect), Some(hwnd)),
                Err(_) => return Err(()),
            }
        };
        let final_bytes = bytes;

        let debug_path = if debug_screenshot_enabled {
            if let Some(ref dir) = debug_dir {
                let _ = std::fs::create_dir_all(dir);
                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
                let path = dir.join(format!("screenshot_{ts}.jpg"));
                let txt_path = dir.join(format!("screenshot_{ts}.txt"));

                if let Some(hwnd) = hwnd_opt {
                    #[cfg(windows)]
                    {
                        let info = capture::get_window_info(hwnd);
                        let _ = std::fs::write(&txt_path, info);
                    }
                }

                if std::fs::write(&path, &final_bytes).is_ok() {
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Native-res OCR image, captured NOW — the overlay is cleared and the streamed subtitle
        // hasn't been shown yet, so the locator's OCR never reads our own caption and we avoid the
        // clear/redraw flicker of capturing it at locate time.
        let (ocr_png, ocr_rect) = if !is_fs {
            match hwnd_opt {
                Some(h) => match capture::recapture_window_raw(h, &exclude) {
                    Ok((raw, rect)) => (capture::encode_png_for_ocr(&raw).ok(), Some(rect)),
                    Err(_) => (None, None),
                },
                None => (None, None),
            }
        } else {
            // Full-screen: OCR must see the SAME region the AI saw (chosen monitor or
            // whole desktop), at native resolution — not the foreground window — so the
            // OCR coordinate space matches the AI image. rect_opt is that capture rect.
            match rect_opt {
                Some(r) => match capture::capture_region_raw(r, &exclude) {
                    Ok(raw) => (capture::encode_png_for_ocr(&raw).ok(), Some(r)),
                    Err(_) => (None, None),
                },
                None => (None, None),
            }
        };

        let thumb_b64 = make_chat_thumbnail(&final_bytes);
        let pre_hash = ahash_of_jpeg(&final_bytes);
        let b64 = capture::to_base64(&final_bytes);
        Ok((b64, rect_opt, hwnd_opt, debug_path, thumb_b64, final_bytes, pre_hash, ocr_png, ocr_rect))
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    // App-data-relative filename of the saved training JPEG (None unless training
    // capture is on and the save succeeded) — recorded on the prompt-log entry.
    let mut training_shot_file: Option<String> = None;
    let (
        screenshot_b64,
        capture_rect_opt,
        new_hwnd_opt,
        debug_screenshot_path,
        chat_thumb_b64,
        pre_hash,
        pre_ocr,
    ) = match capture_result {
        Ok((b64, rect_opt, hwnd_opt, dbg, thumb, full_bytes, pre_hash, ocr_png, ocr_rect)) => {
            if training_enabled {
                let base = app.path().app_local_data_dir().ok();
                training_shot_file = save_training_shot(base.as_deref(), &request_id, &full_bytes);
            }
            *state.chat_full_jpeg.lock() = Some(full_bytes);
            (
                b64,
                rect_opt,
                hwnd_opt,
                dbg,
                thumb,
                pre_hash,
                ocr_png.zip(ocr_rect),
            )
        }
        Err(()) => {
            return Ok(GuideResponse {
                ok: false,
                session_id,
                request_id: None,
                steps: vec![],
                step_index: 0,
                instruction: String::new(),
                located: None,
                needs_input: false,
                provider: String::new(),
                model: None,
                input_tokens: None,
                output_tokens: None,
                error: Some(
                    "No application window found. Please click on the program you want \
                     help with to bring it into focus, then try Guide me again."
                        .to_string(),
                ),
                debug_screenshot_path: None,
                chat_thumb_b64: None,
                locate_trace: None,
                ai_bbox: None,
                suggested_tasks: Vec::new(),
        hint_shown: false,
        candidates: Vec::new(),
            });
        }
    };

    // Phase 0.2 — flash the shared-app boundary so the user can see what
    // we're capturing. Emits the `app_changed` event for the header chip too.
    if let Some(hwnd_raw) = new_hwnd_opt {
        state.guidance.lock().last_announced_hwnd = Some(hwnd_raw);
        announce_shared_app(&app, Some(hwnd_raw), true);
    }

    // S.1 — Structured-Context enumeration at AI-capture time (v0.7 Workstream S): the
    // element list the AI can select from, same freshness contract as the screenshot.
    // Gated on: (a) a single captured window (full-screen mode has no one tree), and
    // (b) the provider being able to USE the block without choking on it — the managed
    // free tier's weak OpenRouter vision models hang past the client timeout on the big
    // [Screen Elements] block (confirmed 2026-07-10) and can't select from it well
    // anyway, so it's skipped for them; every other provider (paid managed, all BYOK
    // incl. text-only DeepSeek) keeps it. A warm tree is ~ms; enumerate_context_snapshot_bounded
    // caps the worst case (see its doc comment) before the (multi-second) AI call starts.
    let sc_enabled = state.ai_router.lock().await.structured_context_enabled();
    let context_elements: Option<std::sync::Arc<Vec<locator::ContextElement>>> = match (
        is_fs,
        new_hwnd_opt,
        sc_enabled,
    ) {
        (false, Some(hwnd), true) => enumerate_context_snapshot_bounded(hwnd)
            .await
            .map(std::sync::Arc::new),
        _ => None,
    };

    let mut router = state.ai_router.lock().await;

    let mut window_context = String::new();
    if let Some(hwnd) = new_hwnd_opt {
        let info = capture::get_window_info(hwnd);
        window_context = format!("\n[Current Window Info]\n{}", info);
        window_context.push_str(&active_pack_context(&state.packs, hwnd));
    }
    if let (Some(els), Some(rect)) = (context_elements.as_deref(), capture_rect_opt) {
        window_context.push_str(&ai::prompts::elements_context_block(els, rect));
    }
    // L1 app state from a script channel (Blender bridge today) — facts the screenshot
    // can't convey. Same capture-time atomicity as [Screen Elements]; absent when no
    // channel applies.
    if let Some(block) = app_state_snapshot(new_hwnd_opt) {
        window_context.push_str(&block);
    }

    // Append window context to the prompt (no grid suffix any more — AI returns
    // target_bbox instead).
    let add_grid = |text: String| -> String {
        if !window_context.is_empty() {
            format!("{text}\n{window_context}")
        } else {
            text
        }
    };

    // Streaming-first surfacing: as the instruction streams in token-by-token,
    // push it to BOTH the panel (stream_chunk) and the on-screen caption
    // (overlay Subtitle). The caption used to appear only after the full
    // response + locate (~7-10 s); now it forms live so perceived latency drops
    // to first-token (~1-2 s). The overlay honours subtitle_enabled, so this is
    // a no-op when captions are off. execute_step later replaces this transient
    // caption with the real pointer + final instruction.
    let app_clone = app.clone();
    let mut streamed = String::new();
    // Warm the target window's UIA tree as soon as the AI starts streaming, so Chromium/
    // Electron materialises its lazy a11y tree during generation and find_element hits by
    // locate time (seconds later). Fired once, on a background thread; no-op off-Chromium.
    let prime_hwnd = new_hwnd_opt;
    let mut primed = false;
    let on_chunk = move |chunk: &str, steps_seen: usize| {
        if !primed {
            primed = true;
            #[cfg(windows)]
            if let Some(h) = prime_hwnd {
                std::thread::spawn(move || crate::locator::a11y::prime(h));
                // Keep the target's a11y tree built for the whole session — an active UIA
                // subscription so lazy apps (Qt/VLC, Chromium past its ~30s fade) expose their
                // tree to our locator. Idempotent; re-targets when the focused app changes.
                crate::locator::keepwarm::warm(h);
            }
            #[cfg(not(windows))]
            let _ = prime_hwnd;
        }
        streamed.push_str(chunk);
        let _ = app_clone.emit(
            "stream_chunk",
            StreamChunkPayload {
                delta: chunk.to_string(),
                steps_seen,
            },
        );
        if let Ok(update) =
            overlay::make_update(overlay::OverlayKind::Subtitle, None, Some(streamed.clone()))
        {
            let _ = overlay::emit_update(&app_clone, update);
        }
    };

    // Measure the pure AI round-trip (excludes capture + locate) for the model
    // latency log. Provider captured before the borrow; the actual model is read
    // after the request (managed routes to a concrete model server-side).
    let timing_provider = router.config.api_provider.clone();
    let ai_started = std::time::Instant::now();

    // The original user request, restated each continuation turn as a language + goal
    // anchor (see prompts::language_anchor — fixes Qwen drifting to Chinese on multi-turn
    // English sessions). Empty for the first turn, where the request IS the prompt.
    let original_task = router
        .session_manager
        .current_session
        .as_ref()
        .map(|s| s.task_description.clone())
        .unwrap_or_default();
    // An explicitly-chosen Language setting is an authoritative reply-language signal (it also
    // rescues the pinyin-mis-transcription case); "auto" defers to the request's own script.
    let voice_language = router.config.voice_language.clone();
    // Session-sticky language sample — the user's last substantial message. The directive treats
    // it as the language authority, so it survives the machine turns that carry no language signal.
    let reply_sample = state.guidance.lock().reply_lang_sample.clone();

    let (resp, sent_user_prompt) = if task.is_empty() || is_next_requery {
        let summary = {
            let g = state.guidance.lock();
            g.state_summary.clone()
        };
        let base = if task.is_empty() {
            crate::ai::prompts::session_resume_template(&summary)
        } else {
            format!(
                "{task} The previous state summary: {summary}. \
                Here is the current screen. Please provide the next instruction.",
            )
        };
        let prompt = format!(
            "{}{}",
            add_grid(base),
            crate::ai::prompts::reply_language_directive(
                reply_sample.as_deref(),
                &original_task,
                &voice_language,
            )
        );
        (
            router
                .send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk)
                .await,
            prompt,
        )
    } else if is_reply {
        let summary = {
            let g = state.guidance.lock();
            g.state_summary.clone()
        };
        let prompt = format!(
            "{}{}",
            add_grid(task.clone()),
            crate::ai::prompts::reply_language_directive(
                reply_sample.as_deref(),
                &original_task,
                &voice_language,
            )
        );
        (
            router
                .send_guidance_request(&prompt, Some(&screenshot_b64), Some(&summary), on_chunk)
                .await,
            prompt,
        )
    } else {
        // Turn 1: the request IS the prompt, but it sits near the TOP with the whole screenshot +
        // [Screen Elements] list (often in the app's UI language) between it and the model's reply.
        // Anchor on `task` itself at the tail so a short request can't be out-shouted by a screen
        // full of another language — the user's language ≠ app-UI-language case.
        let prompt = format!(
            "{}{}",
            add_grid(crate::ai::prompts::initial_context_template(&task)),
            crate::ai::prompts::reply_language_directive(
                reply_sample.as_deref(),
                &task,
                &voice_language,
            )
        );
        (
            router
                .send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk)
                .await,
            prompt,
        )
    };

    let ai_elapsed_ms = ai_started.elapsed().as_millis();

    // Payload audit (debug captures on): write the exact dynamic text we sent to
    // the AI alongside screenshot_<ts>.jpg. The appended [Current Window Info]
    // (incl. the window title) is the least-obvious thing that leaves the machine,
    // so this lets you verify nothing unintended is sent. System prompt is static
    // (src-tauri/src/ai/prompts.rs); conversation history is reset on a new task.
    if debug_screenshot_enabled {
        if let Ok(base) = app.path().app_local_data_dir() {
            let dir = base.join("debug");
            let _ = std::fs::create_dir_all(&dir);
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
            let dump = format!(
                "Dynamic text sent to the AI (system prompt is static — see \
                 src-tauri/src/ai/prompts.rs; screenshot is screenshot_<ts>.jpg).\n\n\
                 === USER MESSAGE ===\n{sent_user_prompt}\n"
            );
            let _ = std::fs::write(dir.join(format!("prompt_{ts}.txt")), dump);
        }
    }

    let (timing_ok, timing_steps) = match &resp {
        Ok(r) => (true, r.steps.len()),
        Err(_) => (false, 0),
    };
    // The model that actually handled this request: for managed, the concrete model
    // OpenRouter routed to (relay sends the `openrouter/free` router); else the configured one.
    let used_model = router
        .get_managed_routed_model()
        .unwrap_or_else(|| router.active_model());
    let (in_tok, out_tok) = router.get_last_usage();
    let app_version = app.package_info().version.to_string();
    let resp_err_str = resp.as_ref().err().map(|e| e.to_string());
    maybe_log_prompt(
        &app,
        router.config.debug_prompt_log_file_enabled,
        training_enabled,
        prompt_log::PromptLogFields {
            session_id: &session_id,
            request_id: Some(&request_id),
            prev_request_id: prev_request_id.as_deref(),
            app_version: Some(&app_version),
            call_kind: if is_reply {
                "reply"
            } else if is_next_requery {
                "requery"
            } else if task.is_empty() {
                "resume"
            } else {
                "task"
            },
            provider: &timing_provider,
            model: &used_model,
            has_screenshot: true,
            screenshot_file: training_shot_file.as_deref(),
            prompt: &sent_user_prompt,
            response: resp
                .as_ref()
                .ok()
                .and_then(|r| serde_json::to_value(r).ok()),
            response_error: resp_err_str.as_deref(),
        },
    );
    log_model_timing(
        &app,
        &timing_provider,
        &used_model,
        ai_elapsed_ms,
        timing_ok,
        timing_steps,
    );

    // Emit balance update for managed provider before processing the result.
    if let Some(remaining) = router.get_managed_free_remaining() {
        let _ = app.emit("balance_update", remaining);
    }
    if let Some(coins) = router.get_managed_coin_balance() {
        let _ = app.emit("coin_balance_update", coins);
    }
    // One-shot: only present when THIS request billed real coins despite a
    // "Free" quality-tier preference (ran out mid-preference, silently fell
    // back to paid — reported live 2026-07-11, needed a user-visible notice).
    if let Some((tier, price_micro)) = router.take_managed_tier_auto_selected() {
        let _ = app.emit("tier_auto_selected", (tier, price_micro));
    }

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            let err_str = e.to_string();
            if err_str == "free_trial_exhausted" {
                let _ = app.emit("trial_exhausted", ());
            } else if err_str == "insufficient_coins" {
                let _ = app.emit("insufficient_coins", ());
            }
            return Ok(GuideResponse {
                ok: false,
                session_id,
                request_id: Some(request_id),
                steps: vec![],
                step_index: 0,
                instruction: String::new(),
                located: None,
                needs_input: false,
                provider: router.config.api_provider.clone(),
                model: Some(used_model.clone()),
                input_tokens: Some(in_tok),
                output_tokens: Some(out_tok),
                error: Some(match err_str.as_str() {
                    "free_trial_exhausted" => "Your free requests have been used.".to_string(),
                    "insufficient_coins" => {
                        "Not enough coins for this quality tier. Buy more to continue.".to_string()
                    }
                    _ => err_str,
                }),
                debug_screenshot_path: None,
                chat_thumb_b64: None,
                locate_trace: None,
                ai_bbox: None,
                suggested_tasks: Vec::new(),
        hint_shown: false,
        candidates: Vec::new(),
            });
        }
    };

    let mut steps = response.steps;
    // Rule-14 defense in depth: strip leaked element ids / markdown from the
    // user-facing text; recover an unambiguous leaked id into an empty
    // target_element_id (verification-gated). See ai::types::sanitize_steps.
    ai::types::sanitize_steps(&mut steps);
    let steps = steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    // Workstream P: the toggle gates the data at the source — when off, suggestions
    // never reach the frontend (the static prompt rule stays; making it dynamic
    // would break Anthropic prompt caching for ~4 lines of text).
    let suggested_tasks = if router.config.task_suggestions {
        response.suggested_tasks
    } else {
        Vec::new()
    };
    let provider = router.config.api_provider.clone();
    let bbox_distrust = router.config.bbox_distrust_models.clone();

    if let Some(session) = &mut router.session_manager.current_session {
        session.update_state(state_summary.clone());
        let user_turn_text = if task.is_empty() {
            "Next".to_string()
        } else {
            task.clone()
        };
        session.add_turn("user", user_turn_text, None);
        let content = steps
            .iter()
            .map(|s| s.instruction.clone())
            .collect::<Vec<_>>()
            .join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    // Release the ai_router Mutex before execute_step so that concurrent
    // commands (next_step, send_correction) do not deadlock while the
    // locator runs its blocking A11y/OCR calls.
    drop(router);

    {
        let mut g = state.guidance.lock();
        g.session_id = Some(session_id.clone());
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
        g.provider = provider.clone();
        g.capture_rect = capture_rect_opt;
        g.target_hwnd = new_hwnd_opt;
        // Also stored when None — a skipped enumeration must clear the previous
        // snapshot, or next_step would resolve ids against a stale list.
        g.context_elements = context_elements.clone();
        // These steps came from THIS request — next_step's locate traces and the
        // frontend's feedback rows attribute to it (llm-finetuning-eval.md §5b).
        g.request_id = Some(request_id.clone());
    }

    if steps.is_empty() {
        // Still anchor the autopilot baseline + run stale detection so that the
        // needs_input branch behaves the same as a normal response.
        let post_hash = anchor_autopilot_baseline(&state).await;
        emit_stale_if_drifted(&app, pre_hash, post_hash);
        return Ok(GuideResponse {
            ok: true,
            session_id,
            request_id: Some(request_id),
            steps,
            step_index: 0,
            instruction: String::new(),
            located: None,
            needs_input,
            provider,
            model: Some(used_model.clone()),
            input_tokens: Some(in_tok),
            output_tokens: Some(out_tok),
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
            // A no-step "looks complete" reply is exactly where suggestions matter.
            suggested_tasks,
            hint_shown: false,
            candidates: Vec::new(),
        });
    }

    let log_trace = state
        .ai_router
        .lock()
        .await
        .config
        .debug_locate_log_file_enabled;
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path()
            .app_local_data_dir()
            .ok()
            .map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[0], capture_rect_opt, &provider);
    let bbox_decisive = ai::bbox::bbox_is_decisive(&used_model, &bbox_distrust);

    // Stale detection must run BEFORE execute_step draws the new pointer.
    // Capturing afterwards would include our own overlay pointer — a large
    // visual change that trips the threshold on every response. The overlay
    // was cleared before the AI capture, so the screen is pointer-free here
    // too; any drift now reflects a real user change while the AI was thinking.
    // Hash the SAME window the AI capture used (new_hwnd_opt) so pre/post compare
    // like-for-like (C5) — pre_hash came from that window, not the foreground.
    let stale_target = if is_fs { None } else { new_hwnd_opt };
    let stale_hash = tokio::task::spawn_blocking(move || ahash_of_screen(stale_target))
        .await
        .ok()
        .flatten();
    emit_stale_if_drifted(&app, pre_hash, stale_hash);

    let (located, mut locate_trace, hint_shown, shown_candidates) = execute_step(
        &app,
        &steps[0],
        new_hwnd_opt,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        bbox_decisive,
        Vec::new(),
        capture_rect_opt,
        pre_ocr,
        &state.packs,
        context_elements,
        None,
        &[],
    )
    .unwrap_or((None, None, false, Vec::new()));
    // Flow B: a first-locate ambiguity set was drawn — arm the state readback.
    arm_candidates_if_shown(
        &state,
        Some(request_id.clone()),
        steps[0].target_text.as_deref(),
        &shown_candidates,
        new_hwnd_opt,
    )
    .await;
    if let Some(ref mut t) = locate_trace {
        t.request_id = Some(request_id.clone());
        t.model = Some(used_model.clone());
        t.provider = Some(provider.clone());
        t.input_tokens = Some(in_tok);
        t.output_tokens = Some(out_tok);
        t.ai_elapsed_ms = Some(ai_elapsed_ms as u32);
        t.app_name = trace_app_name(new_hwnd_opt);
        maybe_log_trace(&app, t, log_trace, training_enabled);
    }

    // Anchor the autopilot baseline AFTER the pointer is drawn so that
    // check_screen_changed (which also sees the pointer) compares like-for-like.
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        request_id: Some(request_id),
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        provider,
        model: Some(used_model),
        input_tokens: Some(in_tok),
        output_tokens: Some(out_tok),
        error: None,
        debug_screenshot_path,
        chat_thumb_b64,
        locate_trace,
        ai_bbox,
        suggested_tasks,
        hint_shown,
        candidates: shown_candidates,
    })
}

#[tauri::command]
async fn next_step(
    app: AppHandle,
    state: State<'_, AppState>,
    step_index: usize,
) -> Result<GuideResponse, String> {
    // Flow A: resolve candidate boxes from the app state the user's click produced.
    resolve_pending_candidates(&app, &state, false).await;
    let (steps, session_id, needs_input, provider, capture_rect, context_elements, request_id) = {
        let g = state.guidance.lock();
        (
            g.steps.clone(),
            g.session_id.clone().unwrap_or_default(),
            g.needs_input,
            g.provider.clone(),
            g.capture_rect,
            // The snapshot from the capture that produced these steps — ids in
            // steps[1..] resolve against the SAME list the AI saw (v0.7 S.1).
            g.context_elements.clone(),
            // No AI call here — attribute this locate to the request whose
            // response the advanced-to step came from (LocateTrace doc comment).
            g.request_id.clone(),
        )
    };

    if step_index >= steps.len() {
        return Err(format!(
            "step_index {step_index} out of range ({})",
            steps.len()
        ));
    }

    let (log_trace, training_enabled, debug_screenshot_enabled, bbox_decisive, used_model) = {
        let router = state.ai_router.lock().await;
        // No AI call here; the cached routed/active model is the one that produced
        // these steps (and their bboxes), so its trust still applies.
        let used_model = router
            .get_managed_routed_model()
            .unwrap_or_else(|| router.active_model());
        (
            router.config.debug_locate_log_file_enabled,
            router.config.training_capture_enabled,
            router.config.debug_screenshot_enabled,
            ai::bbox::bbox_is_decisive(&used_model, &router.config.bbox_distrust_models),
            used_model,
        )
    };
    let stored_hwnd = {
        let g = state.guidance.lock();
        g.pinned_hwnd.or(g.target_hwnd)
    };
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path()
            .app_local_data_dir()
            .ok()
            .map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[step_index], capture_rect, &provider);
    let (located, mut locate_trace, hint_shown, shown_candidates) = execute_step(
        &app,
        &steps[step_index],
        stored_hwnd,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        bbox_decisive,
        Vec::new(),
        capture_rect,
        None, // next_step reuses the prior capture; locator re-captures for OCR
        &state.packs,
        context_elements,
        None,
        &[],
    )
    .unwrap_or((None, None, false, Vec::new()));
    arm_candidates_if_shown(
        &state,
        request_id.clone(),
        steps[step_index].target_text.as_deref(),
        &shown_candidates,
        stored_hwnd,
    )
    .await;
    if let Some(ref mut t) = locate_trace {
        t.request_id = request_id.clone();
        t.model = Some(used_model.clone());
        t.provider = Some(provider.clone());
        // No AI call this turn — nothing new to attribute (see LocateTrace doc comment).
        t.app_name = trace_app_name(stored_hwnd);
        maybe_log_trace(&app, t, log_trace, training_enabled);
    }

    // Local step advance — no AI call, so no stale check. But anchor the
    // autopilot baseline to the new pointer state so autopilot waits for the
    // *next* change (user completing this step) rather than firing on the
    // change that just triggered this advance.
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        request_id,
        steps: steps.clone(),
        step_index,
        instruction: steps[step_index].instruction.clone(),
        located,
        needs_input,
        provider,
        model: None, // local advance, no AI call — frontend keeps the prior routed model
        input_tokens: None,
        output_tokens: None,
        error: None,
        debug_screenshot_path: None,
        chat_thumb_b64: None,
        locate_trace,
        ai_bbox,
        suggested_tasks: Vec::new(), // local advance — no AI call, no new guesses
        hint_shown,
        candidates: shown_candidates,
    })
}

/// B5 (llm-finetuning-eval.md §5c): LOCAL re-locate of the current step after a
/// ✗ Wrong — no AI call, no request consumed. The frontend routes here first when
/// the failing layer was plausibly the locator (final_decision HitA11y/HitOcr/
/// HitTemplate for "Wrong spot"; a Miss for "Can't find it" — where the lazy a11y
/// tree has had seconds to warm since the original attempt), accumulating every
/// rejected pointer bbox in `avoid_bboxes` so no rejected spot can be re-picked
/// by ANY pass. Returns `located: None` when no new gate-passing candidate exists
/// ("no pointer beats wrong pointer" holds for retries) — the frontend then falls
/// through to `send_correction`, avoid list attached.
#[tauri::command]
async fn retry_locate(
    app: AppHandle,
    state: State<'_, AppState>,
    step_index: usize,
    // Target-tagged rejections (locator::candidates::AvoidEntry) — filtered to the
    // entries recorded for THIS step's target below (scoped_avoid doc comment).
    avoid_bboxes: Vec<locator::candidates::AvoidEntry>,
) -> Result<GuideResponse, String> {
    // Flow A: a second "Wrong spot" while candidates are showing means the user did
    // NOT click any of them — resolve (likely unresolved, honest) before re-arming.
    resolve_pending_candidates(&app, &state, true).await;
    let (steps, session_id, needs_input, provider, capture_rect, context_elements, request_id) = {
        let g = state.guidance.lock();
        (
            g.steps.clone(),
            g.session_id.clone().unwrap_or_default(),
            g.needs_input,
            g.provider.clone(),
            g.capture_rect,
            g.context_elements.clone(),
            // Attribute to the request whose response produced this step — same
            // contract as next_step (LocateTrace doc comment).
            g.request_id.clone(),
        )
    };

    if step_index >= steps.len() {
        return Err(format!(
            "step_index {step_index} out of range ({})",
            steps.len()
        ));
    }
    let avoid_bboxes = locator::candidates::scoped_avoid(
        &avoid_bboxes,
        steps[step_index].target_text.as_deref(),
    );

    let (log_trace, training_enabled, debug_screenshot_enabled, bbox_decisive, used_model) = {
        let router = state.ai_router.lock().await;
        let used_model = router
            .get_managed_routed_model()
            .unwrap_or_else(|| router.active_model());
        (
            router.config.debug_locate_log_file_enabled,
            router.config.training_capture_enabled,
            router.config.debug_screenshot_enabled,
            ai::bbox::bbox_is_decisive(&used_model, &router.config.bbox_distrust_models),
            used_model,
        )
    };
    let stored_hwnd = {
        let g = state.guidance.lock();
        g.pinned_hwnd.or(g.target_hwnd)
    };

    // Clear the rejected pointer before the locator's fresh OCR capture — unlike
    // next_step (whose old pointer sits at an unrelated spot), the retry's OCR
    // would otherwise read our own ripple/brackets at exactly the region under
    // scrutiny. Same clear + DWM-composite wait as guide().
    state.tracker.clear();
    if let Ok(update) = overlay::make_update(overlay::OverlayKind::None, None, None) {
        let _ = overlay::emit_update(&app, update);
    }
    tokio::time::sleep(std::time::Duration::from_millis(33)).await;

    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path()
            .app_local_data_dir()
            .ok()
            .map(|p| p.join("debug").join(format!("ocr_retry_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[step_index], capture_rect, &provider);

    // Flow A — candidate collection. Run the primary locate, then up to 2 more with
    // the previous winners added to the avoid list: each pass's own ranking yields the
    // "next-best distinct match", exactly the sequence repeated Wrong-spot presses
    // would have walked one rejection at a time. IoU-dedupe (a large overlapper can
    // survive the centre-based avoid veto), then show ALL of them at once — the user
    // is never asked to choose; their next real click in the app resolves it.
    let (primary, primary_trace) = locate_for_step(
        &steps[step_index],
        stored_hwnd,
        debug_ocr_path,
        ai_bbox,
        bbox_decisive,
        avoid_bboxes.clone(),
        &state.packs,
        context_elements.clone(),
        None, // fresh OCR re-capture — the screen may have changed since the AI saw it
    );
    let mut candidate_boxes: Vec<capture::Rect> = Vec::new();
    if let Some(ref w1) = primary {
        candidate_boxes.push(w1.bbox);
        let mut extra_avoid = avoid_bboxes.clone();
        for _ in 0..2 {
            extra_avoid.extend(candidate_boxes.iter().copied());
            let (next, _t) = locate_for_step(
                &steps[step_index],
                stored_hwnd,
                None, // debug OCR image only for the primary run
                ai_bbox,
                bbox_decisive,
                extra_avoid.clone(),
                &state.packs,
                context_elements.clone(),
                None,
            );
            match next {
                Some(r) => candidate_boxes.push(r.bbox),
                None => break,
            }
        }
        candidate_boxes = locator::candidates::dedupe_candidates(candidate_boxes);
    }

    let (located, mut locate_trace, hint_shown, shown_candidates) = execute_step(
        &app,
        &steps[step_index],
        stored_hwnd,
        None, // locate already ran above (precomputed) — no second OCR debug image
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        bbox_decisive,
        avoid_bboxes,
        capture_rect,
        None,
        &state.packs,
        context_elements,
        Some((primary, primary_trace)),
        &candidate_boxes,
    )
    .unwrap_or((None, None, false, Vec::new()));

    // Arm the state-readback on whatever was actually drawn — the Flow A collection,
    // or (when the collection came up short and the retry's own locate missed on a
    // recorded tie) a Flow B ambiguity set.
    arm_candidates_if_shown(
        &state,
        request_id.clone(),
        steps[step_index].target_text.as_deref(),
        &shown_candidates,
        stored_hwnd,
    )
    .await;
    if let Some(ref mut t) = locate_trace {
        t.request_id = request_id.clone();
        t.local_retry = true;
        t.model = Some(used_model.clone());
        t.provider = Some(provider.clone());
        t.app_name = trace_app_name(stored_hwnd);
        maybe_log_trace(&app, t, log_trace, training_enabled);
    }

    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        request_id,
        steps: steps.clone(),
        step_index,
        instruction: steps[step_index].instruction.clone(),
        located,
        needs_input,
        provider,
        model: None, // no AI call — frontend keeps the prior routed model
        input_tokens: None,
        output_tokens: None,
        error: None,
        debug_screenshot_path: None,
        chat_thumb_b64: None,
        locate_trace,
        ai_bbox,
        suggested_tasks: Vec::new(),
        hint_shown,
        candidates: shown_candidates,
    })
}

#[tauri::command]
async fn send_correction(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
    // "Wrong spot" memory: every bbox a rejected pointer occupied this step,
    // TAGGED with the target it was rejected for. The correction's AI response may
    // re-target, so the filter runs AFTER the response arrives, against the new
    // step's own target_text — a rejection of "the heading" must not blanket-veto
    // a later 'Save' whose best answer sits inside that heading (live 2026-07-18).
    avoid_bboxes: Option<Vec<locator::candidates::AvoidEntry>>,
) -> Result<GuideResponse, String> {
    // Flow A: resolve any on-screen candidate boxes before the correction reshapes
    // the step (a "None of these" Wrong press lands here — resolves unresolved).
    resolve_pending_candidates(&app, &state, true).await;
    let session_id = {
        let g = state.guidance.lock();
        g.session_id.clone()
    };
    let session_id = session_id.ok_or("no active session")?;

    // Clear the stored HWND so the correction capture re-discovers the currently
    // focused window. If the first guide pointed at the wrong app, the user can
    // switch focus to the right app then press Wrong and the next capture will
    // find the correct window. That rediscovery logic is for AUTO-DETECT only —
    // an explicit 📌 pin is a user-set capture scope that a correction must
    // honor exactly like guide() does (audit 2026-07-12 F3: corrections used to
    // capture whatever was foreground, sending a different app's pixels to the
    // AI despite the pin). In sticky "Entire desktop" mode (full_screen_mode)
    // the capture grabs the whole virtual desktop instead.
    let (pinned_hwnd, is_fs, fs_monitor) = {
        let mut g = state.guidance.lock();
        g.target_hwnd = None;
        (g.pinned_hwnd, g.full_screen_mode, g.full_screen_monitor)
    };

    // Same refuse-don't-leak guard as guide(): a fully occluded pinned target
    // would BitBlt as an all-grey image (the occlusion mask fails safe) — a
    // wasted request and a confused AI reply instead of a clear error.
    #[cfg(windows)]
    if !is_fs {
        if let Some(hwnd) = pinned_hwnd {
            if capture::window_fully_occluded(hwnd) {
                let app_name = capture::get_window_info_for_hwnd(hwnd)
                    .map(|i| i.app_name)
                    .unwrap_or_else(|| "The pinned app".to_string());
                return Err(format!(
                    "{app_name} is hidden behind another window, so Navisual can't see it. \
                     Bring it to the front (click it in the taskbar), or pick a different \
                     app with the target selector (🎯), then try again."
                ));
            }
        }
    }

    let exclude = capture::get_panel_rects();

    let router = state.ai_router.lock().await;
    let debug_screenshot_enabled = router.config.debug_screenshot_enabled;
    let training_enabled = router.config.training_capture_enabled;
    drop(router); // Release lock before blocking capture

    // Training-data join key — same contract as guide()'s. prev_request_id here is
    // the request whose response the user just rejected: (prev, this) becomes a
    // (rejected, chosen) preference pair once this retry is accepted (§5b).
    let request_id = uuid::Uuid::new_v4().to_string();
    let prev_request_id = state.guidance.lock().request_id.clone();

    let debug_dir = app.path().app_local_data_dir().map(|p| p.join("debug")).ok();

    // Clear the previous pointer before capture.
    state.tracker.clear();
    if let Ok(update) = overlay::make_update(overlay::OverlayKind::None, None, None) {
        let _ = overlay::emit_update(&app, update);
    }
    tokio::time::sleep(std::time::Duration::from_millis(33)).await;

    // Fresh capture — no stored HWND, always walks z-order to the focused window.
    #[allow(clippy::type_complexity)]
    let (
        screenshot_b64,
        new_capture_rect,
        new_hwnd,
        debug_screenshot_path,
        chat_thumb_b64,
        full_jpeg_opt,
        pre_hash,
        pre_ocr,
    ): (
        String,
        Option<capture::Rect>,
        Option<usize>,
        Option<String>,
        Option<String>,
        Option<Vec<u8>>,
        Option<u64>,
        Option<(Vec<u8>, capture::Rect)>,
    ) = tokio::task::spawn_blocking(move || {
        // Full desktop (sticky "Entire desktop" user choice), the pinned app
        // (explicit 📌 scope — honored here exactly like guide() does), or the
        // focused window (auto-detect rediscovery).
        let captured: Option<(Vec<u8>, capture::Rect, Option<usize>)> = if is_fs {
            match fs_monitor {
                Some(r) => capture::capture_region_jpeg(r, 75, &exclude),
                None => capture::capture_virtual_desktop_jpeg(75, &exclude),
            }
            .ok()
            .map(|(bytes, rect)| (bytes, rect, None))
        } else if let Some(hwnd_raw) = pinned_hwnd {
            // Pinned window closed/minimised mid-session → fall back to
            // rediscovery rather than failing the correction outright
            // (mirrors guide()'s recapture fallback).
            capture::recapture_window_jpeg(hwnd_raw, 75, &exclude)
                .ok()
                .map(|(bytes, rect)| (bytes, rect, Some(hwnd_raw)))
                .or_else(|| {
                    capture::capture_active_window_jpeg(75, &exclude)
                        .ok()
                        .map(|(bytes, rect, hwnd)| (bytes, rect, Some(hwnd)))
                })
        } else {
            capture::capture_active_window_jpeg(75, &exclude)
                .ok()
                .map(|(bytes, rect, hwnd)| (bytes, rect, Some(hwnd)))
        };
        if let Some((bytes, rect, hwnd_opt)) = captured {
            let final_bytes = bytes;
            // Native-res OCR image, captured now (overlay cleared, before the streamed subtitle)
            // so the locator's OCR never reads our own caption — see guide(). In full-screen
            // mode there's no single window, so OCR re-captures the SAME region the AI saw
            // (chosen monitor / whole desktop) so its coordinate space matches the AI image.
            let pre_ocr = match hwnd_opt {
                Some(hwnd) => capture::recapture_window_raw(hwnd, &exclude)
                    .ok()
                    .and_then(|(raw, r)| capture::encode_png_for_ocr(&raw).ok().map(|png| (png, r))),
                None => capture::capture_region_raw(rect, &exclude)
                    .ok()
                    .and_then(|raw| capture::encode_png_for_ocr(&raw).ok().map(|png| (png, rect))),
            };

            let debug_path = if debug_screenshot_enabled {
                if let Some(ref dir) = debug_dir {
                    let _ = std::fs::create_dir_all(dir);
                    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
                    let path = dir.join(format!("screenshot_corr_{ts}.jpg"));
                    let txt_path = dir.join(format!("screenshot_corr_{ts}.txt"));

                    #[cfg(windows)]
                    if let Some(hwnd) = hwnd_opt {
                        let info = capture::get_window_info(hwnd);
                        let _ = std::fs::write(&txt_path, info);
                    }

                    if std::fs::write(&path, &final_bytes).is_ok() {
                        Some(path.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let thumb_b64 = make_chat_thumbnail(&final_bytes);
            let pre_hash = ahash_of_jpeg(&final_bytes);
            let b64 = capture::to_base64(&final_bytes);
            (
                b64,
                Some(rect),
                hwnd_opt,
                debug_path,
                thumb_b64,
                Some(final_bytes),
                pre_hash,
                pre_ocr,
            )
        } else {
            (String::new(), None, None, None, None, None, None, None)
        }
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    let mut training_shot_file: Option<String> = None;
    if let Some(bytes) = full_jpeg_opt {
        if training_enabled {
            let base = app.path().app_local_data_dir().ok();
            training_shot_file = save_training_shot(base.as_deref(), &request_id, &bytes);
        }
        *state.chat_full_jpeg.lock() = Some(bytes);
    }

    // Phase 0.2 — show shared-app boundary on correction too.
    if let Some(hwnd_raw) = new_hwnd {
        state.guidance.lock().last_announced_hwnd = Some(hwnd_raw);
        announce_shared_app(&app, Some(hwnd_raw), true);
    }

    // S.1 — fresh Structured-Context snapshot for the correction capture (the retry
    // may be looking at a different window/state than the original guide()). Skipped for
    // the managed free tier — see the matching gate + rationale in guide().
    let sc_enabled = state.ai_router.lock().await.structured_context_enabled();
    let context_elements: Option<std::sync::Arc<Vec<locator::ContextElement>>> =
        match (is_fs, new_hwnd, sc_enabled) {
            (false, Some(hwnd), true) => enumerate_context_snapshot_bounded(hwnd)
                .await
                .map(std::sync::Arc::new),
            _ => None,
        };

    let mut router = state.ai_router.lock().await;
    let summary = {
        let g = state.guidance.lock();
        g.state_summary.clone()
    };

    let user_text_owned = match note.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => format!(
            "{} User note: {}",
            crate::ai::prompts::CORRECTION_CONTEXT,
            n
        ),
        None => crate::ai::prompts::CORRECTION_CONTEXT.to_string(),
    };

    let mut window_context = String::new();
    if let Some(hwnd) = new_hwnd {
        let info = capture::get_window_info(hwnd);
        window_context = format!("\n[Current Window Info]\n{}", info);
        window_context.push_str(&active_pack_context(&state.packs, hwnd));
    }
    if let (Some(els), Some(rect)) = (context_elements.as_deref(), new_capture_rect) {
        window_context.push_str(&ai::prompts::elements_context_block(els, rect));
    }
    // L1 app state — same as guide()'s capture path.
    if let Some(block) = app_state_snapshot(new_hwnd) {
        window_context.push_str(&block);
    }

    // Same language + goal anchor as guide()'s continuation turns — a correction is a
    // multi-turn continuation, equally prone to the Chinese-drift feedback loop. `router`
    // is already locked above (line ~2108); reuse it, don't re-lock (tokio Mutex isn't
    // re-entrant — a second lock().await here would deadlock).
    let original_task = router
        .session_manager
        .current_session
        .as_ref()
        .map(|s| s.task_description.clone())
        .unwrap_or_default();
    // Honor the sticky session language sample here too (a correction is a continuation).
    let reply_sample = state.guidance.lock().reply_lang_sample.clone();
    let anchor = crate::ai::prompts::reply_language_directive(
        reply_sample.as_deref(),
        &original_task,
        &router.config.voice_language,
    );

    let final_user_text = if !window_context.is_empty() {
        format!("{user_text_owned}\n{window_context}{anchor}")
    } else {
        format!("{user_text_owned}{anchor}")
    };
    let user_text = final_user_text.as_str();

    // Streaming-first surfacing: as the instruction streams in token-by-token,
    // push it to BOTH the panel (stream_chunk) and the on-screen caption
    // (overlay Subtitle). The caption used to appear only after the full
    // response + locate (~7-10 s); now it forms live so perceived latency drops
    // to first-token (~1-2 s). The overlay honours subtitle_enabled, so this is
    // a no-op when captions are off. execute_step later replaces this transient
    // caption with the real pointer + final instruction.
    let app_clone = app.clone();
    let mut streamed = String::new();
    // Warm the target window's UIA tree on first stream chunk (see guide()).
    let prime_hwnd = new_hwnd;
    let mut primed = false;
    let on_chunk = move |chunk: &str, steps_seen: usize| {
        if !primed {
            primed = true;
            #[cfg(windows)]
            if let Some(h) = prime_hwnd {
                std::thread::spawn(move || crate::locator::a11y::prime(h));
                // Keep the target's a11y tree built for the whole session — an active UIA
                // subscription so lazy apps (Qt/VLC, Chromium past its ~30s fade) expose their
                // tree to our locator. Idempotent; re-targets when the focused app changes.
                crate::locator::keepwarm::warm(h);
            }
            #[cfg(not(windows))]
            let _ = prime_hwnd;
        }
        streamed.push_str(chunk);
        let _ = app_clone.emit(
            "stream_chunk",
            StreamChunkPayload {
                delta: chunk.to_string(),
                steps_seen,
            },
        );
        if let Ok(update) =
            overlay::make_update(overlay::OverlayKind::Subtitle, None, Some(streamed.clone()))
        {
            let _ = overlay::emit_update(&app_clone, update);
        }
    };

    let timing_provider = router.config.api_provider.clone();
    let ai_started = std::time::Instant::now();

    let resp = router
        .send_guidance_request(user_text, Some(&screenshot_b64), Some(&summary), on_chunk)
        .await;

    let ai_elapsed_ms = ai_started.elapsed().as_millis();
    let (timing_ok, timing_steps) = match &resp {
        Ok(r) => (true, r.steps.len()),
        Err(_) => (false, 0),
    };
    let used_model = router
        .get_managed_routed_model()
        .unwrap_or_else(|| router.active_model());
    let (in_tok, out_tok) = router.get_last_usage();
    let app_version = app.package_info().version.to_string();
    let resp_err_str = resp.as_ref().err().map(|e| e.to_string());
    maybe_log_prompt(
        &app,
        router.config.debug_prompt_log_file_enabled,
        training_enabled,
        prompt_log::PromptLogFields {
            session_id: &session_id,
            request_id: Some(&request_id),
            prev_request_id: prev_request_id.as_deref(),
            app_version: Some(&app_version),
            call_kind: "correction",
            provider: &timing_provider,
            model: &used_model,
            has_screenshot: true,
            screenshot_file: training_shot_file.as_deref(),
            prompt: user_text,
            response: resp
                .as_ref()
                .ok()
                .and_then(|r| serde_json::to_value(r).ok()),
            response_error: resp_err_str.as_deref(),
        },
    );
    log_model_timing(
        &app,
        &timing_provider,
        &used_model,
        ai_elapsed_ms,
        timing_ok,
        timing_steps,
    );

    if let Some(remaining) = router.get_managed_free_remaining() {
        let _ = app.emit("balance_update", remaining);
    }
    if let Some(coins) = router.get_managed_coin_balance() {
        let _ = app.emit("coin_balance_update", coins);
    }
    if let Some((tier, price_micro)) = router.take_managed_tier_auto_selected() {
        let _ = app.emit("tier_auto_selected", (tier, price_micro));
    }

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            let err_str = e.to_string();
            if err_str == "free_trial_exhausted" {
                let _ = app.emit("trial_exhausted", ());
                return Err("Your free requests have been used.".to_string());
            } else if err_str == "insufficient_coins" {
                let _ = app.emit("insufficient_coins", ());
                return Err("Not enough coins for this quality tier. Buy more to continue.".to_string());
            }
            return Err(err_str);
        }
    };

    let mut steps = response.steps;
    // Rule-14 defense in depth — same sanitation as guide().
    ai::types::sanitize_steps(&mut steps);
    let steps = steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    // Workstream P: same source-gating as guide().
    let suggested_tasks = if router.config.task_suggestions {
        response.suggested_tasks
    } else {
        Vec::new()
    };
    let provider = router.config.api_provider.clone();
    let bbox_distrust = router.config.bbox_distrust_models.clone();

    if let Some(session) = &mut router.session_manager.current_session {
        session.update_state(state_summary.clone());
        session.add_turn("user", user_text.to_string(), None);
        let content = steps
            .iter()
            .map(|s| s.instruction.clone())
            .collect::<Vec<_>>()
            .join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    // Release the Mutex before execute_step — same pattern as guide().
    // The locator runs blocking UIA/OCR calls that can take 1-3 s; holding the
    // Mutex during that time would deadlock any concurrent Tauri command.
    drop(router);

    {
        let mut g = state.guidance.lock();
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
        g.capture_rect = new_capture_rect;
        g.target_hwnd = new_hwnd;
        // Stored even when None — a skipped enumeration must clear the previous
        // snapshot, or next_step would resolve ids against a stale list.
        g.context_elements = context_elements.clone();
        // These steps came from THIS request — see guide()'s matching line.
        g.request_id = Some(request_id.clone());
    }

    if steps.is_empty() {
        let post_hash = anchor_autopilot_baseline(&state).await;
        emit_stale_if_drifted(&app, pre_hash, post_hash);
        return Ok(GuideResponse {
            ok: true,
            session_id,
            request_id: Some(request_id),
            steps,
            step_index: 0,
            instruction: String::new(),
            located: None,
            needs_input,
            provider,
            model: Some(used_model.clone()),
            input_tokens: Some(in_tok),
            output_tokens: Some(out_tok),
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
            suggested_tasks,
            hint_shown: false,
            candidates: Vec::new(),
        });
    }

    let (log_trace, debug_screenshot_enabled) = {
        let cfg = &state.ai_router.lock().await.config;
        (
            cfg.debug_locate_log_file_enabled,
            cfg.debug_screenshot_enabled,
        )
    };
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path()
            .app_local_data_dir()
            .ok()
            .map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[0], new_capture_rect, &provider);
    let bbox_decisive = ai::bbox::bbox_is_decisive(&used_model, &bbox_distrust);

    // Stale detection before the pointer is drawn — see guide() for rationale.
    // Hash the same window the correction capture used (C5).
    let stale_target = if is_fs { None } else { new_hwnd };
    let stale_hash = tokio::task::spawn_blocking(move || ahash_of_screen(stale_target))
        .await
        .ok()
        .flatten();
    emit_stale_if_drifted(&app, pre_hash, stale_hash);

    // Scope the rejections to the NEW step's target — the AI may have re-targeted,
    // and only rejections recorded against this exact target still apply.
    let scoped = locator::candidates::scoped_avoid(
        avoid_bboxes.as_deref().unwrap_or(&[]),
        steps[0].target_text.as_deref(),
    );
    let (located, mut locate_trace, hint_shown, shown_candidates) = execute_step(
        &app,
        &steps[0],
        new_hwnd,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        bbox_decisive,
        scoped,
        new_capture_rect,
        pre_ocr,
        &state.packs,
        context_elements,
        None,
        &[],
    )
    .unwrap_or((None, None, false, Vec::new()));
    arm_candidates_if_shown(
        &state,
        Some(request_id.clone()),
        steps[0].target_text.as_deref(),
        &shown_candidates,
        new_hwnd,
    )
    .await;
    if let Some(ref mut t) = locate_trace {
        t.request_id = Some(request_id.clone());
        t.model = Some(used_model.clone());
        t.provider = Some(provider.clone());
        t.input_tokens = Some(in_tok);
        t.output_tokens = Some(out_tok);
        t.ai_elapsed_ms = Some(ai_elapsed_ms as u32);
        t.app_name = trace_app_name(new_hwnd);
        maybe_log_trace(&app, t, log_trace, training_enabled);
    }

    // Anchor the autopilot baseline AFTER the pointer is drawn (pointer-inclusive).
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        request_id: Some(request_id),
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        provider,
        model: Some(used_model),
        input_tokens: Some(in_tok),
        output_tokens: Some(out_tok),
        error: None,
        debug_screenshot_path,
        chat_thumb_b64,
        locate_trace,
        ai_bbox,
        suggested_tasks,
        hint_shown,
        candidates: shown_candidates,
    })
}

#[tauri::command]
fn speak(
    text: String,
    lang: Option<String>,
    // The user's ORIGINAL request text — in "auto" mode its script outranks the OS
    // locale when the reply itself is Latin-ambiguous (the LANGUAGE rule pins the reply
    // language to the request; design suggestion #7). Reply script still wins when strong.
    request_hint: Option<String>,
    // OS UI locale (navigator.language) — the last-resort fallback when both the reply
    // and the request are Latin script (script detection can't tell en/fr/es/de apart). C7.
    fallback_locale: Option<String>,
    state: State<'_, AppState>,
) {
    state.tts.speak(
        text,
        lang.unwrap_or_default(),
        request_hint.unwrap_or_default(),
        fallback_locale.unwrap_or_default(),
    );
}

/// Focus give-back (locator-testing.md 2026-07-17): after the user interacts
/// with the PANEL by mouse (task submit, → Next click), hand OS focus straight
/// back to the target window so their next physical click on the target acts
/// immediately instead of being consumed by activation (the "click once for
/// focus, then click again" annoyance the Ctrl+~ hotkey never had). Safe to
/// call unconditionally: `focus_window_if_own_foreground` is a no-op unless a
/// window of our own process currently holds the foreground — so the hotkey
/// and autopilot paths (target already focused) are untouched by construction.
#[tauri::command]
fn focus_target_window(state: State<'_, AppState>) {
    let hwnd = {
        let g = state.guidance.lock();
        if g.full_screen_mode {
            None // no single target to hand focus to
        } else {
            g.pinned_hwnd.or(g.target_hwnd)
        }
    };
    if let Some(h) = hwnd {
        let _ = capture::focus_window_if_own_foreground(h);
    }
}

#[tauri::command]
async fn clear_overlay(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.tracker.clear();
    match overlay::make_update(overlay::OverlayKind::None, None, None) {
        Ok(update) => overlay::emit_update(&app, update).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn restore_overlay(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let last = match state.last_overlay.lock().clone() {
        Some(l) => l,
        None => return Ok(()),
    };
    let update =
        overlay::make_update_with_ai_bbox(last.kind, last.bbox, last.text.clone(), last.ai_bbox)
            .map_err(|e| e.to_string())?;
    overlay::emit_update(&app, update).map_err(|e| e.to_string())?;
    // Re-arm the tracker — clear_overlay stopped it, so without this the pointer would
    // no longer follow the window or auto-hide/redraw with the target's visibility.
    if let Some(b) = last.bbox {
        state.tracker.start(
            &b,
            last.kind,
            last.text,
            app.clone(),
            last.target_hwnd,
            true,
        );
    }
    Ok(())
}

/// Item 5 — enumerate installed TTS voices for the Settings voice picker.
#[tauri::command]
fn list_tts_voices(state: State<'_, AppState>) -> Vec<tts::VoiceInfo> {
    state.tts.list_voices()
}

/// Item 1 — enumerate candidate windows for the target-picker dropdown.
#[tauri::command]
fn list_target_windows() -> Vec<capture::TargetWindowInfo> {
    #[cfg(windows)]
    {
        capture::list_target_windows()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

/// Item 1 — pin a specific window as the guidance target. Survives new tasks;
/// only cleared by `unpin_target_window` or when the window is no longer valid.
#[tauri::command]
fn pin_target_window(app: AppHandle, state: State<'_, AppState>, hwnd: usize) {
    {
        let mut g = state.guidance.lock();
        g.pinned_hwnd = Some(hwnd);
        g.target_hwnd = Some(hwnd);
        g.full_screen_mode = false; // a specific window and full-screen are mutually exclusive
        g.full_screen_monitor = None;
        g.last_announced_hwnd = Some(hwnd);
    }
    #[cfg(windows)]
    announce_shared_app(&app, Some(hwnd), true);
    #[cfg(not(windows))]
    let _ = app;
}

/// Select a full-screen capture target — the user-initiated replacement for the old
/// AI-requested full-screen consent flow. `monitor_index` (from `list_monitors`) pins a
/// single screen; `None` shares the whole virtual desktop (single-monitor systems). On a
/// multi-monitor setup the picker only offers individual screens — a stitched all-screens
/// capture is downscaled past the point of usefulness. Sticky like a pin: every
/// subsequent capture grabs this scope until the user picks a window or Auto-detect.
#[tauri::command]
fn pin_full_screen_target(state: State<'_, AppState>, monitor_index: Option<usize>) {
    #[cfg(windows)]
    let monitor: Option<capture::Rect> = monitor_index.and_then(capture::monitor_rect);
    #[cfg(not(windows))]
    let monitor: Option<capture::Rect> = {
        let _ = monitor_index;
        None
    };
    let mut g = state.guidance.lock();
    g.full_screen_mode = true;
    g.full_screen_monitor = monitor;
    g.pinned_hwnd = None;
}

/// List connected monitors for the target picker's per-screen "share this screen" choices.
#[tauri::command]
fn list_monitors() -> Vec<capture::MonitorInfo> {
    #[cfg(windows)]
    {
        capture::list_monitors()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

/// Reset target_hwnd (and session state) when the user explicitly starts a new task.
/// Called by the "＋ New task" button in the panel. Preserves pinned_hwnd — the user
/// explicitly chose that window and it should survive a session reset.
#[tauri::command]
fn new_session(state: State<'_, AppState>) {
    let mut g = state.guidance.lock();
    g.session_id = None;
    g.steps = vec![];
    g.state_summary = String::new();
    g.target_hwnd = None;
}

/// Item 1 — clear the pinned window and return to auto-detection.
#[tauri::command]
fn unpin_target_window(app: AppHandle, state: State<'_, AppState>) {
    {
        let mut g = state.guidance.lock();
        g.pinned_hwnd = None;
        g.full_screen_mode = false; // back to the active-window default
        g.full_screen_monitor = None;
        // target_hwnd retains the last auto-discovered window for the current session.
    }
    #[cfg(windows)]
    refresh_active_window(&app);
}

/// Phase 0.2: structured info about the window being shared with the AI.
/// Used by the panel to show the "Shared: <App>" header chip.
#[tauri::command]
fn get_shared_app_info(state: State<'_, AppState>) -> Option<SharedAppInfoPayload> {
    #[cfg(windows)]
    {
        let stored = {
            let g = state.guidance.lock();
            g.pinned_hwnd.or(g.target_hwnd)
        };
        let info = match stored {
            Some(hwnd) => {
                capture::get_window_info_for_hwnd(hwnd).or_else(capture::get_active_window_info)
            }
            None => capture::get_active_window_info(),
        };
        info.map(|i| SharedAppInfoPayload {
            hwnd: i.hwnd as u64,
            rect: i.rect,
            app_name: i.app_name,
            exe_name: i.exe_name,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        None
    }
}

#[tauri::command]
async fn ping_sidecar(_state: State<'_, AppState>) -> Result<String, String> {
    Ok("pong".to_string())
}

#[tauri::command]
async fn sidecar_echo(text: String, _state: State<'_, AppState>) -> Result<String, String> {
    Ok(text)
}

#[tauri::command]
async fn capture_screen(quality: Option<u8>) -> Result<CaptureResult, String> {
    let q = quality.unwrap_or(80);
    let start = std::time::Instant::now();
    let bytes = tokio::task::spawn_blocking(move || capture::capture_primary_monitor_jpeg(q))
        .await
        .map_err(|e| format!("task join: {e}"))?
        .map_err(|e| e.to_string())?;
    let (w, h) = image::load_from_memory(&bytes)
        .map(|img| (img.width(), img.height()))
        .unwrap_or((0, 0));
    Ok(CaptureResult {
        jpeg_base64: capture::to_base64(&bytes),
        width: w,
        height: h,
        crop_rect: None,
        bytes: bytes.len(),
        elapsed_ms: start.elapsed().as_millis(),
    })
}

fn emit_box_overlay(app: &AppHandle, result: &locator::LocateResult) {
    match overlay::make_update(overlay::OverlayKind::Box, Some(result.bbox), None) {
        Ok(update) => {
            if let Err(e) = overlay::emit_update(app, update) {
                log::warn!("overlay emit failed: {e}");
            }
        }
        Err(e) => log::warn!("overlay make_update failed: {e}"),
    }
}

#[tauri::command]
async fn locate_a11y(
    app: AppHandle,
    text: String,
    role: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Option<locator::LocateResult>, String> {
    #[cfg(windows)]
    {
        let opts = locator::orchestrator::LocateOptions {
            role,
            nearby_text: None,
            ai_bbox: None,
            bbox_decisive: false,
            avoid_bboxes: Vec::new(),
            a11y_timeout_ms: timeout_ms.unwrap_or(1500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
            icon_templates: Vec::new(),
            icon_region: None,
            icon_target: false,
            icon_authoring_scale: 1.0,
            context_elements: None,
            selected_element_id: None,
        };
        let (result, _trace) =
            tokio::task::spawn_blocking(move || locator::a11y::find_element(&text, &opts))
                .await
                .map_err(|e| format!("task join: {e}"))?
                .map_err(|e| e.to_string())?;
        if let Some(ref r) = result {
            emit_box_overlay(&app, r);
        }
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, text, role, timeout_ms);
        Err("A11y only implemented for Windows".to_string())
    }
}

#[tauri::command]
async fn locate_element(
    app: AppHandle,
    text: String,
    role: Option<String>,
    nearby_text: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Option<locator::LocateResult>, String> {
    #[cfg(windows)]
    {
        let opts = locator::orchestrator::LocateOptions {
            role,
            nearby_text,
            ai_bbox: None,
            bbox_decisive: false,
            avoid_bboxes: Vec::new(),
            a11y_timeout_ms: timeout_ms.unwrap_or(500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
            icon_templates: Vec::new(),
            icon_region: None,
            icon_target: false,
            icon_authoring_scale: 1.0,
            context_elements: None,
            selected_element_id: None,
        };
        let (result, _trace) =
            tokio::task::spawn_blocking(move || locator::orchestrator::locate(&text, &opts, None))
                .await
                .map_err(|e| format!("task join: {e}"))?
                .map_err(|e| e.to_string())?;
        if let Some(ref r) = result {
            emit_box_overlay(&app, r);
        }
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, text, role, nearby_text, timeout_ms);
        Err("locate_element only implemented for Windows".to_string())
    }
}

#[tauri::command]
async fn capture_active_window(quality: Option<u8>) -> Result<CaptureResult, String> {
    let q = quality.unwrap_or(80);
    let start = std::time::Instant::now();
    let (bytes, rect, _hwnd) = tokio::task::spawn_blocking(move || {
        let exclude = capture::get_panel_rects();
        capture::capture_active_window_jpeg(q, &exclude)
    })
    .await
    .map_err(|e| format!("task join: {e}"))?
    .map_err(|e| e.to_string())?;
    let (w, h) = image::load_from_memory(&bytes)
        .map(|img| (img.width(), img.height()))
        .unwrap_or((0, 0));
    Ok(CaptureResult {
        jpeg_base64: capture::to_base64(&bytes),
        width: w,
        height: h,
        crop_rect: Some(rect),
        bytes: bytes.len(),
        elapsed_ms: start.elapsed().as_millis(),
    })
}

/// Open the debug screenshot folder in Windows Explorer (creates it if missing).
/// Gated behind NAVISUAL_DEV — public installs reject the call.
#[tauri::command]
async fn open_debug_folder(app: AppHandle) -> Result<(), String> {
    if !developer_mode_enabled() {
        return Err("Developer mode not enabled".into());
    }
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?
        .join("debug");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir: {e}"))?;
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("explorer: {e}"))?;
    }
    Ok(())
}

/// List the models installed on an Ollama server (`GET /api/tags`) so the
/// Settings → Ollama model dropdown can offer the user's actual pulled models
/// instead of a hardcoded guess. Returns sorted model names (e.g. "gemma4:e4b").
/// Best-effort: returns an error string the UI shows inline when the server is
/// unreachable, so the user falls back to typing the name.
#[tauri::command]
async fn list_ollama_models(base_url: String) -> Result<Vec<String>, String> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Ollama server returned {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut models: Vec<String> = body
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    Ok(models)
}

/// One row of the Settings → Usage panel: token totals + estimated cost for a model.
#[derive(serde::Serialize)]
struct UsageRow {
    provider: String,
    model: String,
    daily_in: u64,
    daily_out: u64,
    monthly_in: u64,
    monthly_out: u64,
    /// Estimated USD: Some(0.0)=free (local), Some(n)=priced BYOK, None=managed or unknown model.
    daily_cost: Option<f64>,
    monthly_cost: Option<f64>,
    free: bool,
}

#[derive(serde::Serialize)]
struct UsagePayload {
    rows: Vec<UsageRow>,
    /// Managed free-tier requests remaining (the metric that matters there, not tokens).
    managed_free_remaining: Option<u32>,
}

/// Per-(provider, model) token usage + estimated cost for the Settings → Usage panel.
/// Costs are estimates from list pricing (see `ai/pricing.rs`); the UI discloses this.
#[tauri::command]
async fn get_usage(state: State<'_, AppState>) -> Result<UsagePayload, String> {
    let mut router = state.ai_router.lock().await;
    let breakdown = router.cost_tracker.breakdown();
    let managed_free_remaining = router.get_managed_free_remaining();
    let rows = breakdown
        .into_iter()
        .map(|(key, u)| {
            let (provider, model) = key.split_once('|').unwrap_or(("", key.as_str()));
            UsageRow {
                provider: provider.to_string(),
                model: model.to_string(),
                daily_in: u.daily_in,
                daily_out: u.daily_out,
                monthly_in: u.monthly_in,
                monthly_out: u.monthly_out,
                daily_cost: crate::ai::pricing::estimate_cost(
                    provider,
                    model,
                    u.daily_in,
                    u.daily_out,
                ),
                monthly_cost: crate::ai::pricing::estimate_cost(
                    provider,
                    model,
                    u.monthly_in,
                    u.monthly_out,
                ),
                free: provider == "ollama",
            }
        })
        .collect();
    Ok(UsagePayload {
        rows,
        managed_free_remaining,
    })
}

/// Clear all recorded token usage (Settings → Usage → Reset).
#[tauri::command]
async fn reset_usage(state: State<'_, AppState>) -> Result<(), String> {
    state.ai_router.lock().await.cost_tracker.reset();
    Ok(())
}

#[tauri::command]
async fn get_settings(state: State<'_, AppState>) -> Result<SettingsPayload, String> {
    let router = state.ai_router.lock().await;
    let c = &router.config;
    Ok(SettingsPayload {
        api_provider: c.api_provider.clone(),
        anthropic_api_key: c.anthropic_api_key.clone().unwrap_or_default(),
        anthropic_model: c.anthropic_model.clone(),
        anthropic_fast_model: c.anthropic_fast_model.clone(),
        gemini_api_key: c.gemini_api_key.clone().unwrap_or_default(),
        gemini_model: c.gemini_model.clone(),
        gemini_fast_model: c.gemini_fast_model.clone(),
        ollama_base_url: c.ollama_base_url.clone(),
        ollama_model: c.ollama_model.clone(),
        openai_api_key: c.openai_api_key.clone().unwrap_or_default(),
        openai_model: c.openai_model.clone(),
        deepseek_api_key: c.deepseek_api_key.clone().unwrap_or_default(),
        deepseek_model: c.deepseek_model.clone(),
        qwen_api_key: c.qwen_api_key.clone().unwrap_or_default(),
        qwen_model: c.qwen_model.clone(),
        qwen_base_url: c.qwen_base_url.clone(),
        custom_api_key: c.custom_api_key.clone().unwrap_or_default(),
        custom_model: c.custom_model.clone(),
        custom_base_url: c.custom_base_url.clone(),
        managed_tier: c.managed_tier.clone(),
        overlay_color: c.overlay_color.clone(),
        overlay_thickness: c.overlay_thickness,
        subtitle_enabled: c.subtitle_enabled,
        auto_advance: c.auto_advance,
        tts_enabled: c.tts_enabled,
        tts_voice: c.tts_voice.clone(),
        voice_input_enabled: c.voice_input_enabled,
        voice_language: c.voice_language.clone(),
        hotkey_next: c.hotkey_next.clone(),
        hotkey_wrong: c.hotkey_wrong.clone(),
        hotkey_pause: c.hotkey_pause.clone(),
        hotkey_icon: c.hotkey_icon.clone(),
        hotkey_talk: c.hotkey_talk.clone(),
        debug_screenshot_enabled: c.debug_screenshot_enabled,
        debug_show_response_info: c.debug_show_response_info,
        debug_locate_trace_enabled: c.debug_locate_trace_enabled,
        debug_locate_log_file_enabled: c.debug_locate_log_file_enabled,
        debug_prompt_log_file_enabled: c.debug_prompt_log_file_enabled,
        training_capture_enabled: c.training_capture_enabled,
        task_suggestions: c.task_suggestions,
        debug_show_ai_bbox: c.debug_show_ai_bbox,
        developer_mode: developer_mode_enabled(),
    })
}

/// Returns true when the process was launched with NAVISUAL_DEV=true or =1.
/// Read live so unsetting + relaunching reverts the gate without a save.
fn developer_mode_enabled() -> bool {
    matches!(
        std::env::var("NAVISUAL_DEV").as_deref(),
        Ok("true") | Ok("1")
    )
}

#[tauri::command]
async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: SettingsPayload,
) -> Result<(), String> {
    let env_path = state.env_path.clone();

    // Always-written settings (non-sensitive)
    let mut updates: Vec<(String, String)> = vec![
        ("API_PROVIDER".into(), payload.api_provider.clone()),
        ("ANTHROPIC_MODEL".into(), payload.anthropic_model.clone()),
        (
            "ANTHROPIC_FAST_MODEL".into(),
            payload.anthropic_fast_model.clone(),
        ),
        ("GEMINI_MODEL".into(), payload.gemini_model.clone()),
        (
            "GEMINI_FAST_MODEL".into(),
            payload.gemini_fast_model.clone(),
        ),
        ("OLLAMA_BASE_URL".into(), payload.ollama_base_url.clone()),
        ("OLLAMA_MODEL".into(), payload.ollama_model.clone()),
        ("OPENAI_MODEL".into(), payload.openai_model.clone()),
        ("DEEPSEEK_MODEL".into(), payload.deepseek_model.clone()),
        ("QWEN_MODEL".into(), payload.qwen_model.clone()),
        ("QWEN_BASE_URL".into(), payload.qwen_base_url.clone()),
        ("CUSTOM_MODEL".into(), payload.custom_model.clone()),
        ("CUSTOM_BASE_URL".into(), payload.custom_base_url.clone()),
        ("MANAGED_TIER".into(), payload.managed_tier.clone()),
        ("OVERLAY_COLOR".into(), payload.overlay_color.clone()),
        (
            "OVERLAY_THICKNESS".into(),
            payload.overlay_thickness.to_string(),
        ),
        (
            "SUBTITLE_ENABLED".into(),
            payload.subtitle_enabled.to_string(),
        ),
        ("AUTO_ADVANCE".into(), payload.auto_advance.to_string()),
        ("TTS_ENABLED".into(), payload.tts_enabled.to_string()),
        ("TTS_VOICE".into(), payload.tts_voice.clone()),
        (
            "VOICE_INPUT_ENABLED".into(),
            payload.voice_input_enabled.to_string(),
        ),
        ("VOICE_LANGUAGE".into(), payload.voice_language.clone()),
        ("HOTKEY_NEXT".into(), payload.hotkey_next.clone()),
        ("HOTKEY_WRONG".into(), payload.hotkey_wrong.clone()),
        ("HOTKEY_PAUSE".into(), payload.hotkey_pause.clone()),
        ("HOTKEY_ICON".into(), payload.hotkey_icon.clone()),
        ("HOTKEY_TALK".into(), payload.hotkey_talk.clone()),
        (
            "DEBUG_SCREENSHOT_ENABLED".into(),
            payload.debug_screenshot_enabled.to_string(),
        ),
        (
            "DEBUG_SHOW_RESPONSE_INFO".into(),
            payload.debug_show_response_info.to_string(),
        ),
        (
            "DEBUG_LOCATE_TRACE_ENABLED".into(),
            payload.debug_locate_trace_enabled.to_string(),
        ),
        (
            "DEBUG_LOCATE_LOG_FILE_ENABLED".into(),
            payload.debug_locate_log_file_enabled.to_string(),
        ),
        (
            "DEBUG_PROMPT_LOG_FILE_ENABLED".into(),
            payload.debug_prompt_log_file_enabled.to_string(),
        ),
        (
            "TRAINING_CAPTURE_ENABLED".into(),
            payload.training_capture_enabled.to_string(),
        ),
        (
            "TASK_SUGGESTIONS".into(),
            payload.task_suggestions.to_string(),
        ),
        (
            "DEBUG_SHOW_AI_BBOX".into(),
            payload.debug_show_ai_bbox.to_string(),
        ),
    ];

    // API keys: only overwrite if the user actually typed something. A typed key
    // goes to the Windows Credential Manager; .env gets the sentinel (see
    // credvault.rs). If the vault write fails, fall back to plaintext .env so
    // the key is never lost — Config::load handles both forms.
    for (env_name, typed) in [
        ("ANTHROPIC_API_KEY", &payload.anthropic_api_key),
        ("GEMINI_API_KEY", &payload.gemini_api_key),
        ("OPENAI_API_KEY", &payload.openai_api_key),
        ("DEEPSEEK_API_KEY", &payload.deepseek_api_key),
        ("QWEN_API_KEY", &payload.qwen_api_key),
        ("CUSTOM_API_KEY", &payload.custom_api_key),
    ] {
        let typed = typed.trim();
        if typed.is_empty() || typed == credvault::SENTINEL {
            // Nothing typed (or the masked sentinel round-tripped) — leave as-is.
            continue;
        }
        if credvault::store(env_name, typed) {
            updates.push((env_name.into(), credvault::SENTINEL.into()));
        } else {
            updates.push((env_name.into(), typed.into()));
        }
    }

    // Atomic write to .env
    let refs: Vec<(&str, &str)> = updates
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    update_env_file(&env_path, &refs)?;

    // Propagate to current process so Config::load() picks them up
    for (key, value) in &updates {
        std::env::set_var(key, value);
    }

    // Reload config and reinitialize the AI client
    let new_config = Config::load(Some(&env_path));
    {
        let mut router = state.ai_router.lock().await;
        router.reload_config(new_config);
    }

    // Apply TTS voice immediately (no restart required).
    state.tts.set_voice(payload.tts_voice.clone());

    // Notify the overlay canvas of the new theme (broadcasts to all webview windows).
    let _ = app.emit(
        "overlay:theme",
        OverlayThemePayload {
            color: payload.overlay_color,
            thickness: payload.overlay_thickness,
            subtitle_enabled: payload.subtitle_enabled,
        },
    );

    Ok(())
}

#[derive(serde::Serialize)]
struct SessionStatus {
    signed_in: bool,
    free_remaining: Option<u32>,
}

/// Sign in anonymously to Supabase (managed provider). Idempotent — if a
/// session is already loaded (from `supabase_session.json` or a previous
/// call), this returns immediately. Without this guard every onMount would
/// mint a brand-new anon user (and a fresh 50-request quota), defeating
/// the trial cap. Refresh of expired sessions is handled by ensure_token()
/// before each AI call.
#[tauri::command]
async fn sign_in_anon(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    // Already have a session (from disk or a prior call)? Don't mint another.
    {
        let sess = state.supabase_session.lock().await;
        if sess.is_some() {
            let router = state.ai_router.lock().await;
            return Ok(SessionStatus {
                signed_in: true,
                free_remaining: router.get_managed_free_remaining(),
            });
        }
    }

    let (supabase_url, anon_key) = {
        let router = state.ai_router.lock().await;
        let url = router
            .config
            .supabase_url
            .clone()
            .ok_or("SUPABASE_URL not configured")?;
        let key = router
            .config
            .supabase_anon_key
            .clone()
            .ok_or("SUPABASE_ANON_KEY not configured")?;
        (url, key)
    };

    let new_session = server::sign_in_anonymously(&supabase_url, &anon_key)
        .await
        .map_err(|e| e.to_string())?;

    server::save_session(&state.supabase_session_path, &new_session);
    save_app_session(&state, new_session.clone()).await;

    Ok(SessionStatus {
        signed_in: true,
        free_remaining: None,
    })
}

/// Fetch the managed-provider balance (tier, free_remaining, coin_balance_microdollars).
#[tauri::command]
async fn get_balance(state: State<'_, AppState>) -> Result<server::BalanceResponse, String> {
    let (supabase_url, access_token) = {
        let (url, _key) = managed_url_key(&state).await?;
        let token = acct_session_token(&state).await?;
        (url, token)
    };
    let balance = server::get_balance(&supabase_url, &access_token)
        .await
        .map_err(|e| e.to_string())?;
    // Record the billing tier so Structured-Context can be gated off for the managed
    // free tier before the first request even runs (the startup balance fetch sets it).
    state
        .ai_router
        .lock()
        .await
        .set_managed_billing_tier(&balance.tier);
    Ok(balance)
}

/// Sign in with Google via PKCE OAuth in the system browser.
///
/// **In-place identity linking (S.2.1 §4).** Opens the Google consent page in the
/// default browser, runs a loopback HTTP server on port 9876 for the callback,
/// exchanges the code for a session, and emits `oauth_complete` + `account_changed`.
///
/// The flow tries to **link** the Google identity onto the *current* (anonymous)
/// user first — preserving its `user_profiles` row (free-request count + coins),
/// matching the email-upgrade path. It falls back to the original **replace**
/// sign-in (a fresh session for the Google account) when in-place linking can't
/// apply:
///   * "Manual linking" is disabled in the dashboard, or there is no session to
///     link onto → the link init fails before the browser opens; or
///   * the Google identity already belongs to a *different* Navisual account
///     (a returning user) → GoTrue reports it on the callback. We then sign them
///     into that existing account (their coins live there, not on the throwaway
///     anon session); Google usually skips the second prompt since consent was
///     just granted.
#[tauri::command]
async fn start_google_oauth(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (supabase_url, anon_key) = managed_url_key(&state).await?;

    let pkce = server::generate_pkce(9876);

    // Bind the callback port FIRST so a busy port (a prior attempt still waiting)
    // fails fast before we send the user to Google. One listener serves the whole
    // flow — including the in-place-link → replace fallback's second round-trip —
    // so we never rebind (which could race the just-closed port on Windows).
    let listener = server::bind_callback_listener(pkce.port)
        .await
        .map_err(|e| e.to_string())?;

    // A Bearer token for the current session is required to link in place. With no
    // session yet, there's nothing to preserve → go straight to replace.
    let access_token = acct_session_token(&state).await.ok();
    let Some(access_token) = access_token else {
        return google_oauth_replace(&state, &app, &supabase_url, &anon_key, &listener).await;
    };

    // 1) Ask GoTrue for the in-place link consent URL (Bearer = current session).
    let consent_url = match server::link_identity_url(
        &supabase_url,
        &anon_key,
        &access_token,
        "google",
        &pkce,
    )
    .await
    {
        Ok(url) => url,
        Err(e) => {
            // Manual linking off / not linkable → degrade to the replace sign-in so
            // Google sign-in still works (without the in-place row-preserve benefit).
            log::warn!("[oauth] in-place link unavailable ({e}); using replace flow");
            return google_oauth_replace(&state, &app, &supabase_url, &anon_key, &listener).await;
        }
    };

    tauri_plugin_opener::open_url(&consent_url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {e}"))?;

    match server::accept_oauth_callback(&listener)
        .await
        .map_err(|e| e.to_string())?
    {
        server::OAuthCallback::Code(code) => {
            // Same user id, now carrying the Google identity → the row is preserved.
            let session =
                server::exchange_pkce_code(&supabase_url, &anon_key, &code, &pkce.verifier)
                    .await
                    .map_err(|e| e.to_string())?;
            server::save_session(&state.supabase_session_path, &session);
            save_app_session(&state, session).await;
            let _ = app
                .get_webview_window("panel")
                .map(|w| w.emit("oauth_complete", ()));
            emit_account_changed(&app);
            Ok(())
        }
        server::OAuthCallback::Error { error, description } => {
            // Surface the exact GoTrue error so a returning-user/conflict callback is
            // diagnosable live (and confirms the heuristic below matched its wording).
            log::info!("[oauth] link callback returned error: {error} — {description}");
            if oauth_identity_already_linked(&format!("{error} {description}")) {
                // Returning user: this Google account is attached to a DIFFERENT
                // Navisual account. Sign into that one (replace) so they recover it,
                // reusing the still-bound listener for the second round-trip.
                log::info!("[oauth] google identity already linked elsewhere; signing in to it");
                google_oauth_replace(&state, &app, &supabase_url, &anon_key, &listener).await
            } else {
                Err(oauth_error_message(&error, &description))
            }
        }
    }
}

/// The original "replace the session" Google sign-in. Used as the fallback from
/// `start_google_oauth` when in-place linking can't apply (manual linking off, no
/// session to link onto, or the identity already belongs to another account).
/// Loads/mints the Google account's OWN session, replacing the current one.
/// Reuses the caller's already-bound loopback `listener` (no rebind).
async fn google_oauth_replace(
    state: &State<'_, AppState>,
    app: &tauri::AppHandle,
    supabase_url: &str,
    anon_key: &str,
    listener: &tokio::net::TcpListener,
) -> Result<(), String> {
    let pkce = server::generate_pkce(9876);
    let auth_url = server::google_oauth_url(supabase_url, &pkce);

    tauri_plugin_opener::open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {e}"))?;

    let code = match server::accept_oauth_callback(listener)
        .await
        .map_err(|e| e.to_string())?
    {
        server::OAuthCallback::Code(c) => c,
        server::OAuthCallback::Error { error, description } => {
            return Err(oauth_error_message(&error, &description));
        }
    };

    let new_session = server::exchange_pkce_code(supabase_url, anon_key, &code, &pkce.verifier)
        .await
        .map_err(|e| e.to_string())?;

    server::save_session(&state.supabase_session_path, &new_session);
    save_app_session(state, new_session).await;
    let _ = app
        .get_webview_window("panel")
        .map(|w| w.emit("oauth_complete", ()));
    emit_account_changed(app);
    Ok(())
}

/// True when an OAuth callback error means the provider identity is already
/// attached to a different account (so an in-place link can't proceed and we
/// should sign into that existing account instead).
fn oauth_identity_already_linked(haystack: &str) -> bool {
    let h = haystack.to_lowercase();
    h.contains("already")
        && (h.contains("link")
            || h.contains("regist")
            || h.contains("exist")
            || h.contains("identit"))
}

/// Human-readable message for a non-recoverable OAuth callback error.
fn oauth_error_message(error: &str, description: &str) -> String {
    let detail = if description.is_empty() { error } else { description };
    if detail.is_empty() {
        "Google sign-in failed.".to_string()
    } else {
        format!("Google sign-in failed: {detail}")
    }
}

/// Open a Stripe Checkout session for a coin top-up.
/// Returns the checkout URL. The frontend is responsible for opening it
/// (via tauri-plugin-opener) so the system browser handles the payment page.
#[tauri::command]
async fn create_checkout(
    state: State<'_, AppState>,
    amount_usd: f64,
) -> Result<String, String> {
    let (supabase_url, access_token) = {
        let (url, _key) = managed_url_key(&state).await?;
        let token = acct_session_token(&state).await?;
        (url, token)
    };

    let amount = if amount_usd > 0.0 { amount_usd } else { 20.0 };
    server::create_checkout_session(&supabase_url, &access_token, amount)
        .await
        .map_err(|e| e.to_string())
}

// ── Email / password auth + account management (S.2.1) ───────────────────────

/// (supabase_url, anon_key) from config. Lightweight — no token needed
/// (used by calls that authenticate with the anon key only: sign-in, verify,
/// recover, fresh anonymous sign-in).
async fn managed_url_key(state: &State<'_, AppState>) -> Result<(String, String), String> {
    let router = state.ai_router.lock().await;
    let url = router
        .config
        .supabase_url
        .clone()
        .ok_or("SUPABASE_URL not configured")?;
    let key = router
        .config
        .supabase_anon_key
        .clone()
        .ok_or("SUPABASE_ANON_KEY not configured")?;
    Ok((url, key))
}

/// Store a Supabase session in the provider-independent AppState slot AND sync
/// it to the router's ManagedClient (if present) so relay requests use it too.
async fn save_app_session(state: &State<'_, AppState>, session: server::SupabaseSession) {
    *state.supabase_session.lock().await = Some(session.clone());
    let mut router = state.ai_router.lock().await;
    router.set_managed_session(session);
}

/// (supabase_url, anon_key, access_token) — provider-independent.
/// Uses the AppState session (not the router's ManagedClient) so account
/// commands work regardless of which `API_PROVIDER` is active.
async fn managed_auth_ctx(
    state: &State<'_, AppState>,
) -> Result<(String, String, String), String> {
    let (url, key) = managed_url_key(state).await?;
    let token = acct_session_token(state).await?;
    Ok((url, key, token))
}

/// Get the current Supabase access token from the AppState session, refreshing
/// it first if expired. This is provider-independent — works whether the active
/// AI provider is `managed`, `gemini`, `anthropic`, etc.
async fn acct_session_token(
    state: &State<'_, AppState>,
) -> Result<String, String> {
    let mut sess_guard = state.supabase_session.lock().await;
    match sess_guard.as_ref() {
        Some(s) if !s.is_expired() => Ok(s.access_token.clone()),
        Some(s) => {
            // Expired — try refreshing.
            let refresh_token = s.refresh_token.clone();
            let (url, key) = {
                let router = state.ai_router.lock().await;
                let url = router.config.supabase_url.clone()
                    .ok_or("SUPABASE_URL not configured")?;
                let key = router.config.supabase_anon_key.clone()
                    .ok_or("SUPABASE_ANON_KEY not configured")?;
                (url, key)
            };
            let refreshed = server::refresh_session(&url, &key, &refresh_token)
                .await
                .map_err(|e| format!("Session refresh failed: {e}"))?;
            server::save_session(&state.supabase_session_path, &refreshed);
            let token = refreshed.access_token.clone();
            // Sync to ManagedClient if active so relay requests use the fresh token.
            {
                let mut router = state.ai_router.lock().await;
                router.set_managed_session(refreshed.clone());
            }
            *sess_guard = Some(refreshed);
            Ok(token)
        }
        None => Err("Not signed in".to_string()),
    }
}

/// Replace the on-disk + in-memory session with a fresh anonymous one (used after
/// sign-out and account deletion so the free tier keeps working). Returns the
/// new free-remaining count.
///
/// Re-anonymizing is SAFE against free-tier farming because the free quota is
/// enforced per-device server-side (the relay keys the 50-request cap on the
/// `X-Device-Hash` it receives, not on the throwaway anonymous user id — see
/// `server::device_hash` + the `device_free_usage` table). So a new anon on the
/// same machine inherits the same remaining count (e.g. 47 left), and cannot
/// reset to a fresh 50 by signing out.
async fn reset_to_anonymous(
    state: &State<'_, AppState>,
    url: &str,
    key: &str,
) -> Result<Option<u32>, String> {
    {
        let mut r = state.ai_router.lock().await;
        r.clear_managed_session();
    }
    *state.supabase_session.lock().await = None;
    let _ = std::fs::remove_file(&state.supabase_session_path);
    let new_session = server::sign_in_anonymously(url, key)
        .await
        .map_err(|e| e.to_string())?;
    server::save_session(&state.supabase_session_path, &new_session);
    save_app_session(state, new_session).await;
    let r = state.ai_router.lock().await;
    Ok(r.get_managed_free_remaining())
}

fn emit_account_changed(app: &tauri::AppHandle) {
    let _ = app
        .get_webview_window("panel")
        .map(|w| w.emit("account_changed", ()));
}

/// Upgrade the current anonymous account in place by adding an email + password.
/// Triggers a confirmation email with the 6-digit OTP; the session stays
/// anonymous until `verify_email_otp` confirms it.
#[tauri::command]
async fn sign_up_email(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<(), String> {
    let (url, key, token) = managed_auth_ctx(&state).await?;
    server::sign_up_email(&url, &key, &token, email.trim(), &password)
        .await
        .map_err(|e| e.to_string())
}

/// Resend the sign-up verification code (a fresh OTP; the previous one is voided).
/// Used by the "Resend code" action and the unverified-login recovery path.
#[tauri::command]
async fn resend_email_otp(state: State<'_, AppState>, email: String) -> Result<(), String> {
    let (url, key, token) = managed_auth_ctx(&state).await?;
    server::resend_signup_otp(&url, &key, &token, email.trim())
        .await
        .map_err(|e| e.to_string())
}

/// Confirm a sign-up OTP. GoTrue's OTP `type` for an anonymous→email upgrade
/// varies by version, so try the candidates in order (a failed verify does not
/// consume the token). On success the (same-id) account is confirmed.
///
/// `email_change` is tried FIRST: live testing (2026-06-19) confirmed the
/// anonymous→email upgrade fires GoTrue's email-change flow (the confirmation
/// email is titled "Confirm Change of Email"), so its OTP type is `email_change`,
/// not `signup`. The others remain as version fallbacks.
#[tauri::command]
async fn verify_email_otp(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    email: String,
    token: String,
) -> Result<(), String> {
    let (url, key) = managed_url_key(&state).await?;
    let email = email.trim();
    let token = token.trim();
    let mut diags: Vec<String> = Vec::new();
    for otp_type in ["email_change", "signup", "email"] {
        match server::verify_email_otp(&url, &key, email, token, otp_type).await {
            Ok(session) => {
                server::save_session(&state.supabase_session_path, &session);
                save_app_session(&state, session).await;
                emit_account_changed(&app);
                return Ok(());
            }
            Err(e) => diags.push(format!("{}={}", otp_type, e)),
        }
    }
    // Surface the per-type breakdown: identical messages across all three ⇒ the
    // token isn't stored for ANY type (stale/superseded/expired code), whereas a
    // single differing message points at a type/flow mismatch.
    log::warn!("[verify_email_otp] all types failed: {}", diags.join(" | "));
    Err(format!("Verification failed — {}", diags.join("  |  ")))
}

/// Sign in with an existing email + password.
#[tauri::command]
async fn sign_in_email(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    email: String,
    password: String,
) -> Result<(), String> {
    let (url, key) = managed_url_key(&state).await?;
    let session = server::sign_in_email(&url, &key, email.trim(), &password)
        .await
        .map_err(|e| e.to_string())?;
    server::save_session(&state.supabase_session_path, &session);
    save_app_session(&state, session).await;
    emit_account_changed(&app);
    Ok(())
}

/// Sign out: revoke the session server-side (best-effort), clear it locally, and
/// seed a fresh anonymous session so the free tier keeps working.
#[tauri::command]
async fn sign_out(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<SessionStatus, String> {
    if let Ok((url, key, token)) = managed_auth_ctx(&state).await {
        let _ = server::sign_out(&url, &key, &token).await;
    }
    let (url, key) = managed_url_key(&state).await?;
    let free_remaining = reset_to_anonymous(&state, &url, &key).await?;
    emit_account_changed(&app);
    Ok(SessionStatus {
        signed_in: true,
        free_remaining,
    })
}

/// Send a password-reset email containing the recovery OTP.
#[tauri::command]
async fn request_password_reset(state: State<'_, AppState>, email: String) -> Result<(), String> {
    let (url, key) = managed_url_key(&state).await?;
    server::request_password_reset(&url, &key, email.trim())
        .await
        .map_err(|e| e.to_string())
}

/// Verify the recovery OTP and set a new password (forgot-password flow).
#[tauri::command]
async fn verify_password_reset(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    email: String,
    token: String,
    new_password: String,
) -> Result<(), String> {
    let (url, key) = managed_url_key(&state).await?;
    let session = server::verify_recovery_otp(&url, &key, email.trim(), token.trim())
        .await
        .map_err(|e| e.to_string())?;
    server::change_password(&url, &key, &session.access_token, &new_password)
        .await
        .map_err(|e| e.to_string())?;
    server::save_session(&state.supabase_session_path, &session);
    save_app_session(&state, session).await;
    emit_account_changed(&app);
    Ok(())
}

/// Change the password of the signed-in account.
#[tauri::command]
async fn change_password(state: State<'_, AppState>, new_password: String) -> Result<(), String> {
    let (url, key, token) = managed_auth_ctx(&state).await?;
    server::change_password(&url, &key, &token, &new_password)
        .await
        .map_err(|e| e.to_string())
}

/// Current account email + anonymous flag, for the Account UI.
#[tauri::command]
async fn get_account_info(state: State<'_, AppState>) -> Result<server::AccountInfo, String> {
    let (url, key, token) = managed_auth_ctx(&state).await?;
    server::get_account_info(&url, &key, &token)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete the account (service-role Edge Function), then fall back to
/// a fresh anonymous session. Coins are NOT refunded.
#[tauri::command]
async fn delete_account(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<SessionStatus, String> {
    let (url, _key, token) = managed_auth_ctx(&state).await?;
    server::delete_account(&url, &token)
        .await
        .map_err(|e| e.to_string())?;
    let (url, key) = managed_url_key(&state).await?;
    let free_remaining = reset_to_anonymous(&state, &url, &key).await?;
    emit_account_changed(&app);
    Ok(SessionStatus {
        signed_in: true,
        free_remaining,
    })
}

/// Called from the frontend after `downloadAndInstall()` has spawned the NSIS
/// installer in the background. NSIS is waiting for us to exit so it can
/// replace the locked binary; it re-launches the new app itself via /UPDATE.
///
/// Do NOT re-spawn current_exe() here — it would lock the (still-old) binary
/// again before NSIS could replace it, leaving the user on the old version.
#[tauri::command]
fn exit_for_update(app: tauri::AppHandle) {
    log::info!("exit_for_update invoked — exiting so NSIS can replace binary");
    app.exit(0);
}

/// Return whether the app currently has a Supabase session.
#[tauri::command]
async fn get_session_status(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    let router = state.ai_router.lock().await;
    let free_remaining = router.get_managed_free_remaining();
    let signed_in = state.supabase_session.lock().await.is_some();
    Ok(SessionStatus {
        signed_in,
        free_remaining,
    })
}

/// One test-user feedback row: a "worked" success ping (sent on → Next) or a
/// categorized "wrong" report. Mirrors the Supabase `feedback` table columns.
#[derive(serde::Serialize, serde::Deserialize)]
struct FeedbackPayload {
    kind: String,
    note: Option<String>,
    app_version: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    task_prompt: Option<String>,
    instruction: Option<String>,
    target_text: Option<String>,
    located: Option<bool>,
    locate_role: Option<String>,
    locate_conf: Option<f32>,
    app_window: Option<String>,
    session_id: Option<String>,
    /// Training-data join key (llm-finetuning-eval.md §5b) — the AI request this
    /// feedback is about. LOCAL-ONLY: stripped before the Supabase insert (the
    /// `feedback` table has no such column, and the join lives on the dev machine
    /// where the prompt/response/screenshot records are anyway); with training
    /// capture on, the full payload is mirrored to `training/feedback.jsonl`.
    request_id: Option<String>,
}

/// Insert a feedback row into Supabase. Best-effort: the frontend ignores
/// failures (offline / not configured / not signed in). Uses the managed JWT
/// when present so `user_id` is attributed, else the anon role. With training
/// capture on, the row (incl. `request_id`) is also mirrored to the local
/// `training/feedback.jsonl` FIRST — the human worked/wrong signal is the
/// training label, and it must not depend on network reachability.
#[tauri::command]
async fn submit_feedback(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: FeedbackPayload,
) -> Result<(), String> {
    let training_enabled = state
        .ai_router
        .lock()
        .await
        .config
        .training_capture_enabled;
    if training_enabled {
        if let Ok(dir) = app.path().app_local_data_dir() {
            let mut local = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
            if let Some(obj) = local.as_object_mut() {
                obj.insert(
                    "timestamp_ms".into(),
                    serde_json::json!(std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0)),
                );
            }
            let path = dir.join("training").join("feedback.jsonl");
            if let Err(e) = jsonl_log::append_line(&path, &local.to_string(), true) {
                log::warn!("training feedback.jsonl write failed: {e}");
            }
        }
    }

    let (supabase_url, anon_key, token) = {
        let (url, key) = managed_url_key(&state).await?;
        let token = acct_session_token(&state).await.ok();
        (url, key, token)
    };
    let mut row = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
    // Strip the local-only join key — the server table has no request_id column,
    // and PostgREST rejects inserts with unknown fields.
    if let Some(obj) = row.as_object_mut() {
        obj.remove("request_id");
    }
    server::submit_feedback(&supabase_url, &anon_key, token.as_deref(), &row)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .level_for("tauri_plugin_updater", log::LevelFilter::Debug)
                // `Builder::new()` already ships DEFAULT_LOG_TARGETS (Stdout + LogDir) and
                // `.target()` APPENDS — adding them again wrote every record to the file (and
                // stdout) TWICE. `.targets()` REPLACES the set, so we get exactly these two.
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                ])
                .max_file_size(2_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());
            // Show panel after a short delay — the JS onMount also calls show() once
            // it has positioned the window, but this Rust fallback ensures the panel
            // is visible even if the WebView2 JS execution is delayed (production builds).
            let panel_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Some(win) = panel_handle.get_webview_window("panel") {
                    let _ = win.show();
                    log::info!("panel window shown from Rust setup");
                }
            });

            let overlay_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                if let Some(win) = overlay_handle.get_webview_window("overlay") {
                    match overlay::configure(&win) {
                        Ok(()) => {
                            let _ = win.show();
                            log::info!("overlay window configured and shown");
                        }
                        Err(e) => log::error!("overlay configure failed — NOT showing: {e}"),
                    }
                } else {
                    log::error!("overlay window not found from tauri.conf.json!");
                }
            });

            let handle = app.handle().clone();
            let tts = tts::TtsEngine::new();
            let tracker = track::WindowTracker::new();

            // Resolve the local app data directory (machine-specific, never roams).
            // Falls back to CWD so dev builds with no installation still work.
            let app_data_dir = app
                .path()
                .app_local_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            std::fs::create_dir_all(&app_data_dir).ok();
            // One-time migration: move files written to Roaming AppData before v0.5.24.
            if let Ok(old_roaming) = app.path().app_data_dir() {
                migrate_roaming_to_local(&old_roaming, &app_data_dir);
            }
            cleanup_old_debug_artifacts(&app_data_dir);
            let env_path = app_data_dir.join(".env");
            // One-time (per key) migration: any plaintext BYOK key still in .env
            // moves into the Windows Credential Manager, its line replaced by the
            // sentinel. Runs BEFORE Config::load so the very first load already
            // resolves through the vault. A hand-pasted raw key keeps working —
            // it just gets migrated here on the next launch.
            migrate_env_secrets_to_credvault(&env_path);

            // Init AI Router
            let config = Config::load(Some(&env_path));
            // Apply configured TTS voice (if set) now that config is loaded.
            if !config.tts_voice.is_empty() {
                tts.set_voice(config.tts_voice.clone());
            }
            let cost_tracker = CostTracker::new(Some(app_data_dir.join("usage.json")));
            let session_manager = SessionManager::new(app_data_dir.join("sessions"));
            let supabase_session_path = app_data_dir.join("supabase_session.json");

            // Load the Supabase session from disk so account identity survives
            // restarts — regardless of which AI provider is active.
            let initial_session = server::load_session(&supabase_session_path);

            let router = AiRouter::new(
                config,
                cost_tracker,
                session_manager,
                Some(supabase_session_path.clone()),
            );
            log::info!("AiRouter ready (provider: {})", router.config.api_provider);

            // Nav-Packs: user packs (writable app-data dir) shadow bundled packs (Tauri
            // resource). Both dirs are optional — a missing dir just yields fewer packs.
            let user_packs_dir = app_data_dir.join("packs");
            // Bundled packs live in the Tauri resource dir for a release build. In debug
            // (`tauri dev`) read them straight from the source tree instead: Tauri only re-copies
            // resources on a rebuild, so a pack-data-only edit wouldn't otherwise appear until the
            // next Rust rebuild. `CARGO_MANIFEST_DIR` is `src-tauri/`, baked at compile time.
            let bundled_packs_dir = if cfg!(debug_assertions) {
                Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs"))
            } else {
                app.path().resource_dir().ok().map(|r| r.join("packs"))
            };
            let packs = packs::PackRegistry::load(
                Some(user_packs_dir.as_path()),
                bundled_packs_dir.as_deref(),
            );

            handle.manage(AppState {
                ai_router: tokio::sync::Mutex::new(router),
                guidance: parking_lot::Mutex::new(GuidanceState::default()),
                tts,
                tracker,
                last_overlay: parking_lot::Mutex::new(None),
                env_path,
                supabase_session_path,
                supabase_session: tokio::sync::Mutex::new(initial_session),
                screen_hash: parking_lot::Mutex::new(None),
                chat_full_jpeg: parking_lot::Mutex::new(None),
                packs,
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the panel quits the whole app. Use app_handle().exit()
            // rather than std::process::exit() so Tauri can close all windows
            // (including the overlay) and WebView2 gets time to release its
            // user data folder lock — preventing the 30–40 s stale-lock delay
            // on the next launch.
            if window.label() == "panel" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    // chat_full_jpeg lives only in process memory — exiting
                    // drops it. No disk files to clean up.
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ping_sidecar,
            sidecar_echo,
            capture_screen,
            capture_active_window,
            locate_a11y,
            locate_element,
            guide,
            next_step,
            retry_locate,
            send_correction,
            focus_target_window,
            check_screen_changed,
            clear_overlay,
            restore_overlay,
            get_shared_app_info,
            get_pack_starters,
            blender_addon_status,
            install_blender_addon,
            speak,
            get_settings,
            save_settings,
            list_ollama_models,
            get_usage,
            reset_usage,
            open_debug_folder,
            sign_in_anon,
            get_balance,
            get_session_status,
            start_google_oauth,
            create_checkout,
            sign_up_email,
            resend_email_otp,
            verify_email_otp,
            sign_in_email,
            sign_out,
            request_password_reset,
            verify_password_reset,
            change_password,
            get_account_info,
            delete_account,
            submit_feedback,
            exit_for_update,
            list_target_windows,
            list_monitors,
            pin_target_window,
            pin_full_screen_target,
            unpin_target_window,
            new_session,
            list_tts_voices,
            get_chat_full_screenshot,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
