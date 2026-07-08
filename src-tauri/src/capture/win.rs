//! Windows-specific helpers for capture: pick the best target window (skipping
//! AI Navigator's own panel) and return its DWM extended frame bounds.
//!
//! When the panel is foreground (typical whenever the user clicks a button in
//! our UI), we walk the z-order and return the highest visible, non-self,
//! non-shell window instead. This keeps OCR working on whatever the user is
//! actually looking at — e.g. Task Manager — rather than capturing our own
//! panel contents.

use super::Rect;
use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use std::mem;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDIBits, GetMonitorInfoW, MonitorFromPoint, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowLongW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetWindowPos, WindowFromPoint, GA_ROOT, GA_ROOTOWNER, GWL_EXSTYLE, HWND_TOPMOST,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

/// Class names we never treat as a capture target (shell, IME, overlays).
const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "CEF-OSC-WIDGET",
    "IME",
    "MSCTFIME UI",
    "Default IME",
];

/// Validate that `hwnd` is still a usable capture target and return its current
/// DWM frame bounds. Returns None if the window was closed, minimised, or
/// otherwise unusable.
pub fn validate_hwnd(hwnd: HWND) -> Option<Rect> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
    }
    frame_rect_of(hwnd)
}

/// Convenience wrapper: validate a window by raw HWND value (usize).
pub fn validate_hwnd_raw(hwnd_raw: usize) -> Option<Rect> {
    validate_hwnd(HWND(hwnd_raw as *mut _))
}

pub fn get_window_info(hwnd_raw: usize) -> String {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let mut title = [0u16; 256];
        let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title);
        let title_str = String::from_utf16_lossy(&title[..len as usize]);

        let mut rect = windows::Win32::Foundation::RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect);

        // Title (app + document) + size are the only useful context for the AI.
        // Class (Win32-internal name) and PID (random per-launch integer) were
        // dropped — noise to the model, no guidance value. The locator uses the
        // HWND directly, not this string.
        format!(
            "Title: '{}'\nRect: [{}, {}, {}, {}]",
            title_str, rect.left, rect.top, rect.right, rect.bottom
        )
    }
}

/// Raw, untruncated window title for `hwnd_raw` — used to match Nav-Pack title patterns.
pub fn get_window_title(hwnd_raw: usize) -> String {
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let mut buf = [0u16; 512];
        let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf);
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// True when the target window (`target_hwnd`) is **visible anywhere** within the
/// located rect `(x, y, w, h)` (physical pixels) — i.e. the pointer's target area
/// overlaps the part of the target window that isn't hidden behind another app.
///
/// Pure geometry, no sampling: take the located rect ∩ the target window's frame, then
/// subtract every higher-z window that belongs to a DIFFERENT app; if any area is left,
/// the target shows through → draw. If nothing is left, the spot is fully covered →
/// suppress. The target app's own windows and our click-through overlay (the pointer
/// itself) aren't occluders, but our **opaque panel** is — covering the target with the
/// Navisual panel hides the pointer. Returns true (don't suppress) for degenerate inputs.
pub fn target_visible_in_rect(x: i32, y: i32, w: i32, h: i32, target_hwnd: usize) -> bool {
    if target_hwnd == 0 || w <= 0 || h <= 0 {
        return true;
    }
    let target = HWND(target_hwnd as *mut core::ffi::c_void);
    let mut target_pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(target, Some(&mut target_pid));
    }
    if target_pid == 0 {
        return true;
    }
    let bbox = Rect {
        x,
        y,
        width: w as u32,
        height: h as u32,
    };
    // The part of the located rect that actually lies on the target window.
    let Some(tframe) = frame_rect_of(target) else {
        return true;
    };
    let target_area = rect_intersect(&tframe, &bbox);
    if target_area.width == 0 || target_area.height == 0 {
        return true; // located rect isn't on the target window — nothing to check
    }

    // Collect the rects of windows ABOVE the target in z-order that belong to a
    // DIFFERENT app. EnumWindows enumerates top→bottom, so we stop counting once we
    // reach the target; our own windows and the target app's own windows don't occlude.
    struct State {
        target: HWND,
        our_pid: u32,
        target_pid: u32,
        area: Rect,
        reached_target: bool,
        occluders: Vec<Rect>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let st = &mut *(lparam.0 as *mut State);
        if st.reached_target {
            return TRUE;
        }
        if hwnd.0 == st.target.0 {
            st.reached_target = true;
            return TRUE;
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == st.target_pid {
            return TRUE; // nothing, or the target app itself — not an occluder
        }
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return TRUE;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        // Our own click-through overlay (WS_EX_TRANSPARENT) IS the pointer — never an
        // occluder. But our opaque panel DOES occlude: if it covers the target the user
        // can't see or reach it, so the pointer should hide. Other apps' tool windows
        // (tooltips, helpers) don't occlude; our own panel can be a tool window, so the
        // tool-window skip is scoped to other processes.
        if (ex & WS_EX_TRANSPARENT.0) != 0 {
            return TRUE;
        }
        if pid != st.our_pid && (ex & WS_EX_TOOLWINDOW.0) != 0 {
            return TRUE;
        }
        if let Some(r) = frame_rect_of(hwnd) {
            let clipped = rect_intersect(&r, &st.area);
            if clipped.width > 0 && clipped.height > 0 {
                st.occluders.push(clipped);
            }
        }
        TRUE
    }

    let mut st = State {
        target,
        our_pid: std::process::id(),
        target_pid,
        area: target_area,
        reached_target: false,
        occluders: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut st as *mut State as isize));
    }
    // Anything left of the target's area after subtracting other apps above it means
    // the target shows through somewhere in the located rect.
    !rect_subtract_many(target_area, &st.occluders).is_empty()
}

/// Phase 0.2: lightweight info about the window currently being captured.
/// Used for the "Shared: <App>" header chip and the animated boundary overlay.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActiveWindowInfo {
    pub hwnd: usize,
    pub rect: Rect,
    pub app_name: String,
    pub exe_name: String,
}

/// A candidate window the user can pin as the guidance target.
#[derive(serde::Serialize, Clone)]
pub struct TargetWindowInfo {
    pub hwnd: usize,
    pub title: String,
    pub exe_stem: String,
    pub display_name: String,
}

