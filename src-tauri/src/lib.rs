// Copyright (c) 2024-2026 Jin Fu
// Licensed under the Functional Source License, Version 1.1 (Apache 2.0).
// See the LICENSE file in the root of this repository for complete details.

//! Navisual — Rust/Tauri backend entry point.

mod capture;
mod locator;
mod overlay;
mod ai;
mod tts;
mod track;
mod server;

use ai::router::AiRouter;
use ai::config::Config;
use ai::cost_tracker::CostTracker;
use ai::session::SessionManager;
use ai::types::{GuidanceStep, OverlayType};

use std::path::PathBuf;
use tokio::sync::Mutex;
use tauri::{AppHandle, Manager, State, Emitter};
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
}

/// Shared app state.
struct AppState {
    ai_router: Mutex<AiRouter>,
    guidance: std::sync::Mutex<GuidanceState>,
    tts: tts::TtsEngine,
    tracker: track::WindowTracker,
    /// Last non-None overlay emitted — used by restore_overlay to bring it back after Clear.
    last_overlay: std::sync::Mutex<Option<LastOverlay>>,
    /// Resolved path to the .env settings file — always writable (app data dir).
    env_path: PathBuf,
    /// Path to the Supabase session JSON file (managed provider only).
    supabase_session_path: PathBuf,
    /// Previous aHash for Autopilot on-demand screen-change polling.
    screen_hash: std::sync::Mutex<Option<u64>>,
}

