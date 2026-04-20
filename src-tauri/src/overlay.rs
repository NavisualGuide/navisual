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

/// Compute the union rect of all monitors (virtual desktop) in physical px.
fn virtual_desktop_rect() -> Result<Rect> {
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
    Ok(Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0) as u32,
        height: (max_y - min_y).max(0) as u32,
    })
}

/// Size & position the overlay to span the virtual desktop, and apply
/// click-through window styles. Safe to call multiple times (e.g. on
/// monitor hot-plug — later).
pub fn configure(window: &WebviewWindow) -> Result<()> {
    let rect = virtual_desktop_rect()?;
    window
        .set_position(PhysicalPosition::new(rect.x, rect.y))
        .map_err(|e| anyhow!("set_position: {e}"))?;
    window
        .set_size(PhysicalSize::new(rect.width, rect.height))
        .map_err(|e| anyhow!("set_size: {e}"))?;

    #[cfg(windows)]
    apply_click_through(window)?;

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

#[cfg(windows)]
fn apply_click_through(window: &WebviewWindow) -> Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WINDOW_EX_STYLE, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    let hwnd_raw = window
        .hwnd()
        .map_err(|e| anyhow!("hwnd: {e}"))?;
    let hwnd = HWND(hwnd_raw.0 as *mut _);
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = WINDOW_EX_STYLE(cur as u32)
            | WS_EX_TRANSPARENT
            | WS_EX_LAYERED
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style.0 as isize);
    }
    Ok(())
}