/// Map well-known exe stems to friendly display names.
fn friendly_exe_name(stem: &str) -> String {
    match stem.to_lowercase().as_str() {
        "olk" | "outlook" => "Outlook",
        "code" => "VS Code",
        "code - insiders" => "VS Code Insiders",
        "winword" => "Word",
        "excel" => "Excel",
        "powerpnt" => "PowerPoint",
        "onenote" => "OneNote",
        "msedge" => "Edge",
        "chrome" => "Chrome",
        "firefox" => "Firefox",
        "slack" => "Slack",
        "teams" => "Teams",
        "windowsterminal" | "wt" => "Terminal",
        "wechat" => "WeChat",
        "notion" => "Notion",
        "obsidian" => "Obsidian",
        "discord" => "Discord",
        "zoom" => "Zoom",
        "notepad" => "Notepad",
        "explorer" => "Explorer",
        other => return other.to_string(),
    }
    .to_string()
}

/// Read the process exe path for `pid`. Returns the full path or empty string.
fn exe_path_of_pid(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if ok {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

/// Resolve a friendly app display name. Priority:
/// 1. Window title (truncated to 60 chars)
/// 2. Exe file basename minus `.exe`
fn resolve_app_name(hwnd: HWND, exe_path: &str) -> String {
    unsafe {
        let mut title = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut title) as usize;
        if len > 0 {
            let s = String::from_utf16_lossy(&title[..len]);
            let s = s.trim();
            if !s.is_empty() {
                return if s.chars().count() > 60 {
                    s.chars().take(57).chain("...".chars()).collect()
                } else {
                    s.to_string()
                };
            }
        }
    }

    if !exe_path.is_empty() {
        let base = std::path::Path::new(exe_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown");
        return base.to_string();
    }

    "Unknown app".to_string()
}

/// Phase 0.2: gather info about the active capture target without actually
/// capturing pixels. Mirrors `get_foreground_target` HWND-selection logic.
pub fn get_active_window_info() -> Option<ActiveWindowInfo> {
    let (hwnd, rect) = get_foreground_target()?;
    let mut pid: u32 = 0;
    unsafe {
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    }
    let exe_path = exe_path_of_pid(pid);
    let exe_name = std::path::Path::new(&exe_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let app_name = resolve_app_name(hwnd, &exe_path);
    Some(ActiveWindowInfo {
        hwnd: hwnd.0 as usize,
        rect,
        app_name,
        exe_name,
    })
}

/// Phase 0.2: same as `get_active_window_info` but resolves an existing HWND
/// (used when `g.target_hwnd` is already known so the callsite doesn't have
/// to re-walk the z-order).
pub fn get_window_info_for_hwnd(hwnd_raw: usize) -> Option<ActiveWindowInfo> {
    let hwnd = HWND(hwnd_raw as *mut _);
    let rect = frame_rect_of(hwnd)?;
    let mut pid: u32 = 0;
    unsafe {
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    }
    let exe_path = exe_path_of_pid(pid);
    let exe_name = std::path::Path::new(&exe_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let app_name = resolve_app_name(hwnd, &exe_path);
    Some(ActiveWindowInfo {
        hwnd: hwnd_raw,
        rect,
        app_name,
        exe_name,
    })
}

/// Enumerate all plausible capture targets visible on screen. Used by the
/// "target window picker" so the user can explicitly pin an app.
///
/// Deduplicates by (exe stem, title): multiple windows of the same app are
/// only collapsed to one entry when their titles are indistinguishable (equal
/// once trimmed + lowercased, including two blank titles) — otherwise every
/// distinctly-titled window is listed (e.g. two VS Code windows on different
/// projects both appear; the frontend renders `title` as the sub-line under
/// `display_name` whenever they differ). EnumWindows walks in z-order, so
/// among indistinguishable duplicates the one kept is always the topmost
/// (most recently active).
pub fn list_target_windows() -> Vec<TargetWindowInfo> {
    struct State {
        our_pid: u32,
        seen_keys: Vec<(String, String)>,
        results: Vec<TargetWindowInfo>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if !is_target_candidate(hwnd, state.our_pid) {
            return TRUE;
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf) as usize;
        let title = String::from_utf16_lossy(&buf[..len]).trim().to_string();

        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let exe_path = exe_path_of_pid(pid);
        let exe_stem = std::path::Path::new(&exe_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();

        // One entry per (app, distinguishable title) — skip only true duplicates.
        let key = (exe_stem.to_lowercase(), title.to_lowercase());
        if state.seen_keys.contains(&key) {
            return TRUE;
        }
        state.seen_keys.push(key);

        // UWP/Store apps (Microsoft To Do, Mail, Calculator, ...) don't get their own
        // exe — they all run hosted inside the shared ApplicationFrameHost.exe process, so
        // exe_stem is useless for naming them (every UWP app looks identical by that
        // measure). The window title is what the user actually sees, so use it directly
        // rather than falling through friendly_exe_name to the generic host name.
        let display_name = if exe_stem.eq_ignore_ascii_case("ApplicationFrameHost") {
            if title.is_empty() {
                exe_stem.clone()
            } else {
                title.clone()
            }
        } else {
            friendly_exe_name(&exe_stem)
        };
        state.results.push(TargetWindowInfo {
            hwnd: hwnd.0 as usize,
            title,
            exe_stem,
            display_name,
        });
        TRUE
    }

    let our_pid = std::process::id();
    let mut state = State {
        our_pid,
        seen_keys: Vec::new(),
        results: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.results
}

/// Return the screen rect of `hwnd` without visibility checks — used to get
/// the panel's current position so it can be blanked from captures.
#[allow(dead_code)]
pub fn window_screen_rect(hwnd: HWND) -> Option<Rect> {
    frame_rect_of(hwnd)
}

/// Find the top-most visible candidate window whose title contains `needle` (case-insensitive)
/// and does NOT contain `exclude` (when non-empty). Returns its HWND as `usize`. Diagnostic /
/// nav-pack-authoring helper for capturing a *specific* app window (e.g. the main Blender window,
/// not its "Blender Preferences" dialog). EnumWindows walks top→bottom z-order, so the first
/// match is the front-most.
#[allow(dead_code)] // used by the nav-pack-authoring `#[ignore]` test harnesses
pub fn find_window_by_title(needle: &str, exclude: &str) -> Option<usize> {
    struct State {
        our_pid: u32,
        needle: String,
        exclude: String,
        found: Option<usize>,
    }
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if state.found.is_some() || !is_target_candidate(hwnd, state.our_pid) {
            return TRUE;
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf) as usize;
        let title = String::from_utf16_lossy(&buf[..len]).to_lowercase();
        if title.contains(&state.needle)
            && (state.exclude.is_empty() || !title.contains(&state.exclude))
        {
            state.found = Some(hwnd.0 as usize);
            return FALSE; // stop — first (front-most) match
        }
        TRUE
    }
    let mut state = State {
        our_pid: std::process::id(),
        needle: needle.to_lowercase(),
        exclude: exclude.to_lowercase(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.found
}

/// Enumerate every connected monitor and return its rect in virtual-desktop
/// coordinates. Replaces ad-hoc `xcap::Monitor::all()` enumeration elsewhere
/// in the crate — we already have a per-monitor pipeline for capture, so
/// monitor enumeration goes through the same code path.
pub fn enumerate_monitor_rects() -> Vec<Rect> {
    collect_all_monitors()
        .iter()
        .map(|info| {
            let r = info.monitorInfo.rcMonitor;
            Rect {
                x: r.left,
                y: r.top,
                width: (r.right - r.left).max(0) as u32,
                height: (r.bottom - r.top).max(0) as u32,
            }
        })
        .collect()
}

/// A single connected monitor, for the target picker's per-screen "share this screen"
/// choices. `index` is the position in `collect_all_monitors()` and is what
/// `pin_full_screen_target` resolves back to a rect.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Enumerate connected monitors as `MonitorInfo` (index, primary flag, rect). Same
/// ordering as `enumerate_monitor_rects()` so an index from one matches the other.
pub fn list_monitors() -> Vec<MonitorInfo> {
    collect_all_monitors()
        .iter()
        .enumerate()
        .map(|(index, info)| {
            let r = info.monitorInfo.rcMonitor;
            MonitorInfo {
                index,
                // MONITORINFOF_PRIMARY (0x1) — not re-exported by windows-rs 0.62, inlined.
                primary: info.monitorInfo.dwFlags & 0x0000_0001 != 0,
                x: r.left,
                y: r.top,
                width: (r.right - r.left).max(0) as u32,
                height: (r.bottom - r.top).max(0) as u32,
            }
        })
        .collect()
}

/// Returns the rect of the entire multi-monitor virtual desktop.
pub fn get_virtual_desktop_rect() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32,
        }
    }
}

/// Find visible top-level panel windows belonging to our own process. Excludes
/// the overlay window (which is click-through via `WS_EX_TRANSPARENT`) so we
/// don't blank the screen guide's full canvas over the captured image.
///
/// The previous size-based filter (`< 2000 px`) silently failed on Windows 10
/// single-monitor setups where the overlay is exactly 1920×1080 — small enough
/// to slip through, large enough to cover the entire capture in light grey.
/// `WS_EX_TRANSPARENT` is the precise way to identify our overlay regardless
/// of monitor configuration.
pub fn own_panel_rects() -> Vec<Rect> {
    let our_pid = std::process::id();

    struct State {
        pid: u32,
        rects: Vec<Rect>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != state.pid {
            return TRUE;
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        // Skip the overlay — it's a click-through canvas that covers the whole
        // virtual desktop; blanking it would wipe the entire captured image.
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TRANSPARENT.0) != 0 {
            return TRUE;
        }
        if let Some(r) = frame_rect_of(hwnd) {
            state.rects.push(r);
        }
        TRUE
    }

    let mut state = State {
        pid: our_pid,
        rects: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.rects
}

/// Re-assert the overlay window's TOPMOST z-order so the guidance pointer stays
/// above transient popups (ribbon dropdowns, combo lists, context menus,
/// tooltips) that other apps create as freshly-activated topmost windows.
///
/// Windows places a newly shown menu above existing topmost windows of equal
/// band, so a one-time `alwaysOnTop` at window creation is not enough — the very
/// menu the user just opened can cover the pointer. Calling this on the tracker's
/// 200 ms tick (only while a pointer is active) keeps the overlay on top.
///
/// The overlay is the only own-process window with `WS_EX_TRANSPARENT`, so we
/// identify it the same way `own_panel_rects` does. `SWP_NOACTIVATE` ensures we
/// never steal focus from the app the user is working in.
pub fn raise_overlay_topmost() {
    let our_pid = std::process::id();

    struct State {
        pid: u32,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != state.pid {
            return TRUE;
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TRANSPARENT.0) != 0 && IsWindowVisible(hwnd).as_bool() {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            return FALSE; // overlay found — stop enumerating
        }
        TRUE
    }

    let mut state = State { pid: our_pid };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
}

/// Overwrite pixels in `img` that fall within any of `exclude_rects`.
///
/// `capture_rect` is the screen region that `img` represents (used to convert
/// from screen coordinates to image-local pixel coordinates). Regions that do
/// not overlap `capture_rect` are silently skipped.
pub fn blank_rects(
    img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    capture_rect: &Rect,
    exclude_rects: &[Rect],
) {
    const BLANK: Rgba<u8> = Rgba([220, 220, 220, 255]);
    for ex in exclude_rects {
        let ix = ex.x.max(capture_rect.x);
        let iy = ex.y.max(capture_rect.y);
        let ir = (ex.x + ex.width as i32).min(capture_rect.x + capture_rect.width as i32);
        let ib = (ex.y + ex.height as i32).min(capture_rect.y + capture_rect.height as i32);
        if ir <= ix || ib <= iy {
            continue;
        }
        let px = (ix - capture_rect.x) as u32;
        let py = (iy - capture_rect.y) as u32;
        let pw = ((ir - ix) as u32).min(img.width().saturating_sub(px));
        let ph = ((ib - iy) as u32).min(img.height().saturating_sub(py));
        for y in py..(py + ph) {
            for x in px..(px + pw) {
                img.put_pixel(x, y, BLANK);
            }
        }
    }
}

/// Grey every pixel of `img` that is NOT covered by at least one `keep_rect`.
/// `keep_rects` and `capture_rect` are in virtual-desktop screen coords; `img`
/// is the capture of `capture_rect`.
///
/// The capture region is the union *bounding box* of the target app's windows
/// (`pid_union_rect`). When the app has non-adjacent windows (detached toolbar,
/// floating palette, two-monitor spread) that bbox includes gaps that show
/// whatever is behind — other apps, the desktop. This blanks those gaps so the
/// AI only ever sees the target program.
///
/// No-op in the common single-window case (one keep rect covers the whole
/// capture) and when `keep_rects` is empty (defensive — never produces an
/// all-grey screenshot).
pub fn blank_outside_rects(
    img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    capture_rect: &Rect,
    keep_rects: &[Rect],
) {
    const BLANK: Rgba<u8> = Rgba([220, 220, 220, 255]);
    let iw = img.width() as i32;
    let ih = img.height() as i32;
    if iw <= 0 || ih <= 0 {
        return;
    }

    // Keep rects → image-local pixel bounds, clipped to the image.
    let locals: Vec<(i32, i32, i32, i32)> = keep_rects
        .iter()
        .filter_map(|k| {
            let x0 = (k.x - capture_rect.x).max(0);
            let y0 = (k.y - capture_rect.y).max(0);
            let x1 = (k.x + k.width as i32 - capture_rect.x).min(iw);
            let y1 = (k.y + k.height as i32 - capture_rect.y).min(ih);
            if x1 <= x0 || y1 <= y0 {
                None
            } else {
                Some((x0, y0, x1, y1))
            }
        })
        .collect();

    // Nothing to key off — leave the image untouched rather than blanking all.
    if locals.is_empty() {
        return;
    }

    // Common case: one window already spans the whole capture — no gaps.
    for &(x0, y0, x1, y1) in &locals {
        if x0 <= 0 && y0 <= 0 && x1 >= iw && y1 >= ih {
            return;
        }
    }

    // Per-row: blank the x-runs not covered by any keep rect spanning that row.
    for y in 0..ih {
        let mut intervals: Vec<(i32, i32)> = locals
            .iter()
            .filter(|&&(_, y0, _, y1)| y >= y0 && y < y1)
            .map(|&(x0, _, x1, _)| (x0, x1))
            .collect();

        if intervals.is_empty() {
            for x in 0..iw {
                img.put_pixel(x as u32, y as u32, BLANK);
            }
            continue;
        }

        intervals.sort_by_key(|&(a, _)| a);
        let mut cursor = 0;
        for (a, b) in intervals {
            if a > cursor {
                for x in cursor..a {
                    img.put_pixel(x as u32, y as u32, BLANK);
                }
            }
            cursor = cursor.max(b);
        }
        for x in cursor..iw {
            img.put_pixel(x as u32, y as u32, BLANK);
        }
    }
}

/// Returns the DWM extended frame bounds of `hwnd`, or GetWindowRect as a
/// fallback for classic/non-DWM windows. None if both fail.
fn frame_rect_of(hwnd: HWND) -> Option<Rect> {
    unsafe {
        let mut rect = RECT::default();
        let res = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if res.is_err() && GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if width == 0 || height == 0 {
            return None;
        }
        Some(Rect {
            x: rect.left,
            y: rect.top,
            width,
            height,
        })
    }
}

/// Return the bounding rect of all monitors that intersect with `rect`. For a
/// window straddling two monitors this returns the bounding box covering both,
/// so the full window can be captured. Falls back to the monitor containing
/// the rect's centre if no monitor intersects (shouldn't happen for visible
/// windows).
fn monitor_union_for(rect: &Rect) -> Rect {
    let r_right = rect.x + rect.width as i32;
    let r_bottom = rect.y + rect.height as i32;
    let mut bounds: Option<Rect> = None;
    for info in collect_all_monitors() {
        let mr = info.monitorInfo.rcMonitor;
        if rect.x < mr.right && r_right > mr.left && rect.y < mr.bottom && r_bottom > mr.top {
            bounds = Some(match bounds {
                None => Rect {
                    x: mr.left,
                    y: mr.top,
                    width: (mr.right - mr.left) as u32,
                    height: (mr.bottom - mr.top) as u32,
                },
                Some(b) => {
                    let left = b.x.min(mr.left);
                    let top = b.y.min(mr.top);
                    let right = (b.x + b.width as i32).max(mr.right);
                    let bottom = (b.y + b.height as i32).max(mr.bottom);
                    Rect {
                        x: left,
                        y: top,
                        width: (right - left).max(0) as u32,
                        height: (bottom - top).max(0) as u32,
                    }
                }
            });
        }
    }
    bounds.unwrap_or_else(|| {
        let cx = rect.x + rect.width as i32 / 2;
        let cy = rect.y + rect.height as i32 / 2;
        monitor_rect_containing(cx, cy).unwrap_or(*rect)
    })
}

/// Return the monitor rect containing the point `(x, y)`. Falls back to the
/// nearest monitor if the point is outside any monitor's bounds.
fn monitor_rect_containing(x: i32, y: i32) -> Option<Rect> {
    unsafe {
        let hmon: HMONITOR = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        if hmon.0.is_null() {
            return None;
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return None;
        }
        let r = mi.rcMonitor;
        Some(Rect {
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
        })
    }
}

/// Physical DPI scale (1.0 = 100 %, 1.25 = 125 %, 1.5 = 150 %, 2.0 = 200 %) of the monitor the
/// given rect's **centre** sits on — the DPI prior for template matching.
///
/// Uses the display **monitor's** effective DPI (`GetDpiForMonitor` / `MDT_EFFECTIVE_DPI`), *not*
/// `GetDpiForWindow`. Capture is composited-screen BitBlt (`CreateDCW("DISPLAY")`), so an
/// on-screen icon's pixel size tracks the physical monitor scale — even for a DPI-*unaware* target
/// app, which DWM bitmap-stretches up to the monitor scale before compositing (`GetDpiForWindow`
/// would report 96 for that app and mislead the prior). Because it keys off the rect location, a
/// mixed-DPI multi-monitor setup (e.g. 4K@200 % beside FHD@100 %) is handled per-locate: the scale
/// follows whichever monitor the captured window currently occupies. Falls back to 1.0 on any
/// failure (no prior → the sweep behaves exactly as before).
pub fn monitor_scale_for_rect(rect: &Rect) -> f32 {
    unsafe {
        let cx = rect.x + rect.width as i32 / 2;
        let cy = rect.y + rect.height as i32 / 2;
        let hmon = MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST);
        if hmon.0.is_null() {
            return 1.0;
        }
        let (mut dpix, mut dpiy) = (0u32, 0u32);
        match GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy) {
            Ok(()) if dpix > 0 => (dpix as f32 / 96.0).clamp(0.5, 4.0),
            _ => 1.0,
        }
    }
}

/// Returns true if `hwnd` is at least partially visible on screen — i.e. the
/// top-level window under at least one of five sample points (centre + four
/// inset corners) of `r` is `hwnd` itself. A same-PID sub-window that's fully
/// covered by another application would otherwise extend the capture bbox into
/// that other application's region; gap-blanking then preserves the rect
/// (same PID), but the actual pixels captured are the covering app's. Filter
/// such fully-occluded windows out of both the union and the keep-set so the
/// AI never sees a different program in place of one of the target's hidden
/// sub-windows.
unsafe fn is_visible_to_user(hwnd: HWND, r: &Rect) -> bool {
    let w = r.width as i32;
    let h = r.height as i32;
    if w <= 0 || h <= 0 {
        return false;
    }
    let inset_x = (w / 5).max(1);
    let inset_y = (h / 5).max(1);
    let points = [
        (r.x + w / 2, r.y + h / 2),
        (r.x + inset_x, r.y + inset_y),
        (r.x + w - inset_x, r.y + inset_y),
        (r.x + inset_x, r.y + h - inset_y),
        (r.x + w - inset_x, r.y + h - inset_y),
    ];
    for (px, py) in points {
        let top = WindowFromPoint(POINT { x: px, y: py });
        if top.0.is_null() {
            continue;
        }
        let root = GetAncestor(top, GA_ROOT);
        let root_top = if !root.0.is_null() { root } else { top };
        if root_top.0 == hwnd.0 {
            return true;
        }
    }
    false
}

/// Compute the bounding rect of all visible top-level windows belonging to the
/// same process as `target`, clamped to the monitor containing the target.
///
/// Catches modal dialogs and popups that are separate top-level HWNDs but part
/// of the same logical "app" — e.g. WeChat's Storage settings dialog floating
/// outside the main window, or Word's Find & Replace dialog. Without this the
/// capture would include only the main window's frame and any UI in the dialog
/// would be silently cropped out (and the AI would hallucinate coordinates for
/// content it couldn't see).
///
/// Same-monitor clamp: windows whose centre is on a different monitor are
/// excluded, so a WeChat instance with chat windows scattered across three
/// displays does not blow up the capture to the full virtual desktop.
///
/// Returns `target`'s own frame rect if no extension is possible.
pub fn pid_union_rect(target: HWND) -> Option<Rect> {
    let target_rect = frame_rect_of(target)?;

    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(target, Some(&mut pid));
    }
    if pid == 0 {
        return Some(target_rect);
    }

    // Cover all monitors the target overlaps — keeps a window that straddles
    // two screens captured in full instead of clamping to the centre monitor.
    let monitor = monitor_union_for(&target_rect);

    struct State {
        target: HWND,
        target_pid: u32,
        our_pid: u32,
        monitor: Rect,
        union: Rect,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != state.target_pid || pid == state.our_pid {
            return TRUE;
        }

        // Skip cloaked windows (UWP hidden, etc.)
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return TRUE;
        }

        // Skip tool windows (taskbar icons / hidden helpers) and click-through
        // overlays (transparent), they are never the user's target.
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TOOLWINDOW.0) != 0 || (ex_style & WS_EX_TRANSPARENT.0) != 0 {
            return TRUE;
        }

        // Skip shell / IME classes (defence in depth — same PID is unlikely to
        // hit these but the cost is one strcmp).
        let mut buf = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n as usize]);
        if SKIP_CLASSES.iter().any(|c| *c == class) {
            return TRUE;
        }

        let Some(r) = frame_rect_of(hwnd) else {
            return TRUE;
        };

        // Reject tiny windows (zombie/glitch hidden popups occasionally have
        // 0–50 px dimensions even when WS_VISIBLE).
        if r.width < 50 || r.height < 50 {
            return TRUE;
        }

        // Cross-monitor inclusion only when the window's owner chain roots at
        // the focused target — that's how the OS marks a dialog/popup as
        // belonging to a specific window (Word "Find"/"Font", a modal dialog
        // dragged to a second screen). Other same-PID top-level windows (a VS
        // Code extension panel torn off as its own window, a second editor
        // window, an unrelated tool window) have a different owner root and
        // must stay subject to the same-monitor filter — otherwise the bbox
        // balloons across monitors and stitches "different apps" together
        // even though they share a PID.
        let owned_by_target = GetAncestor(hwnd, GA_ROOTOWNER).0 == state.target.0;
        if !owned_by_target {
            let cx = r.x + r.width as i32 / 2;
            let cy = r.y + r.height as i32 / 2;
            let m = &state.monitor;
            if cx < m.x || cx >= m.x + m.width as i32 || cy < m.y || cy >= m.y + m.height as i32 {
                return TRUE;
            }
        }

        // Skip same-PID sub-windows that are fully occluded on screen. Without
        // this, a hidden VS Code window covered by Chrome on another monitor
        // would extend the capture bbox into Chrome's territory and the AI
        // would see Chrome instead of an empty VS Code rect. The target itself
        // is always included — it's the foreground window by construction.
        if hwnd.0 != state.target.0 && !is_visible_to_user(hwnd, &r) {
            return TRUE;
        }

        // Extend the running union.
        let u_left = state.union.x.min(r.x);
        let u_top = state.union.y.min(r.y);
        let u_right = (state.union.x + state.union.width as i32).max(r.x + r.width as i32);
        let u_bottom = (state.union.y + state.union.height as i32).max(r.y + r.height as i32);
        state.union = Rect {
            x: u_left,
            y: u_top,
            width: (u_right - u_left).max(0) as u32,
            height: (u_bottom - u_top).max(0) as u32,
        };
        TRUE
    }

    let our_pid = std::process::id();
    let mut state = State {
        target,
        target_pid: pid,
        our_pid,
        monitor,
        union: target_rect,
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }

    // Clamp the final union to the bounds of every monitor it now touches.
    // Using monitor_union_for(union) — not just the target's home monitor —
    // lets an owned dialog on a second screen survive the clamp. For the common
    // single-monitor union this is identical to the home monitor.
    let u = state.union;
    let m = monitor_union_for(&u);
    let left = u.x.max(m.x);
    let top = u.y.max(m.y);
    let right = (u.x + u.width as i32).min(m.x + m.width as i32);
    let bottom = (u.y + u.height as i32).min(m.y + m.height as i32);
    let width = (right - left).max(0) as u32;
    let height = (bottom - top).max(0) as u32;
    if width == 0 || height == 0 {
        return Some(target_rect);
    }
    Some(Rect {
        x: left,
        y: top,
        width,
        height,
    })
}

