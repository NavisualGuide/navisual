//! AI Navigator — Rust/Tauri backend entry point.

mod capture;
mod locator;
mod overlay;
mod ai;
mod tts;
mod track;

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
}

fn overlay_kind_for_step(overlay_type: &OverlayType) -> overlay::OverlayKind {
    match overlay_type {
        OverlayType::Arrow => overlay::OverlayKind::Arrow,
        OverlayType::Highlight | OverlayType::Circle => overlay::OverlayKind::Box,
        OverlayType::None => overlay::OverlayKind::None,
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

#[derive(serde::Serialize, Clone)]
struct StreamChunkPayload {
    delta: String,
}

#[tauri::command]
async fn guide(
    app: AppHandle,
    state: State<'_, AppState>,
    task: String,
) -> Result<GuideResponse, String> {
    if !task.is_empty() {
        let mut g = state.guidance.lock().unwrap();
        g.session_id = None;
        g.steps = vec![];
        g.state_summary = String::new();
    }

    let session_id = {
        let mut router = state.ai_router.lock().await;
        if task.is_empty() {
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

    let resp = if task.is_empty() {
        let summary = {
            let g = state.guidance.lock().unwrap();
            g.state_summary.clone()
        };
        router.send_resume_request(&summary, Some(&screenshot_b64), on_chunk).await
    } else {
        router.send_initial_request(&task, Some(&screenshot_b64), on_chunk).await
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

    let user_text = crate::ai::prompts::CORRECTION_CONTEXT;
    
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
            let overlay_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                let build = tauri::WebviewWindowBuilder::new(
                    &overlay_handle,
                    "overlay",
                    tauri::WebviewUrl::App(std::path::PathBuf::from("overlay")),
                )
                .title("AI Navigator Overlay")
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .focused(false)
                .visible(false)
                .build();
                match build {
                    Ok(win) => match overlay::configure(&win) {
                        Ok(()) => {
                            let _ = win.show();
                            log::info!("overlay window created and shown");
                        }
                        Err(e) => log::error!(
                            "overlay configure failed — NOT showing (would freeze input): {e}"
                        ),
                    },
                    Err(e) => log::error!("overlay window creation failed: {e}"),
                }
            });

            let handle = app.handle().clone();
            let tts = tts::TtsEngine::new();
            let tracker = track::WindowTracker::new();

            // Init AI Router
            let config = Config::load();
            let cost_tracker = CostTracker::new(
                config.daily_token_cap,
                config.monthly_token_cap,
                config.cost_safety_margin,
                Some(PathBuf::from(".ai_navigator/usage.json")),
            );
            let session_manager = SessionManager::new(PathBuf::from(".ai_navigator/sessions"));
            
            match AiRouter::new(config, cost_tracker, session_manager) {
                Ok(router) => {
                    handle.manage(AppState {
                        ai_router: tokio::sync::Mutex::new(router),
                        guidance: std::sync::Mutex::new(GuidanceState::default()),
                        tts,
                        tracker,
                    });
                    log::info!("AiRouter ready");
                }
                Err(e) => {
                    log::error!("AiRouter init failed: {e}");
                }
            }

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
            speak,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
