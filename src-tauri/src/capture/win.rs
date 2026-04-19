//! Windows-specific helpers for capture: foreground window rect via DWM
//! extended frame bounds (matches v0.3 `screen_capture.py` behaviour so the
//! crop aligns with what the user sees, not raw GetWindowRect).

use super::Rect;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

/// Returns the extended frame bounds of the foreground window in physical
/// pixels relative to the virtual desktop. Falls back to GetWindowRect if
/// DWM lookup fails (e.g. on a classic/non-DWM window).
pub fn get_foreground_frame_rect() -> Option<Rect> {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

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