/// Convenience wrapper for callers that hold a raw HWND value (usize).
pub fn pid_union_rect_raw(hwnd_raw: usize) -> Option<Rect> {
    pid_union_rect(HWND(hwnd_raw as *mut _))
}

/// Geometric intersection of two rects. Returns a zero-sized rect when disjoint.
fn rect_intersect(a: &Rect, b: &Rect) -> Rect {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width as i32).min(b.x + b.width as i32);
    let y2 = (a.y + a.height as i32).min(b.y + b.height as i32);
    if x2 <= x1 || y2 <= y1 {
        return Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }
    Rect {
        x: x1,
        y: y1,
        width: (x2 - x1) as u32,
        height: (y2 - y1) as u32,
    }
}

/// Return the parts of `a` not covered by `b` (0, 1, 2, 3, or 4 axis-aligned sub-rects).
fn rect_subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let ix = rect_intersect(&a, &b);
    if ix.width == 0 || ix.height == 0 {
        return vec![a];
    }
    let a_right = a.x + a.width as i32;
    let a_bottom = a.y + a.height as i32;
    let i_right = ix.x + ix.width as i32;
    let i_bottom = ix.y + ix.height as i32;
    let mut out = Vec::with_capacity(4);
    // Top strip (full width of a, above the intersection).
    if ix.y > a.y {
        out.push(Rect {
            x: a.x,
            y: a.y,
            width: a.width,
            height: (ix.y - a.y) as u32,
        });
    }
    // Bottom strip (full width of a, below the intersection).
    if i_bottom < a_bottom {
        out.push(Rect {
            x: a.x,
            y: i_bottom,
            width: a.width,
            height: (a_bottom - i_bottom) as u32,
        });
    }
    // Left strip (middle band, left of the intersection).
    if ix.x > a.x {
        out.push(Rect {
            x: a.x,
            y: ix.y,
            width: (ix.x - a.x) as u32,
            height: ix.height,
        });
    }
    // Right strip (middle band, right of the intersection).
    if i_right < a_right {
        out.push(Rect {
            x: i_right,
            y: ix.y,
            width: (a_right - i_right) as u32,
            height: ix.height,
        });
    }
    out
}

