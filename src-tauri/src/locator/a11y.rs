//! Windows UI Automation element locator — port of the v0.3
//! `src/locator/a11y_engine.py` with the same matching semantics:
//!
//! - Unicode dash normalisation so "A — B" matches "A - B".
//! - Case-insensitive name match anchored with `^[\W_]*<target>[\W_]*$` so
//!   "← my claims" matches target "my claims" but "Insert Space" does NOT
//!   match target "insert".
//! - Optional role filter mapped from our schema (`button`, `tab`, `link`…)
//!   to UIA `ControlType`.
//! - Reject container/window roles (`Window`, `TitleBar`, `Pane`) even when
//!   their name happens to contain the target substring.
//! - If the foreground window belongs to AI Navigator itself (e.g. the user
//!   just clicked our Next button), walk the desktop's top-level windows
//!   and search each one that belongs to a different process.
//! - Reject elements with obviously bogus coordinates (|x|,|y| > 10 000 px).

use super::trace::{A11yCandidate, A11yTrace};
use super::LocateResult;
use crate::capture::Rect;
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uiautomation::controls::ControlType;
use uiautomation::types::{TreeScope, UIProperty};
use uiautomation::variants::Variant;
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetTopWindow, GetWindow, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GW_HWNDNEXT, GW_OWNER,
};

/// Chromium/Electron windows build their UIA tree lazily; on a 0-candidate first pass
/// we wait this long before walking again (the first query wakes the build).
const CHROMIUM_RETRY_DELAY_MS: u64 = 250;

/// True when `hwnd_raw`'s window is a Chromium/Electron host (`Chrome_WidgetWin_1`) —
/// these build their UIA tree lazily, so a 0-candidate first pass is worth retrying.
fn window_class_is_chromium(hwnd_raw: usize) -> bool {
    if hwnd_raw == 0 {
        return false;
    }
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    unsafe {
        let hwnd = HWND(hwnd_raw as *mut _);
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        if n <= 0 {
            return false;
        }
        String::from_utf16_lossy(&buf[..n as usize]).starts_with("Chrome_WidgetWin")
    }
}

/// UI framework of the target window, which decides the locate strategy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Framework {
    /// Chromium/Electron/Edge — lazy a11y tree → prime + a single cached deep find.
    Chrome,
    /// WPF / WinForms / WinUI / Win32 — eager tree → the standard matcher + manual walk.
    Eager,
    /// Unknown framework — treated like `Eager` (no prime).
    Other,
}

/// Classify the target window's framework via the Chromium window class (fast, catches
/// Electron/Chrome/Edge) then UIA `FrameworkId` (catches eager frameworks so we can skip
/// priming). Used to route the locate and gate the prime.
fn framework_of(automation: &UIAutomation, hwnd_raw: usize) -> Framework {
    if hwnd_raw == 0 {
        return Framework::Other;
    }
    if window_class_is_chromium(hwnd_raw) {
        return Framework::Chrome;
    }
    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
    let fid = automation
        .element_from_handle(hwnd.into())
        .ok()
        .and_then(|el| el.get_framework_id().ok())
        .unwrap_or_default();
    match fid.as_str() {
        "Chrome" => Framework::Chrome,
        "WPF" | "WinForm" | "Win32" | "XAML" | "DirectUI" => Framework::Eager,
        _ => Framework::Other,
    }
}

/// Map our schema roles → UIA ControlType. `None` means "any role".
fn role_to_control_type(role: &str) -> Option<ControlType> {
    match role.to_ascii_lowercase().as_str() {
        "button" => Some(ControlType::Button),
        // TabItem covers classic WinForms/WPF tabs; ListItem covers WinUI
        // NavigationView items (Task Manager, Settings, etc.). Returning None
        // lets Pass 2 handle the ambiguity rather than blocking on the wrong type.
        "tab" => None,
        "link" => Some(ControlType::Hyperlink),
        "textbox" => Some(ControlType::Edit),
        "menuitem" => Some(ControlType::MenuItem),
        "checkbox" => Some(ControlType::CheckBox),
        "radio" => Some(ControlType::RadioButton),
        "combobox" => Some(ControlType::ComboBox),
        "slider" => Some(ControlType::Slider),
        "image" => Some(ControlType::Image),
        "heading" => Some(ControlType::Text),
        _ => None,
    }
}

