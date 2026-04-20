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

use super::LocateResult;
use crate::capture::Rect;
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uiautomation::controls::ControlType;
use uiautomation::types::TreeScope;
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetTopWindow, GetWindow, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, GW_HWNDNEXT,
};

/// Map our schema roles → UIA ControlType. `None` means "any role".
fn role_to_control_type(role: &str) -> Option<ControlType> {
    match role.to_ascii_lowercase().as_str() {
        "button" => Some(ControlType::Button),
        "tab" => Some(ControlType::TabItem),
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
    let target_norm = norm_dashes(&target.to_ascii_lowercase());
    let pattern = format!(r"(?i)^[\W_]*{}[\W_]*$", regex::escape(&target_norm));
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
        "Progman",              // Desktop window
        "WorkerW",              // Desktop worker
        "Shell_TrayWnd",        // Taskbar
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

/// Public entry point. Returns the first element whose accessible name
/// satisfies the anchored regex and whose role matches (when specified).
///
/// `timeout_ms` caps both the UIA matcher's internal timeout and the
/// manual-walk fallback.
pub fn find_element(
    target_text: &str,
    target_role: Option<&str>,
    timeout_ms: u64,
) -> Result<Option<LocateResult>> {
    if target_text.trim().is_empty() {
        return Ok(None);
    }

    let automation = UIAutomation::new().map_err(|e| anyhow!("UIAutomation init: {e}"))?;
    let name_re = Arc::new(build_name_regex(target_text)?);
    let target_norm_len = norm_dashes(target_text).chars().count();
    let desired_ct = target_role.and_then(role_to_control_type);

    // Decide which top-level window(s) to search.
    let (fg_hwnd, fg_pid) = foreground_pid();
    let our_pid = own_pid();

    let search_roots: Vec<UIElement> = if fg_pid != 0 && fg_pid != our_pid {
        // Normal case — just the foreground window.
        match automation.element_from_handle(fg_hwnd.into()) {
            Ok(el) => vec![el],
            Err(_) => Vec::new(),
        }
    } else {
        // Our panel is focused (or we couldn't get a foreground PID).
        // Walk the top-level windows in z-order (topmost first) and keep only
        // visible, reasonably-sized ones owned by other processes. This puts
        // the app the user was actually looking at at the front of the list.
        let hwnds = collect_visible_top_windows(our_pid, 8);
        hwnds
            .into_iter()
            .filter_map(|h| automation.element_from_handle(h.into()).ok())
            .collect()
    };

    if search_roots.is_empty() {
        return Ok(None);
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    // Give each root a fair slice so we don't burn the whole budget on the
    // first window. Floor at 250 ms so UIMatcher has time to walk.
    let per_root_ms = ((timeout_ms / search_roots.len() as u64).max(250)).min(timeout_ms);

    for root in &search_roots {
        if Instant::now() > deadline {
            break;
        }
        // Pass 1: regex filter with role constraint (fast path via UIMatcher).
        if let Some(res) = match_in_subtree(&automation, root, &name_re, desired_ct, per_root_ms)? {
            return Ok(Some(res));
        }
        // Pass 2: regex filter with no role constraint, relying on
        // element_to_result to reject containers. Catches "Continue" labelled
        // as Hyperlink instead of Button, etc.
        if desired_ct.is_some() {
            if let Some(res) = match_in_subtree(&automation, root, &name_re, None, per_root_ms)? {
                return Ok(Some(res));
            }
        }
        // Pass 3: slow manual walk with substring match — catches edges the
        // regex misses (e.g. accessible names with stray whitespace).
        if let Some(res) = manual_walk(
            root,
            &norm_dashes(&target_text.to_ascii_lowercase()),
            target_norm_len,
            desired_ct,
            deadline,
        ) {
            return Ok(Some(res));
        }
    }

    Ok(None)
}

/// Use UIMatcher with a regex-based filter_fn (+ optional ControlType) to
/// find the first match within `root`'s subtree.
fn match_in_subtree(
    automation: &UIAutomation,
    root: &UIElement,
    name_re: &Arc<Regex>,
    control_type: Option<ControlType>,
    timeout_ms: u64,
) -> Result<Option<LocateResult>> {
    let re = name_re.clone();
    let mut matcher = automation
        .create_matcher()
        .from_ref(root)
        .depth(15)
        .timeout(timeout_ms)
        .filter_fn(Box::new(move |el: &UIElement| {
            // Swallow per-element errors so a single transient failure
            // (E_ELEMENTNOTAVAILABLE during dynamic-UI walks) doesn't abort
            // the whole matcher with not-found.
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
    match matcher.find_first() {
        Ok(el) => Ok(element_to_result(&el)),
        Err(_) => Ok(None), // not-found is the common case, not an error
    }
}

/// Manual tree walk capped at `deadline`. Mirrors the v0.3
/// `_search_descendants` slow path: substring match either direction, with
/// the 4×-length container filter and an optional role preference.
fn manual_walk(
    root: &UIElement,
    target_norm_lower: &str,
    target_len: usize,
    desired_ct: Option<ControlType>,
    deadline: Instant,
) -> Option<LocateResult> {
    let mut role_match: Option<UIElement> = None;
    let mut role_agnostic_match: Option<UIElement> = None;
    walk_recursive(
        root,
        0,
        8,
        deadline,
        target_norm_lower,
        target_len,
        desired_ct,
        &mut role_match,
        &mut role_agnostic_match,
    );
    let candidate = role_match.or(role_agnostic_match)?;
    element_to_result(&candidate)
}

#[allow(clippy::too_many_arguments)]
fn walk_recursive(
    element: &UIElement,
    depth: u32,
    max_depth: u32,
    deadline: Instant,
    target_norm_lower: &str,
    target_len: usize,
    desired_ct: Option<ControlType>,
    role_match: &mut Option<UIElement>,
    role_agnostic_match: &mut Option<UIElement>,
) {
    if depth >= max_depth || Instant::now() > deadline || role_match.is_some() {
        return;
    }

    // Check this element.
    if let Ok(name) = element.get_name() {
        if !name.is_empty() {
            let name_norm = norm_dashes(&name).to_ascii_lowercase();
            // Skip elements whose name is much longer than target — container
            // titles that happen to contain the target as a substring.
            if name.chars().count() <= target_len.saturating_mul(4)
                && (name_norm.contains(target_norm_lower) || target_norm_lower.contains(&name_norm))
            {
                if let Ok(ct) = element.get_control_type() {
                    if !is_container_role(ct) {
                        if let Some(want) = desired_ct {
                            if ct == want && role_match.is_none() {
                                *role_match = Some(element.clone());
                            } else if role_agnostic_match.is_none() {
                                *role_agnostic_match = Some(element.clone());
                            }
                        } else if role_agnostic_match.is_none() {
                            *role_agnostic_match = Some(element.clone());
                        }
                    }
                }
            }
        }
    }

    if role_match.is_some() {
        return;
    }

    // Recurse via the control-view walker (skips noise elements).
    let automation = match UIAutomation::new() {
        Ok(a) => a,
        Err(_) => return,
    };
    let walker = match automation.get_control_view_walker() {
        Ok(w) => w,
        Err(_) => return,
    };
    if let Ok(mut child) = walker.get_first_child(element) {
        loop {
            walk_recursive(
                &child,
                depth + 1,
                max_depth,
                deadline,
                target_norm_lower,
                target_len,
                desired_ct,
                role_match,
                role_agnostic_match,
            );
            if role_match.is_some() || Instant::now() > deadline {
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