/// Subtract a list of covering rects from `a`. Returns the parts of `a` that
/// remain visible. Each subtraction can fragment surviving pieces into smaller
/// rects; this is O(N²) in the worst case but N (windows above) is small.
fn rect_subtract_many(a: Rect, covered: &[Rect]) -> Vec<Rect> {
    let mut current = vec![a];
    for c in covered {
        if current.is_empty() {
            break;
        }
        let mut next = Vec::with_capacity(current.len());
        for r in &current {
            next.extend(rect_subtract(*r, *c));
        }
        current = next;
    }
    current
}

/// Compute the per-pixel-accurate keep-set for gap-blanking: the **visible
/// portions** of every same-PID top-level window, clipped to `bbox`.
///
/// Walks top-level windows in z-order top-to-bottom (EnumWindows order). For
/// each window we compute its visible portion = `rect ∩ bbox − union(higher-z
/// rects)`. Same-PID visible portions go into the keep-set; non-same-PID rects
/// still contribute to `covered` so they correctly hide same-PID stuff below
/// them (e.g. Word covering a hidden VS Code sub-window). Our own pid is
/// skipped entirely — the panel is blanked separately by the caller.
///
/// Result: same-PID pixels actually visible on screen are preserved; covered
/// portions (the other-app slivers that previously leaked into the capture)
/// fall outside the keep-set and `blank_outside_rects` greys them. Returns the
/// target's frame as a single-element fallback when enumeration finds nothing,
/// so the keep-set is never empty.
pub fn pid_visible_keep_rects(target: HWND, bbox: &Rect) -> Vec<Rect> {
    let mut target_pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(target, Some(&mut target_pid));
    }
    if target_pid == 0 {
        return frame_rect_of(target).into_iter().collect();
    }
    let our_pid = std::process::id();

    struct State {
        bbox: Rect,
        our_pid: u32,
        target_pid: u32,
        covered: Vec<Rect>,
        keep: Vec<Rect>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == state.our_pid {
            // Our own panel/overlay is handled by the separate `exclude` pass.
            return TRUE;
        }
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return TRUE;
        }
        let Some(r_full) = frame_rect_of(hwnd) else {
            return TRUE;
        };
        let r = rect_intersect(&r_full, &state.bbox);
        if r.width == 0 || r.height == 0 {
            return TRUE;
        }

        // Visible-now = this window's bbox-clipped rect minus everything above it.
        if pid == state.target_pid {
            let visible = rect_subtract_many(r, &state.covered);
            state.keep.extend(visible);
        }
        state.covered.push(r);
        TRUE
    }

    let mut state = State {
        bbox: *bbox,
        our_pid,
        target_pid,
        covered: Vec::new(),
        keep: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }

    if state.keep.is_empty() {
        // Defensive: don't return an empty keep-set, otherwise blank_outside_rects
        // greys the whole capture. Fall back to the target's clipped frame.
        if let Some(r) = frame_rect_of(target) {
            let clipped = rect_intersect(&r, bbox);
            if clipped.width > 0 && clipped.height > 0 {
                return vec![clipped];
            }
        }
    }
    state.keep
}

