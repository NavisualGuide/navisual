// Copyright (c) 2024-2026 Jin Fu
// Licensed under the Functional Source License, Version 1.1 (Apache 2.0).
// See the LICENSE file in the root of this repository for complete details.

//! Navisual — Rust/Tauri backend entry point.

mod ai;
mod capture;
mod locator;
mod overlay;
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
    /// User-selected "Entire desktop" target (via the target-picker dropdown).
    /// When true, every capture path (guide / correction) grabs the whole virtual
    /// desktop instead of a single window. A deliberate, sticky user choice —
    /// the AI can no longer request full-screen on its own. Mutually exclusive
    /// with `pinned_hwnd`; survives new tasks like a pin.
    full_screen_mode: bool,
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
    /// Path to the Supabase session JSON file (managed provider only).
    supabase_session_path: PathBuf,
    /// Previous aHash for Autopilot on-demand screen-change polling.
    screen_hash: parking_lot::Mutex<Option<u64>>,
    /// Most-recent AI-image JPEG bytes (the one sent to the AI on the latest
    /// `guide`/`next_step`/`send_correction`). Held in RAM only — never
    /// written to disk — so the lightbox can re-open it without persisting
    /// the user's screen content to storage. Cleared on new task / on quit.
    chat_full_jpeg: parking_lot::Mutex<Option<Vec<u8>>>,
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
    ai::bbox::ai_bbox_to_screen_rect(raw, format, ai_w, ai_h, rect)
}

