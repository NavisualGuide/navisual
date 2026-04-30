//! AI Navigator — Rust/Tauri backend entry point.

mod capture;
mod locator;
mod overlay;
mod ai;
mod tts;
mod track;
mod screen_watcher;

use ai::router::AiRouter;
use ai::config::Config;
use ai::cost_tracker::CostTracker;
use ai::session::SessionManager;
use ai::types::{GuidanceStep, OverlayType};

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tauri::{AppHandle, Manager, State, Emitter};
use tokio::time::{timeout, Duration};

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
}

/// Shared app state.
struct AppState {
    ai_router: Mutex<AiRouter>,
    guidance: std::sync::Mutex<GuidanceState>,
    tts: tts::TtsEngine,
    tracker: track::WindowTracker,
    /// Last non-None overlay emitted — used by restore_overlay to bring it back after Clear.
    last_overlay: std::sync::Mutex<Option<(overlay::OverlayKind, Option<capture::Rect>, Option<String>)>>,
    /// Resolved path to the .env settings file — always writable (app data dir).
    env_path: PathBuf,
    #[allow(dead_code)]
    screen_watcher: screen_watcher::ScreenWatcher,
}

fn overlay_kind_for_step(overlay_type: &OverlayType) -> overlay::OverlayKind {
    match overlay_type {
        OverlayType::Arrow => overlay::OverlayKind::Arrow,
        OverlayType::Highlight | OverlayType::Circle => overlay::OverlayKind::Box,
        OverlayType::Subtitle => overlay::OverlayKind::Subtitle,
        OverlayType::None => overlay::OverlayKind::None,
    }
}

fn execute_step(
    app: &AppHandle,
    step: &GuidanceStep,
    tracker: &track::WindowTracker,
    last_overlay: &std::sync::Mutex<Option<(overlay::OverlayKind, Option<capture::Rect>, Option<String>)>>,
) -> Result<Option<locator::LocateResult>, String> {
    let located = if let Some(ref text) = step.target_text {
        #[cfg(windows)]
        {
            let opts = locator::orchestrator::LocateOptions {
                role: step.target_role.as_ref().map(|r| format!("{:?}", r).to_lowercase()),
                nearby_text: step.target_nearby_text.clone(),
                zone: match (step.target_zone_x, step.target_zone_y) {
                    (Some(x), Some(y)) => Some((x as u32, y as u32)),
                    _ => None,
                },
                a11y_timeout_ms: 500,
                min_confidence: 0.5,
            };
            let text_owned = text.clone();
            match locator::orchestrator::locate(&text_owned, &opts) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("locate failed for {:?}: {e}", text);
                    None
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            None
        }
    } else {
        None
    };

    let kind = overlay_kind_for_step(&step.overlay_type);
    let bbox = located.as_ref().map(|r| r.bbox.clone());
    let text_for_overlay = Some(step.instruction.clone());

    if !matches!(kind, overlay::OverlayKind::None) {
        *last_overlay.lock().unwrap() = Some((kind, bbox.clone(), text_for_overlay.clone()));
    }
    match overlay::make_update(kind, bbox.clone(), text_for_overlay.clone()) {
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

    Ok(located)
}

#[derive(serde::Serialize)]
struct GuideResponse {
    ok: bool,
    session_id: String,
    steps: Vec<GuidanceStep>,
    step_index: usize,
    instruction: String,
    located: Option<locator::LocateResult>,
    needs_input: bool,
    provider: String,
    error: Option<String>,
}

#[derive(serde::Serialize, Clone)]
struct StreamChunkPayload {
    delta: String,
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
    overlay_color: String,
    overlay_thickness: u32,
    subtitle_enabled: bool,
    auto_advance: bool,
    tts_enabled: bool,
    voice_input_enabled: bool,
    voice_language: String,
    hotkey_next: String,
    hotkey_wrong: String,
    hotkey_pause: String,
    hotkey_icon: String,
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

#[tauri::command]
async fn guide(
    app: AppHandle,
    state: State<'_, AppState>,
    task: String,
    is_reply: bool,
) -> Result<GuideResponse, String> {
    let is_next_requery = task.starts_with("[User completed:");
    // Only reset session state for a genuine new task (not resume, not reply, not next-requery).
    if !task.is_empty() && !is_reply && !is_next_requery {
        let mut g = state.guidance.lock().unwrap();
        g.session_id = None;
        g.steps = vec![];
        g.state_summary = String::new();
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

    let screenshot_b64 = tokio::task::spawn_blocking(|| {
        capture::capture_active_window_jpeg(75)
            .map(|(bytes, _rect)| capture::to_base64(&bytes))
            .unwrap_or_else(|_| {
                capture::capture_primary_monitor_jpeg(75)
                    .map(|bytes| capture::to_base64(&bytes))
                    .unwrap_or_default()
            })
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    let mut router = state.ai_router.lock().await;
    
    let app_clone = app.clone();
    let on_chunk = move |chunk: &str| {
        let _ = app_clone.emit("stream_chunk", StreamChunkPayload { delta: chunk.to_string() });
    };

    let (resp, sent_user_prompt) = if task.is_empty() || is_next_requery {
        let summary = {
            let g = state.guidance.lock().unwrap();
            g.state_summary.clone()
        };
        let prompt = if task.is_empty() {
            crate::ai::prompts::session_resume_template(&summary)
        } else {
            // Compose: tell the AI what was done + current state + "what's next?"
            format!(
                "{task} The previous state summary: {summary}. \
                Here is the current screen. Please provide the next instruction.",
            )
        };
        (router.send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk).await, prompt)
    } else if is_reply {
        let summary = {
            let g = state.guidance.lock().unwrap();
            g.state_summary.clone()
        };
        (router.send_guidance_request(&task, Some(&screenshot_b64), Some(&summary), on_chunk).await, task.clone())
    } else {
        let prompt = crate::ai::prompts::initial_context_template(&task);
        (router.send_guidance_request(&prompt, Some(&screenshot_b64), None, on_chunk).await, prompt)
    };

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            return Ok(GuideResponse {
                ok: false,
                session_id,
                steps: vec![],
                step_index: 0,
                instruction: String::new(),
                located: None,
                needs_input: false,
                provider: router.config.api_provider.clone(),
                error: Some(e.to_string()),
            });
        }
    };

    let steps = response.steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    let provider = router.config.api_provider.clone();

    if let Some(session) = &mut router.session_manager.current_session {
        session.update_state(state_summary.clone());
        session.add_turn("user", sent_user_prompt, None);
        let content = steps.iter().map(|s| s.instruction.clone()).collect::<Vec<_>>().join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    {
        let mut g = state.guidance.lock().unwrap();
        g.session_id = Some(session_id.clone());
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
        g.provider = provider.clone();
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
            provider,
            error: None,
        });
    }

    let located = execute_step(&app, &steps[0], &state.tracker, &state.last_overlay).unwrap_or(None);

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        provider,
        error: None,
    })
}