/// Convenience wrapper for callers that hold a raw HWND value (usize).
pub fn pid_visible_keep_rects_raw(hwnd_raw: usize, bbox: &Rect) -> Vec<Rect> {
    pid_visible_keep_rects(HWND(hwnd_raw as *mut _), bbox)
}

/// Returns true when `pid` belongs to Tauri's embedded WebView2 renderer
/// (`msedgewebview2.exe`). Used to distinguish that process from a real
/// Chrome/Edge browser window — both use the `Chrome_WidgetWin_1` class.
fn is_webview2_renderer(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
            && String::from_utf16_lossy(&buf[..len as usize])
                .to_ascii_lowercase()
                .ends_with("msedgewebview2.exe");
        let _ = CloseHandle(handle);
        result
    }
}

/// Is `hwnd` a plausible capture target (the actual app the user is interacting with)?
/// We must vigorously filter out "ghost" windows and invisible overlays because
/// Z-order walks (`EnumWindows`) frequently trip over them.
/// This prevents capturing:
/// - Windows 10/11 suspended/cloaked UWP apps (e.g., hidden Search or Settings)
/// - Gaming overlays (NVIDIA GeForce, Steam, Xbox Game Bar, Discord, AMD)
/// - Our own background/renderer processes (Tauri/WebView2 overlay canvas)
fn is_target_candidate(hwnd: HWND, our_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid == 0 || pid == our_pid {
            return false;
        }

        // Filter out windows owned by our app (e.g. WebView2 popups or overlay windows)
        let root = GetAncestor(hwnd, GA_ROOTOWNER);
        let mut root_pid: u32 = 0;
        let _ = GetWindowThreadProcessId(root, Some(&mut root_pid as *mut u32));
        if root_pid == our_pid {
            return false;
        }

        // Filter out Windows 10/11 "cloaked" windows.
        // UWP apps and hidden system overlays are WS_VISIBLE but cloaked.
        let mut cloaked: u32 = 0;
        let res = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if res.is_ok() && cloaked != 0 {
            return false;
        }

        // Generic Overlay Filter:
        // Automatically skips gaming and system overlays (Steam, Discord, Xbox, NVIDIA, AMD).
        // These overlays run invisibly in the background and sit high in the Z-order.
        // They use WS_EX_TOOLWINDOW (to hide from Taskbar/Alt-Tab) or WS_EX_TRANSPARENT
        // (to allow mouse clicks to pass through them to the game beneath).
        // If a window cannot receive mouse clicks, the user cannot be interacting with it!
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TOOLWINDOW.0) != 0 || (ex_style & WS_EX_TRANSPARENT.0) != 0 {
            return false;
        }

        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return false;
        }
        if (wr.right - wr.left) <= 100 || (wr.bottom - wr.top) <= 100 {
            return false;
        }
        let mut buf = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n as usize]);
        if SKIP_CLASSES.iter().any(|c| *c == class) {
            return false;
        }
        // Skip Tauri's embedded WebView2 renderer. Chrome and Edge use
        // chrome.exe / msedge.exe, not msedgewebview2.exe, so this only
        // filters the app's own renderer process.
        if class == "Chrome_WidgetWin_1" && is_webview2_renderer(pid) {
            return false;
        }
        true
    }
}

