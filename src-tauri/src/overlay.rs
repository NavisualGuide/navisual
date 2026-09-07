//! Transparent click-through overlay window — Phase D.1.
//!
//! The overlay is a second Tauri `WebviewWindow` (label `"overlay"`) that:
//! - covers the entire virtual desktop (union of all monitors),
//! - is always-on-top, undecorated, transparent,
//! - has `WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW` applied so
//!   input clicks pass through to whatever app is underneath.
//!
//! The window's HTML/canvas is rendered by the Svelte route `/overlay`.
//! Rust emits an `overlay:update` event whenever the target changes; the
//! frontend consumes it to redraw the arrow/box/subtitle.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::capture::Rect;

/// An overlay command. Mirrors the v0.3 Python `overlay.py` primitives so
/// the Svelte canvas renderer can match it one-for-one.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    /// Arrow pointing at bbox from the nearest panel edge.
    Arrow,
    /// Rounded highlight box around bbox.
    Box,
    /// Subtitle strip along the bottom of the active screen.
    Subtitle,
    /// Phase 0.2: outline the captured app's window with a flash + fade
    /// animation so the user can see exactly what's being shared.
    AppBoundary,
    /// AI-bbox fallback when A11y/OCR both missed but the AI returned a
    /// `target_bbox`. Rendered as a soft diffuse highlight at the inflated
    /// AI bbox — a "look around here" cue, not a precise pointer.
    Hint,
    /// Flow A (candidate hints): after "Wrong spot", 2–3 ranked possibilities
    /// drawn as numbered boxes (`OverlayUpdate.candidates`, strongest first).
    /// The user is never asked to choose — their next real click in the app
    /// resolves it (state-readback labeling). `bbox` = the primary candidate.
    Candidates,
    /// No draw — clears the overlay.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverlayUpdate {
    pub kind: OverlayKind,
    /// Target bbox in virtual-desktop physical pixels, or None for subtitle-only.
    pub bbox: Option<Rect>,
    /// Optional subtitle / instruction text.
    pub text: Option<String>,
    /// Virtual-desktop origin + size so the renderer can convert bbox
    /// coords into overlay-window-relative coords without needing the
    /// Tauri position API (which may lag behind).
    pub virtual_origin: (i32, i32),
    pub virtual_size: (u32, u32),
    /// The monitor the target element lives on (virtual-desktop physical pixels).
    /// Used to confine the subtitle strip to a single screen.
    pub active_screen: Option<Rect>,
    /// That monitor's WORK AREA — the monitor minus the taskbar. The caption is
    /// anchored to the bottom of this, not of `active_screen`, so it sits above
    /// the taskbar instead of across its icons (live report 2026-09-07). With an
    /// auto-hide taskbar the two rects coincide, so nothing is lost there.
    pub work_area: Option<Rect>,
    /// AI-returned bounding box in virtual-desktop physical pixels.
    /// Drawn as a distinct cyan-dashed box alongside the production pointer
    /// when the developer "Show AI bbox" toggle is enabled.
    pub ai_bbox: Option<Rect>,
    /// Candidate boxes for `OverlayKind::Candidates` (virtual-desktop physical
    /// pixels, ranked strongest-first). Empty for every other kind.
    pub candidates: Vec<Rect>,
}

/// Find which monitor contains the centre of `bbox`. When `bbox` is `None`
/// (subtitle-only step, app-boundary clear, etc.), reuse the last-known
/// active screen instead of jumping to monitor-nearest-origin — otherwise the
/// subtitle visibly shifts between monitors mid-session.
fn active_screen_for_bbox(bbox: Option<&Rect>) -> Option<Rect> {
    let monitors = crate::capture::enumerate_monitor_rects();
    if monitors.is_empty() {
        return None;
    }

    if let Some(b) = bbox {
        let cx = b.x + (b.width as i32) / 2;
        let cy = b.y + (b.height as i32) / 2;
        for m in &monitors {
            if cx >= m.x && cx < m.x + m.width as i32 && cy >= m.y && cy < m.y + m.height as i32 {
                *LAST_ACTIVE_SCREEN
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .unwrap() = Some(*m);
                return Some(*m);
            }
        }
    }

    // No bbox (or bbox off-screen) — reuse the last-known active screen so the
    // subtitle stays on the monitor the user is working on.
    if let Some(cached) = *LAST_ACTIVE_SCREEN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
    {
        return Some(cached);
    }

    // First-ever call with no bbox: fall back to the monitor closest to (0, 0).
    monitors
        .iter()
        .min_by_key(|m| m.x.abs() + m.y.abs())
        .copied()
}

