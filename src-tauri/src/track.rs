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
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetAncestor, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindow, KillTimer, RegisterClassExW, SetTimer,
    TranslateMessage, WindowFromPoint, CW_USEDEFAULT, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GA_ROOT, MSG, WINDOW_EX_STYLE,
    WINDOW_STYLE, WINEVENT_OUTOFCONTEXT, WM_DISPLAYCHANGE, WNDCLASSEXW,
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

/// Active one-shot "display settle" timer id (0 = none). See `display_watch_wndproc`.
/// Separate from `SETTLE_TIMER` — different trigger, different (longer) delay, and the
/// two must never cancel each other if they happen to be armed at the same time.
static DISPLAY_SETTLE_TIMER: AtomicUsize = AtomicUsize::new(0);

/// How long to wait after `WM_DISPLAYCHANGE` before re-reading monitor topology. The
/// message can fire before Windows has fully finished re-registering a just-reconnected
/// monitor (EDID renegotiation, driver reinit) — reacting synchronously risked reading an
/// incomplete monitor list and sizing the overlay for only part of the true virtual
/// desktop. Not yet confirmed as the exact mechanism behind a live "squeezed to half"
/// report (2026-07-08) — this is the leading hypothesis, paired with unconditional
/// logging in `overlay::virtual_desktop_rect`/`configure` so the next occurrence has
/// concrete numbers to check against rather than more speculation.
const DISPLAY_SETTLE_DELAY_MS: u32 = 500;

/// When the last `WM_DISPLAYCHANGE` was received (`None` = none yet this session). Lets
/// `recompute` log its decision for *every* call — not just the forced, display-settle-
/// triggered one — for a short window after a real display change, so a natural
/// WinEvent-driven recompute (e.g. the target window's own un-minimize/move as Windows
/// finishes relocating it, which happens on its own timeline, not ours) is visible too.
/// Diagnostic-only; doesn't affect `force`, which is a separate, narrower concern (see
/// `recompute`'s doc comment).
static LAST_DISPLAY_CHANGE: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
/// How long after a display change to keep logging every recompute call's decision.
/// Generous on purpose — live-observed the target app's own OS-driven settling
/// (minimize/move/restore/maximize as it's relocated to the surviving monitor) can take
/// several seconds, well past DISPLAY_SETTLE_DELAY_MS itself.
const DISPLAY_CHANGE_LOG_WINDOW: Duration = Duration::from_secs(15);

#[cfg(windows)]
fn mark_display_change_for_logging() {
    let cell = LAST_DISPLAY_CHANGE.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap() = Some(Instant::now());
}

#[cfg(windows)]
fn recently_had_display_change() -> bool {
    let Some(cell) = LAST_DISPLAY_CHANGE.get() else {
        return false;
    };
    match *cell.lock().unwrap() {
        Some(t) => t.elapsed() < DISPLAY_CHANGE_LOG_WINDOW,
        None => false,
    }
}

/// The AppBoundary flash (see `lib.rs::announce_shared_app`) plays out as a
/// one-shot, client-timed animation with no ongoing backend involvement — so
/// if its window is minimized or closed mid-flash, the box would otherwise
/// keep animating over whatever the user switched to. This watch lets the
/// same event hook that already services the pointer overlay also clear a
/// live boundary flash early.
struct BoundaryWatch {
    hwnd: isize,
    app: AppHandle,
    expires_at: Instant,
}

static BOUNDARY_WATCH: OnceLock<Mutex<Option<BoundaryWatch>>> = OnceLock::new();

