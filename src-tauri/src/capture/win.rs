//! Windows-specific helpers for capture: pick the best target window (skipping
//! AI Navigator's own panel) and return its DWM extended frame bounds.
//!
//! When the panel is foreground (typical whenever the user clicks a button in
//! our UI), we walk the z-order and return the highest visible, non-self,
//! non-shell window instead. This keeps OCR working on whatever the user is
//! actually looking at — e.g. Task Manager — rather than capturing our own
//! panel contents.

use super::Rect;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, GetTopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    GA_ROOTOWNER, GWL_EXSTYLE, GW_HWNDNEXT, WS_EX_TRANSPARENT,
};

/// Class names we never treat as a capture target (shell, IME, overlays).
const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "Windows.UI.Core.CoreWindow",
    "IME",
    "MSCTFIME UI",
    "Default IME",
];

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
        if res.is_err() {
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return None;
            }
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
        if QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            return name.to_ascii_lowercase().ends_with("msedgewebview2.exe");
        }
        false
    }
}

/// Is `hwnd` a plausible capture target (visible, not minimised, has an
/// acceptable class, and is NOT owned by our process)?
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

/// Walk top-level windows in z-order and return the first that passes
/// `is_target_candidate`. Used when the foreground window is ours.
fn first_target_in_z_order(our_pid: u32) -> Option<HWND> {
    unsafe {
        let mut hwnd = GetTopWindow(None).ok()?;
        let mut steps = 0usize;
        while !hwnd.0.is_null() && steps < 64 {
            if is_target_candidate(hwnd, our_pid) {
                return Some(hwnd);
            }
            match GetWindow(hwnd, GW_HWNDNEXT) {
                Ok(next) => hwnd = next,
                Err(_) => return None,
            }
            steps += 1;
        }
        None
    }
}

/// Returns rects of all visible, non-minimised top-level windows owned by the
/// current process, excluding click-through windows (the overlay, identified
/// by WS_EX_TRANSPARENT). Used to blank the Navigator UI from screenshots.
pub fn own_window_rects() -> Vec<Rect> {
    let our_pid = std::process::id();
    let mut rects = Vec::new();
    unsafe {
        let Ok(mut hwnd) = GetTopWindow(None) else { return rects; };
        let mut steps = 0usize;
        while !hwnd.0.is_null() && steps < 64 {
            let mut pid: u32 = 0;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == our_pid
                && IsWindowVisible(hwnd).as_bool()
                && !IsIconic(hwnd).as_bool()
            {
                // WS_EX_TRANSPARENT is set by set_ignore_cursor_events(true) on the
                // overlay window. Skip it — it's mostly transparent and its rect spans
                // the entire virtual desktop.
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                if ex & WS_EX_TRANSPARENT.0 == 0 {
                    let mut wr = RECT::default();
                    if GetWindowRect(hwnd, &mut wr).is_ok() {
                        let w = (wr.right - wr.left).max(0) as u32;
                        let h = (wr.bottom - wr.top).max(0) as u32;
                        if w > 0 && h > 0 {
                            rects.push(Rect { x: wr.left, y: wr.top, width: w, height: h });
                        }
                    }
                }
            }
            match GetWindow(hwnd, GW_HWNDNEXT) {
                Ok(next) => hwnd = next,
                Err(_) => break,
            }
            steps += 1;
        }
    }
    rects
}

/// Returns the frame rect of the best capture target.
///
/// Strategy: get the foreground window, then walk to its root owner via
/// `GetAncestor(GA_ROOTOWNER)`. This resolves any owned window — dropdown,
/// combo popup, confirmation dialog — back to the main application window it
/// belongs to, regardless of size. A dialog box owned by a slicer app will
/// correctly yield the slicer's main window, so the screenshot shows the full
/// app with the dialog visible rather than just the dialog in isolation.
///
/// Falls back to z-order walk when the panel is foreground or no suitable
/// window is found.
pub fn get_foreground_frame_rect() -> Option<Rect> {
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
                    return Some(r);
                }
            }
        }
    }
    let hwnd = first_target_in_z_order(our_pid)?;
    frame_rect_of(hwnd)
}