/// Last monitor a bbox-bearing OverlayUpdate landed on. Used to stabilise the
/// subtitle position across subtitle-only / app-boundary / clear emits.
static LAST_ACTIVE_SCREEN: OnceLock<Mutex<Option<Rect>>> = OnceLock::new();

struct CachedVd {
    rect: Rect,
    at: Instant,
}

static VD_CACHE: OnceLock<Mutex<Option<CachedVd>>> = OnceLock::new();

/// Compute the union rect of all monitors, cached for 30 s.
/// Monitor topology changes are extremely rare; re-enumerating on every
/// 200 ms window-tracker tick adds latency unnecessarily. The underlying
/// `enumerate_monitor_rects` call is sub-millisecond, but the cache keeps
/// the hot path allocation-free.
pub fn virtual_desktop_rect() -> Result<Rect> {
    let cache = VD_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some(ref c) = *guard {
        if c.at.elapsed() < Duration::from_secs(30) {
            return Ok(c.rect);
        }
    }
    let monitors = crate::capture::enumerate_monitor_rects();
    if monitors.is_empty() {
        return Err(anyhow!("no monitors found"));
    }
    // Logged unconditionally (cache misses only happen on startup + every 30s + right
    // after a display-change reconfigure, so this isn't hot-path noise) — the per-monitor
    // list is what actually distinguishes "the OS hadn't finished re-registering a
    // just-reconnected monitor yet" from any other failure mode further down the pipeline.
    log::info!("virtual_desktop_rect: {} monitor(s): {:?}", monitors.len(), monitors);
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        min_x = min_x.min(m.x);
        min_y = min_y.min(m.y);
        max_x = max_x.max(m.x + m.width as i32);
        max_y = max_y.max(m.y + m.height as i32);
    }
    let rect = Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(0) as u32,
        height: (max_y - min_y).max(0) as u32,
    };
    *guard = Some(CachedVd {
        rect,
        at: Instant::now(),
    });
    Ok(rect)
}

/// Size & position the overlay to span the virtual desktop, and apply
/// click-through via Tauri's built-in API (which correctly propagates to
/// the WebView2 child HWND — raw SetWindowLongPtrW on the outer HWND alone
/// does not prevent WebView2 from capturing input).
///
/// CRITICAL: click-through must succeed before show(). A fullscreen
/// transparent window that still captures input freezes the desktop.
/// Two-key gate on ever making the overlay window visible.
///
/// The overlay is transparent, always-on-top and sized to the whole virtual
/// desktop, and `raise_overlay_topmost` re-asserts `HWND_TOPMOST` on a timer --
/// so it outranks even Task Manager. If its webview fails to load, that
/// invisible canvas becomes an OPAQUE browser error page covering every monitor
/// with no way out. Reported live 2026-09-07 after a dev server died under a
/// running app: escaping it needed Win+Tab, because Task Manager itself came up
/// underneath the overlay.
///
/// It used to be shown unconditionally 2s into `setup()`, which assumed the page
/// behind it had loaded. Now both halves must be known good -- geometry applied
/// on this side, and the overlay's own script alive on the other -- and whichever
/// lands last does the showing.
///
/// There is deliberately NO timeout fallback. "Show it anyway" is exactly the
/// behaviour that cost a user both screens. A page that never loads leaves the
/// overlay hidden and says so loudly in the log: that costs the guidance visuals
/// for the session, which is strictly less than costing the desktop.
static OVERLAY_CONFIGURED: AtomicBool = AtomicBool::new(false);
static OVERLAY_SCRIPT_ALIVE: AtomicBool = AtomicBool::new(false);