/// Arm (or replace) the boundary watch for `hwnd`. Call right after emitting
/// the AppBoundary draw update. `duration` must match the frontend's
/// `APP_BOUNDARY_DURATION_MS` (Overlay.svelte) — there's no constant shared
/// across the Rust/Svelte boundary, so keep the two in sync by hand.
pub fn watch_boundary(hwnd: usize, app: AppHandle, duration: Duration) {
    #[cfg(windows)]
    {
        let cell = BOUNDARY_WATCH.get_or_init(|| Mutex::new(None));
        *cell.lock().unwrap() = Some(BoundaryWatch {
            hwnd: hwnd as isize,
            app,
            expires_at: Instant::now() + duration,
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, app, duration);
    }
}

/// Called unconditionally from `win_event_proc` on every qualifying window
/// event (cheap early-out when nothing is watched or `hwnd` isn't it). If
/// `hwnd` is the window a boundary flash is currently watching, re-derives
/// its live state directly — same philosophy as `recompute()` below, rather
/// than trusting any one event code's semantics — and clears the flash early
/// if the window was closed (`!IsWindow`) or minimized (`IsIconic`). An
/// AppBoundary update with a `None` bbox is the existing clear signal; see
/// `announce_shared_app`'s no-window branch.
#[cfg(windows)]
unsafe fn clear_boundary_if_gone(hwnd: HWND) {
    let Some(cell) = BOUNDARY_WATCH.get() else {
        return;
    };
    let Ok(mut guard) = cell.lock() else {
        return;
    };
    let Some(watch) = guard.as_ref() else {
        return;
    };
    if watch.hwnd != hwnd.0 as isize {
        return;
    }
    if Instant::now() >= watch.expires_at {
        // Already finished its own client-side animation — nothing to clear.
        *guard = None;
        return;
    }
    if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
        if let Ok(u) = overlay::make_update(OverlayKind::AppBoundary, None, None) {
            let _ = overlay::emit_update(&watch.app, u);
        }
        *guard = None;
    }
}

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

