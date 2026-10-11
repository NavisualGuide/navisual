//! Last-click observable — what the user actually did, as an *element*, not coordinates.
//!
//! The guidance loop is "user acts → screen changes → AI re-analyses", but the AI has only
//! ever been able to *infer* the action from pixels. Cursor placement, focus changes and
//! selections leave no visible trace at all — which is why prompt Rule 3 exists, telling the
//! model to take the user's word for it. This turns the action into a fact.
//!
//! **It is deliberately reported as a resolved control, never as x/y.** Models are bad at
//! coordinates — that is the entire premise of Structured-Context ("select, don't ground"),
//! and it applies to input as much as output. `Button "Breaks"` is usable; `(-1579, 94)` is not.
//!
//! Privacy, in order of importance:
//!   * **The hook stores nothing unless the click is inside the app being guided.** The
//!     target PID is checked in the hook itself, so a click on another window — a bank tab,
//!     a password manager — never enters this process's memory at all.
//!   * **Role and name only, never value.** `ValuePattern.Value` on an Edit control *is* the
//!     secret; it is never read here. A password field reports as `(password field)`.
//!   * **Mouse only.** A keyboard equivalent would be a keylogger: it would trip AV and Smart
//!     App Control heuristics and contradict the product's promise. Deliberately absent.
//!   * Session-scoped: `set_target_pid(0)` disarms it, and nothing is persisted to disk.
//!   * The last 16 resolved clicks are also kept in order, under the same contract (RAM only,
//!     the guided app only, role and name only), so the learned guide (`packs::learn`) can tell
//!     whether the user clicked each control a path of steps named -- the single slot above
//!     keeps only the latest, and a local advance never reads it. Each carries an identity for
//!     its parent container (role, name and rect, compared and never shown), which is how two
//!     menus on one menu bar are told apart from a menu and the item inside it.

/// The prompt line for a click on `role "name"`. One function, so the replay harness reports a
/// click in exactly the words the app uses.
pub(crate) fn prompt_line(role: &str, name: &str) -> String {
    format!(
        "\n[Last user action] The user clicked {role} \"{name}\". Treat this as what \
         they just did — if it is not what you asked for, say so and correct course \
         instead of assuming your previous step succeeded.\n"
    )
}

