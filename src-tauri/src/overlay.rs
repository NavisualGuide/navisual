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

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::capture::Rect;

/// An overlay command. Mirrors the v0.3 Python `overlay.py` primitives so
/// the Svelte canvas renderer can match it one-for-one.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    /// Arrow pointing at bbox from the nearest panel edge.
    Arrow,
    /// Rounded highlight box around bbox.
    Box,
    /// Subtitle strip along the bottom of the active screen.
    Subtitle,
    /// Phase 0.2: outline the captured app's window with a flash + fade
    /// animation so the user can see exactly what's being shared.
    AppBoundary,
    /// AI-bbox fallback when A11y/OCR both missed but the AI returned a
    /// `target_bbox`. Rendered as a soft diffuse highlight at the inflated
    /// AI bbox — a "look around here" cue, not a precise pointer.
    Hint,
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
    /// The monitor the target element lives on (virtual-desktop physical pixels).
    /// Used to confine the subtitle strip to a single screen.
    pub active_screen: Option<Rect>,
    /// AI-returned bounding box in virtual-desktop physical pixels.
    /// Drawn as a distinct cyan-dashed box alongside the production pointer
    /// when the developer "Show AI bbox" toggle is enabled.
    pub ai_bbox: Option<Rect>,
}

/// Find which monitor contains the centre of `bbox`. When `bbox` is `None`
/// (subtitle-only step, app-boundary clear, etc.), reuse the last-known
/// active screen instead of jumping to monitor-nearest-origin — otherwise the
/// subtitle visibly shifts between monitors mid-session.
fn active_screen_for_bbox(bbox: Option<&Rect>) -> Option<Rect> {
    let monitors = crate::capture::enumerate_monitor_rects();
    if monitors.is_empty() {
        return None;
    }

    if let Some(b) = bbox {
        let cx = b.x + (b.width as i32) / 2;
        let cy = b.y + (b.height as i32) / 2;
        for m in &monitors {
            if cx >= m.x && cx < m.x + m.width as i32 && cy >= m.y && cy < m.y + m.height as i32 {
                *LAST_ACTIVE_SCREEN
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .unwrap() = Some(*m);
                return Some(*m);
            }
        }
    }

    // No bbox (or bbox off-screen) — reuse the last-known active screen so the
    // subtitle stays on the monitor the user is working on.
    if let Some(cached) = *LAST_ACTIVE_SCREEN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
    {
        return Some(cached);
    }

    // First-ever call with no bbox: fall back to the monitor closest to (0, 0).
    monitors
        .iter()
        .min_by_key(|m| m.x.abs() + m.y.abs())
        .copied()
}

/// Last monitor a bbox-bearing OverlayUpdate landed on. Used to stabilise the
/// subtitle position across subtitle-only / app-boundary / clear emits.
static LAST_ACTIVE_SCREEN: OnceLock<Mutex<Option<Rect>>> = OnceLock::new();

struct CachedVd {
    rect: Rect,
    at: Instant,
}

static VD_CACHE: OnceLock<Mutex<Option<CachedVd>>> = OnceLock::new();

/// Compute the union rect of all monitors, cached for 30 s.
/// Monitor topology changes are extremely rare; re-enumerating on every
/// 200 ms window-tracker tick adds latency unnecessarily. The underlying
/// `enumerate_monitor_rects` call is sub-millisecond, but the cache keeps
/// the hot path allocation-free.
pub fn virtual_desktop_rect() -> Result<Rect> {
    let cache = VD_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some(ref c) = *guard {
        if c.at.elapsed() < Duration::from_secs(30) {
            return Ok(c.rect);
        }
    }
    let monitors = crate::capture::enumerate_monitor_rects();
    if monitors.is_empty() {
        return Err(anyhow!("no monitors found"));
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        min_x = min_x.min(m.x);
        min_y = min_y.min(m.y);
        max_x = max_x.max(m.x + m.width as i32);
        max_y = max_y.max(m.y + m.height as i32);
    }
    let rect = Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0) as u32,
        height: (max_y - min_y).max(0) as u32,
    };
    *guard = Some(CachedVd {
        rect,
        at: Instant::now(),
    });
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

/// Drop the cached virtual-desktop rect so the next `virtual_desktop_rect()` call
/// re-enumerates monitors instead of returning up-to-30s-stale data. Called from
/// `reconfigure` below so a real display-configuration change is reflected immediately
/// rather than waiting out the cache.
fn invalidate_virtual_desktop_cache() {
    let cache = VD_CACHE.get_or_init(|| Mutex::new(None));
    *cache.lock().unwrap() = None;
}

/// Re-run `configure` against the live overlay window and current monitor topology.
///
/// `configure` itself only ever runs once, ~2s after app startup (`lib.rs` setup) — nothing
/// previously re-ran it, so the overlay window's *physical* OS-level position and size stayed
/// permanently fixed to whatever topology existed at that moment. Plugging/unplugging a
/// monitor during the session left the window misaligned with the real desktop for the rest
/// of the run (pointer/box invisible, or clipped to stale bounds) — confirmed live 2026-07-07.
/// `track.rs` calls this in response to the OS's `WM_DISPLAYCHANGE` message, which is the
/// correct, immediate signal for "the display configuration just changed."
pub fn reconfigure(app: &AppHandle) {
    invalidate_virtual_desktop_cache();
    let Some(window) = app.get_webview_window("overlay") else {
        log::warn!("overlay reconfigure: overlay window not found");
        return;
    };
    match configure(&window) {
        Ok(()) => log::info!("overlay reconfigured after display change"),
        Err(e) => log::warn!("overlay reconfigure failed: {e}"),
    }
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
    make_update_with_ai_bbox(kind, bbox, text, None)
}

/// Build an OverlayUpdate that also carries an `ai_bbox` for the developer
/// overlay (cyan dashed box). May be `None`.
pub fn make_update_with_ai_bbox(
    kind: OverlayKind,
    bbox: Option<Rect>,
    text: Option<String>,
    ai_bbox: Option<Rect>,
) -> Result<OverlayUpdate> {
    let vd = virtual_desktop_rect()?;
    let active_screen = active_screen_for_bbox(bbox.as_ref().or(ai_bbox.as_ref()));
    Ok(OverlayUpdate {
        kind,
        bbox,
        text,
        virtual_origin: (vd.x, vd.y),
        virtual_size: (vd.width, vd.height),
        active_screen,
        ai_bbox,
    })
}
