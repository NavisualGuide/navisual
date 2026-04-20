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
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetTopWindow, GetWindow, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GW_HWNDNEXT,
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
        !SKIP_CLASSES.iter().any(|c| *c == class)
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

/// Returns the frame rect of the best capture target: foreground window if
/// owned by another process, else the first suitable window in z-order. None
/// if nothing on-screen qualifies.
pub fn get_foreground_frame_rect() -> Option<Rect> {
    let our_pid = std::process::id();
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.0.is_null() && is_target_candidate(fg, our_pid) {
            if let Some(r) = frame_rect_of(fg) {
                return Some(r);
            }
        }
    }
    let hwnd = first_target_in_z_order(our_pid)?;
    frame_rect_of(hwnd)
}
