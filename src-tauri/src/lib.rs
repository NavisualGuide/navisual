//! AI Navigator — Rust/Tauri backend entry point.

mod capture;
mod locator;
mod sidecar;

use sidecar::Sidecar;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};

#[derive(serde::Serialize)]
struct CaptureResult {
    jpeg_base64: String,
    width: u32,
    height: u32,
    crop_rect: Option<capture::Rect>,
    bytes: usize,
    elapsed_ms: u128,
}

/// Shared app state — the sidecar handle lives for the app's lifetime.
struct AppState {
    sidecar: Arc<Sidecar>,
}

/// Locate the sidecar script. In dev, it's at `<project-root>/sidecar/main.py`.
/// In a bundled build this will be replaced by Tauri's resource directory.
fn resolve_sidecar_script() -> PathBuf {
    // backend/ is the Cargo crate root; sidecar/ sits alongside it at repo root.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .map(|p| p.join("sidecar").join("main.py"))
        .unwrap_or_else(|| PathBuf::from("../sidecar/main.py"))
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

/// Capture the primary monitor. Returns base64 JPEG + dimensions for the UI.
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

/// Locate a UI element by text label via Windows UI Automation. Returns
/// None if not found. `role` is one of our schema roles (button, tab, link…).
#[tauri::command]
async fn locate_a11y(
    text: String,
    role: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Option<locator::LocateResult>, String> {
    #[cfg(windows)]
    {
        let timeout = timeout_ms.unwrap_or(100);
        tokio::task::spawn_blocking(move || {
            locator::a11y::find_element(&text, role.as_deref(), timeout)
        })
        .await
        .map_err(|e| format!("task join: {e}"))?
        .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = (text, role, timeout_ms);
        Err("A11y only implemented for Windows".to_string())
    }
}

/// Locate a UI element — A11y first, OCR fallback. Returns None on miss.
#[tauri::command]
async fn locate_element(
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
            a11y_timeout_ms: timeout_ms.unwrap_or(150),
            min_confidence: 0.5,
        };
        tokio::task::spawn_blocking(move || locator::orchestrator::locate(&text, &opts))
            .await
            .map_err(|e| format!("task join: {e}"))?
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = (text, role, nearby_text, zone_x, zone_y, timeout_ms);
        Err("locate_element only implemented for Windows".to_string())
    }
}

/// Capture the active foreground window (DWM extended frame bounds).
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
        .setup(|app| {
            let script = resolve_sidecar_script();
            log::info!("spawning sidecar: {}", script.display());

            // Block-on is acceptable in setup — it runs once at startup.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match Sidecar::spawn(script).await {
                    Ok(sc) => {
                        handle.manage(AppState {
                            sidecar: Arc::new(sc),
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
        .invoke_handler(tauri::generate_handler![
            ping_sidecar,
            sidecar_echo,
            capture_screen,
            capture_active_window,
            locate_a11y,
            locate_element,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