/// Geometry and window styles have been applied.
pub fn mark_configured(window: &WebviewWindow) {
    OVERLAY_CONFIGURED.store(true, Ordering::SeqCst);
    show_if_safe(window);
}

/// The overlay's own script reached `onMount`, so the page really loaded.
/// Idempotent: a dev-server reload calls this again and `show()` is a no-op.
pub fn mark_script_alive(window: &WebviewWindow) {
    OVERLAY_SCRIPT_ALIVE.store(true, Ordering::SeqCst);
    show_if_safe(window);
}

/// True once the overlay has actually been shown -- used by the startup watchdog
/// so a page that never loads is a log line rather than silence.
pub fn is_shown() -> bool {
    OVERLAY_CONFIGURED.load(Ordering::SeqCst) && OVERLAY_SCRIPT_ALIVE.load(Ordering::SeqCst)
}

fn show_if_safe(window: &WebviewWindow) {
    if !is_shown() {
        return;
    }
    match window.show() {
        Ok(()) => log::info!("overlay shown (configured + script alive)"),
        Err(e) => log::error!("overlay show failed: {e}"),
    }
}

pub fn configure(window: &WebviewWindow) -> Result<()> {
    // Use Tauri's API — it handles WebView2's child HWND correctly.
    window
        .set_ignore_cursor_events(true)
        .map_err(|e| anyhow!("set_ignore_cursor_events: {e}"))?;

    // Re-assert "keep this out of the taskbar" every time, not just at creation.
    //
    // On Windows `skipTaskbar` is a ONE-SHOT `ITaskbarList::DeleteTab`: it removes the
    // button that exists at that moment and changes no window style. The overlay is in
    // fact created WITH `WS_EX_APPWINDOW` (measured: EXSTYLE 0x000C0138), the explicit
    // "force this window onto the taskbar" flag — so anything that makes the shell
    // re-register the button brings it straight back, titled "Tauri App" before this
    // window was given a title of its own. Reported live as a phantom second entry
    // beside Navisual that vanished again on its own.
    //
    // Setting WS_EX_TOOLWINDOW directly was tried first and does NOT hold: tao keeps its
    // own WindowFlags model and re-applies it, reverting the external style write within
    // the same second (observed twice in one session). So this goes through the
    // supported API instead. `configure` is the right home for it because it runs at
    // startup AND on every realign — including the realign that follows the overlay
    // restoring itself from a minimize, which is precisely when the button comes back.
    if let Err(e) = window.set_skip_taskbar(true) {
        log::warn!("overlay set_skip_taskbar failed: {e}");
    }

    // Size to virtual desktop — best-effort; failure means overlay is
    // mispositioned but still click-through and safe to show.
    match virtual_desktop_rect() {
        Ok(rect) => {
            // Logged unconditionally (not just on error) so a live monitor-topology
            // test has concrete numbers to check against — the computed rect is the
            // one fact needed to tell "wrong topology was read" apart from "topology
            // was read correctly but something downstream didn't apply it right".
            log::info!(
                "overlay configure: applying rect x={} y={} w={} h={}",
                rect.x,
                rect.y,
                rect.width,
                rect.height
            );
            if let Err(e) = window.set_position(PhysicalPosition::new(rect.x, rect.y)) {
                log::warn!("overlay set_position failed: {e}");
            }
            if let Err(e) = window.set_size(PhysicalSize::new(rect.width, rect.height)) {
                log::warn!("overlay set_size failed: {e}");
            }
        }
        Err(e) => log::warn!("overlay virtual_desktop_rect failed: {e}"),
    }

    Ok(())
}