fn overlay_kind_for_step(overlay_type: &OverlayType) -> overlay::OverlayKind {
    match overlay_type {
        OverlayType::Arrow => overlay::OverlayKind::Arrow,
        OverlayType::Highlight | OverlayType::Circle => overlay::OverlayKind::Box,
        OverlayType::Subtitle => overlay::OverlayKind::Subtitle,
        OverlayType::None => overlay::OverlayKind::None,
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_step(
    app: &AppHandle,
    step: &GuidanceStep,
    target_hwnd: Option<usize>,
    debug_ocr_path: Option<std::path::PathBuf>,
    tracker: &track::WindowTracker,
    last_overlay: &parking_lot::Mutex<Option<LastOverlay>>,
    ai_bbox: Option<capture::Rect>,
    // "Wrong spot" memory: the bbox the previous pointer occupied, which the user
    // explicitly rejected. The locator excludes candidates there (§ fix for the
    // deterministic same-wrong-pick retry loop). Only the correction path sets it.
    avoid_bbox: Option<capture::Rect>,
    capture_rect: Option<capture::Rect>,
    // Native-res OCR image captured at AI-capture time (overlay cleared, before the streamed
    // subtitle). When present the locator's OCR uses it instead of re-capturing — so it never
    // reads our own caption and there's no clear/redraw flicker. None → locator re-captures.
    pre_ocr: Option<(Vec<u8>, capture::Rect)>,
) -> Result<
    (
        Option<locator::LocateResult>,
        Option<locator::trace::LocateTrace>,
    ),
    String,
> {
    // Treat an empty/whitespace target_text as "no target". The Ollama schema now
    // *requires* target_text (so small local models can't silently omit it and leave
    // the locator with nothing), and genuine no-target steps (scroll, press a key)
    // emit an empty string — those must not trigger a bogus locate.
    let (located, trace) =
        if let Some(text) = step.target_text.as_ref().filter(|t| !t.trim().is_empty()) {
            #[cfg(windows)]
            {
                let opts = locator::orchestrator::LocateOptions {
                    role: step
                        .target_role
                        .as_ref()
                        .map(|r| format!("{:?}", r).to_lowercase()),
                    nearby_text: step.target_nearby_text.clone(),
                    ai_bbox,
                    avoid_bbox,
                    a11y_timeout_ms: 500,
                    min_confidence: 0.5,
                    target_hwnd,
                    debug_ocr_image_path: debug_ocr_path,
                };
                let text_owned = text.clone();
                let pre = pre_ocr.as_ref().map(|(p, r)| (p.as_slice(), *r));
                match locator::orchestrator::locate(&text_owned, &opts, pre) {
                    Ok((result, trace)) => (result, Some(trace)),
                    Err(e) => {
                        log::warn!("locate failed for {:?}: {e}", text);
                        (None, None)
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (text, target_hwnd, debug_ocr_path, avoid_bbox, &pre_ocr);
                (None, None)
            }
        } else {
            (None, None)
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
    if located.is_none()
        && step
            .target_text
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty())
    {
        if let Some(ai) = ai_bbox {
            if let Some(hint) = inflate_hint_bbox(ai, capture_rect) {
                kind = overlay::OverlayKind::Hint;
                bbox = Some(hint);
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

    // Persist for restore_overlay — the AI bbox alone is a valid state too.
    if !matches!(kind, overlay::OverlayKind::None) || ai_bbox.is_some() {
        *last_overlay.lock() = Some(LastOverlay {
            kind,
            bbox,
            text: text_for_overlay.clone(),
            ai_bbox,
            target_hwnd,
        });
    }
    if visible {
        match overlay::make_update_with_ai_bbox(kind, bbox, text_for_overlay.clone(), ai_bbox) {
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
        tracker.start(b, kind, text_for_overlay, app.clone(), target_hwnd, visible);
    } else {
        tracker.clear();
    }

    Ok((located, trace))
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

/// Capture the active window raw + compute aHash in one step. Used by both
/// the autopilot polling loop and the post-AI-call baseline anchor. Skipping
/// the JPEG roundtrip used elsewhere saves ~10 ms per call — meaningful at
/// 2 captures/sec while autopilot is on.
fn ahash_of_screen() -> Option<u64> {
    let exclude = capture::get_panel_rects();
    let (img, _rect, _hwnd) = capture::capture_active_window_raw(&exclude).ok()?;
    let luma = image::imageops::grayscale(&img);
    Some(ahash_from_luma8(&luma))
}

fn hamming64(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Capture a fresh active-window hash off the blocking pool and store it as
/// the Autopilot baseline. Called at the end of every AI/local guidance event
/// (`guide`, `next_step`, `send_correction`) so the baseline always reflects
/// the screen the user is being directed against, not a drifting 500 ms-old
/// sample. Returns the captured hash for the caller (used by stale detection).
async fn anchor_autopilot_baseline(state: &AppState) -> Option<u64> {
    let h = tokio::task::spawn_blocking(ahash_of_screen)
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
    let hash = tokio::task::spawn_blocking(ahash_of_screen)
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

/// Optionally append a trace to the rolling JSONL log when enabled in settings.
fn maybe_log_trace(app: &AppHandle, trace: &locator::trace::LocateTrace, log_enabled: bool) {
    if !log_enabled {
        return;
    }
    if let Ok(dir) = app.path().app_local_data_dir() {
        let path = dir.join("locate_log.jsonl");
        if let Err(e) = locator::trace::append_jsonl(&path, trace) {
            log::warn!("locate_log.jsonl write failed: {e}");
        }
    }
}

/// Phase 0.2: emit the animated "shared app boundary" overlay and the
/// `app_changed` event so the panel chip stays in sync with what's captured.
#[cfg(windows)]
fn announce_shared_app(app: &AppHandle, hwnd_raw: usize) {
    let info = match capture::get_window_info_for_hwnd(hwnd_raw) {
        Some(i) => i,
        None => return,
    };
    let payload = SharedAppInfoPayload {
        hwnd: info.hwnd as u64,
        rect: info.rect,
        app_name: info.app_name.clone(),
        exe_name: info.exe_name.clone(),
    };
    let _ = app.emit("app_changed", &payload);

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
}

#[cfg(not(windows))]
fn announce_shared_app(_app: &AppHandle, _hwnd_raw: usize) {}

#[derive(serde::Serialize)]
struct GuideResponse {
    ok: bool,
    session_id: String,
    steps: Vec<GuidanceStep>,
    step_index: usize,
    instruction: String,
    located: Option<locator::LocateResult>,
    needs_input: bool,
    request_full_screen: bool,
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
}

#[derive(serde::Serialize, Clone)]
struct StreamChunkPayload {
    delta: String,
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
        // Drop the previous screenshot from RAM — new task starts fresh.
        *state.chat_full_jpeg.lock() = None;
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

    let debug_screenshot_enabled = { state.ai_router.lock().await.config.debug_screenshot_enabled };

    // `is_fs` is the user's sticky "Entire desktop" choice from the target picker
    // (GuidanceState.full_screen_mode). The AI no longer decides this — full-screen
    // is now an explicit, user-initiated capture scope. When set, pinned/target HWNDs
    // are ignored and the whole virtual desktop is captured.
    let (stored_hwnd, is_fs) = {
        let g = state.guidance.lock();
        (g.pinned_hwnd.or(g.target_hwnd), g.full_screen_mode)
    };

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
            match capture::capture_virtual_desktop_jpeg(75, &exclude) {
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
            (None, None)
        };

        let thumb_b64 = make_chat_thumbnail(&final_bytes);
        let pre_hash = ahash_of_jpeg(&final_bytes);
        let b64 = capture::to_base64(&final_bytes);
        Ok((b64, rect_opt, hwnd_opt, debug_path, thumb_b64, final_bytes, pre_hash, ocr_png, ocr_rect))
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

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
                steps: vec![],
                step_index: 0,
                instruction: String::new(),
                located: None,
                needs_input: false,
                request_full_screen: false,
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
            });
        }
    };

    // Phase 0.2 — flash the shared-app boundary so the user can see what
    // we're capturing. Emits the `app_changed` event for the header chip too.
    if let Some(hwnd_raw) = new_hwnd_opt {
        announce_shared_app(&app, hwnd_raw);
    }

    let mut router = state.ai_router.lock().await;

    let mut window_context = String::new();
    if let Some(hwnd) = new_hwnd_opt {
        let info = capture::get_window_info(hwnd);
        window_context = format!("\n[Current Window Info]\n{}", info);
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
    let on_chunk = move |chunk: &str| {
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
        let prompt = add_grid(base);
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
        let prompt = add_grid(task.clone());
        (
            router
                .send_guidance_request(&prompt, Some(&screenshot_b64), Some(&summary), on_chunk)
                .await,
            prompt,
        )
    } else {
        let prompt = add_grid(crate::ai::prompts::initial_context_template(&task));
        (
            router
                .send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk)
                .await,
            prompt,
        )
    };

    let ai_elapsed_ms = ai_started.elapsed().as_millis();
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

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            let err_str = e.to_string();
            if err_str == "free_trial_exhausted" {
                let _ = app.emit("trial_exhausted", ());
            }
            return Ok(GuideResponse {
                ok: false,
                session_id,
                steps: vec![],
                step_index: 0,
                instruction: String::new(),
                located: None,
                needs_input: false,
                request_full_screen: false,
                provider: router.config.api_provider.clone(),
                model: Some(used_model.clone()),
                input_tokens: Some(in_tok),
                output_tokens: Some(out_tok),
                error: Some(if err_str == "free_trial_exhausted" {
                    "Your 50 free requests have been used.".to_string()
                } else {
                    err_str
                }),
                debug_screenshot_path: None,
                chat_thumb_b64: None,
                locate_trace: None,
                ai_bbox: None,
            });
        }
    };

    let steps = response.steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    let request_full_screen = response.request_full_screen;
    let provider = router.config.api_provider.clone();

    if let Some(session) = &mut router.session_manager.current_session {
        session.update_state(state_summary.clone());
        session.add_turn("user", sent_user_prompt, None);
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
    }

    if steps.is_empty() {
        // Still anchor the autopilot baseline + run stale detection so that the
        // needs_input branch behaves the same as a normal response.
        let post_hash = anchor_autopilot_baseline(&state).await;
        if let (Some(p), Some(q)) = (pre_hash, post_hash) {
            let drift = hamming64(p, q);
            if drift >= STALE_RESPONSE_THRESHOLD {
                let _ = app.emit("ai_response_stale", serde_json::json!({ "drift": drift }));
            }
        }
        return Ok(GuideResponse {
            ok: true,
            session_id,
            steps,
            step_index: 0,
            instruction: String::new(),
            located: None,
            needs_input,
            request_full_screen,
            provider,
            model: Some(used_model.clone()),
            input_tokens: Some(in_tok),
            output_tokens: Some(out_tok),
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
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

    // Stale detection must run BEFORE execute_step draws the new pointer.
    // Capturing afterwards would include our own overlay pointer — a large
    // visual change that trips the threshold on every response. The overlay
    // was cleared before the AI capture, so the screen is pointer-free here
    // too; any drift now reflects a real user change while the AI was thinking.
    let stale_hash = tokio::task::spawn_blocking(ahash_of_screen)
        .await
        .ok()
        .flatten();
    if let (Some(p), Some(q)) = (pre_hash, stale_hash) {
        let drift = hamming64(p, q);
        if drift >= STALE_RESPONSE_THRESHOLD {
            let _ = app.emit("ai_response_stale", serde_json::json!({ "drift": drift }));
        }
    }

    let (located, locate_trace) = execute_step(
        &app,
        &steps[0],
        new_hwnd_opt,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        None,
        capture_rect_opt,
        pre_ocr,
    )
    .unwrap_or((None, None));
    if let Some(ref t) = locate_trace {
        maybe_log_trace(&app, t, log_trace);
    }

    // Anchor the autopilot baseline AFTER the pointer is drawn so that
    // check_screen_changed (which also sees the pointer) compares like-for-like.
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        request_full_screen,
        provider,
        model: Some(used_model),
        input_tokens: Some(in_tok),
        output_tokens: Some(out_tok),
        error: None,
        debug_screenshot_path,
        chat_thumb_b64,
        locate_trace,
        ai_bbox,
    })
}

#[tauri::command]
async fn next_step(
    app: AppHandle,
    state: State<'_, AppState>,
    step_index: usize,
) -> Result<GuideResponse, String> {
    let (steps, session_id, needs_input, provider, capture_rect) = {
        let g = state.guidance.lock();
        (
            g.steps.clone(),
            g.session_id.clone().unwrap_or_default(),
            g.needs_input,
            g.provider.clone(),
            g.capture_rect,
        )
    };

    if step_index >= steps.len() {
        return Err(format!(
            "step_index {step_index} out of range ({})",
            steps.len()
        ));
    }

    let (log_trace, debug_screenshot_enabled) = {
        let cfg = &state.ai_router.lock().await.config;
        (
            cfg.debug_locate_log_file_enabled,
            cfg.debug_screenshot_enabled,
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
    let (located, locate_trace) = execute_step(
        &app,
        &steps[step_index],
        stored_hwnd,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        None,
        capture_rect,
        None, // next_step reuses the prior capture; locator re-captures for OCR
    )
    .unwrap_or((None, None));
    if let Some(ref t) = locate_trace {
        maybe_log_trace(&app, t, log_trace);
    }

    // Local step advance — no AI call, so no stale check. But anchor the
    // autopilot baseline to the new pointer state so autopilot waits for the
    // *next* change (user completing this step) rather than firing on the
    // change that just triggered this advance.
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index,
        instruction: steps[step_index].instruction.clone(),
        located,
        needs_input,
        request_full_screen: false, // next_step just steps forward
        provider,
        model: None, // local advance, no AI call — frontend keeps the prior routed model
        input_tokens: None,
        output_tokens: None,
        error: None,
        debug_screenshot_path: None,
        chat_thumb_b64: None,
        locate_trace,
        ai_bbox,
    })
}

#[tauri::command]
async fn send_correction(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
    // "Wrong spot" memory: the bbox the rejected pointer occupied (virtual-desktop
    // physical pixels). The frontend sends it only for the wrong_spot reason; the
    // locator excludes candidates there so the retry can't repeat the same pick.
    avoid_bbox: Option<capture::Rect>,
) -> Result<GuideResponse, String> {
    let session_id = {
        let g = state.guidance.lock();
        g.session_id.clone()
    };
    let session_id = session_id.ok_or("no active session")?;

    // Clear the stored HWND so the correction capture always re-discovers the
    // currently focused window. If the first guide pointed at the wrong app,
    // the user can switch focus to the right app then press Wrong and the next
    // capture will find the correct window. In sticky "Entire desktop" mode
    // (full_screen_mode) the capture grabs the whole virtual desktop instead.
    let is_fs = {
        let mut g = state.guidance.lock();
        g.target_hwnd = None;
        g.full_screen_mode
    };

    let exclude = capture::get_panel_rects();

    let router = state.ai_router.lock().await;
    let debug_screenshot_enabled = router.config.debug_screenshot_enabled;
    drop(router); // Release lock before blocking capture

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
        // Full desktop (sticky "Entire desktop" user choice) or the focused window.
        let captured: Option<(Vec<u8>, capture::Rect, Option<usize>)> = if is_fs {
            capture::capture_virtual_desktop_jpeg(75, &exclude)
                .ok()
                .map(|(bytes, rect)| (bytes, rect, None))
        } else {
            capture::capture_active_window_jpeg(75, &exclude)
                .ok()
                .map(|(bytes, rect, hwnd)| (bytes, rect, Some(hwnd)))
        };
        if let Some((bytes, rect, hwnd_opt)) = captured {
            let final_bytes = bytes;
            // Native-res OCR image, captured now (overlay cleared, before the streamed subtitle)
            // so the locator's OCR never reads our own caption — see guide(). No single
            // window in full-screen mode, so OCR re-capture is skipped (A11y still runs).
            let pre_ocr = match hwnd_opt {
                Some(hwnd) => capture::recapture_window_raw(hwnd, &exclude)
                    .ok()
                    .and_then(|(raw, r)| capture::encode_png_for_ocr(&raw).ok().map(|png| (png, r))),
                None => None,
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

    if let Some(bytes) = full_jpeg_opt {
        *state.chat_full_jpeg.lock() = Some(bytes);
    }

    // Phase 0.2 — show shared-app boundary on correction too.
    if let Some(hwnd_raw) = new_hwnd {
        announce_shared_app(&app, hwnd_raw);
    }

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
    }

    let final_user_text = if !window_context.is_empty() {
        format!("{user_text_owned}\n{window_context}")
    } else {
        user_text_owned
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
    let on_chunk = move |chunk: &str| {
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

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            let err_str = e.to_string();
            if err_str == "free_trial_exhausted" {
                let _ = app.emit("trial_exhausted", ());
                return Err("Your 50 free requests have been used.".to_string());
            }
            return Err(err_str);
        }
    };

    let steps = response.steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    let request_full_screen = response.request_full_screen;
    let provider = router.config.api_provider.clone();

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
    }

    if steps.is_empty() {
        let post_hash = anchor_autopilot_baseline(&state).await;
        if let (Some(p), Some(q)) = (pre_hash, post_hash) {
            let drift = hamming64(p, q);
            if drift >= STALE_RESPONSE_THRESHOLD {
                let _ = app.emit("ai_response_stale", serde_json::json!({ "drift": drift }));
            }
        }
        return Ok(GuideResponse {
            ok: true,
            session_id,
            steps,
            step_index: 0,
            instruction: String::new(),
            located: None,
            needs_input,
            request_full_screen,
            provider,
            model: Some(used_model.clone()),
            input_tokens: Some(in_tok),
            output_tokens: Some(out_tok),
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
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

    // Stale detection before the pointer is drawn — see guide() for rationale.
    let stale_hash = tokio::task::spawn_blocking(ahash_of_screen)
        .await
        .ok()
        .flatten();
    if let (Some(p), Some(q)) = (pre_hash, stale_hash) {
        let drift = hamming64(p, q);
        if drift >= STALE_RESPONSE_THRESHOLD {
            let _ = app.emit("ai_response_stale", serde_json::json!({ "drift": drift }));
        }
    }

    let (located, locate_trace) = execute_step(
        &app,
        &steps[0],
        new_hwnd,
        debug_ocr_path,
        &state.tracker,
        &state.last_overlay,
        ai_bbox,
        avoid_bbox,
        new_capture_rect,
        pre_ocr,
    )
    .unwrap_or((None, None));
    if let Some(ref t) = locate_trace {
        maybe_log_trace(&app, t, log_trace);
    }

    // Anchor the autopilot baseline AFTER the pointer is drawn (pointer-inclusive).
    let _ = anchor_autopilot_baseline(&state).await;

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        request_full_screen,
        provider,
        model: Some(used_model),
        input_tokens: Some(in_tok),
        output_tokens: Some(out_tok),
        error: None,
        debug_screenshot_path,
        chat_thumb_b64,
        locate_trace,
        ai_bbox,
    })
}

#[tauri::command]
fn speak(text: String, lang: Option<String>, state: State<'_, AppState>) {
    state.tts.speak(text, lang.unwrap_or_default());
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
    }
    #[cfg(windows)]
    announce_shared_app(&app, hwnd);
    #[cfg(not(windows))]
    let _ = app;
}

/// Select "Entire desktop" as the guidance target — the user-initiated replacement
/// for the old AI-requested full-screen consent flow. Sticky like a pin: every
/// subsequent capture (guide / correction) grabs the whole virtual desktop until
/// the user picks a specific window or returns to the active-window default.
#[tauri::command]
fn pin_full_screen_target(state: State<'_, AppState>) {
    let mut g = state.guidance.lock();
    g.full_screen_mode = true;
    g.pinned_hwnd = None;
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
fn unpin_target_window(state: State<'_, AppState>) {
    let mut g = state.guidance.lock();
    g.pinned_hwnd = None;
    g.full_screen_mode = false; // back to the active-window default
    // target_hwnd retains the last auto-discovered window for the current session.
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
            avoid_bbox: None,
            a11y_timeout_ms: timeout_ms.unwrap_or(1500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
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
            avoid_bbox: None,
            a11y_timeout_ms: timeout_ms.unwrap_or(500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
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
            "DEBUG_SHOW_AI_BBOX".into(),
            payload.debug_show_ai_bbox.to_string(),
        ),
    ];

    // API keys: only overwrite if the user actually typed something
    if !payload.anthropic_api_key.trim().is_empty() {
        updates.push((
            "ANTHROPIC_API_KEY".into(),
            payload.anthropic_api_key.clone(),
        ));
    }
    if !payload.gemini_api_key.trim().is_empty() {
        updates.push(("GEMINI_API_KEY".into(), payload.gemini_api_key.clone()));
    }
    if !payload.openai_api_key.trim().is_empty() {
        updates.push(("OPENAI_API_KEY".into(), payload.openai_api_key.clone()));
    }
    if !payload.deepseek_api_key.trim().is_empty() {
        updates.push(("DEEPSEEK_API_KEY".into(), payload.deepseek_api_key.clone()));
    }
    if !payload.qwen_api_key.trim().is_empty() {
        updates.push(("QWEN_API_KEY".into(), payload.qwen_api_key.clone()));
    }
    if !payload.custom_api_key.trim().is_empty() {
        updates.push(("CUSTOM_API_KEY".into(), payload.custom_api_key.clone()));
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
    {
        let router = state.ai_router.lock().await;
        if router.has_managed_session() {
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

    {
        let mut router = state.ai_router.lock().await;
        router.set_managed_session(new_session);
    }

    Ok(SessionStatus {
        signed_in: true,
        free_remaining: None,
    })
}

/// Fetch the managed-provider balance (tier, free_remaining, coin_balance_microdollars).
#[tauri::command]
async fn get_balance(state: State<'_, AppState>) -> Result<server::BalanceResponse, String> {
    let (supabase_url, access_token) = {
        let router = state.ai_router.lock().await;
        let url = router
            .config
            .supabase_url
            .clone()
            .ok_or("SUPABASE_URL not configured")?;
        // Try to get the access token from the managed client's session.
        let token = match &router.client_access_token() {
            Some(t) => t.clone(),
            None => return Err("Not signed in to managed provider".to_string()),
        };
        (url, token)
    };
    server::get_balance(&supabase_url, &access_token)
        .await
        .map_err(|e| e.to_string())
}

/// Sign in with Google via PKCE OAuth in the system browser.
/// Opens the Google consent page in the default browser, starts a local
/// HTTP server on port 9876 for the callback, exchanges the code for a
/// session, and emits `oauth_complete` to the frontend.  The anonymous
/// session is replaced — the user_profiles row starts fresh for the Google
/// account (link-identity / preserve-row is deferred to a later release).
#[tauri::command]
async fn start_google_oauth(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
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

    let pkce = server::generate_pkce(9876);
    let auth_url = server::google_oauth_url(&supabase_url, &pkce);

    // Bind the callback port FIRST so a busy port (a prior attempt still
    // waiting) fails fast before we send the user to Google.
    let listener = server::bind_callback_listener(pkce.port)
        .await
        .map_err(|e| e.to_string())?;

    // Open the OAuth URL in the system browser (not the WebView2).
    tauri_plugin_opener::open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {e}"))?;

    // Wait for the redirect callback (up to 120 s).
    let code = server::accept_oauth_code(listener)
        .await
        .map_err(|e| e.to_string())?;

    let new_session = server::exchange_pkce_code(&supabase_url, &anon_key, &code, &pkce.verifier)
        .await
        .map_err(|e| e.to_string())?;

    server::save_session(&state.supabase_session_path, &new_session);
    {
        let mut router = state.ai_router.lock().await;
        router.set_managed_session(new_session);
    }

    let _ = app
        .get_webview_window("panel")
        .map(|w| w.emit("oauth_complete", ()));
    Ok(())
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
        let router = state.ai_router.lock().await;
        let url = router
            .config
            .supabase_url
            .clone()
            .ok_or("SUPABASE_URL not configured")?;
        let token = router
            .client_access_token()
            .ok_or("Not signed in to managed provider")?;
        (url, token)
    };

    let amount = if amount_usd > 0.0 { amount_usd } else { 20.0 };
    server::create_checkout_session(&supabase_url, &access_token, amount)
        .await
        .map_err(|e| e.to_string())
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

/// Return whether the app currently has a managed-provider session.
#[tauri::command]
async fn get_session_status(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    let router = state.ai_router.lock().await;
    let free_remaining = router.get_managed_free_remaining();
    let signed_in = router.has_managed_session();
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
}

/// Insert a feedback row into Supabase. Best-effort: the frontend ignores
/// failures (offline / not configured / not signed in). Uses the managed JWT
/// when present so `user_id` is attributed, else the anon role.
#[tauri::command]
async fn submit_feedback(
    state: State<'_, AppState>,
    payload: FeedbackPayload,
) -> Result<(), String> {
    let (supabase_url, anon_key, token) = {
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
        let token = router.client_access_token();
        (url, key, token)
    };
    let row = serde_json::to_value(&payload).map_err(|e| e.to_string())?;
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

            // Init AI Router
            let config = Config::load(Some(&env_path));
            // Apply configured TTS voice (if set) now that config is loaded.
            if !config.tts_voice.is_empty() {
                tts.set_voice(config.tts_voice.clone());
            }
            let cost_tracker = CostTracker::new(Some(app_data_dir.join("usage.json")));
            let session_manager = SessionManager::new(app_data_dir.join("sessions"));
            let supabase_session_path = app_data_dir.join("supabase_session.json");

            let router = AiRouter::new(
                config,
                cost_tracker,
                session_manager,
                Some(supabase_session_path.clone()),
            );
            log::info!("AiRouter ready (provider: {})", router.config.api_provider);
            handle.manage(AppState {
                ai_router: tokio::sync::Mutex::new(router),
                guidance: parking_lot::Mutex::new(GuidanceState::default()),
                tts,
                tracker,
                last_overlay: parking_lot::Mutex::new(None),
                env_path,
                supabase_session_path,
                screen_hash: parking_lot::Mutex::new(None),
                chat_full_jpeg: parking_lot::Mutex::new(None),
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
            send_correction,
            check_screen_changed,
            clear_overlay,
            restore_overlay,
            get_shared_app_info,
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
            submit_feedback,
            exit_for_update,
            list_target_windows,
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
