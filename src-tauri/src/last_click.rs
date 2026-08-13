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

#[cfg(windows)]
mod imp {
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use uiautomation::UIElement;
    use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetWindowThreadProcessId, SetWindowsHookExW, WindowFromPoint,
        MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN,
    };

    struct Click {
        x: i32,
        y: i32,
        at: Instant,
    }

    static LAST: OnceLock<Mutex<Option<Click>>> = OnceLock::new();
    /// PID of the app currently being guided. 0 disarms the hook entirely — the default,
    /// so nothing is ever recorded until a session explicitly arms it.
    static TARGET_PID: AtomicU32 = AtomicU32::new(0);

    fn slot() -> &'static Mutex<Option<Click>> {
        LAST.get_or_init(|| Mutex::new(None))
    }

    /// Arm the hook for `pid`, or disarm with 0. Called as the guidance target is
    /// established, so the *next* click is attributed to the app the user is being guided in.
    pub fn set_target_pid(pid: u32) {
        let prev = TARGET_PID.swap(pid, Ordering::Relaxed);
        if prev != pid {
            // Target changed — a click recorded against the old app must not be reported
            // as if it happened in the new one.
            *slot().lock() = None;
        }
    }

    pub fn clear() {
        *slot().lock() = None;
    }

    /// Low-level mouse hook. Runs on the tracker thread's message pump for every click
    /// system-wide, so it must stay trivial: two cheap Win32 calls and an early return.
    /// No UIA, no allocation, no locking beyond the one short store.
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
                        *slot().lock() = Some(Click {
                            x: pt.x,
                            y: pt.y,
                            at: Instant::now(),
                        });
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
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

    /// Resolve the last recorded click to a control and render the prompt line.
    /// `None` when there is no click, it is older than `max_age`, or it cannot be resolved
    /// to something nameable — an unresolvable click is silently dropped rather than
    /// reported as bare coordinates.
    pub fn describe(max_age: Duration) -> Option<String> {
        use uiautomation::types::Point;
        use uiautomation::UIAutomation;

        let (x, y) = {
            let guard = slot().lock();
            let c = guard.as_ref()?;
            if c.at.elapsed() > max_age {
                return None;
            }
            (c.x, c.y)
        };

        let automation = UIAutomation::new().ok()?;
        let mut el = automation.element_from_point(Point::new(x, y)).ok()?;

        // Walk up to something with a usable name — the leaf under the cursor is often an
        // unnamed Text run or Pane inside the real control (same shape as `verify_role`).
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
        let (role, name) = named?;

        // Never report the contents of a protected field. The *name* of a password box is
        // its label ("Password"), which is harmless — but if the provider marks it, say so
        // and stop rather than risk a provider that puts anything else in the name.
        let name = if is_password(&el) {
            "(password field)".to_string()
        } else {
            sanitize(&name)
        };

        Some(format!(
            "\n[Last user action] The user clicked {role} \"{name}\". Treat this as what \
             they just did — if it is not what you asked for, say so and correct course \
             instead of assuming your previous step succeeded.\n"
        ))
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
    }
}

#[cfg(windows)]
pub use imp::{clear, describe, install, set_target_pid};

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