/// Drop the cached virtual-desktop rect so the next `virtual_desktop_rect()` call
/// re-enumerates monitors instead of returning up-to-30s-stale data. Called from
/// `reconfigure` below so a real display-configuration change is reflected immediately
/// rather than waiting out the cache.
fn invalidate_virtual_desktop_cache() {
    let cache = VD_CACHE.get_or_init(|| Mutex::new(None));
    *cache.lock().unwrap() = None;
}

/// Re-run `configure` against the live overlay window and current monitor topology.
///
/// `configure` itself only ever runs once, ~2s after app startup (`lib.rs` setup) — nothing
/// previously re-ran it, so the overlay window's *physical* OS-level position and size stayed
/// permanently fixed to whatever topology existed at that moment. Plugging/unplugging a
/// monitor during the session left the window misaligned with the real desktop for the rest
/// of the run (pointer/box invisible, or clipped to stale bounds) — confirmed live 2026-07-07.
/// `track.rs` calls this in response to the OS's `WM_DISPLAYCHANGE` message, which is the
/// correct, immediate signal for "the display configuration just changed."
pub fn reconfigure(app: &AppHandle) {
    invalidate_virtual_desktop_cache();
    let Some(window) = app.get_webview_window("overlay") else {
        log::warn!("overlay reconfigure: overlay window not found");
        return;
    };

    // Windows minimizes windows anchored to a monitor that just disconnected — and the
    // overlay spans the *whole* virtual desktop, so it's exactly the kind of window this
    // policy targets on a primary-monitor unplug (live-suspected 2026-07-09: Krita's
    // overlay pointer never appeared for the ~25s the primary was unplugged despite
    // `recompute` correctly computing should_show=true and emit_update succeeding
    // throughout — consistent with the OS-level window being iconic the whole time, so
    // nothing was ever actually painted regardless of what got sent to it).
    // `set_position`/`set_size` below do NOT undo this — they update the window's
    // "restored" bounds but a minimized window stays invisible until explicitly restored.
    match window.is_minimized() {
        Ok(true) => {
            log::info!("overlay reconfigure: window was minimized — restoring");
            if let Err(e) = window.unminimize() {
                log::warn!("overlay unminimize failed: {e}");
            }
        }
        Ok(false) => {}
        Err(e) => log::warn!("overlay is_minimized check failed: {e}"),
    }

    match configure(&window) {
        Ok(()) => log::info!("overlay reconfigured after display change"),
        Err(e) => log::warn!("overlay reconfigure failed: {e}"),
    }
}

