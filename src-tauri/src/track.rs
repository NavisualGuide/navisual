//! Window position tracker — Phase D.3.
//!
//! After an element is located, this tracker watches the containing window for
//! moves/resizes and keeps the overlay bbox aligned without re-running the full
//! locate pipeline. It also auto-hides the pointer when the target gets covered by
//! another app (or minimized) and auto-redraws it when the target is visible again,
//! emitting `pointer_occluded` / `pointer_restored` so the panel can sync its banner.
//!
//! A 200 ms polling thread is used instead of SetWinEventHook to avoid the
//! message-loop requirement; at 200 ms latency the overlay "snaps" to the
//! window fast enough that users don't notice.

use crate::capture::Rect;
use crate::overlay::{self, OverlayKind};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, POINT};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowRect, GetWindowThreadProcessId, IsIconic, WindowFromPoint, GA_ROOT,
};

struct TrackState {
    hwnd: isize,
    win_left: i32,
    win_top: i32,
    win_width: i32,
    win_height: i32,
    /// Element bbox relative to the window's top-left corner.
    rel_bbox: Rect,
    kind: OverlayKind,
    text: Option<String>,
    app: AppHandle,
    /// Whether the pointer is currently drawn. Toggled by the poll as the target
    /// window is covered/uncovered by another app, or minimized/restored.
    shown: bool,
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

    /// Begin tracking the window that contains `abs_bbox`. `target_hwnd` is the window
    /// the AI/locator was working in; the overlay is anchored to **that app** so the
    /// pointer only ever follows the right window — never another app that happens to
    /// overlap the located point. `kind` and `text` are replayed on move/restore.
    pub fn start(
        &self,
        abs_bbox: &Rect,
        kind: OverlayKind,
        text: Option<String>,
        app: AppHandle,
        target_hwnd: Option<usize>,
        initially_shown: bool,
    ) {
        #[cfg(windows)]
        {
            let result = unsafe {
                let center = POINT {
                    x: abs_bbox.x + abs_bbox.width as i32 / 2,
                    y: abs_bbox.y + abs_bbox.height as i32 / 2,
                };
                // Window under the located point — handles child controls and the
                // target app's own owned dialogs.
                let child = WindowFromPoint(center);
                let mut hwnd = if child.0.is_null() {
                    child
                } else {
                    let root = GetAncestor(child, GA_ROOT);
                    if root.0.is_null() {
                        child
                    } else {
                        root
                    }
                };

                // Anchor to the TARGET app: if WindowFromPoint landed on a window from
                // a DIFFERENT app (the located point is overlapped by another app at the
                // centre), follow the known target window instead — so the overlay never
                // tracks whatever happens to be on top at that pixel.
                if let Some(th) = target_hwnd {
                    let th_hwnd = HWND(th as *mut core::ffi::c_void);
                    let mut th_pid = 0u32;
                    GetWindowThreadProcessId(th_hwnd, Some(&mut th_pid));
                    let mut hit_pid = 0u32;
                    if !hwnd.0.is_null() {
                        GetWindowThreadProcessId(hwnd, Some(&mut hit_pid));
                    }
                    if hwnd.0.is_null() || (th_pid != 0 && hit_pid != th_pid) {
                        hwnd = th_hwnd;
                    }
                }

                if hwnd.0.is_null() {
                    return;
                }

                let mut wr = windows::Win32::Foundation::RECT::default();
                if GetWindowRect(hwnd, &mut wr).is_err() {
                    return;
                }
                (
                    hwnd.0 as isize,
                    wr.left,
                    wr.top,
                    wr.right - wr.left,
                    wr.bottom - wr.top,
                )
            };

            let (hwnd, win_left, win_top, win_width, win_height) = result;
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
                win_width,
                win_height,
                rel_bbox,
                kind,
                text,
                app,
                shown: initially_shown,
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (abs_bbox, kind, text, app, target_hwnd, initially_shown);
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
            let mut wr = windows::Win32::Foundation::RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                // Window gone — hide the pointer if it was showing.
                if s.shown {
                    if let Ok(u) = overlay::make_update(OverlayKind::None, None, None) {
                        let _ = overlay::emit_update(&s.app, u);
                    }
                    s.shown = false;
                    let _ = s.app.emit("pointer_occluded", ());
                }
                return;
            }

            let new_w = wr.right - wr.left;
            let new_h = wr.bottom - wr.top;
            let moved = wr.left != s.win_left || wr.top != s.win_top;
            let resized = new_w != s.win_width || new_h != s.win_height;
            s.win_left = wr.left;
            s.win_top = wr.top;
            s.win_width = new_w;
            s.win_height = new_h;

            let abs_bbox = Rect {
                x: wr.left + s.rel_bbox.x,
                y: wr.top + s.rel_bbox.y,
                width: s.rel_bbox.width,
                height: s.rel_bbox.height,
            };

            // The pointer should show only when the target window is neither minimized
            // nor covered by another app at the located spot. (s.hwnd is a window of
            // the target app — anchored in start() — so this is the right occlusion ref.)
            let should_show = !IsIconic(hwnd).as_bool()
                && crate::capture::target_visible_in_rect(
                    abs_bbox.x,
                    abs_bbox.y,
                    abs_bbox.width as i32,
                    abs_bbox.height as i32,
                    s.hwnd as usize,
                );

            if should_show {
                // Keep the overlay above any transient popup (ribbon dropdown, combo
                // list, tooltip) the user just opened, which Windows would otherwise
                // stack on top.
                crate::capture::raise_overlay_topmost();
                if !s.shown || moved || resized {
                    if let Ok(u) = overlay::make_update(s.kind, Some(abs_bbox), s.text.clone()) {
                        let _ = overlay::emit_update(&s.app, u);
                    }
                    if !s.shown {
                        s.shown = true;
                        let _ = s.app.emit("pointer_restored", ());
                    }
                }
            } else if s.shown {
                if let Ok(u) = overlay::make_update(OverlayKind::None, None, None) {
                    let _ = overlay::emit_update(&s.app, u);
                }
                s.shown = false;
                let _ = s.app.emit("pointer_occluded", ());
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = state;
    }
}