/// Unicode dashes apps embed in accessible names (em-dash, en-dash, figure
/// dash, box-drawing horizontal, minus sign, hyphen…).
const DASH_CHARS: &[char] = &[
    '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}', '\u{2500}',
];

fn norm_dashes(s: &str) -> String {
    s.chars()
        .map(|c| if DASH_CHARS.contains(&c) { '-' } else { c })
        .collect()
}

/// Build the anchored regex used for name matching.
/// `^[\W_]*<escaped_target>[\W_]*$`, case-insensitive.
fn build_name_regex(target: &str) -> Result<Regex> {
    // Truncated labels (model copied a clipped "…" name) become a prefix match:
    // UIA accessible names are never visually truncated, so anchoring with `$`
    // on the clipped text would never match the real full name.
    let (core, prefix) = super::strip_trailing_ellipsis(target);
    let target_norm = norm_dashes(&core.to_ascii_lowercase());
    let escaped = regex::escape(&target_norm);
    let pattern = if prefix {
        format!(r"(?i)^[\W_]*{}", escaped)
    } else {
        format!(r"(?i)^[\W_]*{}[\W_]*$", escaped)
    };
    Regex::new(&pattern).context("compile name regex")
}

/// Container roles we never return even when their name substring-matches.
fn is_container_role(ct: ControlType) -> bool {
    matches!(
        ct,
        ControlType::Window | ControlType::TitleBar | ControlType::Pane
    )
}

/// Off-screen / bogus coordinate guard (minimised windows report ~-32000).
fn rect_is_onscreen(left: i32, top: i32) -> bool {
    left.abs() <= 10_000 && top.abs() <= 10_000
}

/// Our own process ID (for "foreground is us" detection).
fn own_pid() -> u32 {
    std::process::id()
}

/// PID owning the current foreground HWND (or 0 on failure).
fn foreground_pid() -> (HWND, u32) {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return (hwnd, 0);
        }
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        (hwnd, pid)
    }
}

/// Enumerate visible, non-minimised, reasonably-sized top-level windows
/// in z-order (topmost first). Excludes AI Navigator's own windows, system
/// shell windows, and overlay-style utility windows.
fn collect_visible_top_windows(our_pid: u32, max: usize) -> Vec<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    // Class-name blocklist for system shell / IME / overlay windows.
    const SKIP_CLASSES: &[&str] = &[
        "Progman",       // Desktop window
        "WorkerW",       // Desktop worker
        "Shell_TrayWnd", // Taskbar
        "Shell_SecondaryTrayWnd",
        "NotifyIconOverflowWindow",
        "Windows.UI.Core.CoreWindow", // IME, Xaml islands
        "IME",
        "MSCTFIME UI",
        "Default IME",
    ];

    let mut out = Vec::new();
    unsafe {
        let mut hwnd = GetTopWindow(None).unwrap_or(HWND(std::ptr::null_mut()));
        while !hwnd.0.is_null() && out.len() < max {
            if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
                let mut pid: u32 = 0;
                let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
                if pid != 0 && pid != our_pid {
                    let mut rect = RECT::default();
                    if GetWindowRect(hwnd, &mut rect).is_ok() {
                        let w = rect.right - rect.left;
                        let h = rect.bottom - rect.top;
                        if w > 100 && h > 100 {
                            // Class-name filter.
                            let mut buf = [0u16; 128];
                            let n = GetClassNameW(hwnd, &mut buf);
                            let class = String::from_utf16_lossy(&buf[..n as usize]);
                            if !SKIP_CLASSES.iter().any(|c| *c == class) {
                                out.push(hwnd);
                            }
                        }
                    }
                }
            }
            match GetWindow(hwnd, GW_HWNDNEXT) {
                Ok(next) => hwnd = next,
                Err(_) => break,
            }
        }
    }
    out
}

