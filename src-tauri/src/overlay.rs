//! Transparent click-through overlay window — Phase D.1.
//!
//! The overlay is a second Tauri `WebviewWindow` (label `"overlay"`) that:
//! - covers the entire virtual desktop (union of all monitors),
//! - is always-on-top, undecorated, transparent,
//! - has `WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW` applied so
//!   input clicks pass through to whatever app is underneath.
//!
//! The window's HTML/canvas is rendered by the Svelte route `/overlay`.
//! Rust emits an `overlay:update` event whenever the target changes; the
//! frontend consumes it to redraw the arrow/box/subtitle.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::capture::Rect;

/// An overlay command. Mirrors the v0.3 Python `overlay.py` primitives so
/// the Svelte canvas renderer can match it one-for-one.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayKind {
    /// Arrow pointing at bbox from the nearest panel edge.
    Arrow,
    /// Rounded highlight box around bbox.
    Box,
    /// Subtitle strip along the bottom of the active screen.
    Subtitle,
    /// No draw — clears the overlay.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverlayUpdate {
    pub kind: OverlayKind,
    /// Target bbox in virtual-desktop physical pixels, or None for subtitle-only.
    pub bbox: Option<Rect>,
    /// Optional subtitle / instruction text.
    pub text: Option<String>,
    /// Virtual-desktop origin + size so the renderer can convert bbox
    /// coords into overlay-window-relative coords without needing the
    /// Tauri position API (which may lag behind).
    pub virtual_origin: (i32, i32),
    pub virtual_size: (u32, u32),
}

struct CachedVd {
    rect: Rect,
    at: Instant,
}

static VD_CACHE: OnceLock<Mutex<Option<CachedVd>>> = OnceLock::new();

/// Compute the union rect of all monitors, cached for 30 s.
/// Monitor topology changes are extremely rare; re-enumerating on every
/// 200 ms window-tracker tick (xcap::Monitor::all syscall) adds ~2–5 ms
/// per call unnecessarily.
fn virtual_desktop_rect() -> Result<Rect> {
    let cache = VD_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some(ref c) = *guard {
        if c.at.elapsed() < Duration::from_secs(30) {
            return Ok(c.rect.clone());
        }
    }
    let monitors = xcap::Monitor::all().context("enumerate monitors")?;
    if monitors.is_empty() {
        return Err(anyhow!("no monitors found"));
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        let x = m.x().unwrap_or(0);
        let y = m.y().unwrap_or(0);
        let w = m.width().unwrap_or(0) as i32;
        let h = m.height().unwrap_or(0) as i32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    let rect = Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0) as u32,
        height: (max_y - min_y).max(0) as u32,
    };
    *guard = Some(CachedVd { rect: rect.clone(), at: Instant::now() });
    Ok(rect)
}

/// Size & position the overlay to span the virtual desktop, and apply
/// click-through via Tauri's built-in API (which correctly propagates to
/// the WebView2 child HWND — raw SetWindowLongPtrW on the outer HWND alone
/// does not prevent WebView2 from capturing input).
///
/// CRITICAL: click-through must succeed before show(). A fullscreen
/// transparent window that still captures input freezes the desktop.
pub fn configure(window: &WebviewWindow) -> Result<()> {
    // Use Tauri's API — it handles WebView2's child HWND correctly.
    window
        .set_ignore_cursor_events(true)
        .map_err(|e| anyhow!("set_ignore_cursor_events: {e}"))?;

    // Size to virtual desktop — best-effort; failure means overlay is
    // mispositioned but still click-through and safe to show.
    match virtual_desktop_rect() {
        Ok(rect) => {
            if let Err(e) = window.set_position(PhysicalPosition::new(rect.x, rect.y)) {
                log::warn!("overlay set_position failed: {e}");
            }
            if let Err(e) = window.set_size(PhysicalSize::new(rect.width, rect.height)) {
                log::warn!("overlay set_size failed: {e}");
            }
        }
        Err(e) => log::warn!("overlay virtual_desktop_rect failed: {e}"),
    }

    Ok(())
}

/// Emit an `overlay:update` event to the overlay frontend.
pub fn emit_update(app: &AppHandle, update: OverlayUpdate) -> Result<()> {
    let Some(window) = app.get_webview_window("overlay") else {
        return Err(anyhow!("overlay window not found"));
    };
    window
        .emit("overlay:update", &update)
        .map_err(|e| anyhow!("emit overlay:update: {e}"))?;
    Ok(())
}

/// Build an OverlayUpdate with fresh virtual-desktop metadata.
pub fn make_update(
    kind: OverlayKind,
    bbox: Option<Rect>,
    text: Option<String>,
) -> Result<OverlayUpdate> {
    let vd = virtual_desktop_rect()?;
    Ok(OverlayUpdate {
        kind,
        bbox,
        text,
        virtual_origin: (vd.x, vd.y),
        virtual_size: (vd.width, vd.height),
    })
}

