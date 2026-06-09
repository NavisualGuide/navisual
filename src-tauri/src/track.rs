//! Window position tracker — event-driven (SetWinEventHook).
//!
//! After an element is located, this tracker keeps the overlay aligned with the
//! containing window and in sync with the target's visibility: it auto-hides the
//! pointer when the target gets covered by another app (or minimized) and
//! auto-redraws it when the target is visible again, emitting `pointer_occluded` /
//! `pointer_restored` so the panel can sync its banner.
//!
//! Unlike the previous 200 ms polling loop, this reacts to OS window events
//! (`SetWinEventHook`): foreground/z-order changes, window moves/resizes, popups
//! showing/hiding, and minimize/restore. When nothing on screen moves, no work runs
//! at all. The hook callback runs on a dedicated message-loop thread.

use crate::capture::Rect;
use crate::overlay::{self, OverlayKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, POINT, RECT};
#[cfg(windows)]
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetAncestor, GetMessageW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    KillTimer, SetTimer, TranslateMessage, WindowFromPoint, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GA_ROOT, MSG, WINEVENT_OUTOFCONTEXT,
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
    /// Whether the pointer is currently drawn. Toggled as the target window is
    /// covered/uncovered by another app, or minimized/restored.
    shown: bool,
}

/// Shared state, also reachable from the C `SetWinEventHook` callback (which can't
/// capture environment). Set once when the tracker is created.
static STATE: OnceLock<Arc<Mutex<Option<TrackState>>>> = OnceLock::new();

/// Active one-shot "settle" timer id (0 = none). See `schedule_settle`.
static SETTLE_TIMER: AtomicUsize = AtomicUsize::new(0);

pub struct WindowTracker {
    state: Arc<Mutex<Option<TrackState>>>,
}

impl WindowTracker {
    pub fn new() -> Self {
        let state: Arc<Mutex<Option<TrackState>>> = Arc::new(Mutex::new(None));
        let _ = STATE.set(state.clone());

        #[cfg(windows)]
        std::thread::Builder::new()
            .name("win-tracker".into())
            .spawn(|| unsafe { run_event_thread() })
            .expect("window tracker thread");

        Self { state }
    }

    /// Begin tracking the window that contains `abs_bbox`. `target_hwnd` is the window
    /// the AI/locator was working in; the overlay is anchored to **that app** so the
    /// pointer only ever follows the right window — never another app that happens to
    /// overlap the located point. `initially_shown` is the visibility the caller has
    /// already drawn for the first frame; the tracker maintains it from there.
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

                let mut wr = RECT::default();
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

/// Dedicated thread: register the window-event hooks, then pump messages so the OS
/// delivers `WINEVENT_OUTOFCONTEXT` callbacks to `win_event_proc`.
#[cfg(windows)]
unsafe fn run_event_thread() {
    // Keep the hook handles alive for the life of the thread (process lifetime).
    let _hooks: [HWINEVENTHOOK; 5] = [
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        ),
        SetWinEventHook(
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_SYSTEM_MINIMIZEEND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        ),
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_HIDE,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        ),
        SetWinEventHook(
            EVENT_OBJECT_REORDER,
            EVENT_OBJECT_REORDER,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        ),
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        ),
    ];

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// WinEvent callback (runs on the tracker thread's message loop). Drops the noisy
/// non-window events (cursor/caret/child object location changes) and recomputes the
/// overlay on anything that could move the target or change what's on top of it.
#[cfg(windows)]
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // OBJID_WINDOW (0) + CHILDID_SELF (0): window-level events only.
    if id_object != 0 || id_child != 0 {
        return;
    }
    recompute();
    // Z-order / foreground / show-hide / minimize changes aren't always settled at the
    // instant the event fires (alt-tab to an un-minimizing window, restore animations),
    // and the system can drop OUTOFCONTEXT events during a burst — so re-check shortly
    // after. Moves (LOCATIONCHANGE) are continuous and already settle via the live
    // recompute, so they don't need it.
    if event != EVENT_OBJECT_LOCATIONCHANGE {
        schedule_settle();
    }
}

/// (Re)arm a coalesced one-shot timer that runs `recompute` ~120 ms after the last
/// z-order/foreground event — catching state that hadn't settled when the event fired,
/// and correcting for any events the system dropped during an alt-tab burst. Resetting
/// it on each event means it fires once, just after the dust settles.
#[cfg(windows)]
unsafe fn schedule_settle() {
    let prev = SETTLE_TIMER.swap(0, Ordering::SeqCst);
    if prev != 0 {
        let _ = KillTimer(None, prev);
    }
    let id = SetTimer(None, 0, 120, Some(settle_timer_proc));
    SETTLE_TIMER.store(id, Ordering::SeqCst);
}

#[cfg(windows)]
unsafe extern "system" fn settle_timer_proc(_hwnd: HWND, _msg: u32, id: usize, _time: u32) {
    let _ = KillTimer(None, id);
    let _ = SETTLE_TIMER.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    recompute();
}

/// Re-align the overlay and toggle its visibility against the target window. Same
/// logic the old 200 ms poll ran, now invoked only when an OS event fires.
#[cfg(windows)]
unsafe fn recompute() {
    let Some(arc) = STATE.get() else {
        return;
    };
    let mut guard = match arc.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(s) = guard.as_mut() else {
        return; // no pointer tracked — nothing to do (cheap early-out)
    };

    let hwnd = HWND(s.hwnd as *mut core::ffi::c_void);

    let mut wr = RECT::default();
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

    // Visible only when the target window is neither minimized nor covered by another
    // app at the located spot. (s.hwnd is a window of the target app — anchored in
    // start() — so it's the right occlusion reference.)
    let should_show = !IsIconic(hwnd).as_bool()
        && crate::capture::target_visible_in_rect(
            abs_bbox.x,
            abs_bbox.y,
            abs_bbox.width as i32,
            abs_bbox.height as i32,
            s.hwnd as usize,
        );

    if should_show {
        // Keep the overlay above any transient popup (ribbon dropdown, combo list,
        // tooltip) the user just opened, which Windows would otherwise stack on top.
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
