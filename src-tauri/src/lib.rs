//! AI Navigator — Rust/Tauri backend entry point.

mod capture;
mod locator;
mod overlay;
mod sidecar;
mod tts;
mod track;

use sidecar::Sidecar;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GuidanceStep {
    instruction: String,
    target_text: Option<String>,
    target_role: Option<String>,
    target_nearby_text: Option<String>,
    target_zone_x: Option<i64>,
    target_zone_y: Option<i64>,
    overlay_type: String,
    clipboard: Option<String>,
    checkpoint: bool,
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
    sidecar: Arc<Sidecar>,
    guidance: Mutex<GuidanceState>,
    tts: tts::TtsEngine,
    tracker: track::WindowTracker,
}

/// Locate the sidecar script. In dev, it's at `<project-root>/sidecar/main.py`.
fn resolve_sidecar_script() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .map(|p| p.join("sidecar").join("main.py"))
        .unwrap_or_else(|| PathBuf::from("../sidecar/main.py"))
}

fn overlay_kind_for_step(overlay_type: &str) -> overlay::OverlayKind {
    match overlay_type {
        "arrow" => overlay::OverlayKind::Arrow,
        "highlight" | "box" | "circle" => overlay::OverlayKind::Box,
        "subtitle" => overlay::OverlayKind::Subtitle,
        _ => overlay::OverlayKind::None,
    }
}

fn execute_step(
    app: &AppHandle,
    step: &GuidanceStep,
    tracker: &track::WindowTracker,
) -> Result<Option<locator::LocateResult>, String> {
    let located = if let Some(ref text) = step.target_text {
        #[cfg(windows)]
        {
            let opts = locator::orchestrator::LocateOptions {
                role: step.target_role.clone(),
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

    match overlay::make_update(kind, bbox.clone(), text_for_overlay.clone()) {
        Ok(update) => {
            if let Err(e) = overlay::emit_update(app, update) {
                log::warn!("overlay emit failed: {e}");
            }
        }
        Err(e) => log::warn!("overlay make_update failed: {e}"),
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

#[tauri::command]
async fn guide(
    app: AppHandle,
    state: State<'_, AppState>,
    task: String,
) -> Result<GuideResponse, String> {
    let sidecar = state.sidecar.clone();

    let session_id = {
        let g = state.guidance.lock().unwrap();
        g.session_id.clone()
    };

    let session_id = if let Some(sid) = session_id {
        sid
    } else {
        let resp = sidecar
            .request("start_session", serde_json::json!({ "task": task }))
            .await
            .map_err(|e| e.to_string())?;
        let sid = resp
            .payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("start_session: no session_id")?
            .to_string();
        let provider = resp
            .payload
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        {
            let mut g = state.guidance.lock().unwrap();
            g.session_id = Some(sid.clone());
            g.provider = provider;
        }
        sid
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

    let user_text = if task.is_empty() { "continue".to_string() } else { task };

    let resp = timeout(
        Duration::from_secs(90),
        sidecar.request(
            "send_guidance",
            serde_json::json!({
                "session_id": session_id,
                "user_text": user_text,
                "screenshot_b64": screenshot_b64,
            }),
        ),
    )
    .await
    .map_err(|_| "AI request timed out (90 s)".to_string())?
    .map_err(|e| e.to_string())?;

    if !resp.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = resp
            .payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("send_guidance failed")
            .to_string();
        return Ok(GuideResponse {
            ok: false,
            session_id,
            steps: vec![],
            step_index: 0,
            instruction: String::new(),
            located: None,
            needs_input: false,
            provider: String::new(),
            error: Some(err),
        });
    }

    let response_val = resp
        .payload
        .get("response")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let steps: Vec<GuidanceStep> = response_val
        .get("steps")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let state_summary = response_val
        .get("state_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let needs_input = response_val
        .get("needs_input")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let provider = {
        let g = state.guidance.lock().unwrap();
        g.provider.clone()
    };

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

    let located = execute_step(&app, &steps[0], &state.tracker).unwrap_or(None);

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

    let located = execute_step(&app, &steps[step_index], &state.tracker).unwrap_or(None);

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
) -> Result<GuideResponse, String> {
    let sidecar = state.sidecar.clone();

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

    let resp = timeout(
        Duration::from_secs(90),
        sidecar.request(
            "trigger_correction",
            serde_json::json!({
                "session_id": session_id,
                "screenshot_b64": screenshot_b64,
            }),
        ),
    )
    .await
    .map_err(|_| "AI request timed out (90 s)".to_string())?
    .map_err(|e| e.to_string())?;

    if !resp.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = resp
            .payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("correction failed")
            .to_string();
        return Err(err);
    }

    let response_val = resp
        .payload
        .get("response")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let steps: Vec<GuidanceStep> = response_val
        .get("steps")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let state_summary = response_val
        .get("state_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let needs_input = response_val
        .get("needs_input")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let provider = {
        let g = state.guidance.lock().unwrap();
        g.provider.clone()
    };

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

    let located = execute_step(&app, &steps[0], &state.tracker).unwrap_or(None);

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
async fn ping_sidecar(state: State<'_, AppState>) -> Result<String, String> {
    let resp = state
        .sidecar
        .request("ping", serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.payload.to_string())
}

#[tauri::command]
async fn sidecar_echo(text: String, state: State<'_, AppState>) -> Result<String, String> {
    let resp = state
        .sidecar
        .request("echo", serde_json::json!({ "text": text }))
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.payload.to_string())
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
        let timeout = timeout_ms.unwrap_or(1500);
        let result = tokio::task::spawn_blocking(move || {
            locator::a11y::find_element(&text, role.as_deref(), timeout)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let script = resolve_sidecar_script();
            log::info!("spawning sidecar: {}", script.display());

            if let Some(overlay_win) = app.get_webview_window("overlay") {
                match overlay::configure(&overlay_win) {
                    Ok(()) => {
                        let _ = overlay_win.show();
                        log::info!("overlay window configured and shown");
                    }
                    Err(e) => {
                        log::error!("overlay configure failed — NOT showing (would freeze input): {e}");
                    }
                }
            }

            let handle = app.handle().clone();
            let tts = tts::TtsEngine::new();
            let tracker = track::WindowTracker::new();
            tauri::async_runtime::spawn(async move {
                match Sidecar::spawn(script).await {
                    Ok(sc) => {
                        handle.manage(AppState {
                            sidecar: Arc::new(sc),
                            guidance: Mutex::new(GuidanceState::default()),
                            tts,
                            tracker,
                        });
                        log::info!("sidecar ready");
                    }
                    Err(e) => {
                        log::error!("sidecar spawn failed: {e}");
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "panel" {
                if let tauri::WindowEvent::Destroyed = event {
                    std::process::exit(0);
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
            speak,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
