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
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
    GetDIBits, GetMonitorInfoW, MonitorFromPoint, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    GA_ROOTOWNER,
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
        // Locate the monitor that contains the rect's center point.
        let center = POINT {
            x: rect.x + (rect.width as i32) / 2,
            y: rect.y + (rect.height as i32) / 2,
        };
        let hmon = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
        if hmon.is_invalid() {
            return Err(anyhow!("MonitorFromPoint returned null"));
        }

        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut MONITORINFO).as_bool() {
            return Err(anyhow!("GetMonitorInfoW failed"));
        }
        let mon_left = info.monitorInfo.rcMonitor.left;
        let mon_top = info.monitorInfo.rcMonitor.top;

        // szDevice is e.g. "\\.\DISPLAY1". Per-monitor DC bypasses the
        // negative-coord limitation of full-virtual-desktop DCs.
        let device_name = PCWSTR::from_raw(info.szDevice.as_ptr());
        let hdc_screen = CreateDCW(w!("DISPLAY"), device_name, PCWSTR::null(), None);
        if hdc_screen.is_invalid() {
            return Err(anyhow!("CreateDCW failed for monitor"));
        }

        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        let h_bmp = CreateCompatibleBitmap(hdc_screen, w, h);
        let prev = SelectObject(hdc_mem, h_bmp.into());

        // Source coords are monitor-relative since the DC is per-monitor.
        let src_x = rect.x - mon_left;
        let src_y = rect.y - mon_top;
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
        let _ = DeleteDC(hdc_screen);

        if !blt_ok {
            return Err(anyhow!("BitBlt failed"));
        }
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(rect.width, rect.height, buf)
            .ok_or_else(|| anyhow!("ImageBuffer::from_raw failed"))
    }
}