/// Restore the single invariant the overlay's coordinate mapping depends on:
/// **the overlay window's physical rect must equal the virtual desktop.**
///
/// The canvas backing store is sized in virtual-desktop pixels and everything is
/// drawn in that space, while the canvas CSS box is 100 % of the overlay window —
/// so the browser stretches one onto the other. Let the two diverge and every
/// drawn coordinate is silently scaled by `window / vd` and offset by the
/// difference of their origins. Nothing downstream can detect it: the pointer is
/// simply in the wrong place, and the geometry all the way up the pipeline is
/// still perfectly correct.
///
/// Windows refits a desktop-spanning top-level window on far more occasions than
/// a trigger list has ever managed to enumerate. `WM_DISPLAYCHANGE` was wired up
/// in v0.7.2 for monitor plug/unplug; **hiding the taskbar was not**, and measured
/// live it moves the overlay from `-1920,0 3840x1080` to `-8,-8 1936x1096` —
/// refitted onto the primary monitor — and never puts it back, so one toggle
/// squashes the pointer horizontally (0.50x) for the rest of the session while
/// leaving it vertically correct (1.01x). Rather than grow the list again (taskbar
/// moved or resized, DPI change, a resolution change that doesn't alter topology,
/// RDP reconnect, session unlock), this re-establishes the invariant itself, on
/// the one path every drawn frame passes through.
///
/// Returns the virtual-desktop rect the window is now aligned to, so the caller
/// can emit the frame in exactly that space.
/// Shortest gap between two realign attempts. A genuine one-off drift still heals
/// on the very next frame (the previous attempt is long past), but a repair that
/// *cannot* take — for whatever reason a future Windows build invents — is bounded
/// to two `SetWindowPos` calls and two log lines a second, rather than running
/// once per emitted frame for the rest of the session.
const REALIGN_MIN_INTERVAL: Duration = Duration::from_millis(500);
static LAST_REALIGN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn realign_to_virtual_desktop(window: &WebviewWindow) -> Option<Rect> {
    // Detection deliberately reads the CACHED virtual-desktop rect: this runs on
    // every emitted frame and must not re-enumerate monitors each time. That is
    // sound because the drift being caught is the *window* being moved under us,
    // not the desktop changing shape — a topology change arrives as
    // WM_DISPLAYCHANGE, which invalidates the cache on its own. And a stale cache
    // is self-correcting anyway: the repair below re-reads it fresh, so a false
    // positive costs one extra enumeration and still lands on the right rect.
    let vd = virtual_desktop_rect().ok()?;
    // A minimized overlay is misaligned by definition, and rect comparison alone
    // cannot lead anywhere useful here: Windows parks a minimized window at
    // -32000,-32000 (measured: 160x28 there), so the check below does fire — but
    // `configure`'s set_position/set_size only rewrite a minimized window's
    // *restored* bounds, leaving it minimized and still reading -32000 on the next
    // frame. Without unminimizing, the repair could never succeed and would re-fire
    // on every emitted frame forever. This is the case v0.7.2 hit on a
    // primary-monitor unplug, where Windows minimizes the desktop-spanning overlay
    // of its own accord.
    let minimized = window.is_minimized().unwrap_or(false);
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return Some(vd);
    };
    if !minimized
        && pos.x == vd.x
        && pos.y == vd.y
        && size.width == vd.width
        && size.height == vd.height
    {
        return Some(vd);
    }

    // Bound the damage if a repair cannot take. See REALIGN_MIN_INTERVAL.
    {
        let throttle = LAST_REALIGN.get_or_init(|| Mutex::new(None));
        let mut last = throttle.lock().unwrap();
        if last.is_some_and(|at| at.elapsed() < REALIGN_MIN_INTERVAL) {
            return Some(vd);
        }
        *last = Some(Instant::now());
    }

    if minimized {
        log::warn!(
            "overlay geometry drifted: window is MINIMIZED (parked at {},{}) — restoring",
            pos.x,
            pos.y,
        );
        if let Err(e) = window.unminimize() {
            log::warn!("overlay unminimize failed: {e}");
        }
    }

    log::warn!(
        "overlay geometry drifted: window {}x{} at {},{} vs virtual desktop {}x{} at {},{} \
         (x scale {:.3}, y scale {:.3}) — realigning",
        size.width,
        size.height,
        pos.x,
        pos.y,
        vd.width,
        vd.height,
        vd.x,
        vd.y,
        size.width as f64 / vd.width.max(1) as f64,
        size.height as f64 / vd.height.max(1) as f64,
    );

    // Repair against freshly enumerated topology, never the cached copy that
    // detected the drift — if the desktop itself is what changed, the cached rect
    // is precisely the wrong thing to restore the window to.
    invalidate_virtual_desktop_cache();
    if let Err(e) = configure(window) {
        log::warn!("overlay realign failed: {e}");
    }
    virtual_desktop_rect().ok().or(Some(vd))
}