#[cfg(windows)]
mod imp {
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use super::Resolved;
    use uiautomation::UIElement;
    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetWindowThreadProcessId, SetWindowsHookExW, WindowFromPoint,
        MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN,
    };

    /// `resolved` is filled in by a short-lived background thread spawned right after the
    /// click is recorded (see `hook_proc`) — NOT lazily when `describe()` is finally called.
    /// This matters for anything that live-updates at a fixed screen position (a chat app's
    /// message list, a feed): resolving late means `element_from_point` answers "what's at
    /// (x,y) *now*", which can be a different item than what was actually under the cursor
    /// at click time if the app has since scrolled or inserted new content there. Live-caught
    /// on WeChat: a click was reported to the AI as a different, unrelated ListItem than what
    /// the user actually clicked, because the Official Accounts feed had refreshed by the time
    /// the next AI turn ran `describe()` — which can be arbitrarily long after the click in an
    /// event-driven app. `gen` guards against an out-of-order write: if a second click lands
    /// before the first's resolution thread finishes, the stale thread's result is dropped.
    struct Click {
        resolved: Option<(String, String)>,
        at: Instant,
        gen: u64,
    }

    static LAST: OnceLock<Mutex<Option<Click>>> = OnceLock::new();
    /// Resolved clicks in order, `(gen, role, name, parent)`, at most `HISTORY_CAP`.
    static HISTORY: OnceLock<Mutex<std::collections::VecDeque<Resolved>>> = OnceLock::new();
    const HISTORY_CAP: usize = 16;
    /// PID of the app currently being guided. 0 disarms the hook entirely — the default,
    /// so nothing is ever recorded until a session explicitly arms it.
    static TARGET_PID: AtomicU32 = AtomicU32::new(0);
    static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

    fn slot() -> &'static Mutex<Option<Click>> {
        LAST.get_or_init(|| Mutex::new(None))
    }

    fn history() -> &'static Mutex<std::collections::VecDeque<Resolved>> {
        HISTORY.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
    }

    /// The resolved clicks newer than `gen`, oldest first.
    pub fn history_after(gen: u64) -> Vec<Resolved> {
        let mut out: Vec<_> = history().lock().iter().filter(|r| r.gen > gen).cloned().collect();
        out.sort_by_key(|r| r.gen);
        out
    }

    /// Arm the hook for `pid`, or disarm with 0. Called as the guidance target is
    /// established, so the *next* click is attributed to the app the user is being guided in.
    pub fn set_target_pid(pid: u32) {
        let prev = TARGET_PID.swap(pid, Ordering::Relaxed);
        if prev != pid {
            // Target changed — a click recorded against the old app must not be reported
            // as if it happened in the new one.
            *slot().lock() = None;
            history().lock().clear();
        }
    }

    pub fn clear() {
        *slot().lock() = None;
        history().lock().clear();
    }

    /// Low-level mouse hook. Runs on the tracker thread's message pump for every click
    /// system-wide, so it must stay trivial: two cheap Win32 calls, a slot store, and a
    /// thread spawn (a fast syscall that returns immediately — the actual UIA resolution
    /// happens on that new thread, never inside this callback). Windows silently drops a
    /// `WH_MOUSE_LL` hook that blocks too long, which is why the resolution can't happen here.
    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && wparam.0 as u32 == WM_LBUTTONDOWN {
            let target = TARGET_PID.load(Ordering::Relaxed);
            if target != 0 {
                let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                let pt = POINT {
                    x: info.pt.x,
                    y: info.pt.y,
                };
                // Attribute the click to a process here, in the hook, so clicks outside the
                // guided app are dropped before they are ever stored.
                let hwnd = WindowFromPoint(pt);
                if hwnd.0 as usize != 0 {
                    let mut pid: u32 = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
                    if pid == target {
                        let gen = NEXT_GEN.fetch_add(1, Ordering::Relaxed);
                        *slot().lock() = Some(Click {
                            resolved: None,
                            at: Instant::now(),
                            gen,
                        });
                        std::thread::spawn(move || resolve_and_store(pt.x, pt.y, gen));
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Resolves (x, y) to a named control, as close to click time as the OS scheduler allows,
    /// and writes the result back — but only if this click is still the current one (`gen`
    /// still matches). A later click racing ahead of this resolution finishing must win; this
    /// thread's answer describes a click that is no longer "what the user just did".
    fn resolve_and_store(x: i32, y: i32, gen: u64) {
        let full = resolve_point(x, y);
        // Every resolved click goes into the history, the current one or not.
        if let Some((role, name, parent)) = full.clone() {
            let mut h = history().lock();
            h.push_back(Resolved { gen, role, name, parent });
            while h.len() > HISTORY_CAP {
                h.pop_front();
            }
        }
        let resolved = full.map(|(role, name, _)| (role, name));
        let mut guard = slot().lock();
        if let Some(c) = guard.as_mut() {
            if c.gen == gen {
                c.resolved = resolved;
            }
        }
    }

    /// Walk up from the leaf under (x, y) to the nearest ancestor with a usable name — the
    /// leaf is often an unnamed Text run or Pane inside the real control (same shape as
    /// `verify_role`). Never reports the contents of a protected field: the *name* of a
    /// password box is its label ("Password"), which is harmless, but if the provider marks
    /// the element as a password field, that's reported instead of trusting the name as-is.
    fn resolve_point(x: i32, y: i32) -> Option<(String, String, String)> {
        use uiautomation::types::Point;
        use uiautomation::UIAutomation;

        let automation = UIAutomation::new().ok()?;
        let mut el = automation.element_from_point(Point::new(x, y)).ok()?;
        if is_light_dismiss(&el) {
            // A XAML menu opens on pointer-down and covers its window with a light-dismiss layer
            // at once -- already on top at +0 ms (live 2026-10-11, Notepad's View menu), so no
            // amount of hurrying resolves the point to what was clicked. Look under the layer;
            // and if nothing named is there, report nothing rather than "Close".
            el = under_light_dismiss(&automation, &el, x, y)?;
        }

        let walker = automation.get_control_view_walker().ok();
        let mut named: Option<(String, String)> = None;
        for _ in 0..4 {
            let role = el
                .get_control_type()
                .map(|ct| format!("{ct:?}"))
                .unwrap_or_default();
            let name = el.get_name().unwrap_or_default();
            if !name.trim().is_empty() && !role.is_empty() {
                named = Some((role, name));
                break;
            }
            match walker.as_ref().and_then(|w| w.get_parent(&el).ok()) {
                Some(p) => el = p,
                None => break,
            }
        }
        let (mut role, name) = named?;
        // A label inside a control -- XAML menu items expose their text as a Text child of the
        // same name (live 2026-10-11, Notepad's "Word wrap"): report the control it labels.
        if role == "Text" {
            if let Some(p) = walker.as_ref().and_then(|w| w.get_parent(&el).ok()) {
                let same = p.get_name().is_ok_and(|n| n.trim() == name.trim());
                let prole = p.get_control_type().map(|ct| format!("{ct:?}")).unwrap_or_default();
                if same && !prole.is_empty() {
                    role = prole;
                    el = p;
                }
            }
        }

        let name = if is_password(&el) {
            "(password field)".to_string()
        } else {
            sanitize(&name)
        };
        // The container's identity: role, name and rect. Two menus on one menu bar share it; a
        // menu and the item inside it do not.
        let parent = walker
            .as_ref()
            .and_then(|w| w.get_parent(&el).ok())
            .map(|p| {
                let role = p.get_control_type().map(|ct| format!("{ct:?}")).unwrap_or_default();
                let rect = p
                    .get_bounding_rectangle()
                    .map(|r| format!("{},{},{},{}", r.get_left(), r.get_top(), r.get_right(), r.get_bottom()))
                    .unwrap_or_default();
                format!("{role}|{}|{rect}", sanitize(&p.get_name().unwrap_or_default()))
            })
            .unwrap_or_default();
        Some((role, name, parent))
    }

    /// XAML's light-dismiss layer, by its class and automation id -- the framework's own names
    /// for it, which stay the same whatever the UI language (its Name, "Close", does not).
    fn is_light_dismiss(el: &UIElement) -> bool {
        el.get_classname().is_ok_and(|c| c == "PopupRoot")
            || el.get_automation_id().is_ok_and(|a| a == "Light Dismiss")
    }

    /// The smallest named control under (x, y) beside the light-dismiss layer, among the
    /// layer's parent's descendants -- the island the menu bar lives in.
    fn under_light_dismiss(automation: &uiautomation::UIAutomation, layer: &UIElement, x: i32, y: i32) -> Option<UIElement> {
        let walker = automation.get_control_view_walker().ok()?;
        let island = walker.get_parent(layer).ok()?;
        let cond = automation.create_true_condition().ok()?;
        let all = island.find_all(uiautomation::types::TreeScope::Descendants, &cond).ok()?;
        all.into_iter()
            .filter(|e| !is_light_dismiss(e) && !e.get_name().unwrap_or_default().trim().is_empty())
            .filter_map(|e| {
                let r = e.get_bounding_rectangle().ok()?;
                let inside = x >= r.get_left() && x < r.get_right() && y >= r.get_top() && y < r.get_bottom();
                let area = i64::from(r.get_right() - r.get_left()) * i64::from(r.get_bottom() - r.get_top());
                (inside && area > 0).then_some((area, e))
            })
            .min_by_key(|(area, _)| *area)
            .map(|(_, e)| e)
    }

    /// Install the hook. MUST be called from a thread that runs a message pump — a
    /// `WH_MOUSE_LL` hook is dispatched via the installing thread's queue and is silently
    /// dead without one. Called from the tracker thread, which already pumps for its
    /// WinEvent hooks.
    pub fn install() {
        unsafe {
            match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) {
                Ok(_h) => log::info!("[click] low-level mouse hook installed"),
                Err(e) => log::warn!("[click] hook install failed: {e}"),
            }
        }
    }

    /// Render the prompt line for the last recorded click. `None` when there is no click, it
    /// is older than `max_age`, or resolution hasn't produced a nameable control (either it
    /// genuinely can't be resolved, or — rarely — the background resolve from `hook_proc`
    /// hasn't finished yet; either way, an unresolved click is silently dropped rather than
    /// reported as bare coordinates or guessed at).
    pub fn describe(max_age: Duration) -> Option<String> {
        let guard = slot().lock();
        let c = guard.as_ref()?;
        if c.at.elapsed() > max_age {
            return None;
        }
        let (role, name) = c.resolved.clone()?;
        Some(super::prompt_line(&role, &name))
    }

    /// The same click as a bare display string -- `Button "Insert"` -- for the panel, and
    /// **consumed in the process**: one click decorates exactly one turn.
    ///
    /// Consumption is the point. The slot holds a click until the next one replaces it, so
    /// without taking it here a second *Next* with no click in between would show the earlier
    /// step's control beside a step the user reached some other way. `describe` still does not
    /// consume, because the prompt is built first in the same request and must keep seeing it.
    pub fn take(max_age: Duration) -> Option<String> {
        let mut guard = slot().lock();
        let out = guard.as_ref().and_then(|c| {
            if c.at.elapsed() > max_age {
                return None;
            }
            c.resolved
                .as_ref()
                .map(|(role, name)| format!("{role} \"{name}\""))

        });
        if out.is_some() {
            *guard = None;
        }
        out
    }

    fn is_password(el: &UIElement) -> bool {
        el.is_password().unwrap_or(false)
    }

    /// Cap length and strip newlines so one long label can't dominate the prompt or forge
    /// extra prompt lines.
    fn sanitize(name: &str) -> String {
        let flat = name.replace(['\r', '\n'], " ");
        let flat = flat.trim();
        if flat.chars().count() > 80 {
            let cut: String = flat.chars().take(80).collect();
            format!("{cut}…")
        } else {
            flat.to_string()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::sanitize;

        #[test]
        fn sanitize_flattens_and_caps() {
            assert_eq!(sanitize("  Save As  "), "Save As");
            assert_eq!(sanitize("two\nlines"), "two lines");
            let long = "x".repeat(200);
            let out = sanitize(&long);
            assert_eq!(out.chars().count(), 81, "80 chars plus the ellipsis");
            assert!(out.ends_with('…'));
        }

        /// Live probe: click (NAVISUAL_PROBE_X, NAVISUAL_PROBE_Y), then print what the resolver
        /// and the raw element chain say at several delays -- for a control whose menu opens
        /// under the cursor. `cargo test --lib live_click_resolution -- --ignored --nocapture`
        #[test]
        #[ignore]
        fn live_click_resolution() {
            use uiautomation::types::Point;
            use uiautomation::UIAutomation;
            use windows::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP};
            use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
            let x: i32 = std::env::var("NAVISUAL_PROBE_X").unwrap().parse().unwrap();
            let y: i32 = std::env::var("NAVISUAL_PROBE_Y").unwrap().parse().unwrap();
            let click = std::env::var("NAVISUAL_PROBE_CLICK").map(|v| v == "1").unwrap_or(true);
            if click {
                unsafe {
                    let _ = SetCursorPos(x, y);
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
                    mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
                }
            }
            let automation = UIAutomation::new().unwrap();
            let walker = automation.get_control_view_walker().unwrap();
            for ms in [0u64, 40, 150, 500] {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                println!("+{ms} ms resolve_point: {:?}", super::resolve_point(x, y));
                let mut el = automation.element_from_point(Point::new(x, y)).unwrap();
                for depth in 0..5 {
                    let r = el.get_bounding_rectangle().ok();
                    println!(
                        "   {depth}: {:?} name={:?} class={:?} aid={:?} rect={:?}",
                        el.get_control_type().ok(),
                        el.get_name().unwrap_or_default(),
                        el.get_classname().unwrap_or_default(),
                        el.get_automation_id().unwrap_or_default(),
                        r.map(|r| (r.get_left(), r.get_top(), r.get_right(), r.get_bottom()))
                    );
                    match walker.get_parent(&el) {
                        Ok(p) => el = p,
                        Err(_) => break,
                    }
                }
            }
        }
    }
}

/// One resolved click, for the learned guide.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub gen: u64,
    pub role: String,
    pub name: String,
    /// The parent container's identity; empty when it could not be read.
    pub parent: String,
}

#[cfg(windows)]
pub use imp::{clear, describe, history_after, install, set_target_pid, take};

#[cfg(not(windows))]
pub fn install() {}
#[cfg(not(windows))]
pub fn set_target_pid(_pid: u32) {}
#[cfg(not(windows))]
pub fn clear() {}
#[cfg(not(windows))]
pub fn describe(_max_age: std::time::Duration) -> Option<String> {
    None
}
#[cfg(not(windows))]
pub fn take(_max_age: std::time::Duration) -> Option<String> {
    None
}
#[cfg(not(windows))]
pub fn history_after(_gen: u64) -> Vec<Resolved> {
    Vec::new()
}
