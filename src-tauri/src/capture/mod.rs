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

const MAX_CAP_W: u32 = 1536;
const MAX_CAP_H: u32 = 768;

#[cfg(windows)]
mod win;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Enumerate every connected monitor's rect in virtual-desktop coordinates.
/// Stable cross-platform signature so callers don't reach for xcap directly.
pub fn enumerate_monitor_rects() -> Vec<Rect> {
    #[cfg(windows)]
    {
        win::enumerate_monitor_rects()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Re-export so lib.rs can use `capture::MonitorInfo`.
#[cfg(windows)]
pub use win::MonitorInfo;

/// Enumerate connected monitors (index, primary flag, rect) for the target picker's
/// per-screen choices.
#[cfg(windows)]
pub fn list_monitors() -> Vec<win::MonitorInfo> {
    win::list_monitors()
}

/// Resolve a monitor index (from `list_monitors`) back to its virtual-desktop rect.
/// `None` if the index is out of range (e.g. a monitor was unplugged after picking).
#[cfg(windows)]
pub fn monitor_rect(index: usize) -> Option<Rect> {
    win::list_monitors().get(index).map(|m| Rect {
        x: m.x,
        y: m.y,
        width: m.width,
        height: m.height,
    })
}

/// Capture the primary monitor and encode as JPEG. The primary monitor is, by
/// Windows convention, the monitor whose top-left is at (0, 0) in virtual-
/// desktop coordinates.
pub fn capture_primary_monitor_jpeg(quality: u8) -> Result<Vec<u8>> {
    #[cfg(windows)]
    {
        let primary = win::enumerate_monitor_rects()
            .into_iter()
            .find(|r| r.x == 0 && r.y == 0)
            .ok_or_else(|| anyhow!("no primary monitor"))?;
        let img = win::capture_desktop_region(&primary).context("capture primary monitor")?;
        encode_jpeg(&cap_size(img), quality)
    }

    #[cfg(not(windows))]
    {
        let _ = quality;
        Err(anyhow!(
            "primary monitor capture only implemented for Windows"
        ))
    }
}

/// Capture the active foreground window area as JPEG.
///
/// `exclude` — screen rects to blank (neutral grey) before encoding. Pass the
/// panel's current rect so it does not appear in the AI's screenshot.
/// `GA_ROOTOWNER` is applied to dialogs so the stored HWND is always the
/// stable main-window handle, not an owned dialog that may close at any time.
///
/// The capture rect is the **union of all visible same-PID top-level windows
/// on the target's monitor**, not just the foreground window's frame. This
/// catches modal dialogs and popups that float outside the main window (e.g.
/// WeChat's Storage dialog, Word's Find & Replace) — otherwise those would be
/// silently cropped out and the AI would hallucinate coordinates.
///
/// Returns (jpeg bytes, capture rect in physical pixels, raw HWND as usize).
/// Store the HWND and pass it to `recapture_window_jpeg` on subsequent calls.
pub fn capture_active_window_jpeg(quality: u8, exclude: &[Rect]) -> Result<(Vec<u8>, Rect, usize)> {
    #[cfg(windows)]
    {
        let (hwnd, frame_rect) =
            win::get_foreground_target().ok_or_else(|| anyhow!("no foreground window found"))?;
        let rect = win::pid_union_rect(hwnd).unwrap_or(frame_rect);
        let mut img = win::capture_desktop_region(&rect)?;
        // Grey gaps between the target's windows (other apps / desktop showing
        // through the union bbox) so the AI only sees the target program.
        win::blank_outside_rects(&mut img, &rect, &win::pid_visible_keep_rects(hwnd, &rect));
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        Ok((buf, rect, hwnd.0 as usize))
    }

    #[cfg(not(windows))]
    {
        let _ = (quality, exclude);
        Err(anyhow!(
            "active-window capture only implemented for Windows"
        ))
    }
}

/// Captures the entire multi-monitor virtual desktop.
/// Returns (jpeg bytes, virtual desktop rect).
pub fn capture_virtual_desktop_jpeg(quality: u8, exclude: &[Rect]) -> Result<(Vec<u8>, Rect)> {
    #[cfg(windows)]
    {
        let rect = win::get_virtual_desktop_rect();
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        Ok((buf, rect))
    }

    #[cfg(not(windows))]
    {
        let _ = (quality, exclude);
        Err(anyhow!(
            "virtual desktop capture only implemented for Windows"
        ))
    }
}

/// Capture one explicit desktop region (e.g. a single chosen monitor) and encode as
/// JPEG. Same pipeline as `capture_virtual_desktop_jpeg` but for a caller-supplied
/// rect instead of the whole virtual desktop. Returns (jpeg bytes, the rect).
pub fn capture_region_jpeg(rect: Rect, quality: u8, exclude: &[Rect]) -> Result<(Vec<u8>, Rect)> {
    #[cfg(windows)]
    {
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        Ok((buf, rect))
    }

    #[cfg(not(windows))]
    {
        let _ = (rect, quality, exclude);
        Err(anyhow!("region capture only implemented for Windows"))
    }
}

/// Capture one explicit desktop region as a raw RGBA ImageBuffer (no JPEG, no downscale).
/// The OCR-path counterpart of `capture_region_jpeg` — used so that in full-screen mode the
/// locator's OCR sees the *same* region the AI did (the chosen monitor / whole desktop) at
/// native resolution, instead of the foreground window. Blanks `exclude` (our own panel).
pub fn capture_region_raw(rect: Rect, exclude: &[Rect]) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    #[cfg(windows)]
    {
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_rects(&mut img, &rect, exclude);
        Ok(img)
    }

    #[cfg(not(windows))]
    {
        let _ = (rect, exclude);
        Err(anyhow!("region capture only implemented for Windows"))
    }
}

/// Capture the active foreground window as a raw RGBA ImageBuffer (no JPEG encode,
/// no downscale cap). Used by the OCR path so it sees native-resolution pixels.
///
/// Returns (raw_image, window rect in physical pixels, raw HWND as usize).
#[allow(clippy::type_complexity)]
pub fn capture_active_window_raw(
    exclude: &[Rect],
) -> Result<(ImageBuffer<Rgba<u8>, Vec<u8>>, Rect, usize)> {
    #[cfg(windows)]
    {
        let (hwnd, frame_rect) =
            win::get_foreground_target().ok_or_else(|| anyhow!("no foreground window found"))?;
        let rect = win::pid_union_rect(hwnd).unwrap_or(frame_rect);
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_outside_rects(&mut img, &rect, &win::pid_visible_keep_rects(hwnd, &rect));
        win::blank_rects(&mut img, &rect, exclude);
        Ok((img, rect, hwnd.0 as usize))
    }

    #[cfg(not(windows))]
    {
        let _ = exclude;
        Err(anyhow!("raw capture only implemented for Windows"))
    }
}

/// Encode a raw RGBA image as lossless PNG for OCR consumption.
///
/// RGB channels are preserved exactly; alpha is dropped (OCR doesn't need it).
/// Uses default PNG compression (level 6) — correct and reasonably fast for
/// the sizes we handle (typically ≤ 1920×1080 before any upscale).
pub fn encode_png_for_ocr(img: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Result<Vec<u8>> {
    use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
    let (w, h) = (img.width(), img.height());
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in img.pixels() {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&rgb, w, h, ColorType::Rgb8.into())
        .context("png encode for ocr")?;
    Ok(out)
}

/// Re-capture a previously discovered window by its stored raw HWND, returning
/// raw RGBA pixels (no JPEG, no downscale). Used by the OCR locator path so it
/// always sees the same window the AI was shown — even if the user switched
/// focus between the AI call and the locate.
#[allow(clippy::type_complexity)]
pub fn recapture_window_raw(
    hwnd_raw: usize,
    exclude: &[Rect],
) -> Result<(ImageBuffer<Rgba<u8>, Vec<u8>>, Rect)> {
    #[cfg(windows)]
    {
        let frame_rect = win::validate_hwnd_raw(hwnd_raw)
            .ok_or_else(|| anyhow!("stored window is no longer valid (closed or minimised)"))?;
        let rect = win::pid_union_rect_raw(hwnd_raw).unwrap_or(frame_rect);
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_outside_rects(
            &mut img,
            &rect,
            &win::pid_visible_keep_rects_raw(hwnd_raw, &rect),
        );
        win::blank_rects(&mut img, &rect, exclude);
        Ok((img, rect))
    }

    #[cfg(not(windows))]
    {
        let _ = (hwnd_raw, exclude);
        Err(anyhow!("recapture_window_raw only implemented for Windows"))
    }
}

/// Re-capture a previously discovered window by its stored raw HWND.
/// Validates the window is still alive and not minimised before capturing.
/// Returns an error if the window is gone — caller should then call
/// `capture_active_window_jpeg` to rediscover.
///
/// `exclude` — same semantics as `capture_active_window_jpeg`.
pub fn recapture_window_jpeg(
    hwnd_raw: usize,
    quality: u8,
    exclude: &[Rect],
) -> Result<(Vec<u8>, Rect)> {
    #[cfg(windows)]
    {
        let frame_rect = win::validate_hwnd_raw(hwnd_raw)
            .ok_or_else(|| anyhow!("stored window is no longer valid (closed or minimised)"))?;
        let rect = win::pid_union_rect_raw(hwnd_raw).unwrap_or(frame_rect);
        let mut img = win::capture_desktop_region(&rect)?;
        win::blank_outside_rects(
            &mut img,
            &rect,
            &win::pid_visible_keep_rects_raw(hwnd_raw, &rect),
        );
        win::blank_rects(&mut img, &rect, exclude);
        let buf = encode_jpeg(&cap_size(img), quality)?;
        Ok((buf, rect))
    }

    #[cfg(not(windows))]
    {
        let _ = (hwnd_raw, quality, exclude);
        Err(anyhow!("recapture only implemented for Windows"))
    }
}

/// True when the target window is visible anywhere within the located rect — i.e. the
/// pointer's target area shows through at least partly, so it's safe to draw. Only
/// when the whole target spot is hidden behind another app is this false (suppress the
/// pointer). Always true off-Windows (don't suppress).
pub fn target_visible_in_rect(x: i32, y: i32, w: i32, h: i32, target_hwnd: usize) -> bool {
    #[cfg(windows)]
    {
        win::target_visible_in_rect(x, y, w, h, target_hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y, w, h, target_hwnd);
        true
    }
}

/// Get debug information about a specific window by its raw HWND.
pub fn get_window_info(hwnd_raw: usize) -> String {
    #[cfg(windows)]
    {
        win::get_window_info(hwnd_raw)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_raw;
        String::from("Window info only available on Windows")
    }
}

/// Raw window title for `hwnd_raw` (untruncated) — used to match Nav-Pack
/// `window_title_pattern`s. Empty string on failure / non-Windows.
pub fn get_window_title(hwnd_raw: usize) -> String {
    #[cfg(windows)]
    {
        win::get_window_title(hwnd_raw)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_raw;
        String::new()
    }
}

/// Phase 0.2: structured info about the active capture target (used for the
/// "Shared: <App>" indicator). Returns `None` if no plausible target.
#[cfg(windows)]
pub fn get_active_window_info() -> Option<win::ActiveWindowInfo> {
    win::get_active_window_info()
}

#[cfg(not(windows))]
pub fn get_active_window_info() -> Option<()> {
    None
}

/// Phase 0.2: structured info about a specific HWND (cheaper than walking
/// foreground when the capture path already knows the HWND).
#[cfg(windows)]
pub fn get_window_info_for_hwnd(hwnd_raw: usize) -> Option<win::ActiveWindowInfo> {
    win::get_window_info_for_hwnd(hwnd_raw)
}

#[cfg(not(windows))]
pub fn get_window_info_for_hwnd(_hwnd_raw: usize) -> Option<()> {
    None
}

/// Item 1: re-export TargetWindowInfo so lib.rs can use capture::TargetWindowInfo.
#[cfg(windows)]
pub use win::TargetWindowInfo;

/// Item 1: enumerate all candidate windows for the target-picker dropdown.
#[cfg(windows)]
pub fn list_target_windows() -> Vec<win::TargetWindowInfo> {
    win::list_target_windows()
}

/// Predict the dimensions of the AI-image after `cap_size()` would be applied
/// to a source of (`src_w`, `src_h`). Mirrors `cap_size` exactly so the AI-bbox
/// converter knows the pixel space the model actually sees.
pub fn ai_image_dims(src_w: u32, src_h: u32) -> (u32, u32) {
    if src_w <= MAX_CAP_W && src_h <= MAX_CAP_H {
        return (src_w, src_h);
    }
    let scale = (MAX_CAP_W as f32 / src_w as f32).min(MAX_CAP_H as f32 / src_h as f32);
    let nw = ((src_w as f32 * scale).round() as u32).max(1);
    let nh = ((src_h as f32 * scale).round() as u32).max(1);
    (nw, nh)
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
    encoder
        .encode(&rgb, w, h, ColorType::Rgb8.into())
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

/// Re-assert the overlay window's TOPMOST z-order so the guidance pointer stays
/// above transient popups (dropdown menus, combo lists, tooltips) created by
/// other apps. No-op on non-Windows. See `win::raise_overlay_topmost`.
pub fn raise_overlay_topmost() {
    #[cfg(windows)]
    {
        win::raise_overlay_topmost();
    }
}

/// Convenience: base64-encode JPEG bytes (suitable for AI API payloads).
pub fn to_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
