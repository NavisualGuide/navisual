//! AI Navigator — Rust/Tauri backend entry point.

mod sidecar;

use sidecar::Sidecar;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};

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
        .invoke_handler(tauri::generate_handler![ping_sidecar, sidecar_echo])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
