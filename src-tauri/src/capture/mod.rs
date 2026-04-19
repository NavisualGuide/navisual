//! Screen capture — Phase C.1.
//!
//! Two primary entry points:
//! - `capture_primary_monitor_jpeg()`: full primary monitor, JPEG bytes.
//! - `capture_active_window_jpeg()`: foreground window cropped from the
//!   virtual desktop using DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS),
//!   JPEG bytes. Falls back to full primary monitor on failure.
//!
//! Returns JPEG (quality 80) because the AI vision APIs accept JPEG and it's
//! ~10× smaller than raw BGRA on stdout.

use anyhow::{anyhow, Context, Result};
use image::{codecs::jpeg::JpegEncoder, ColorType, ImageBuffer, Rgba};
use xcap::Monitor;

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
    encode_jpeg(&img, quality)
}

/// Capture just the active foreground window (cropped from the virtual desktop).
/// Returns the JPEG bytes and the crop rect in physical pixels (virtual-desktop coords).
pub fn capture_active_window_jpeg(quality: u8) -> Result<(Vec<u8>, Rect)> {
    #[cfg(windows)]
    {
        let rect = win::get_foreground_frame_rect()
            .ok_or_else(|| anyhow!("no foreground window rect"))?;
        let img = capture_region(rect)?;
        let buf = encode_jpeg(&img, quality)?;
        return Ok((buf, rect));
    }

    #[cfg(not(windows))]
    {
        let _ = quality;
        Err(anyhow!("active-window capture only implemented for Windows"))
    }
}

/// Capture a specific rectangular region by first grabbing the monitor that
/// contains the rect's center point, then sub-cropping.
fn capture_region(rect: Rect) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let cx = rect.x + rect.width as i32 / 2;
    let cy = rect.y + rect.height as i32 / 2;
    let monitor = Monitor::from_point(cx, cy)
        .context("find monitor containing rect center")?;
    let img = monitor.capture_image().context("capture monitor")?;

    let mx = monitor.x().unwrap_or(0);
    let my = monitor.y().unwrap_or(0);
    let local_x = (rect.x - mx).max(0) as u32;
    let local_y = (rect.y - my).max(0) as u32;
    let mut w = rect.width;
    let mut h = rect.height;
    if local_x + w > img.width() {
        w = img.width().saturating_sub(local_x);
    }
    if local_y + h > img.height() {
        h = img.height().saturating_sub(local_y);
    }
    if w == 0 || h == 0 {
        return Err(anyhow!(
            "empty crop region after clamping (rect={:?}, monitor={}x{})",
            rect, img.width(), img.height()
        ));
    }
    let cropped = image::imageops::crop_imm(&img, local_x, local_y, w, h).to_image();
    Ok(cropped)
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

/// Convenience: base64-encode JPEG bytes (suitable for AI API payloads).
pub fn to_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