/// Find the first top-level window that passes `is_target_candidate` via
/// `EnumWindows`. More reliable than a manual `GetTopWindow`/`GetWindow` walk
/// because `GetWindow(GW_HWNDNEXT)` can fail at the topmost→non-topmost
/// z-order boundary, cutting the walk short before reaching the target app.
fn first_target_in_z_order(our_pid: u32) -> Option<HWND> {
    struct State {
        our_pid: u32,
        result: Option<HWND>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if is_target_candidate(hwnd, state.our_pid) {
            state.result = Some(hwnd);
            FALSE // stop enumeration
        } else {
            TRUE // continue enumeration
        }
    }

    let mut state = State {
        our_pid,
        result: None,
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.result
}

/// Returns the HWND and frame rect of the best capture target.
///
/// Strategy:
/// 1. GetForegroundWindow() → walk owner chain to root. If it's a valid
///    non-Navigator window, use it directly.
/// 2. Navigator is foreground (user clicked our button) → z-order walk via
///    EnumWindows. The window the user was just working in is #2 in z-order
///    (right behind Navigator), so the walk finds it immediately. PrintWindow
///    then captures it correctly regardless of monitor position.
pub fn get_foreground_target() -> Option<(HWND, Rect)> {
    let our_pid = std::process::id();
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.0.is_null() {
            // Walk the owner chain to reach the root application window.
            // For a top-level main window GA_ROOTOWNER returns itself.
            // For an owned dialog/popup it returns the owning main window.
            let root = GetAncestor(fg, GA_ROOTOWNER);
            let target = if !root.0.is_null() { root } else { fg };
            if is_target_candidate(target, our_pid) {
                if let Some(r) = frame_rect_of(target) {
                    return Some((target, r));
                }
            }
        }
    }
    // Navigator is foreground — z-order walk finds the most recently active app.
    // If it found an owned dialog (e.g. Word's Phonetic Guide dialog is z=2
    // while the main Word window is z=3), walk up to GA_ROOTOWNER so we return
    // the main window HWND. The xcap screen-region crop on the owner rect then
    // includes the dialog naturally (it's drawn on top of the owner on screen).
    let hwnd = first_target_in_z_order(our_pid)?;
    let root = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    let target = if !root.0.is_null() && root.0 != hwnd.0 && is_target_candidate(root, our_pid) {
        root
    } else {
        hwnd
    };
    frame_rect_of(target).map(|r| (target, r))
}