/// Convert a UIA element to LocateResult, filtering out containers + off-screen.
fn element_to_result(el: &UIElement) -> Option<LocateResult> {
    let ct = el.get_control_type().ok()?;
    if is_container_role(ct) {
        return None;
    }
    let rect = el.get_bounding_rectangle().ok()?;
    let left = rect.get_left();
    let top = rect.get_top();
    let width = rect.get_width().max(0) as u32;
    let height = rect.get_height().max(0) as u32;
    if width == 0 || height == 0 || !rect_is_onscreen(left, top) {
        return None;
    }
    Some(LocateResult {
        bbox: Rect {
            x: left,
            y: top,
            width,
            height,
        },
        name: el.get_name().unwrap_or_default(),
        role: format!("{:?}", ct),
        confidence: 1.0,
    })
}

/// Like `element_to_result` but reads **cached** properties (Name/ControlType/Rect) populated
/// by `find_all_build_cache` — zero per-element COM round-trips. The cached rect is moments old
/// (the find just ran), so it's used directly.
fn element_to_result_cached(el: &UIElement) -> Option<LocateResult> {
    let ct = el.get_cached_control_type().ok()?;
    if is_container_role(ct) {
        return None;
    }
    let rect = el.get_cached_bounding_rectangle().ok()?;
    let left = rect.get_left();
    let top = rect.get_top();
    let width = rect.get_width().max(0) as u32;
    let height = rect.get_height().max(0) as u32;
    if width == 0 || height == 0 || !rect_is_onscreen(left, top) {
        return None;
    }
    Some(LocateResult {
        bbox: Rect {
            x: left,
            y: top,
            width,
            height,
        },
        name: el.get_cached_name().unwrap_or_default(),
        role: format!("{:?}", ct),
        confidence: 1.0,
    })
}

/// Collect visible, non-minimised top-level windows that are OWNED by another
/// window and belong to the same process as `target` — i.e. modal dialogs and
/// popups (Excel's "PivotTable from table or range", Word's Find/Font/Save As,
/// etc.). These are separate top-level windows, NOT children of the main
/// window's UIA element, so a subtree walk rooted at the main window never
/// reaches their controls. Without searching them, dialog buttons like "OK"
/// and "Cancel" are reported NOT LOCATED and the locator falls back to the AI
/// bbox hint. Mirrors the owned-window handling in `capture::pid_union_rect`.
fn collect_owned_popups(target: HWND) -> Vec<HWND> {
    let mut target_pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(target, Some(&mut target_pid));
    }
    if target_pid == 0 {
        return Vec::new();
    }

    struct State {
        pid: u32,
        hwnds: Vec<HWND>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != state.pid {
            return TRUE;
        }
        // Owned windows only (GW_OWNER non-null) — dialogs/popups, not the
        // main window itself or unrelated top-level documents.
        let owned = GetWindow(hwnd, GW_OWNER)
            .map(|o| !o.0.is_null())
            .unwrap_or(false);
        if owned {
            state.hwnds.push(hwnd);
        }
        TRUE
    }

    let mut state = State {
        pid: target_pid,
        hwnds: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut state as *mut State as isize));
    }
    state.hwnds
}

