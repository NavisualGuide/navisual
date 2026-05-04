//! Screen capture — Phase C.1.
//!
//! Two primary entry points:
//! - `capture_primary_monitor_jpeg()`: full primary monitor, JPEG bytes.
//! - `capture_active_window_jpeg(quality, exclude)`: foreground window via
//!   per-monitor BitBlt. Returns (jpeg, rect, raw_hwnd). Store the HWND and
//!   pass it to `recapture_window_jpeg` on subsequent calls.
//!
//! Unlike the old PrintWindow path, per-monitor BitBlt reads the composited
//! screen surface — dialogs floating above the target window appear naturally,
//! BUT so does our own panel if it overlaps. Callers must pass the panel's
//! current screen rect in `exclude` so it is blanked (neutral grey) before
//! JPEG encoding.

use anyhow::{anyhow, Context, Result};
use image::{codecs::jpeg::JpegEncoder, ColorType, ImageBuffer, Rgba};
use xcap::Monitor;

const MAX_CAP_W: u32 = 1536;
const MAX_CAP_H: u32 = 768;

#[cfg(windows)]
mod win;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Capture the primary monitor and encode as JPEG.
pub fn capture_primary_monitor_jpeg(quality: u8) -> Result<Vec<u8>> {
    let monitors = Monitor::all().context("enumerate monitors")?;
    let primary = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .ok_or_else(|| anyhow!("no primary monitor"))?;
    let img = primary.capture_image().context("capture primary monitor")?;
    encode_jpeg(&cap_size(img), quality)
}

/// Capture the active foreground window area as JPEG.
///
/// `exclude` — screen rects to blank (neutral grey) before encoding. Pass the
/// panel's current rect so it does not appear in the AI's screenshot.
/// `GA_ROOTOWNER` is applied to dialogs so the stored HWND is always the
/// stable main-window handle, not an owned dialog that may close at any time.
///
/// Returns (jpeg bytes, window rect in physical pixels, raw HWND as usize).
/// Store the HWND and pass it to `recapture_window_jpeg` on subsequent calls.
pub fn capture_active_window_jpeg(quality: u8, exclude: &[Rect]) -> Result<(Vec<u8>, Rect, usize)> {
    #[cfg(windows)]
    {
        let (hwnd, rect) = win::get_foreground_target()
            .ok_or_else(|| anyhow!("no foreground window found"))?;
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        return Ok((buf, rect, hwnd.0 as usize));
    }

    #[cfg(not(windows))]
    {
        let _ = (quality, exclude);
        Err(anyhow!("active-window capture only implemented for Windows"))
    }
}

/// Re-capture a previously discovered window by its stored raw HWND.
/// Validates the window is still alive and not minimised before capturing.
/// Returns an error if the window is gone — caller should then call
/// `capture_active_window_jpeg` to rediscover.
///
/// `exclude` — same semantics as `capture_active_window_jpeg`.
pub fn recapture_window_jpeg(hwnd_raw: usize, quality: u8, exclude: &[Rect]) -> Result<(Vec<u8>, Rect)> {
    #[cfg(windows)]
    {
        let rect = win::validate_hwnd_raw(hwnd_raw)
            .ok_or_else(|| anyhow!("stored window is no longer valid (closed or minimised)"))?;
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        return Ok((buf, rect));
    }

    #[cfg(not(windows))]
    {
        let _ = (hwnd_raw, quality, exclude);
        Err(anyhow!("recapture only implemented for Windows"))
    }
}

/// Downscale `img` to fit within MAX_CAP_W × MAX_CAP_H, preserving aspect ratio.
/// Returns the original unchanged if already within bounds.
fn cap_size(img: ImageBuffer<Rgba<u8>, Vec<u8>>) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    if w <= MAX_CAP_W && h <= MAX_CAP_H {
        return img;
    }
    let scale = (MAX_CAP_W as f32 / w as f32).min(MAX_CAP_H as f32 / h as f32);
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
}

fn encode_jpeg(img: &ImageBuffer<Rgba<u8>, Vec<u8>>, quality: u8) -> Result<Vec<u8>> {
    // JPEG doesn't support alpha; convert RGBA → RGB.
    let (w, h) = (img.width(), img.height());
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in img.pixels() {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }
    let mut out = Vec::with_capacity(rgb.len() / 4);
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    encoder.encode(&rgb, w, h, ColorType::Rgb8.into())
        .context("jpeg encode")?;
    Ok(out)
}

/// Return the screen rects of all AI Navigator windows that should be blanked
/// from captures (currently: the panel; the overlay is excluded by size).
///
/// Uses EnumWindows by PID + size filter instead of Tauri's `hwnd()` to avoid
/// windows-rs version conflicts between Tauri's dependency and ours.
///
/// BitBlt reads the composited display — the panel appears in screenshots if
/// it overlaps the target app. Blanking it keeps the AI's image clean and
/// prevents the panel's own UI updates from triggering false screen-change events.
pub fn get_panel_rects() -> Vec<Rect> {
    #[cfg(windows)]
    {
        win::own_panel_rects()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Convenience: base64-encode JPEG bytes (suitable for AI API payloads).
pub fn to_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