/// Snapshot of the most recent non-clear overlay. Stored so `restore_overlay`
/// can re-emit after the user clears the screen guide.
#[derive(Clone)]
struct LastOverlay {
    kind: overlay::OverlayKind,
    bbox: Option<capture::Rect>,
    text: Option<String>,
    ai_bbox: Option<capture::Rect>,
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
        ai_bbox, x, y, w, h
    );
    Some(capture::Rect { x, y, width: w, height: h })
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
    last_overlay: &std::sync::Mutex<Option<LastOverlay>>,
    ai_bbox: Option<capture::Rect>,
    capture_rect: Option<capture::Rect>,
) -> Result<(Option<locator::LocateResult>, Option<locator::trace::LocateTrace>), String> {
    let (located, trace) = if let Some(ref text) = step.target_text {
        #[cfg(windows)]
        {
            let opts = locator::orchestrator::LocateOptions {
                role: step.target_role.as_ref().map(|r| format!("{:?}", r).to_lowercase()),
                nearby_text: step.target_nearby_text.clone(),
                ai_bbox,
                a11y_timeout_ms: 500,
                min_confidence: 0.5,
                target_hwnd,
                debug_ocr_image_path: debug_ocr_path,
            };
            let text_owned = text.clone();
            match locator::orchestrator::locate(&text_owned, &opts) {
                Ok((result, trace)) => (result, Some(trace)),
                Err(e) => {
                    log::warn!("locate failed for {:?}: {e}", text);
                    (None, None)
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (text, target_hwnd, debug_ocr_path);
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
    if located.is_none() && step.target_text.is_some() {
        if let Some(ai) = ai_bbox {
            if let Some(hint) = inflate_hint_bbox(ai, capture_rect) {
                kind = overlay::OverlayKind::Hint;
                bbox = Some(hint);
            }
        }
    }

    let text_for_overlay = Some(step.instruction.clone());

    // Persist for restore_overlay — the AI bbox alone is a valid state too.
    if !matches!(kind, overlay::OverlayKind::None) || ai_bbox.is_some() {
        *last_overlay.lock().unwrap() = Some(LastOverlay {
            kind,
            bbox,
            text: text_for_overlay.clone(),
            ai_bbox,
        });
    }
    match overlay::make_update_with_ai_bbox(kind, bbox, text_for_overlay.clone(), ai_bbox) {
        Ok(update) => {
            if let Err(e) = overlay::emit_update(app, update) {
                log::warn!("overlay emit failed: {e}");
            }
        }
        Err(e) => log::warn!("overlay make_update failed: {e}"),
    }

    // E.4 — Clipboard: if the AI supplied text to copy, write it now so
    // it's in the clipboard before the user acts on the instruction.
    if let Some(ref clip_text) = step.clipboard {
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(clip_text.clone())) {
            Ok(()) => log::info!("clipboard: wrote {} chars", clip_text.len()),
            Err(e) => log::warn!("clipboard write failed: {e}"),
        }
    }

    // Start tracking the window so the overlay follows if it moves.
    if let Some(ref b) = bbox {
        tracker.start(b, kind, text_for_overlay, app.clone());
    } else {
        tracker.clear();
    }

    Ok((located, trace))
}

// ---------- Autopilot on-demand screen-change polling ----------

const AUTOPILOT_CHANGE_THRESHOLD: u32 = 6;

fn ahash_of_screen() -> Option<u64> {
    let exclude = capture::get_panel_rects();
    let (jpeg, _rect, _hwnd) = capture::capture_active_window_jpeg(30, &exclude).ok()?;
    let img = image::load_from_memory(&jpeg).ok()?;
    let thumb = image::imageops::resize(&img.to_luma8(), 8, 8, image::imageops::FilterType::Triangle);
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

fn hamming64(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Called by the frontend Autopilot polling loop every 500 ms.
/// Captures the active window, computes aHash, compares to the last stored hash.
/// Updates the stored hash unconditionally so the baseline stays current.
/// Returns `changed=true` when Hamming distance exceeds threshold.
#[tauri::command]
async fn check_screen_changed(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let hash = ahash_of_screen();
    let mut prev_opt = state.screen_hash.lock().unwrap();
    let changed = match (hash, *prev_opt) {
        (Some(h), Some(prev)) => hamming64(prev, h) >= AUTOPILOT_CHANGE_THRESHOLD,
        _ => false,
    };
    if let Some(h) = hash {
        *prev_opt = Some(h);
    }
    Ok(serde_json::json!({ "changed": changed }))
}

/// Optionally append a trace to the rolling JSONL log when enabled in settings.
fn maybe_log_trace(app: &AppHandle, trace: &locator::trace::LocateTrace, log_enabled: bool) {
    if !log_enabled {
        return;
    }
    if let Ok(dir) = app.path().app_data_dir() {
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

/// Save one 160×90 thumbnail + one full-resolution screenshot to the app data dir.
/// Both files share the same lifecycle: overwritten together, deleted together.
/// Completely separate from the developer debug screenshot path.
fn make_chat_thumbnail(jpeg_bytes: &[u8], app_data_dir: &std::path::Path) -> Option<String> {
    let thumb_path = app_data_dir.join("chat_thumb.jpg");
    let full_path  = app_data_dir.join("chat_full.jpg");
    // Replace both files atomically (old ones deleted first).
    let _ = std::fs::remove_file(&thumb_path);
    let _ = std::fs::remove_file(&full_path);
    let _ = std::fs::write(&full_path, jpeg_bytes); // full AI screenshot for lightbox
    let img = image::load_from_memory(jpeg_bytes).ok()?;
    let thumb = img.resize(160, 90, image::imageops::FilterType::Nearest);
    let mut buf = Vec::new();
    {
        use image::codecs::jpeg::JpegEncoder;
        let mut enc = JpegEncoder::new_with_quality(&mut buf, 40);
        enc.encode_image(&thumb).ok()?;
    }
    let _ = std::fs::write(&thumb_path, &buf);
    Some(capture::to_base64(&buf))
}

/// Return the full-resolution chat screenshot as base64 (for the lightbox).
/// Returns None if no screenshot has been taken yet this session.
#[tauri::command]
fn get_chat_full_screenshot(app: AppHandle) -> Option<String> {
    let path = app.path().app_data_dir().ok()?.join("chat_full.jpg");
    let bytes = std::fs::read(path).ok()?;
    Some(capture::to_base64(&bytes))
}

#[tauri::command]
async fn guide(
    app: AppHandle,
    state: State<'_, AppState>,
    task: String,
    is_reply: bool,
    full_screen: Option<bool>,
) -> Result<GuideResponse, String> {
    let is_next_requery = task.starts_with("[User completed:");
    // Only reset session state for a genuine new task (not resume, not reply, not next-requery).
    if !task.is_empty() && !is_reply && !is_next_requery {
        let mut g = state.guidance.lock().unwrap();
        g.session_id = None;
        g.steps = vec![];
        g.state_summary = String::new();
        g.target_hwnd = None;
        // Delete the previous chat thumbnail + full screenshot — new task starts fresh.
        if let Ok(dir) = app.path().app_data_dir() {
            let _ = std::fs::remove_file(dir.join("chat_thumb.jpg"));
            let _ = std::fs::remove_file(dir.join("chat_full.jpg"));
        }
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

    let stored_hwnd = {
        let g = state.guidance.lock().unwrap();
        g.pinned_hwnd.or(g.target_hwnd)
    };

    // Get the panel rect before entering spawn_blocking — blanked from the
    // capture so the AI never sees our own UI chrome in screenshots.
    let exclude = capture::get_panel_rects();

    // Debug folder is a sub-directory of the app data dir.
    let debug_dir = app.path().app_data_dir()
        .map(|p| p.join("debug"))
        .ok();

    // Chat thumbnail + full screenshot — single pair of files, separate from debug.
    let chat_dir = app.path().app_data_dir().ok();

    // Clear the previous step's pointer before capture — prevents it from
    // appearing in the AI's screenshot. Stop the tracker first so it can't
    // re-emit the old overlay during the 33 ms DWM composite wait.
    state.tracker.clear();
    if let Ok(update) = overlay::make_update(overlay::OverlayKind::None, None, None) {
        let _ = overlay::emit_update(&app, update);
    }
    tokio::time::sleep(std::time::Duration::from_millis(33)).await;

    #[allow(clippy::type_complexity)]
    let capture_result = tokio::task::spawn_blocking(move || -> Result<(String, Option<capture::Rect>, Option<usize>, Option<String>, Option<String>), ()> {
        let is_fs = full_screen.unwrap_or(false);
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

        let thumb_b64 = chat_dir.as_ref()
            .and_then(|d| make_chat_thumbnail(&final_bytes, d));
        Ok((capture::to_base64(&final_bytes), rect_opt, hwnd_opt, debug_path, thumb_b64))
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    let (screenshot_b64, capture_rect_opt, new_hwnd_opt, debug_screenshot_path, chat_thumb_b64) = match capture_result {
        Ok((b64, rect_opt, hwnd_opt, dbg, thumb)) => (b64, rect_opt, hwnd_opt, dbg, thumb),
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
                error: Some(
                    "No application window found. Please click on the program you want \
                     help with to bring it into focus, then try Guide me again.".to_string()
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

    let app_clone = app.clone();
    let on_chunk = move |chunk: &str| {
        let _ = app_clone.emit("stream_chunk", StreamChunkPayload { delta: chunk.to_string() });
    };

    let (resp, sent_user_prompt) = if task.is_empty() || is_next_requery {
        let summary = {
            let g = state.guidance.lock().unwrap();
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
        (router.send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk).await, prompt)
    } else if is_reply {
        let summary = {
            let g = state.guidance.lock().unwrap();
            g.state_summary.clone()
        };
        let prompt = add_grid(task.clone());
        (router.send_guidance_request(&prompt, Some(&screenshot_b64), Some(&summary), on_chunk).await, prompt)
    } else {
        let prompt = add_grid(crate::ai::prompts::initial_context_template(&task));
        (router.send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk).await, prompt)
    };

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
        let content = steps.iter().map(|s| s.instruction.clone()).collect::<Vec<_>>().join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    // Release the ai_router Mutex before execute_step so that concurrent
    // commands (next_step, send_correction) do not deadlock while the
    // locator runs its blocking A11y/OCR calls.
    drop(router);

    {
        let mut g = state.guidance.lock().unwrap();
        g.session_id = Some(session_id.clone());
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
        g.provider = provider.clone();
        g.capture_rect = capture_rect_opt;
        g.target_hwnd = new_hwnd_opt;
    }

    if steps.is_empty() {
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
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
        });
    }

    let log_trace = state.ai_router.lock().await.config.debug_locate_log_file_enabled;
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path().app_data_dir().ok().map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[0], capture_rect_opt, &provider);
    let (located, locate_trace) = execute_step(&app, &steps[0], new_hwnd_opt, debug_ocr_path, &state.tracker, &state.last_overlay, ai_bbox, capture_rect_opt)
        .unwrap_or((None, None));
    if let Some(ref t) = locate_trace { maybe_log_trace(&app, t, log_trace); }

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
        let g = state.guidance.lock().unwrap();
        (
            g.steps.clone(),
            g.session_id.clone().unwrap_or_default(),
            g.needs_input,
            g.provider.clone(),
            g.capture_rect,
        )
    };

    if step_index >= steps.len() {
        return Err(format!("step_index {step_index} out of range ({})", steps.len()));
    }

    let (log_trace, debug_screenshot_enabled) = {
        let cfg = &state.ai_router.lock().await.config;
        (cfg.debug_locate_log_file_enabled, cfg.debug_screenshot_enabled)
    };
    let stored_hwnd = {
        let g = state.guidance.lock().unwrap();
        g.pinned_hwnd.or(g.target_hwnd)
    };
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path().app_data_dir().ok().map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[step_index], capture_rect, &provider);
    let (located, locate_trace) = execute_step(&app, &steps[step_index], stored_hwnd, debug_ocr_path, &state.tracker, &state.last_overlay, ai_bbox, capture_rect)
        .unwrap_or((None, None));
    if let Some(ref t) = locate_trace { maybe_log_trace(&app, t, log_trace); }

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
) -> Result<GuideResponse, String> {
    let session_id = {
        let g = state.guidance.lock().unwrap();
        g.session_id.clone()
    };
    let session_id = session_id.ok_or("no active session")?;

    // Clear the stored HWND so the correction capture always re-discovers the
    // currently focused window. If the first guide pointed at the wrong app,
    // the user can switch focus to the right app then press Wrong and the next
    // capture will find the correct window.
    {
        let mut g = state.guidance.lock().unwrap();
        g.target_hwnd = None;
    }

    let exclude = capture::get_panel_rects();

    let router = state.ai_router.lock().await;
    let debug_screenshot_enabled = router.config.debug_screenshot_enabled;
    drop(router); // Release lock before blocking capture

    let debug_dir = app.path().app_data_dir().map(|p| p.join("debug")).ok();
    let chat_dir = app.path().app_data_dir().ok();

    // Clear the previous pointer before capture.
    state.tracker.clear();
    if let Ok(update) = overlay::make_update(overlay::OverlayKind::None, None, None) {
        let _ = overlay::emit_update(&app, update);
    }
    tokio::time::sleep(std::time::Duration::from_millis(33)).await;

    // Fresh capture — no stored HWND, always walks z-order to the focused window.
    let (screenshot_b64, new_capture_rect, new_hwnd, debug_screenshot_path, chat_thumb_b64) = tokio::task::spawn_blocking(move || {
        if let Ok((bytes, rect, hwnd)) = capture::capture_active_window_jpeg(75, &exclude) {
            let final_bytes = bytes;

            let debug_path = if debug_screenshot_enabled {
                if let Some(ref dir) = debug_dir {
                    let _ = std::fs::create_dir_all(dir);
                    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
                    let path = dir.join(format!("screenshot_corr_{ts}.jpg"));
                    let txt_path = dir.join(format!("screenshot_corr_{ts}.txt"));

                    #[cfg(windows)]
                    {
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

            let thumb_b64 = chat_dir.as_ref()
                .and_then(|d| make_chat_thumbnail(&final_bytes, d));
            (capture::to_base64(&final_bytes), Some(rect), Some(hwnd), debug_path, thumb_b64)
        } else {
            (String::new(), None, None, None, None)
        }
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    // Phase 0.2 — show shared-app boundary on correction too.
    if let Some(hwnd_raw) = new_hwnd {
        announce_shared_app(&app, hwnd_raw);
    }

    let mut router = state.ai_router.lock().await;
    let summary = {
        let g = state.guidance.lock().unwrap();
        g.state_summary.clone()
    };

    let user_text_owned = match note.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => format!("{} User note: {}", crate::ai::prompts::CORRECTION_CONTEXT, n),
        None    => crate::ai::prompts::CORRECTION_CONTEXT.to_string(),
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

    let app_clone = app.clone();
    let on_chunk = move |chunk: &str| {
        let _ = app_clone.emit("stream_chunk", StreamChunkPayload { delta: chunk.to_string() });
    };

    let resp = router.send_guidance_request(user_text, Some(&screenshot_b64), Some(&summary), on_chunk).await;

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
        let content = steps.iter().map(|s| s.instruction.clone()).collect::<Vec<_>>().join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    // Release the Mutex before execute_step — same pattern as guide().
    // The locator runs blocking UIA/OCR calls that can take 1-3 s; holding the
    // Mutex during that time would deadlock any concurrent Tauri command.
    drop(router);

    {
        let mut g = state.guidance.lock().unwrap();
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
        g.capture_rect = new_capture_rect;
        g.target_hwnd = new_hwnd;
    }

    if steps.is_empty() {
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
            error: None,
            debug_screenshot_path,
            chat_thumb_b64,
            locate_trace: None,
            ai_bbox: None,
        });
    }

    let (log_trace, debug_screenshot_enabled) = {
        let cfg = &state.ai_router.lock().await.config;
        (cfg.debug_locate_log_file_enabled, cfg.debug_screenshot_enabled)
    };
    let debug_ocr_path = if debug_screenshot_enabled {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
        app.path().app_data_dir().ok().map(|p| p.join("debug").join(format!("ocr_{ts}.png")))
    } else {
        None
    };
    let ai_bbox = compute_ai_bbox_for_step(&steps[0], new_capture_rect, &provider);
    let (located, locate_trace) = execute_step(&app, &steps[0], new_hwnd, debug_ocr_path, &state.tracker, &state.last_overlay, ai_bbox, new_capture_rect)
        .unwrap_or((None, None));
    if let Some(ref t) = locate_trace { maybe_log_trace(&app, t, log_trace); }

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
        error: None,
        debug_screenshot_path,
        chat_thumb_b64,
        locate_trace,
        ai_bbox,
    })
}

#[tauri::command]
fn speak(text: String, state: State<'_, AppState>) {
    state.tts.speak(text);
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
    if let Some(last) = state.last_overlay.lock().unwrap().clone() {
        match overlay::make_update_with_ai_bbox(last.kind, last.bbox, last.text, last.ai_bbox) {
            Ok(update) => overlay::emit_update(&app, update).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        Ok(())
    }
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
    { capture::list_target_windows() }
    #[cfg(not(windows))]
    { vec![] }
}

/// Item 1 — pin a specific window as the guidance target. Survives new tasks;
/// only cleared by `unpin_target_window` or when the window is no longer valid.
#[tauri::command]
fn pin_target_window(app: AppHandle, state: State<'_, AppState>, hwnd: usize) {
    {
        let mut g = state.guidance.lock().unwrap();
        g.pinned_hwnd = Some(hwnd);
        g.target_hwnd = Some(hwnd);
    }
    #[cfg(windows)]
    announce_shared_app(&app, hwnd);
    #[cfg(not(windows))]
    let _ = app;
}

/// Item 1 — clear the pinned window and return to auto-detection.
#[tauri::command]
fn unpin_target_window(state: State<'_, AppState>) {
    let mut g = state.guidance.lock().unwrap();
    g.pinned_hwnd = None;
    // target_hwnd retains the last auto-discovered window for the current session.
}

/// Phase 0.2: structured info about the window being shared with the AI.
/// Used by the panel to show the "Shared: <App>" header chip.
#[tauri::command]
fn get_shared_app_info(state: State<'_, AppState>) -> Option<SharedAppInfoPayload> {
    #[cfg(windows)]
    {
        let stored = {
            let g = state.guidance.lock().unwrap();
            g.pinned_hwnd.or(g.target_hwnd)
        };
        let info = match stored {
            Some(hwnd) => capture::get_window_info_for_hwnd(hwnd)
                .or_else(capture::get_active_window_info),
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
    match overlay::make_update(
        overlay::OverlayKind::Box,
        Some(result.bbox),
        None,
    ) {
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
            a11y_timeout_ms: timeout_ms.unwrap_or(1500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
        };
        let (result, _trace) = tokio::task::spawn_blocking(move || {
            locator::a11y::find_element(&text, &opts)
        })
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
            a11y_timeout_ms: timeout_ms.unwrap_or(500),
            min_confidence: 0.5,
            target_hwnd: None,
            debug_ocr_image_path: None,
        };
        let (result, _trace) = tokio::task::spawn_blocking(move || locator::orchestrator::locate(&text, &opts))
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
#[tauri::command]
async fn open_debug_folder(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
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
        overlay_color: c.overlay_color.clone(),
        overlay_thickness: c.overlay_thickness,
        subtitle_enabled: c.subtitle_enabled,
        auto_advance: c.auto_advance,
        tts_enabled: c.tts_enabled,
        tts_voice: c.tts_voice.clone(),
        voice_input_enabled: c.voice_input_enabled,
        voice_language: c.voice_language.clone(),
        hotkey_next:  c.hotkey_next.clone(),
        hotkey_wrong: c.hotkey_wrong.clone(),
        hotkey_pause: c.hotkey_pause.clone(),
        hotkey_icon:  c.hotkey_icon.clone(),
        debug_screenshot_enabled: c.debug_screenshot_enabled,
        debug_show_response_info: c.debug_show_response_info,
        debug_locate_trace_enabled: c.debug_locate_trace_enabled,
        debug_locate_log_file_enabled: c.debug_locate_log_file_enabled,
        debug_show_ai_bbox: c.debug_show_ai_bbox,
    })
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
        ("API_PROVIDER".into(),         payload.api_provider.clone()),
        ("ANTHROPIC_MODEL".into(),      payload.anthropic_model.clone()),
        ("ANTHROPIC_FAST_MODEL".into(), payload.anthropic_fast_model.clone()),
        ("GEMINI_MODEL".into(),         payload.gemini_model.clone()),
        ("GEMINI_FAST_MODEL".into(),    payload.gemini_fast_model.clone()),
        ("OLLAMA_BASE_URL".into(),      payload.ollama_base_url.clone()),
        ("OLLAMA_MODEL".into(),         payload.ollama_model.clone()),
        ("OPENAI_MODEL".into(),         payload.openai_model.clone()),
        ("DEEPSEEK_MODEL".into(),       payload.deepseek_model.clone()),
        ("QWEN_MODEL".into(),           payload.qwen_model.clone()),
        ("QWEN_BASE_URL".into(),        payload.qwen_base_url.clone()),
        ("OVERLAY_COLOR".into(),        payload.overlay_color.clone()),
        ("OVERLAY_THICKNESS".into(),    payload.overlay_thickness.to_string()),
        ("SUBTITLE_ENABLED".into(),     payload.subtitle_enabled.to_string()),
        ("AUTO_ADVANCE".into(),         payload.auto_advance.to_string()),
        ("TTS_ENABLED".into(),          payload.tts_enabled.to_string()),
        ("TTS_VOICE".into(),            payload.tts_voice.clone()),
        ("VOICE_INPUT_ENABLED".into(),  payload.voice_input_enabled.to_string()),
        ("VOICE_LANGUAGE".into(),       payload.voice_language.clone()),
        ("HOTKEY_NEXT".into(),          payload.hotkey_next.clone()),
        ("HOTKEY_WRONG".into(),         payload.hotkey_wrong.clone()),
        ("HOTKEY_PAUSE".into(),         payload.hotkey_pause.clone()),
        ("HOTKEY_ICON".into(),          payload.hotkey_icon.clone()),
        ("DEBUG_SCREENSHOT_ENABLED".into(),       payload.debug_screenshot_enabled.to_string()),
        ("DEBUG_SHOW_RESPONSE_INFO".into(),       payload.debug_show_response_info.to_string()),
        ("DEBUG_LOCATE_TRACE_ENABLED".into(),     payload.debug_locate_trace_enabled.to_string()),
        ("DEBUG_LOCATE_LOG_FILE_ENABLED".into(),  payload.debug_locate_log_file_enabled.to_string()),
        ("DEBUG_SHOW_AI_BBOX".into(),             payload.debug_show_ai_bbox.to_string()),
    ];

    // API keys: only overwrite if the user actually typed something
    if !payload.anthropic_api_key.trim().is_empty() {
        updates.push(("ANTHROPIC_API_KEY".into(), payload.anthropic_api_key.clone()));
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

    // Atomic write to .env
    let refs: Vec<(&str, &str)> = updates.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
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
    let _ = app.emit("overlay:theme", OverlayThemePayload {
        color: payload.overlay_color,
        thickness: payload.overlay_thickness,
        subtitle_enabled: payload.subtitle_enabled,
    });

    Ok(())
}

#[derive(serde::Serialize)]
struct SessionStatus {
    signed_in: bool,
    free_remaining: Option<u32>,
}

/// Sign in anonymously to Supabase (managed provider). Safe to call even if already signed in.
#[tauri::command]
async fn sign_in_anon(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    let (supabase_url, anon_key) = {
        let router = state.ai_router.lock().await;
        let url = router.config.supabase_url.clone().ok_or("SUPABASE_URL not configured")?;
        let key = router.config.supabase_anon_key.clone().ok_or("SUPABASE_ANON_KEY not configured")?;
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

    Ok(SessionStatus { signed_in: true, free_remaining: None })
}

/// Fetch the managed-provider balance (tier, free_remaining, coin_balance_microdollars).
#[tauri::command]
async fn get_balance(state: State<'_, AppState>) -> Result<server::BalanceResponse, String> {
    let (supabase_url, access_token) = {
        let router = state.ai_router.lock().await;
        let url = router.config.supabase_url.clone().ok_or("SUPABASE_URL not configured")?;
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
    Ok(SessionStatus { signed_in, free_remaining })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .level_for("tauri_plugin_updater", log::LevelFilter::Debug)
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir { file_name: None },
                ))
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Stdout,
                ))
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

            // Resolve the app data directory (user-writable on all platforms).
            // Falls back to CWD so dev builds with no installation still work.
            let app_data_dir = app.path().app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            std::fs::create_dir_all(&app_data_dir).ok();
            let env_path = app_data_dir.join(".env");

            // Init AI Router
            let config = Config::load(Some(&env_path));
            // Apply configured TTS voice (if set) now that config is loaded.
            if !config.tts_voice.is_empty() {
                tts.set_voice(config.tts_voice.clone());
            }
            let cost_tracker = CostTracker::new(
                config.daily_token_cap,
                config.monthly_token_cap,
                config.cost_safety_margin,
                Some(app_data_dir.join("usage.json")),
            );
            let session_manager = SessionManager::new(app_data_dir.join("sessions"));
            let supabase_session_path = app_data_dir.join("supabase_session.json");

            let router = AiRouter::new(config, cost_tracker, session_manager, Some(supabase_session_path.clone()));
            log::info!("AiRouter ready (provider: {})", router.config.api_provider);
            handle.manage(AppState {
                ai_router: tokio::sync::Mutex::new(router),
                guidance: std::sync::Mutex::new(GuidanceState::default()),
                tts,
                tracker,
                last_overlay: std::sync::Mutex::new(None),
                env_path,
                supabase_session_path,
                screen_hash: std::sync::Mutex::new(None),
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
                    // Clean up chat thumbnail + full screenshot before exit.
                    if let Ok(dir) = window.app_handle().path().app_data_dir() {
                        let _ = std::fs::remove_file(dir.join("chat_thumb.jpg"));
                        let _ = std::fs::remove_file(dir.join("chat_full.jpg"));
                    }
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
            open_debug_folder,
            sign_in_anon,
            get_balance,
            get_session_status,
            exit_for_update,
            list_target_windows,
            pin_target_window,
            unpin_target_window,
            list_tts_voices,
            get_chat_full_screenshot,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
