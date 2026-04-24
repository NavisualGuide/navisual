//! Window position tracker — Phase D.3.
//!
//! After an element is located, this tracker watches the containing window for
//! moves/resizes and keeps the overlay bbox aligned without re-running the full
//! locate pipeline.  On minimize it clears the overlay; on restore it re-shows
//! at the new position.
//!
//! A 200 ms polling thread is used instead of SetWinEventHook to avoid the
//! message-loop requirement; at 200 ms latency the overlay "snaps" to the
//! window fast enough that users don't notice.

use crate::capture::Rect;
use crate::overlay::{self, OverlayKind};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, POINT};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowRect, IsIconic, WindowFromPoint, GA_ROOT,
};

struct TrackState {
    hwnd: isize,
    win_left: i32,
    win_top: i32,
    /// Element bbox relative to the window's top-left corner.
    rel_bbox: Rect,
    kind: OverlayKind,
    text: Option<String>,
    app: AppHandle,
    was_minimized: bool,
}

pub struct WindowTracker {
    state: Arc<Mutex<Option<TrackState>>>,
}

impl WindowTracker {
    pub fn new() -> Self {
        let state: Arc<Mutex<Option<TrackState>>> = Arc::new(Mutex::new(None));
        let state_clone = state.clone();

        std::thread::Builder::new()
            .name("win-tracker".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(200));
                poll_once(&state_clone);
            })
            .expect("window tracker thread");

        Self { state }
    }

    /// Begin tracking the window that contains `abs_bbox`.
    /// `kind` and `text` are replayed when the window moves or is restored.
    pub fn start(
        &self,
        abs_bbox: &Rect,
        kind: OverlayKind,
        text: Option<String>,
        app: AppHandle,
    ) {
        #[cfg(windows)]
        {
            let result = unsafe {
                let center = POINT {
                    x: abs_bbox.x + abs_bbox.width as i32 / 2,
                    y: abs_bbox.y + abs_bbox.height as i32 / 2,
                };
                let child = WindowFromPoint(center);
                if child.0.is_null() {
                    return;
                }
                let root = GetAncestor(child, GA_ROOT);
                let hwnd = if root.0.is_null() { child } else { root };

                let mut wr = windows::Win32::Foundation::RECT::default();
                if GetWindowRect(hwnd, &mut wr).is_err() {
                    return;
                }
                (hwnd.0 as isize, wr.left, wr.top)
            };

            let (hwnd, win_left, win_top) = result;
            let rel_bbox = Rect {
                x: abs_bbox.x - win_left,
                y: abs_bbox.y - win_top,
                width: abs_bbox.width,
                height: abs_bbox.height,
            };

            *self.state.lock().unwrap() = Some(TrackState {
                hwnd,
                win_left,
                win_top,
                rel_bbox,
                kind,
                text,
                app,
                was_minimized: false,
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (abs_bbox, kind, text, app);
        }
    }

    /// Stop tracking and suppress overlay updates.
    pub fn clear(&self) {
        *self.state.lock().unwrap() = None;
    }
}

fn poll_once(state: &Mutex<Option<TrackState>>) {
    #[cfg(windows)]
    {
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let s = match guard.as_mut() {
            Some(s) => s,
            None => return,
        };

        let hwnd = HWND(s.hwnd as *mut core::ffi::c_void);

        unsafe {
            let minimized = IsIconic(hwnd).as_bool();

            if minimized && !s.was_minimized {
                s.was_minimized = true;
                if let Ok(u) = overlay::make_update(OverlayKind::None, None, None) {
                    let _ = overlay::emit_update(&s.app, u);
                }
                return;
            }
            if minimized {
                return;
            }

            let mut wr = windows::Win32::Foundation::RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return;
            }

            let restored = s.was_minimized;
            s.was_minimized = false;

            if wr.left != s.win_left || wr.top != s.win_top || restored {
                s.win_left = wr.left;
                s.win_top = wr.top;

                let new_bbox = Rect {
                    x: wr.left + s.rel_bbox.x,
                    y: wr.top + s.rel_bbox.y,
                    width: s.rel_bbox.width,
                    height: s.rel_bbox.height,
                };

                let kind = s.kind;
                let text = s.text.clone();
                if let Ok(u) = overlay::make_update(kind, Some(new_bbox), text) {
                    let _ = overlay::emit_update(&s.app, u);
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = state;
    }
}
