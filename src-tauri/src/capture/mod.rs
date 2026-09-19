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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Physical DPI scale (1.0 = 100 %, 2.0 = 200 %) of the monitor the `rect`'s centre sits on —
/// The Windows version for the prompt, e.g. `Windows 11 (build 26200)`. See
/// `win::os_version` for why it is read the way it is. Non-Windows → None.
pub fn os_version() -> Option<String> {
    #[cfg(windows)]
    {
        win::os_version()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// the template-matching DPI prior (see `win::monitor_scale_for_rect`). Non-Windows → 1.0.
pub fn monitor_scale_for_rect(rect: &Rect) -> f32 {
    #[cfg(windows)]
    {
        win::monitor_scale_for_rect(rect)
    }
    #[cfg(not(windows))]
    {
        let _ = rect;
        1.0
    }
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

/// Ceiling for session-export frames. Deliberately NOT `MAX_CAP_W`/`MAX_CAP_H`.
///
/// Those exist to cap the image *tokens* an AI request pays for; an export pays no
/// tokens. Worse, reusing them made export frames strictly softer than what the
/// model itself saw: the AI's image is a cropped WINDOW squeezed into 1536×768,
/// while an export frame is a whole MONITOR squeezed into the same box, so detail
/// per on-screen element came out lower. Live 2026-09-04: a 1920×1080 monitor
/// exported at 1365×768 — half the pixels — and the founder read the result as
/// low-resolution, correctly.
///
/// 2560×1440 leaves 1080p and 1440p untouched and still halves a 4K frame, which
/// keeps the worst-case ring buffer sane on a high-DPI machine.
const EXPORT_MAX_W: u32 = 2560;
const EXPORT_MAX_H: u32 = 1440;

/// JPEG quality for export frames. Higher than the AI path's 75 for the same
/// reason: these are read by people, at full width, and are click-to-enlarge
/// targets on a published page.
const EXPORT_QUALITY: u8 = 88;

/// Capture a region for session export: native resolution up to a generous cap,
/// and nothing blanked. The caller passes the monitor rect, so the Navisual panel
/// stays in shot (`session-export-design.md` §0.4).
pub fn capture_region_for_export(rect: Rect) -> Result<(Vec<u8>, Rect)> {
    let (img, rect) = capture_region_for_export_raw(rect)?;
    Ok((encode_export_frame(img)?, rect))
}

/// Screen pixels plus the region they came from, before any encoding.
pub type RawExportFrame = (ImageBuffer<Rgba<u8>, Vec<u8>>, Rect);

/// The half of the export frame that MUST happen at the clean moment: read the
/// pixels while the overlay is down and the streamed caption does not exist yet.
///
/// Split out from the encode because the two have opposite constraints. Measured on
/// a 1920x1080 primary monitor: BitBlt ~138 ms, JPEG encode ~364 ms. Only the first
/// is time-critical, so the second is done while the AI request is already in flight
/// (see guide()) instead of making every request wait for both.
pub fn capture_region_for_export_raw(rect: Rect) -> Result<RawExportFrame> {
    #[cfg(windows)]
    {
        let img = win::capture_desktop_region(&rect)?;
        Ok((img, rect))
    }

    #[cfg(not(windows))]
    {
        let _ = rect;
        Err(anyhow!("export capture only implemented for Windows"))
    }
}

/// The half that can wait: downscale-if-needed and JPEG-encode. Safe to run long
/// after the pixels were read, and deliberately does so.
pub fn encode_export_frame(img: ImageBuffer<Rgba<u8>, Vec<u8>>) -> Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    // A cap, not a resize: 1080p and 1440p are untouched, 4K is halved.
    let img = if w <= EXPORT_MAX_W && h <= EXPORT_MAX_H {
        img
    } else {
        let scale = (EXPORT_MAX_W as f32 / w as f32).min(EXPORT_MAX_H as f32 / h as f32);
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
    };
    encode_jpeg(&img, EXPORT_QUALITY)
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
/// The virtual-desktop rect (all monitors' union), for callers that need the region
/// before deciding how to capture it.
pub fn virtual_desktop_rect() -> Rect {
    #[cfg(windows)]
    {
        win::get_virtual_desktop_rect()
    }
    #[cfg(not(windows))]
    {
        Rect { x: 0, y: 0, width: 0, height: 0 }
    }
}

/// Encode an already-captured frame as the AI-bound JPEG: token-cap downscale, then
/// encode. Split out so one raw capture can serve both the AI image and the OCR PNG
/// instead of the screen being read twice.
pub fn encode_capture_jpeg(img: ImageBuffer<Rgba<u8>, Vec<u8>>, quality: u8) -> Result<Vec<u8>> {
    encode_jpeg(&cap_size(img), quality)
}

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

/// Focus give-back: hand OS focus to the target window when (and only when) our
/// own panel currently holds the foreground — the activation-eat fix (see
/// `win::focus_window_if_own_foreground`). No-op off-Windows.
pub fn focus_window_if_own_foreground(hwnd_raw: usize) -> bool {
    #[cfg(windows)]
    {
        win::focus_window_if_own_foreground(hwnd_raw)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_raw;
        false
    }
}

/// True when the target window is fully hidden behind other apps — capturing it
/// would leak the occluding app's pixels, so the caller should refuse. Never
/// occluded off-Windows.
pub fn window_fully_occluded(hwnd_raw: usize) -> bool {
    #[cfg(windows)]
    {
        win::window_fully_occluded(hwnd_raw)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_raw;
        false
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

/// Diagnostic/nav-pack-authoring helper: find a specific app window by title substring.
#[cfg(windows)]
#[allow(unused_imports)] // used by the nav-pack-authoring `#[ignore]` test harnesses
pub use win::find_window_by_title;
#[cfg(windows)]
pub use win::exe_stem_for_hwnd;
#[cfg(windows)]
pub use win::pid_for_hwnd;
#[cfg(windows)]
pub use win::restore_window;

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

/// The inverse, for reading a picture back out of an artifact (§6). `None` on anything that
/// is not valid base64, so a hand-edited file yields no picture rather than a broken one.
pub fn from_base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

// ─── Side-by-side docking ───────────────────────────────────────────────────
//
// See `win::work_area_containing` for why Navisual tiles the panel and its
// docked partner itself instead of leaning on the OS snap-group divider.

/// The work area (monitor minus taskbar) of the monitor containing `(x, y)`.
pub fn work_area_containing(x: i32, y: i32) -> Option<Rect> {
    #[cfg(windows)]
    {
        win::work_area_containing(x, y)
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        None
    }
}

/// Raw handle of our own panel window (never the click-through overlay).
pub fn own_panel_hwnd() -> Option<usize> {
    #[cfg(windows)]
    {
        win::own_panel_hwnd()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A window's visible frame (DWM extended bounds, not `GetWindowRect`).
pub fn window_frame(hwnd_raw: usize) -> Option<Rect> {
    #[cfg(windows)]
    {
        win::window_frame(hwnd_raw)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_raw;
        None
    }
}

/// Move/resize a window so its visible frame lands exactly on `target`.
pub fn set_window_frame(hwnd_raw: usize, target: Rect) -> bool {
    #[cfg(windows)]
    {
        win::set_window_frame(hwnd_raw, target)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd_raw, target);
        false
    }
}

/// Make a minimize request on the panel collapse it to the floating icon instead.
/// Must be called on the window's own (main) thread.
pub fn intercept_panel_minimize() -> bool {
    #[cfg(windows)]
    {
        win::intercept_panel_minimize()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Turn Windows 11's accent border on (expanded) or off (collapsed) for the panel.
pub fn set_panel_border(hwnd_raw: usize, enabled: bool) {
    #[cfg(windows)]
    {
        win::set_panel_border(hwnd_raw, enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd_raw, enabled);
    }
}

#[cfg(all(test, windows))]
mod export_cost_tests {
    /// Measures what the session-export frame actually costs on THIS machine's
    /// primary monitor, since it now runs on every request at AI-capture time.
    /// Ignored: it reads the live screen, so it is a measurement harness rather
    /// than an assertion.
    ///
    /// RUN IT IN RELEASE, ALWAYS:
    ///   cargo test --release --lib export_frame_cost -- --ignored --nocapture
    ///
    /// `image`'s resize and encode paths are 3-15x slower unoptimised, so a debug
    /// run reports numbers that are real for `tauri dev` but wildly wrong for what
    /// ships. Measured both ways on one 1920x1080 monitor: export frame 602 ms debug
    /// vs 124 ms release, AI JPEG 2140 ms vs 142 ms. A debug figure was quoted as a
    /// per-request user cost once already; don't repeat it.
    /// Live probe for `restore_window`: does picking a minimized app actually
    /// bring it back? Names the window by title substring, so it can be pointed at
    /// something harmless.
    ///
    /// `NAVISUAL_TEST_TITLE=Calculator cargo test --lib -- --ignored restore_window_live --nocapture`
    ///
    /// Puts the window back the way it found it, so running this does not leave the
    /// desktop rearranged.
    #[test]
    #[ignore]
    fn restore_window_live() {
        let needle = std::env::var("NAVISUAL_TEST_TITLE").unwrap_or_default();
        if needle.is_empty() {
            println!("set NAVISUAL_TEST_TITLE to a minimized window's title substring");
            return;
        }
        let needle_low = needle.to_lowercase();
        let Some(w) = super::list_target_windows()
            .into_iter()
            .find(|w| w.title.to_lowercase().contains(&needle_low))
        else {
            println!("no window matching {needle:?}");
            return;
        };
        println!("found {:?} minimized={}", w.title, w.minimized);
        assert!(w.minimized, "minimize it first — this probe tests the restore path");

        let ok = super::restore_window(w.hwnd);
        let still_iconic = super::list_target_windows()
            .into_iter()
            .find(|x| x.hwnd == w.hwnd)
            .map(|x| x.minimized);
        println!("restore_window -> {ok}; still minimized: {still_iconic:?}");
        assert!(ok, "restore_window reported failure");
        assert_eq!(still_iconic, Some(false), "window did not actually come back");

        // Leave the desktop as it was found.
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE};
            let _ = ShowWindow(HWND(w.hwnd as *mut std::ffi::c_void), SW_MINIMIZE);
        }
        println!("re-minimized; desktop restored");
    }

    /// Live probe for the capture mask: would this window come back as a grey
    /// rectangle, and if so, what buried it?
    ///
    /// `NAVISUAL_TEST_TITLE="System Properties" cargo test --lib -- --ignored keep_rects_live --nocapture`
    ///
    /// Born from a live report on 2026-09-14: System Properties captured as a flat
    /// grey box, so the model saw a blank picture, concluded something was covering
    /// the window, and spent four requests asking the user to move the panel -- while
    /// the dialog sat in plain view. The mask greys the whole frame when the keep-set
    /// is empty (deliberate: it refuses to leak the occluder's pixels), but nothing
    /// on that path logged anything, so the reason was unknowable after the fact.
    ///
    /// Read-only: enumerates and measures, touches no window.
    #[test]
    #[ignore]
    fn keep_rects_live() {
        let needle = std::env::var("NAVISUAL_TEST_TITLE").unwrap_or_default();
        if needle.is_empty() {
            println!("set NAVISUAL_TEST_TITLE to a substring of the window's title");
            return;
        }
        let needle_low = needle.to_lowercase();
        let Some(w) = super::list_target_windows()
            .into_iter()
            .find(|w| w.title.to_lowercase().contains(&needle_low))
        else {
            println!("no window matching {needle:?}");
            return;
        };

        println!("target      : {:?}", w.title);
        println!("  app       : {} (hwnd {})", w.display_name, w.hwnd);
        println!("  minimized : {}", w.minimized);

        // pid_union_rect is what capture_active_window_jpeg uses as the capture
        // rect, so the probe measures the same frame the AI would have received.
        let Some(rect) = super::win::pid_union_rect_raw(w.hwnd) else {
            println!("  NO UNION RECT -- the handle is dead or has no geometry");
            return;
        };
        println!(
            "  rect      : {}x{} @ {},{}",
            rect.width, rect.height, rect.x, rect.y
        );

        // The mask skips the CALLING process's own windows. Run out-of-process and
        // the app's full-desktop overlay is no longer skipped, so the probe would
        // invent an occlusion that does not exist in the app. Pass the running
        // app's pid via NAVISUAL_TEST_PID to reproduce what the app actually sees.
        let our_pid: u32 = std::env::var("NAVISUAL_TEST_PID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(std::process::id);
        if our_pid != std::process::id() {
            println!("  (treating pid {our_pid} as \"ours\", i.e. the app's own windows)");
        }

        // NAVISUAL_TEST_IGNORE_PIDS="123,456" pretends those processes are gone,
        // which isolates whether one overlay is the whole story.
        let ignore: Vec<u32> = std::env::var("NAVISUAL_TEST_IGNORE_PIDS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if !ignore.is_empty() {
            println!("  (pretending these pids do not exist: {ignore:?})");
        }
        let (keep, occluders) =
            super::win::keep_rects_diagnostic(w.hwnd, &rect, our_pid, &ignore);
        let total: i64 = keep.iter().map(|r| r.width as i64 * r.height as i64).sum();
        let area = (rect.width as i64 * rect.height as i64).max(1);

        println!("\nkeep-set    : {} rect(s), {}% of the frame", keep.len(), total * 100 / area);
        for r in &keep {
            println!("  keep      : {}x{} @ {},{}", r.width, r.height, r.x, r.y);
        }

        println!("\ncounted as covering it (z-order, topmost first):");
        if occluders.is_empty() {
            println!("  (nothing)");
        }
        for o in &occluders {
            let (t, p, r, ex, alpha) =
                (&o.title, o.pid, o.rect, o.ex_style, o.uniform_alpha);
            // 0x20 = WS_EX_TRANSPARENT (click-through), 0x80000 = WS_EX_LAYERED,
            // 0x8 = WS_EX_TOPMOST. A click-through layered window is invisible to
            // the user by construction -- it cannot be "covering" anything.
            let mut flags = Vec::new();
            if ex & 0x20 != 0 {
                flags.push("CLICK-THROUGH");
            }
            if ex & 0x80000 != 0 {
                flags.push("LAYERED");
            }
            if ex & 0x8 != 0 {
                flags.push("TOPMOST");
            }
            let transparency = match alpha {
                Some(a) => format!("uniform alpha {a}/255 (LWA flags {})", o.layer_flags),
                None if ex & 0x80000 != 0 && o.layer_flags != 0 => {
                    format!("COLOUR-KEY only, visible (LWA flags {})", o.layer_flags)
                }
                None if ex & 0x80000 != 0 => "PER-PIXEL alpha (no single value)".to_string(),
                None => "opaque".to_string(),
            };
            println!(
                "  {}'{}' pid={} {}x{} @ {},{} ex=0x{:X} {} | {}",
                if o.skipped_by_filter { "[filtered] " } else { "[COVERS]   " },
                if t.is_empty() { "<untitled>" } else { t.as_str() },
                p,
                r.width,
                r.height,
                r.x,
                r.y,
                ex,
                flags.join(" "),
                transparency
            );
        }

        // window_fully_occluded is the exact condition the refuse-before-capture
        // guard in guide() branches on, so print it beside the keep-set.
        println!("
window_fully_occluded() -> {}", super::window_fully_occluded(w.hwnd));

        // Now exercise the REAL entry point with a logger attached, so the line
        // that ships is the line that gets read, not one inferred from the code.
        {
            use std::sync::{Mutex, OnceLock};
            static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
            struct Cap;
            impl log::Log for Cap {
                fn enabled(&self, _: &log::Metadata) -> bool {
                    true
                }
                fn log(&self, r: &log::Record) {
                    if let Some(m) = LINES.get() {
                        m.lock().unwrap().push(format!("{} {}", r.level(), r.args()));
                    }
                }
                fn flush(&self) {}
            }
            LINES.get_or_init(|| Mutex::new(Vec::new()));
            let _ = log::set_boxed_logger(Box::new(Cap));
            log::set_max_level(log::LevelFilter::Trace);

            LINES.get().unwrap().lock().unwrap().clear();
            let _ = super::win::pid_visible_keep_rects_raw(w.hwnd, &rect);
            println!("\nwhat the shipped log line actually says:");
            for l in LINES.get().unwrap().lock().unwrap().iter() {
                println!("  {l}");
            }
        }

        if keep.is_empty() {
            println!("\n>>> THIS WINDOW WOULD CAPTURE AS A FLAT GREY BOX <<<");
        } else if total * 100 / area < 90 {
            println!("\n>>> partially greyed: {}% kept <<<", total * 100 / area);
        } else {
            println!("\ncaptures cleanly");
        }
    }

    /// Live probe: what the target picker would show right now, and whether a
    /// minimized window survives the filter with its flag set.
    ///
    /// `cargo test --lib -- --ignored target_picker_live --nocapture`
    ///
    /// Minimize a real app first. It should appear with `MIN`, and any app that is
    /// on screen should appear without it. Two things this is here to catch, both
    /// of which would silently undo the feature: a minimized window failing the
    /// 100px size gate (Windows parks one at 160x28), and a minimized twin winning
    /// the (exe, title) dedupe over a window the user can actually see.
    #[test]
    #[ignore]
    fn target_picker_live() {
        let list = super::list_target_windows();
        println!("{} window(s) the picker would offer:", list.len());
        let mut minimized = 0;
        for w in &list {
            if w.minimized {
                minimized += 1;
            }
            println!(
                "  {:<3} {:<22} {}",
                if w.minimized { "MIN" } else { "" },
                w.display_name,
                if w.title.len() > 52 { &w.title[..52] } else { &w.title }
            );
        }
        println!("{minimized} of {} are minimized", list.len());
        // A duplicate (exe, title) pair means the dedupe let a twin through.
        let mut keys: Vec<(String, String)> = list
            .iter()
            .map(|w| (w.exe_stem.to_lowercase(), w.title.to_lowercase()))
            .collect();
        let before = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate (exe, title) in the picker list");
    }

    #[test]
    #[ignore]
    fn export_frame_cost() {
        let Some(mon) = super::enumerate_monitor_rects().into_iter().next() else {
            println!("no monitors");
            return;
        };
        println!("monitor {}x{}", mon.width, mon.height);
        // One warm-up: the first BitBlt pays for DC setup that later ones do not.
        let _ = super::capture_region_for_export(mon);
        let mut total = 0u128;
        let runs = 5;
        let mut bytes = 0usize;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            match super::capture_region_for_export(mon) {
                Ok((buf, _)) => {
                    total += t.elapsed().as_micros();
                    bytes = buf.len();
                }
                Err(e) => {
                    println!("capture failed: {e}");
                    return;
                }
            }
        }
        println!(
            "export frame: {:.1} ms avg over {runs} runs, {} KB per frame",
            (total as f64 / runs as f64) / 1000.0,
            bytes / 1024
        );
        // Split it: if the JPEG encode dominates, the pixels can be grabbed at the
        // clean moment (which is what correctness needs) and encoded off the
        // critical path, instead of making every request wait for both.
        let mut blit = 0u128;
        let mut enc = 0u128;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            let img = super::win::capture_desktop_region(&mon).expect("blit");
            blit += t.elapsed().as_micros();
            let t2 = std::time::Instant::now();
            let _ = super::encode_jpeg(&img, super::EXPORT_QUALITY).expect("encode");
            enc += t2.elapsed().as_micros();
        }
        // What the AI-capture closure pays synchronously, per request: one BitBlt plus
        // BOTH encodings. Only the export frame's encode is deferred today.
        let mut png = 0u128;
        let mut jpg = 0u128;
        for _ in 0..runs {
            let img = super::win::capture_desktop_region(&mon).expect("blit");
            let t = std::time::Instant::now();
            let _ = super::encode_png_for_ocr(&img).expect("png");
            png += t.elapsed().as_micros();
            let t2 = std::time::Instant::now();
            let _ = super::encode_capture_jpeg(img, 75).expect("jpeg");
            jpg += t2.elapsed().as_micros();
        }
        println!(
            "  critical path per request: OCR PNG {:.1} ms | AI JPEG {:.1} ms",
            (png as f64 / runs as f64) / 1000.0,
            (jpg as f64 / runs as f64) / 1000.0
        );
        println!(
            "  split: BitBlt {:.1} ms | JPEG encode {:.1} ms",
            (blit as f64 / runs as f64) / 1000.0,
            (enc as f64 / runs as f64) / 1000.0
        );
    }
}