#[tauri::command]
async fn next_step(
    app: AppHandle,
    state: State<'_, AppState>,
    step_index: usize,
) -> Result<GuideResponse, String> {
    let (steps, session_id, needs_input, provider) = {
        let g = state.guidance.lock().unwrap();
        (
            g.steps.clone(),
            g.session_id.clone().unwrap_or_default(),
            g.needs_input,
            g.provider.clone(),
        )
    };

    if step_index >= steps.len() {
        return Err(format!("step_index {step_index} out of range ({})", steps.len()));
    }

    let located = execute_step(&app, &steps[step_index], &state.tracker, &state.last_overlay).unwrap_or(None);

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index,
        instruction: steps[step_index].instruction.clone(),
        located,
        needs_input,
        provider,
        error: None,
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
    }
    .ok_or("no active session")?;

    let screenshot_b64 = tokio::task::spawn_blocking(|| {
        capture::capture_active_window_jpeg(75)
            .map(|(bytes, _rect)| capture::to_base64(&bytes))
            .unwrap_or_else(|_| {
                capture::capture_primary_monitor_jpeg(75)
                    .map(|bytes| capture::to_base64(&bytes))
                    .unwrap_or_default()
            })
    })
    .await
    .map_err(|e| format!("capture task join: {e}"))?;

    let mut router = state.ai_router.lock().await;
    let summary = {
        let g = state.guidance.lock().unwrap();
        g.state_summary.clone()
    };

    let user_text_owned = match note.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => format!("{} User note: {}", crate::ai::prompts::CORRECTION_CONTEXT, n),
        None    => crate::ai::prompts::CORRECTION_CONTEXT.to_string(),
    };
    let user_text = user_text_owned.as_str();

    let app_clone = app.clone();
    let on_chunk = move |chunk: &str| {
        let _ = app_clone.emit("stream_chunk", StreamChunkPayload { delta: chunk.to_string() });
    };

    let resp = router.send_guidance_request(user_text, Some(&screenshot_b64), Some(&summary), on_chunk).await;

    let response = match resp {
        Ok(r) => r,
        Err(e) => return Err(e.to_string()),
    };

    let steps = response.steps;
    let state_summary = response.state_summary;
    let needs_input = response.needs_input;
    let provider = router.config.api_provider.clone();

    if let Some(session) = &mut router.session_manager.current_session {
        session.update_state(state_summary.clone());
        session.add_turn("user", user_text.to_string(), None);
        let content = steps.iter().map(|s| s.instruction.clone()).collect::<Vec<_>>().join("\n");
        session.add_turn("assistant", content, Some("...".to_string()));
        router.session_manager.save_session(None);
    }

    {
        let mut g = state.guidance.lock().unwrap();
        g.steps = steps.clone();
        g.state_summary = state_summary;
        g.needs_input = needs_input;
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
            provider,
            error: None,
        });
    }

    let located = execute_step(&app, &steps[0], &state.tracker, &state.last_overlay).unwrap_or(None);

    Ok(GuideResponse {
        ok: true,
        session_id,
        steps: steps.clone(),
        step_index: 0,
        instruction: steps[0].instruction.clone(),
        located,
        needs_input,
        provider,
        error: None,
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
    if let Some((kind, bbox, text)) = state.last_overlay.lock().unwrap().clone() {
        match overlay::make_update(kind, bbox, text) {
            Ok(update) => overlay::emit_update(&app, update).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        Ok(())
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
        Some(result.bbox.clone()),
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
            zone: None,
            a11y_timeout_ms: timeout_ms.unwrap_or(1500),
            min_confidence: 0.5,
        };
        let result = tokio::task::spawn_blocking(move || {
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
    zone_x: Option<u32>,
    zone_y: Option<u32>,
    timeout_ms: Option<u64>,
) -> Result<Option<locator::LocateResult>, String> {
    #[cfg(windows)]
    {
        let opts = locator::orchestrator::LocateOptions {
            role,
            nearby_text,
            zone: match (zone_x, zone_y) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => None,
            },
            a11y_timeout_ms: timeout_ms.unwrap_or(500),
            min_confidence: 0.5,
        };
        let result = tokio::task::spawn_blocking(move || locator::orchestrator::locate(&text, &opts))
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
        let _ = (app, text, role, nearby_text, zone_x, zone_y, timeout_ms);
        Err("locate_element only implemented for Windows".to_string())
    }
}

#[tauri::command]
async fn capture_active_window(quality: Option<u8>) -> Result<CaptureResult, String> {
    let q = quality.unwrap_or(80);
    let start = std::time::Instant::now();
    let (bytes, rect) = tokio::task::spawn_blocking(move || capture::capture_active_window_jpeg(q))
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
        overlay_color: c.overlay_color.clone(),
        overlay_thickness: c.overlay_thickness,
        subtitle_enabled: c.subtitle_enabled,
        auto_advance: c.auto_advance,
        tts_enabled: c.tts_enabled,
        voice_input_enabled: c.voice_input_enabled,
        voice_language: c.voice_language.clone(),
        hotkey_next:  c.hotkey_next.clone(),
        hotkey_wrong: c.hotkey_wrong.clone(),
        hotkey_pause: c.hotkey_pause.clone(),
        hotkey_icon:  c.hotkey_icon.clone(),
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
        ("OVERLAY_COLOR".into(),        payload.overlay_color.clone()),
        ("OVERLAY_THICKNESS".into(),    payload.overlay_thickness.to_string()),
        ("SUBTITLE_ENABLED".into(),     payload.subtitle_enabled.to_string()),
        ("AUTO_ADVANCE".into(),         payload.auto_advance.to_string()),
        ("TTS_ENABLED".into(),          payload.tts_enabled.to_string()),
        ("VOICE_INPUT_ENABLED".into(),  payload.voice_input_enabled.to_string()),
        ("VOICE_LANGUAGE".into(),       payload.voice_language.clone()),
        ("HOTKEY_NEXT".into(),          payload.hotkey_next.clone()),
        ("HOTKEY_WRONG".into(),         payload.hotkey_wrong.clone()),
        ("HOTKEY_PAUSE".into(),         payload.hotkey_pause.clone()),
        ("HOTKEY_ICON".into(),          payload.hotkey_icon.clone()),
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

    // Notify the overlay canvas of the new theme (broadcasts to all webview windows).
    let _ = app.emit("overlay:theme", OverlayThemePayload {
        color: payload.overlay_color,
        thickness: payload.overlay_thickness,
        subtitle_enabled: payload.subtitle_enabled,
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
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
            let watcher = screen_watcher::ScreenWatcher::start(handle.clone());

            // Resolve the app data directory (user-writable on all platforms).
            // Falls back to CWD so dev builds with no installation still work.
            let app_data_dir = app.path().app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            std::fs::create_dir_all(&app_data_dir).ok();
            let env_path = app_data_dir.join(".env");

            // Init AI Router
            let config = Config::load(Some(&env_path));
            let cost_tracker = CostTracker::new(
                config.daily_token_cap,
                config.monthly_token_cap,
                config.cost_safety_margin,
                Some(app_data_dir.join("usage.json")),
            );
            let session_manager = SessionManager::new(app_data_dir.join("sessions"));

            let router = AiRouter::new(config, cost_tracker, session_manager);
            log::info!("AiRouter ready (provider: {})", router.config.api_provider);
            handle.manage(AppState {
                ai_router: tokio::sync::Mutex::new(router),
                guidance: std::sync::Mutex::new(GuidanceState::default()),
                tts,
                tracker,
                last_overlay: std::sync::Mutex::new(None),
                env_path,
                screen_watcher: watcher,
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
            clear_overlay,
            restore_overlay,
            speak,
            get_settings,
            save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