/// Dedicated thread: register the window-event hooks, create the hidden
/// `WM_DISPLAYCHANGE` watch window, then pump messages so the OS delivers
/// `WINEVENT_OUTOFCONTEXT` callbacks to `win_event_proc` and window messages to
/// `display_watch_wndproc`.
#[cfg(windows)]
unsafe fn run_event_thread() {
    create_display_watch_window();

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

/// Create a genuinely invisible (never shown) top-level window purely to receive
/// `WM_DISPLAYCHANGE`. This message is broadcast by the OS to top-level windows when the
/// display configuration changes (monitor plugged/unplugged, resolution changed) — a
/// message-only window (`HWND_MESSAGE` parent) does *not* qualify to receive it, so a real,
/// if invisible, top-level window is required. Created on this thread so the existing
/// `GetMessageW`/`DispatchMessageW` pump above (already running for the WinEvent hooks)
/// delivers its messages too, with no separate thread or second message loop needed.
/// Never destroyed — lives for the process's lifetime, same as the hook handles.
#[cfg(windows)]
unsafe fn create_display_watch_window() {
    let class_name: Vec<u16> = "NavisualDisplayWatch\0".encode_utf16().collect();
    let hinstance = GetModuleHandleW(None).unwrap_or_default();

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(display_watch_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    if RegisterClassExW(&wc) == 0 {
        log::warn!("display-watch: RegisterClassExW failed");
        return;
    }

    // No WS_VISIBLE, no parent (None) -> a real top-level window that's never shown.
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class_name.as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(0),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        0,
        0,
        None,
        None,
        Some(hinstance.into()),
        None,
    );
    match hwnd {
        Ok(h) if !h.0.is_null() => log::info!("display-watch: window created {:#x}", h.0 as usize),
        _ => log::warn!("display-watch: CreateWindowExW failed"),
    }
}

/// WNDPROC for the hidden display-watch window. `WM_DISPLAYCHANGE` arms a debounced
/// one-shot reconfigure (`schedule_display_settle`) rather than acting immediately — see
/// `DISPLAY_SETTLE_DELAY_MS`'s doc comment for why. Everything else passes straight to the
/// default handler.
#[cfg(windows)]
unsafe extern "system" fn display_watch_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        log::info!("display-watch: WM_DISPLAYCHANGE received, scheduling reconfigure");
        mark_display_change_for_logging();
        schedule_display_settle();
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// (Re)arm a coalesced one-shot timer that reconfigures the overlay
/// `DISPLAY_SETTLE_DELAY_MS` after the last `WM_DISPLAYCHANGE` — Windows can send several
/// in quick succession while settling a topology change (e.g. one per monitor), so this
/// resets on each and fires once, after the dust settles. Mirrors `schedule_settle`'s
/// coalescing shape but is a distinct timer: different trigger, different delay, must not
/// cancel or be cancelled by it.
#[cfg(windows)]
unsafe fn schedule_display_settle() {
    let prev = DISPLAY_SETTLE_TIMER.swap(0, Ordering::SeqCst);
    if prev != 0 {
        let _ = KillTimer(None, prev);
    }
    let id = SetTimer(
        None,
        0,
        DISPLAY_SETTLE_DELAY_MS,
        Some(display_settle_timer_proc),
    );
    DISPLAY_SETTLE_TIMER.store(id, Ordering::SeqCst);
}

#[cfg(windows)]
unsafe extern "system" fn display_settle_timer_proc(_hwnd: HWND, _msg: u32, id: usize, _time: u32) {
    let _ = KillTimer(None, id);
    let _ = DISPLAY_SETTLE_TIMER.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    if let Some(app) = crate::APP_HANDLE.get() {
        crate::overlay::reconfigure(app);
    }
    // reconfigure() only resizes/repositions the overlay's own OS window — it has no way
    // to know what's currently being tracked, so whatever was already drawn stays exactly
    // as it was, now potentially misaligned with (or entirely outside) the just-changed
    // window bounds. force=true is load-bearing here, not just belt-and-suspenders: the
    // target window itself very often does NOT move or resize during a monitor topology
    // change, so recompute(false)'s normal `moved || resized` gate would silently emit
    // nothing at all, leaving the frontend on stale virtual_origin/virtual_size even
    // though recompute ran. Root-caused live 2026-07-08 from exactly this symptom (an
    // already-shown pointer going invisible, or landing on the wrong screen distorted,
    // after a monitor was unplugged/replugged with the target app's own position
    // unchanged) after two earlier attempts (WM_DISPLAYCHANGE handling, then an
    // unforced recompute) both still reproduced it.
    recompute(true);
    if let Some(app) = crate::APP_HANDLE.get() {
        crate::refresh_active_window(app);
    }
}

/// WinEvent callback (runs on the tracker thread's message loop). Drops the noisy
/// non-window events (cursor/caret/child object location changes) and recomputes the
/// overlay on anything that could move the target or change what's on top of it.
#[cfg(windows)]
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // OBJID_WINDOW (0) + CHILDID_SELF (0): window-level events only.
    if id_object != 0 || id_child != 0 {
        return;
    }
    clear_boundary_if_gone(hwnd);
    recompute(false);
    // Z-order / foreground / show-hide / minimize changes aren't always settled at the
    // instant the event fires (alt-tab to an un-minimizing window, restore animations),
    // and the system can drop OUTOFCONTEXT events during a burst — so re-check shortly
    // after. Moves (LOCATIONCHANGE) are continuous and already settle via the live
    // recompute, so they don't need it.
    if event != EVENT_OBJECT_LOCATIONCHANGE {
        if let Some(app) = crate::APP_HANDLE.get() {
            crate::refresh_active_window(app);
        }
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
    recompute(false);
    if let Some(app) = crate::APP_HANDLE.get() {
        crate::refresh_active_window(app);
    }
}

/// Re-align the overlay and toggle its visibility against the target window. Same
/// logic the old 200 ms poll ran, now invoked only when an OS event fires.
///
/// `force`: `moved`/`resized` below only compare the *target window's own* rect against
/// what's cached — they say nothing about whether the overlay's virtual-desktop
/// coordinate space itself changed. A monitor topology change can leave the target
/// window's own position untouched while `virtual_origin`/`virtual_size` (what the
/// frontend needs to place `abs_bbox` correctly) changes underneath it — in that case
/// the normal gate silently skips emitting anything, leaving the frontend on stale
/// coordinates (live-confirmed 2026-07-08: an already-shown pointer went invisible or
/// rendered on the wrong screen, distorted, after a monitor was unplugged/replugged,
/// because nothing re-sent it fresh virtual_origin/virtual_size). `force: true` bypasses
/// `moved || resized` and emits whenever `should_show` holds, regardless. Normal
/// WinEvent-driven calls pass `force: false`, preserving the original "only emit when
/// something visually relevant changed" behaviour.
#[cfg(windows)]
unsafe fn recompute(force: bool) {
    // Widen logging to cover natural (non-forced) calls too, for a while after a real
    // display change. The one forced call per display-settle cycle only captures a single
    // instant; live testing (2026-07-09) showed the target window's own OS-driven settling
    // (un-minimize/move/maximize as Windows relocates it to a surviving monitor) happens on
    // its own timeline afterward, via ordinary WinEvent-triggered `recompute(false)` calls —
    // which this function previously had zero visibility into.
    let log_this = force || recently_had_display_change();
    let Some(arc) = STATE.get() else {
        if log_this {
            log::info!("recompute(force={force}): STATE not initialised, bailing");
        }
        return;
    };
    let mut guard = match arc.lock() {
        Ok(g) => g,
        Err(_) => {
            if log_this {
                log::info!("recompute(force={force}): STATE lock poisoned, bailing");
            }
            return;
        }
    };
    let Some(s) = guard.as_mut() else {
        // no pointer tracked — nothing to do (cheap early-out). Logged under log_this since
        // this is the exact branch that would explain "still nothing shown after unplug"
        // if, for whatever reason, no pointer was actually being tracked at that moment.
        if log_this {
            log::info!("recompute(force={force}): no pointer currently tracked, nothing to do");
        }
        return;
    };

    let hwnd = HWND(s.hwnd as *mut core::ffi::c_void);

    let mut wr = RECT::default();
    if GetWindowRect(hwnd, &mut wr).is_err() {
        // Window gone — hide the pointer if it was showing.
        if log_this {
            log::info!("recompute(force={force}): GetWindowRect failed for target hwnd {:#x} — treating as gone", s.hwnd);
        }
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
    let iconic = IsIconic(hwnd).as_bool();
    let visible = crate::capture::target_visible_in_rect(
        abs_bbox.x,
        abs_bbox.y,
        abs_bbox.width as i32,
        abs_bbox.height as i32,
        s.hwnd as usize,
    );
    let should_show = !iconic && visible;

    if log_this {
        log::info!(
            "recompute(force={force}): target_rect=({},{},{}x{}) abs_bbox=({},{},{}x{}) iconic={} visible={} should_show={} shown={} moved={} resized={}",
            wr.left, wr.top, new_w, new_h,
            abs_bbox.x, abs_bbox.y, abs_bbox.width, abs_bbox.height,
            iconic, visible, should_show, s.shown, moved, resized,
        );
    }

    if should_show {
        // Keep the overlay above any transient popup (ribbon dropdown, combo list,
        // tooltip) the user just opened, which Windows would otherwise stack on top.
        crate::capture::raise_overlay_topmost();
        if !s.shown || moved || resized || force {
            match overlay::make_update(s.kind, Some(abs_bbox), s.text.clone()) {
                Ok(u) => {
                    if log_this {
                        log::info!(
                            "recompute(force={force}): emitting kind={:?} bbox={:?} virtual_origin={:?} virtual_size={:?}",
                            u.kind, u.bbox, u.virtual_origin, u.virtual_size
                        );
                    }
                    match overlay::emit_update(&s.app, u) {
                        Ok(()) => {
                            if log_this {
                                log::info!("recompute(force={force}): emit_update succeeded");
                            }
                        }
                        Err(e) => log::warn!("recompute(force={force}): emit_update failed: {e}"),
                    }
                }
                Err(e) => log::warn!("recompute(force={force}): make_update failed: {e}"),
            }
            if !s.shown {
                s.shown = true;
                let _ = s.app.emit("pointer_restored", ());
            }
        } else if log_this {
            log::info!(
                "recompute(force={force}): should_show=true but not re-emitting (shown={}, moved={moved}, resized={resized}) — pointer stays as-is",
                s.shown
            );
        }
    } else if s.shown {
        if log_this {
            log::info!("recompute(force={force}): should_show=false, hiding previously-shown pointer");
        }
        if let Ok(u) = overlay::make_update(OverlayKind::None, None, None) {
            let _ = overlay::emit_update(&s.app, u);
        }
        s.shown = false;
        let _ = s.app.emit("pointer_occluded", ());
    } else if log_this {
        log::info!("recompute(force={force}): should_show=false, already hidden — no-op");
    }
}
