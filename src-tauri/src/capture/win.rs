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
use windows::Win32::Foundation::{CloseHandle, FALSE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    GetDIBits, SelectObject, GetDC, ReleaseDC,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, GetWindowTextW,
    GetWindowLongW, GWL_EXSTYLE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    GetSystemMetrics, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    GA_ROOTOWNER,
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

        let mut class = [0u16; 256];
        let clen = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class);
        let class_str = String::from_utf16_lossy(&class[..clen as usize]);

        let mut rect = windows::Win32::Foundation::RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect);

        let mut pid: u32 = 0;
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));

        format!("Title: '{}'\nClass: '{}'\nRect: [{}, {}, {}, {}]\nPID: {}", title_str, class_str, rect.left, rect.top, rect.right, rect.bottom, pid)
    }
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
/// Deduplicates by exe stem: if the same app has multiple windows open,
/// only the topmost (most recently focused) window is returned — EnumWindows
/// walks in z-order so the first hit per exe is always the most recently active.
pub fn list_target_windows() -> Vec<TargetWindowInfo> {
    struct State {
        our_pid: u32,
        seen_exe_stems: Vec<String>,
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

        // One entry per app — skip duplicate exe stems (same app, different window).
        let key = exe_stem.to_lowercase();
        if state.seen_exe_stems.contains(&key) {
            return TRUE;
        }
        state.seen_exe_stems.push(key);

        let display_name = friendly_exe_name(&exe_stem);
        state.results.push(TargetWindowInfo {
            hwnd: hwnd.0 as usize,
            title,
            exe_stem,
            display_name,
        });
        TRUE
    }

    let our_pid = std::process::id();
    let mut state = State { our_pid, seen_exe_stems: Vec::new(), results: Vec::new() };
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

/// Find all visible top-level windows belonging to our own process that are
/// small enough to be the panel (< 2000 px on either axis — excludes the
/// always-visible overlay window which spans the virtual desktop).
/// Returns the DWM extended frame rect of each matched window.
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
        if let Some(r) = frame_rect_of(hwnd) {
            // Overlay spans the virtual desktop (thousands of px wide).
            // The panel is at most a few hundred px in each direction.
            if r.width < 2000 && r.height < 2000 {
                state.rects.push(r);
            }
        }
        TRUE
    }

    let mut state = State { pid: our_pid, rects: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.rects
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

    let mut state = State { our_pid, result: None };
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
    let target = if !root.0.is_null()
        && root.0 != hwnd.0
        && is_target_candidate(root, our_pid)
    {
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

/// Capture a region of the desktop using a per-monitor device context.
///
/// Why per-monitor `CreateDCW(L"DISPLAY", deviceName, ...)` instead of
/// `GetDC(NULL)` or `CreateDCW(L"DISPLAY", NULL, ...)`?
///   - The "whole virtual desktop" DCs are documented as primary-monitor-only
///     on many systems and silently fail to capture content from secondary
///     monitors at negative coordinates (xcap exhibits the same issue).
///   - A DC scoped to a specific display device works regardless of where the
///     monitor sits in virtual space — source coords are monitor-relative
///     (always non-negative), so the negative-x problem disappears entirely.
///
/// Why not `PrintWindow`? It renders a single window's surface only — owned
/// dialogs (separate top-level windows like Word's Phonetic Guide) are
/// invisible. BitBlt from the screen DC reads the composited image, so any
/// dialog/popup/tooltip drawn on top of the target window is captured naturally.
pub fn capture_desktop_region(rect: &Rect) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let w = rect.width as i32;
    let h = rect.height as i32;
    if w <= 0 || h <= 0 {
        return Err(anyhow!("zero dimensions {}×{}", w, h));
    }
    unsafe {
        // Use GetDC(NULL) to get the true Virtual Desktop DC that spans all monitors.
        // CreateDCW("DISPLAY", NULL) only returns the primary monitor DC.
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err(anyhow!("GetDC failed for virtual desktop"));
        }

        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        let h_bmp = CreateCompatibleBitmap(hdc_screen, w, h);
        let prev = SelectObject(hdc_mem, h_bmp.into());

        // Source coords are absolute virtual desktop coordinates.
        let src_x = rect.x;
        let src_y = rect.y;
        let blt_ok = BitBlt(
            hdc_mem, 0, 0, w, h,
            Some(hdc_screen), src_x, src_y, SRCCOPY,
        ).is_ok();

        let buf_len = (w * h * 4) as usize;
        let mut buf = vec![0u8; buf_len];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down row order
                biPlanes: 1,
                biBitCount: 32,
                biSizeImage: buf_len as u32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        GetDIBits(
            hdc_mem, h_bmp, 0, h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi, DIB_RGB_COLORS,
        );

        // GDI returns BGRA; swap B↔R to produce RGBA expected by the image crate.
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        SelectObject(hdc_mem, prev);
        let _ = DeleteObject(h_bmp.into());
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(None, hdc_screen);

        if !blt_ok {
            return Err(anyhow!("BitBlt failed"));
        }
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(rect.width, rect.height, buf)
            .ok_or_else(|| anyhow!("ImageBuffer::from_raw failed"))
    }
}