// SHELVED — instant foreground tracking via SetWinEventHook.
// Activate if z-order proves insufficient (e.g. user navigates between apps
// without directly clicking the target before Guide me).
//
// Required: add "Win32_UI_Accessibility" to Cargo.toml windows features,
// add imports: SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK (Accessibility),
// DispatchMessageW, GetMessageW, MSG, EVENT_SYSTEM_FOREGROUND,
// WINEVENT_OUTOFCONTEXT (WindowsAndMessaging),
// and AtomicUsize/Ordering from std::sync::atomic.
//
// static LAST_TARGET_HWND: AtomicUsize = AtomicUsize::new(0);
//
// unsafe extern "system" fn on_foreground_change(
//     _hook: HWINEVENTHOOK, _event: u32, hwnd: HWND,
//     _id_object: i32, _id_child: i32, _id_event_thread: u32, _event_time: u32,
// ) {
//     if hwnd.0.is_null() { return; }
//     let our_pid = std::process::id();
//     let root = GetAncestor(hwnd, GA_ROOTOWNER);
//     let target = if !root.0.is_null() { root } else { hwnd };
//     if is_target_candidate(target, our_pid) {
//         LAST_TARGET_HWND.store(target.0 as usize, Ordering::Relaxed);
//     }
// }
//
// pub fn start_foreground_tracking() {
//     std::thread::Builder::new().name("foreground-tracker".into())
//         .spawn(|| unsafe {
//             let hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
//                 None, Some(on_foreground_change), 0, 0, WINEVENT_OUTOFCONTEXT);
//             if hook.is_invalid() { return; }
//             let mut msg = MSG::default();
//             while GetMessageW(&mut msg, None, 0, 0).as_bool() {
//                 let _ = DispatchMessageW(&msg);
//             }
//             let _ = UnhookWinEvent(hook);
//         }).expect("spawn foreground-tracker");
// }
//
// In get_foreground_target(), after the direct-foreground check fails, add
// before the z-order walk:
//   let last = LAST_TARGET_HWND.load(Ordering::Relaxed);
//   if last != 0 {
//       let hwnd = HWND(last as *mut _);
//       unsafe {
//           if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
//               if let Some(r) = frame_rect_of(hwnd) { return Some((hwnd, r)); }
//           }
//       }
//       LAST_TARGET_HWND.store(0, Ordering::Relaxed);
//   }
// Call capture::start_foreground_tracking() from lib.rs setup (#[cfg(windows)]).
// END SHELVED