/// Emit an `overlay:update` event to the overlay frontend.
///
/// Every path that draws anything on screen funnels through here, which makes it
/// the one place a geometry guard cannot be forgotten — see
/// `realign_to_virtual_desktop`. The update's own `virtual_origin`/`virtual_size`
/// are overwritten with whatever that settles on, so the frame is always emitted
/// in the same space the window actually occupies rather than in whatever the
/// caller measured a moment earlier.
pub fn emit_update(app: &AppHandle, mut update: OverlayUpdate) -> Result<()> {
    let Some(window) = app.get_webview_window("overlay") else {
        return Err(anyhow!("overlay window not found"));
    };
    if let Some(vd) = realign_to_virtual_desktop(&window) {
        update.virtual_origin = (vd.x, vd.y);
        update.virtual_size = (vd.width, vd.height);
    }
    // EDGE-TRIGGERED so streaming (one emit per chunk) cannot flood the log: report
    // only when what is on screen actually changes shape. Every draw path funnels
    // through here, so this is the whole picture of what the overlay was told to
    // show and in what order — which is exactly what a "the caption blinks out
    // between the answer and the pointer" report needs and nothing else records.
    {
        use std::sync::Mutex;
        static LAST: Mutex<Option<(u8, bool, bool)>> = Mutex::new(None);
        let sig = (
            update.kind as u8,
            update.bbox.is_some(),
            update.text.as_deref().is_some_and(|t| !t.is_empty()),
        );
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() != Some(&sig) {
            log::info!(
                "[overlay] emit kind={:?} bbox={} text={}",
                update.kind,
                sig.1,
                sig.2
            );
            *last = Some(sig);
        }
    }
    window
        .emit("overlay:update", &update)
        .map_err(|e| anyhow!("emit overlay:update: {e}"))?;
    Ok(())
}

/// Build an OverlayUpdate with fresh virtual-desktop metadata.
pub fn make_update(
    kind: OverlayKind,
    bbox: Option<Rect>,
    text: Option<String>,
) -> Result<OverlayUpdate> {
    make_update_with_ai_bbox(kind, bbox, text, None)
}

/// Build an OverlayUpdate that also carries an `ai_bbox` for the developer
/// overlay (cyan dashed box). May be `None`.
pub fn make_update_with_ai_bbox(
    kind: OverlayKind,
    bbox: Option<Rect>,
    text: Option<String>,
    ai_bbox: Option<Rect>,
) -> Result<OverlayUpdate> {
    make_update_full(kind, bbox, text, ai_bbox, Vec::new())
}

/// Full-fat builder — additionally carries the ranked candidate boxes for
/// `OverlayKind::Candidates` (Flow A).
pub fn make_update_full(
    kind: OverlayKind,
    bbox: Option<Rect>,
    text: Option<String>,
    ai_bbox: Option<Rect>,
    candidates: Vec<Rect>,
) -> Result<OverlayUpdate> {
    let vd = virtual_desktop_rect()?;
    let active_screen = active_screen_for_bbox(bbox.as_ref().or(ai_bbox.as_ref()));
    // Work area of the same monitor, looked up by its centre so a monitor at a
    // negative virtual-desktop origin resolves like any other.
    let work_area = active_screen.and_then(|m| {
        crate::capture::work_area_containing(m.x + m.width as i32 / 2, m.y + m.height as i32 / 2)
    });
    // Logged once per session: the caption's bottom anchor is the one number that
    // decides whether it clears the taskbar, and it is otherwise invisible from
    // outside the webview. `reserved` is the taskbar inset actually being avoided.
    static LOGGED_WORK_AREA: std::sync::Once = std::sync::Once::new();
    LOGGED_WORK_AREA.call_once(|| match (active_screen, work_area) {
        (Some(m), Some(w)) => log::info!(
            "[overlay] caption anchor: monitor {}x{} at {},{} -> work area {}x{} at {},{} (taskbar reserves {}px at bottom)",
            m.width, m.height, m.x, m.y, w.width, w.height, w.x, w.y,
            (m.y + m.height as i32) - (w.y + w.height as i32)
        ),
        _ => log::warn!("[overlay] caption anchor: no work area resolved — falling back to the monitor's bottom edge, which sits under the taskbar"),
    });
    Ok(OverlayUpdate {
        kind,
        bbox,
        text,
        virtual_origin: (vd.x, vd.y),
        virtual_size: (vd.width, vd.height),
        active_screen,
        work_area,
        ai_bbox,
        candidates,
    })
}