/// Public entry point. Returns the first element whose accessible name
/// satisfies the anchored regex and whose role matches (when specified),
/// plus a trace recording every candidate considered.
///
/// `timeout_ms` caps both the UIA matcher's internal timeout and the
/// manual-walk fallback.
pub fn find_element(
    target_text: &str,
    opts: &super::orchestrator::LocateOptions,
) -> Result<(Option<LocateResult>, A11yTrace)> {
    let mut trace = A11yTrace {
        ran: true,
        ..Default::default()
    };

    if target_text.trim().is_empty() {
        return Ok((None, trace));
    }

    let started = Instant::now();
    let timeout_ms = if opts.a11y_timeout_ms == 0 {
        150
    } else {
        opts.a11y_timeout_ms
    };
    let automation = UIAutomation::new().map_err(|e| anyhow!("UIAutomation init: {e}"))?;
    let name_re = Arc::new(build_name_regex(target_text)?);
    trace.regex_used = name_re.as_str().to_string();
    let target_norm_len = norm_dashes(target_text).chars().count();
    let desired_ct = opts.role.as_deref().and_then(role_to_control_type);

    // Decide which top-level window(s) to search.
    //
    // Priority: if the caller pinned a target HWND (the one the AI saw), use
    // it directly. This prevents focus changes between AI capture and locate
    // from redirecting us to the wrong window — common when the AI takes a
    // long time (e.g. local Ollama models) and the user switches focus.
    let our_pid = own_pid();

    let search_roots: Vec<UIElement> = if let Some(hwnd_raw) = opts.target_hwnd {
        let hwnd = HWND(hwnd_raw as *mut _);
        let mut roots = Vec::new();
        unsafe {
            // Owned dialogs/popups first — they sit on top of the main window
            // and are the active interaction surface (e.g. an open "OK"/"Cancel"
            // dialog). They are separate top-level windows, so they must be
            // added as explicit search roots; the main window's subtree walk
            // does not reach them.
            for popup in collect_owned_popups(hwnd) {
                if let Ok(el) = automation.element_from_handle(popup.into()) {
                    roots.push(el);
                }
            }
            if !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
                if let Ok(el) = automation.element_from_handle(hwnd.into()) {
                    roots.push(el);
                }
            }
        }
        roots
    } else {
        let (fg_hwnd, fg_pid) = foreground_pid();
        if fg_pid != 0 && fg_pid != our_pid {
            match automation.element_from_handle(fg_hwnd.into()) {
                Ok(el) => vec![el],
                Err(_) => Vec::new(),
            }
        } else {
            collect_visible_top_windows(our_pid, 8)
                .into_iter()
                .filter_map(|h| automation.element_from_handle(h.into()).ok())
                .collect()
        }
    };
    trace.search_roots_count = search_roots.len();

    if search_roots.is_empty() {
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    let framework = opts
        .target_hwnd
        .map(|h| framework_of(&automation, h))
        .unwrap_or(Framework::Other);
    let is_chrome = framework == Framework::Chrome;
    trace.framework = Some(format!("{framework:?}"));
    let per_root_ms = ((timeout_ms / search_roots.len() as u64).max(250)).min(timeout_ms);

    let mut candidates: Vec<LocateResult> = Vec::new();
    // Up to two attempts: Chromium/Electron apps build their UIA tree lazily, so the
    // first find can return 0 while the tree is still materialising — the query itself
    // wakes it. On a Chrome miss, wait briefly and find once more.
    for attempt in 0..2u8 {
        if attempt == 1 {
            if !is_chrome || !candidates.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(CHROMIUM_RETRY_DELAY_MS));
            trace.retried = true;
        }
        candidates.clear();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        for root in &search_roots {
            if Instant::now() > deadline {
                trace.timed_out = true;
                break;
            }
            if is_chrome {
                // Chrome/Electron: one cached native find. The tree is deep and the per-element
                // COM of the matcher/manual passes is too slow here; a control-type condition +
                // CacheRequest (batched Name/ControlType/Rect) reaches deep items (activity bar,
                // search box) fast. `deep_role_match` is now cached.
                candidates.extend(deep_role_match(
                    &automation,
                    root,
                    target_text,
                    opts.role.as_deref(),
                ));
                trace.cached = true;
            } else {
                // Eager-tree frameworks (WPF/WinForms/WinUI/Win32): the standard matcher + manual
                // walk are fast and proven here.
                // Pass 1
                candidates.extend(match_in_subtree_all(
                    &automation,
                    root,
                    &name_re,
                    desired_ct,
                    per_root_ms,
                )?);
                if Instant::now() > deadline {
                    trace.timed_out = true;
                    break;
                }
                // Pass 2
                if desired_ct.is_some() {
                    candidates.extend(match_in_subtree_all(
                        &automation,
                        root,
                        &name_re,
                        None,
                        per_root_ms,
                    )?);
                }
                if Instant::now() > deadline {
                    trace.timed_out = true;
                    break;
                }
                // Pass 3
                candidates.extend(manual_walk_all(
                    root,
                    &norm_dashes(&target_text.to_ascii_lowercase()),
                    target_norm_len,
                    desired_ct,
                    deadline,
                ));
            }
        }
        if !candidates.is_empty() {
            break;
        }
    }

    if candidates.is_empty() {
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    // Sort by AI-bbox proximity if a predicted bbox is provided. Both bboxes
    // are already in virtual-desktop physical pixels so the comparison is direct.
    if let Some(ai) = opts.ai_bbox {
        let target_x = ai.x as f32 + ai.width as f32 / 2.0;
        let target_y = ai.y as f32 + ai.height as f32 / 2.0;

        candidates.sort_by(|a, b| {
            let acx = a.bbox.x as f32 + a.bbox.width as f32 / 2.0;
            let acy = a.bbox.y as f32 + a.bbox.height as f32 / 2.0;
            let bcx = b.bbox.x as f32 + b.bbox.width as f32 / 2.0;
            let bcy = b.bbox.y as f32 + b.bbox.height as f32 / 2.0;
            let da = (acx - target_x).powi(2) + (acy - target_y).powi(2);
            let db = (bcx - target_x).powi(2) + (bcy - target_y).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Record candidates in the trace. The first one is selected; the rest are
    // listed with their reject reason so the debug drawer can show why they
    // weren't chosen.
    for (i, c) in candidates.iter().enumerate() {
        trace.candidates.push(A11yCandidate {
            name: c.name.clone(),
            role: c.role.clone(),
            bbox: (c.bbox.x, c.bbox.y, c.bbox.width, c.bbox.height),
            selected: i == 0,
            reject_reason: if i == 0 {
                None
            } else if opts.ai_bbox.is_some() {
                Some("farther from AI bbox".to_string())
            } else {
                Some("not first match".to_string())
            },
        });
    }

    trace.elapsed_ms = started.elapsed().as_millis() as u32;
    Ok((candidates.into_iter().next(), trace))
}

/// Warm a window's UIA tree so Chromium/Electron starts building its **lazy** accessibility
/// tree before the locate runs. Fire-and-forget (errors ignored). Meant to be called on a
/// background thread when the AI begins streaming, so by locate time (seconds later) the tree
/// is materialised and `find_element` hits. No-op for non-Chromium windows (they build eagerly).
pub fn prime(hwnd_raw: usize) {
    if !window_class_is_chromium(hwnd_raw) {
        return;
    }
    let Ok(automation) = UIAutomation::new() else {
        return;
    };
    let hwnd = HWND(hwnd_raw as *mut _);
    let Ok(root) = automation.element_from_handle(hwnd.into()) else {
        return;
    };
    // A deep FindFirst forces UIA to traverse *into* the renderer subtree — that traversal
    // is what actually triggers Chromium/Electron to build its lazy accessibility tree (a
    // shallow get_name walk doesn't reach the render widget, so the tree never materialises).
    // The filter never matches; we only want the traversal it performs. The build it kicks
    // off is ready by the time find_element runs (seconds later, once the AI finishes).
    let started = Instant::now();
    let _ = automation
        .create_matcher()
        .from_ref(&root)
        .depth(40)
        .timeout(1500)
        .filter_fn(Box::new(|_el: &UIElement| Ok(false)))
        .find_first();
    log::info!(
        "a11y::prime walked window {hwnd_raw:#x} in {} ms",
        started.elapsed().as_millis()
    );
}

fn match_in_subtree_all(
    automation: &UIAutomation,
    root: &UIElement,
    name_re: &Arc<Regex>,
    control_type: Option<ControlType>,
    timeout_ms: u64,
) -> Result<Vec<LocateResult>> {
    let re = name_re.clone();
    // Cap the UIA internal timeout at 100ms. The UIA matcher uses this as a
    // "wait for element to appear" poll; if set to the full budget it blocks
    // the whole allocation when nothing is found, starving Passes 2 and 3.
    // The outer deadline already enforces the total budget.
    let internal_timeout = timeout_ms.min(100);
    let mut matcher = automation
        .create_matcher()
        .from_ref(root)
        .depth(15)
        .timeout(internal_timeout)
        .filter_fn(Box::new(move |el: &UIElement| {
            let name = el.get_name().unwrap_or_default();
            if name.is_empty() {
                return Ok(false);
            }
            let normed = norm_dashes(&name);
            Ok(re.is_match(&normed))
        }));
    if let Some(ct) = control_type {
        matcher = matcher.control_type(ct);
    }
    match matcher.find_all() {
        Ok(els) => Ok(els
            .into_iter()
            .filter_map(|e| element_to_result(&e))
            .collect()),
        Err(_) => Ok(Vec::new()),
    }
}

/// Deep, role-aware matcher for Chromium/Electron windows: finds far-down items (VS Code's
/// activity bar, search boxes, …) that the standard passes miss. Uses a **native** UIA
/// control-type OR-condition so `find_all` filters *in-process* — only the few matching
/// elements cross the COM boundary, instead of a per-element round-trip over the whole tree
/// (which made the manual approach take seconds). Restricts to the control types appropriate
/// for the AI's role (a "button" target ignores bulk Text content; a "textbox" target keeps
/// Edit/Text), then substring-matches the name (so "Extensions (Ctrl+Shift+X)" matches).
fn deep_role_match(
    automation: &UIAutomation,
    root: &UIElement,
    target: &str,
    role: Option<&str>,
) -> Vec<LocateResult> {
    let needle = norm_dashes(&target.to_ascii_lowercase());
    if needle.chars().count() < 2 {
        return Vec::new();
    }
    let mut cond = None;
    for &id in role_control_type_ids(role) {
        let Ok(c) = automation.create_property_condition(
            UIProperty::ControlType,
            Variant::from(id),
            None,
        ) else {
            continue;
        };
        cond = Some(match cond.take() {
            None => c,
            Some(prev) => match automation.create_or_condition(prev, c) {
                Ok(o) => o,
                Err(_) => return Vec::new(),
            },
        });
    }
    let Some(cond) = cond else {
        return Vec::new();
    };
    // CacheRequest: batch Name + ControlType + BoundingRectangle so the in-process filter
    // reads CACHED props with zero per-element COM round-trips — the dominant cost on huge
    // Electron trees (a per-element get_name walk took seconds; one batched call is ~ms).
    let cache = match automation.create_cache_request() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let _ = cache.add_property(UIProperty::Name);
    let _ = cache.add_property(UIProperty::ControlType);
    let _ = cache.add_property(UIProperty::BoundingRectangle);
    let els = match root.find_all_build_cache(TreeScope::Descendants, &cond, &cache) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    els.into_iter()
        .filter(|el| {
            el.get_cached_name()
                .map(|n| norm_dashes(&n.to_ascii_lowercase()).contains(&needle))
                .unwrap_or(false)
        })
        .filter_map(|el| element_to_result_cached(&el))
        .collect()
}

/// UIA `UIA_*ControlTypeId` values to consider in the deep Chromium pass, by AI role family.
/// Clickable roles exclude bulk Text/Edit content; "textbox" keeps it; unknown is broad.
fn role_control_type_ids(role: Option<&str>) -> &'static [i32] {
    // Button 50000, CheckBox 50002, ComboBox 50003, Edit 50004, Hyperlink 50005,
    // ListItem 50007, MenuItem 50011, RadioButton 50013, TabItem 50019, Text 50020,
    // TreeItem 50024, SplitButton 50031.
    const CLICKABLE: &[i32] = &[
        50000, 50019, 50007, 50011, 50005, 50002, 50013, 50003, 50031, 50024,
    ];
    const TEXTUAL: &[i32] = &[50004, 50020, 50003];
    const BROAD: &[i32] = &[
        50000, 50019, 50007, 50011, 50005, 50002, 50013, 50003, 50031, 50024, 50004, 50020,
    ];
    match role.map(|r| r.to_ascii_lowercase()).as_deref() {
        Some("textbox") | Some("searchbox") | Some("combobox") => TEXTUAL,
        Some("button") | Some("tab") | Some("menuitem") | Some("checkbox") | Some("radio")
        | Some("link") | Some("listitem") | Some("treeitem") => CLICKABLE,
        _ => BROAD,
    }
}

fn manual_walk_all(
    root: &UIElement,
    target_norm_lower: &str,
    target_len: usize,
    desired_ct: Option<ControlType>,
    deadline: Instant,
) -> Vec<LocateResult> {
    // Create one UIAutomation + walker for the entire recursive walk instead of
    // one per visited node — the old per-node allocation consumed most of the
    // 100 ms budget before reaching deep WinUI 3 elements (e.g. NavigationViewItem).
    let automation = match UIAutomation::new() {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };
    let walker = match automation.get_control_view_walker() {
        Ok(w) => w,
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    walk_recursive(
        root,
        &walker,
        0,
        12,
        deadline,
        target_norm_lower,
        target_len,
        desired_ct,
        &mut candidates,
    );
    candidates
        .into_iter()
        .filter_map(|e| element_to_result(&e))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn walk_recursive(
    element: &UIElement,
    walker: &uiautomation::UITreeWalker,
    depth: u32,
    max_depth: u32,
    deadline: Instant,
    target_norm_lower: &str,
    target_len: usize,
    desired_ct: Option<ControlType>,
    candidates: &mut Vec<UIElement>,
) {
    if depth >= max_depth || Instant::now() > deadline {
        return;
    }

    if let Ok(name) = element.get_name() {
        if !name.is_empty() {
            let name_norm = norm_dashes(&name).to_ascii_lowercase();
            if name.chars().count() <= target_len.saturating_mul(4) {
                let mut is_match = false;
                if target_norm_lower.contains(&name_norm) {
                    is_match = true;
                } else if name_norm.contains(target_norm_lower) {
                    if target_len >= 4 {
                        is_match = true;
                    } else {
                        is_match = regex::Regex::new(&format!(
                            r"(?i)\b{}\b",
                            regex::escape(target_norm_lower)
                        ))
                        .map(|re| re.is_match(&name_norm))
                        .unwrap_or(false);
                    }
                }
                if is_match {
                    if let Ok(ct) = element.get_control_type() {
                        if !is_container_role(ct) {
                            if let Some(want) = desired_ct {
                                if ct == want {
                                    candidates.push(element.clone());
                                }
                            } else {
                                candidates.push(element.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    if let Ok(mut child) = walker.get_first_child(element) {
        loop {
            walk_recursive(
                &child,
                walker,
                depth + 1,
                max_depth,
                deadline,
                target_norm_lower,
                target_len,
                desired_ct,
                candidates,
            );
            if Instant::now() > deadline {
                break;
            }
            match walker.get_next_sibling(&child) {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }
}

// Silence unused-import warnings on non-windows targets (module is cfg-gated).
#[allow(dead_code)]
fn _scope_hint(_: TreeScope) {}