/// Enumerate all connected monitors and return their `MONITORINFOEXW` descriptors.
/// The `szDevice` field inside each descriptor is the device name used by `CreateDCW`.
fn collect_all_monitors() -> Vec<MONITORINFOEXW> {
    struct State(Vec<HMONITOR>);

    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        state.0.push(hmon);
        TRUE
    }

    let mut state = State(Vec::new());
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_cb),
            LPARAM(&mut state as *mut State as isize),
        );
    }

    state
        .0
        .iter()
        .filter_map(|&hmon| unsafe {
            let mut info: MONITORINFOEXW = mem::zeroed();
            info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool()
            {
                Some(info)
            } else {
                None
            }
        })
        .collect()
}

/// `(dst_x, dst_y, image)` — a piece captured from a single monitor and where
/// to place it within the caller's output canvas.
type MonitorPiece = (i64, i64, ImageBuffer<Rgba<u8>, Vec<u8>>);

/// Capture the portion of `rect` that overlaps with monitor `info` using a
/// per-monitor GDI DC created with `CreateDCW`. Source coordinates are
/// monitor-relative (always non-negative), which is why this works for
/// left-secondary monitors at negative virtual-desktop x — unlike `GetDC(NULL)`
/// (primary-only) or xcap/DXGI (silently fails on negative-x monitors).
unsafe fn capture_from_monitor(rect: &Rect, info: &MONITORINFOEXW) -> Result<MonitorPiece> {
    let mr = info.monitorInfo.rcMonitor;
    let clip_left = rect.x.max(mr.left);
    let clip_top = rect.y.max(mr.top);
    let clip_right = (rect.x + rect.width as i32).min(mr.right);
    let clip_bottom = (rect.y + rect.height as i32).min(mr.bottom);
    let cw = clip_right - clip_left;
    let ch = clip_bottom - clip_top;
    if cw <= 0 || ch <= 0 {
        return Err(anyhow!("no overlap with monitor"));
    }

    let hdc_mon = CreateDCW(
        windows::core::w!("DISPLAY"),
        PCWSTR(info.szDevice.as_ptr()),
        PCWSTR(std::ptr::null()),
        None,
    );
    if hdc_mon.is_invalid() {
        return Err(anyhow!("CreateDCW failed"));
    }

    let hdc_mem = CreateCompatibleDC(Some(hdc_mon));
    let h_bmp = CreateCompatibleBitmap(hdc_mon, cw, ch);
    let prev = SelectObject(hdc_mem, h_bmp.into());

    // Convert virtual-desktop coords → monitor-relative (always ≥ 0).
    let src_x = clip_left - mr.left;
    let src_y = clip_top - mr.top;
    let blt_ok = BitBlt(hdc_mem, 0, 0, cw, ch, Some(hdc_mon), src_x, src_y, SRCCOPY).is_ok();

    let buf_len = (cw * ch * 4) as usize;
    let mut buf = vec![0u8; buf_len];
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: cw,
            biHeight: -ch, // negative = top-down scan order
            biPlanes: 1,
            biBitCount: 32,
            biSizeImage: buf_len as u32,
            biCompression: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    GetDIBits(
        hdc_mem,
        h_bmp,
        0,
        ch as u32,
        Some(buf.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    } // GDI returns BGRA → swap to RGBA

    SelectObject(hdc_mem, prev);
    let _ = DeleteObject(h_bmp.into());
    let _ = DeleteDC(hdc_mem);
    let _ = DeleteDC(hdc_mon); // DeleteDC (not ReleaseDC) for CreateDCW-created DCs

    if !blt_ok {
        return Err(anyhow!("BitBlt failed"));
    }

    let img = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(cw as u32, ch as u32, buf)
        .ok_or_else(|| anyhow!("ImageBuffer::from_raw failed"))?;
    Ok(((clip_left - rect.x) as i64, (clip_top - rect.y) as i64, img))
}

/// Capture a region of the desktop using per-monitor `CreateDCW` GDI DCs.
///
/// Source coordinates are monitor-relative (always non-negative), making this
/// correct for left-secondary monitors at negative virtual-desktop x. For rects
/// that span multiple monitors (virtual-desktop full-screen requests) each
/// overlapping monitor is captured and stitched into a single canvas.
pub fn capture_desktop_region(rect: &Rect) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let w = rect.width as i32;
    let h = rect.height as i32;
    if w <= 0 || h <= 0 {
        return Err(anyhow!("zero dimensions {}×{}", w, h));
    }

    let monitors = collect_all_monitors();
    let overlapping: Vec<_> = monitors
        .iter()
        .filter(|info| {
            let r = info.monitorInfo.rcMonitor;
            rect.x < r.right && rect.x + w > r.left && rect.y < r.bottom && rect.y + h > r.top
        })
        .collect();

    if overlapping.is_empty() {
        return Err(anyhow!("rect does not intersect any monitor"));
    }

    // Single-monitor fast path (the common case for active-window captures).
    if overlapping.len() == 1 {
        let (_, _, img) = unsafe { capture_from_monitor(rect, overlapping[0]) }?;
        return Ok(img);
    }

    // Multi-monitor path: stitch each monitor's contribution onto a canvas.
    let mut canvas =
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(rect.width, rect.height, Rgba([0, 0, 0, 255]));
    for info in &overlapping {
        if let Ok((dx, dy, piece)) = unsafe { capture_from_monitor(rect, info) } {
            image::imageops::overlay(&mut canvas, &piece, dx, dy);
        }
    }
    Ok(canvas)
}
