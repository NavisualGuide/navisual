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

use super::trace::{A11yCandidate, A11yTrace, BboxProbe};
use super::LocateResult;
use crate::capture::Rect;
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uiautomation::controls::ControlType;
use uiautomation::core::UICondition;
use uiautomation::types::{Point, TreeScope, UIProperty};
use uiautomation::variants::Variant;
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::Foundation::{FALSE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetForegroundWindow, GetTopWindow, GetWindow, GetWindowLongW,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_STYLE, GW_HWNDNEXT,
    GW_OWNER, WS_POPUP,
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
    let hwnd = HWND(hwnd_raw as *mut _);
    if class_is_chromium(hwnd) {
        return true;
    }
    // WebView2 hybrids (new Outlook "olk", Teams 2.0, Tauri apps) host Chromium
    // in a CHILD window while the top-level class and UIA FrameworkId both read
    // as native (Win32/XAML) — so a top-level-only check misroutes them to the
    // Eager path and they never get primed/kept-warm. Verified on new Outlook:
    // child classes include Chrome_WidgetWin_0/1.
    has_chromium_child(hwnd)
}

fn class_is_chromium(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    unsafe {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        n > 0 && String::from_utf16_lossy(&buf[..n as usize]).starts_with("Chrome_WidgetWin")
    }
}

fn has_chromium_child(hwnd: HWND) -> bool {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let found = &mut *(lparam.0 as *mut bool);
        if class_is_chromium(hwnd) {
            *found = true;
            return FALSE; // stop enumeration
        }
        TRUE
    }
    let mut found = false;
    unsafe {
        // EnumChildWindows walks ALL descendants, not just direct children.
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(callback),
            LPARAM(&mut found as *mut bool as isize),
        );
    }
    found
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

/// Strip a trailing keyboard accelerator / mnemonic that menus append to the accessible name —
/// `"Playback Alt+I"` → `"Playback"`, `"Save\tCtrl+S"` → `"Save"`, `"文件(&F)"` → `"文件"`. Many
/// Win32/Qt menu bars expose the shortcut as part of the UIA Name, which defeats the anchored
/// `^target$` match (VLC's menu bar, confirmed via Accessibility Insights). Conservative: requires
/// a real modifier+key or a `(&X)` mnemonic so ordinary labels are never truncated.
fn strip_accelerator(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(?:[\s\u{00a0}]+(?:alt|ctrl|shift|win|cmd|meta)\+\S+|\s*\(&\w\))\s*$")
            .unwrap()
    });
    re.replace(s, "").trim().to_string()
}

/// Strip ONE trailing parenthesized suffix from an accessible name —
/// `"Auto (Bridge View)"` → `"Auto"`. Adobe's custom toolkit (Lightroom,
/// Photoshop family) suffixes every exposed element name with its view class,
/// which defeats the anchored `^target$` match. Conservative: only a trailing
/// `( … )` group is removed, so names with internal parens are untouched.
fn strip_paren_suffix(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\s*\([^()]*\)\s*$").unwrap());
    re.replace(s, "").trim().to_string()
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

/// Interactive (clickable / focusable) control types — the probe accepts only these.
/// Mirrors `hit_test::is_interactive`.
fn is_interactive_ct(ct: ControlType) -> bool {
    matches!(
        ct,
        ControlType::Button
            | ControlType::Hyperlink
            | ControlType::MenuItem
            | ControlType::TabItem
            | ControlType::ListItem
            | ControlType::CheckBox
            | ControlType::RadioButton
            | ControlType::ComboBox
            | ControlType::SplitButton
            | ControlType::TreeItem
    )
}

/// Does the resolved control type satisfy the AI's requested role? Exact match, plus a lenient
/// "button" family — web UIs surface buttons inconsistently as Button / Hyperlink / SplitButton /
/// MenuItem. All other roles must match exactly (so a textbox request can't accept a button).
fn ct_family_matches(resolved: ControlType, want: ControlType) -> bool {
    resolved == want
        || (want == ControlType::Button
            && matches!(
                resolved,
                ControlType::Button
                    | ControlType::SplitButton
                    | ControlType::Hyperlink
                    | ControlType::MenuItem
            ))
}

/// True when `b`'s centre is outside `ai` expanded ±300% on each side (a 7× keep-box).
/// Mirrors the OCR ai-bbox filter — generous enough that a slightly-off bbox doesn't reject a
/// real match, but a stray same-named element across the screen is caught.
fn center_outside_expanded(b: Rect, ai: Rect) -> bool {
    let pad_x = ai.width as f32 * 3.0;
    let pad_y = ai.height as f32 * 3.0;
    let cx = b.x as f32 + b.width as f32 / 2.0;
    let cy = b.y as f32 + b.height as f32 / 2.0;
    cx < ai.x as f32 - pad_x
        || cx > (ai.x + ai.width as i32) as f32 + pad_x
        || cy < ai.y as f32 - pad_y
        || cy > (ai.y + ai.height as i32) as f32 + pad_y
}

/// Find a Button inside a composite item (the close X is a child of a browser TabItem, not
/// reachable by walking up from a point on the tab body). Returns the first Button descendant,
/// preferring one whose name mentions "close". Control-type id 50000 = Button.
fn find_button_descendant(automation: &UIAutomation, parent: &UIElement) -> Option<UIElement> {
    let cond = automation
        .create_property_condition(UIProperty::ControlType, Variant::from(50000i32), None)
        .ok()?;
    let buttons = parent.find_all(TreeScope::Descendants, &cond).ok()?;
    let mut first: Option<UIElement> = None;
    for b in buttons {
        if b.get_name()
            .map(|n| n.to_ascii_lowercase().contains("close"))
            .unwrap_or(false)
        {
            return Some(b);
        }
        if first.is_none() {
            first = Some(b);
        }
    }
    first
}

/// AI-bbox hit-test probe — the name-agnostic fallback. `ElementFromPoint` is a *spatial*
/// query: it reaches on-screen controls the role-family `find_all` can miss (a browser tab's
/// close button) and sidesteps name mismatches (Chrome names it "Close", the AI said "Close
/// tab"). The bbox is NOT trusted blindly — we VERIFY the element it lands on: walk up to the
/// nearest interactive control, then accept only if its role matches the AI's requested family
/// and its rect is control-sized (≤ 20× the bbox area, on-screen). A bad bbox lands on the
/// wrong thing / a container and is rejected → no pointer (the safe outcome).
fn ai_bbox_probe(
    automation: &UIAutomation,
    ai_bbox: Rect,
    desired_ct: Option<ControlType>,
) -> (Option<LocateResult>, BboxProbe) {
    let mut probe = BboxProbe {
        attempted: true,
        ..Default::default()
    };
    let cx = ai_bbox.x + ai_bbox.width as i32 / 2;
    let cy = ai_bbox.y + ai_bbox.height as i32 / 2;

    let Ok(mut el) = automation.element_from_point(Point::new(cx, cy)) else {
        probe.detail = "element_from_point failed".to_string();
        return (None, probe);
    };
    // The deepest element under the point is often a Text/Image run inside the control —
    // walk up to the first interactive ancestor, stopping at a container (Window/Pane).
    let walker = automation.get_control_view_walker().ok();
    let mut found_ct: Option<ControlType> = None;
    for _ in 0..4 {
        let Ok(ct) = el.get_control_type() else { break };
        if is_interactive_ct(ct) {
            found_ct = Some(ct);
            break;
        }
        if is_container_role(ct) {
            break; // reached a window/pane before any interactive ancestor
        }
        match walker.as_ref().and_then(|w| w.get_parent(&el).ok()) {
            Some(parent) => el = parent,
            None => break,
        }
    }

    let Some(mut ct) = found_ct else {
        probe.detail = "no interactive control under bbox".to_string();
        return (None, probe);
    };

    // Descend for a "close the tab" target. The AI asks for a `button` and points at the tab,
    // but the X is a *child* of the TabItem — ElementFromPoint on the tab body resolves the
    // TabItem, and the X is off to the side, so walking UP can't reach it. When we wanted a
    // Button but landed on a composite item (tab / list row / tree row), descend to a Button
    // inside it (Chrome's tab close button is a child Button named "Close").
    let mut descended = false;
    if desired_ct == Some(ControlType::Button)
        && matches!(
            ct,
            ControlType::TabItem | ControlType::ListItem | ControlType::TreeItem
        )
    {
        if let Some(btn) = find_button_descendant(automation, &el) {
            el = btn;
            ct = ControlType::Button;
            descended = true;
        }
    }

    probe.resolved_role = Some(format!("{ct:?}"));
    probe.resolved_name = el.get_name().ok().filter(|s| !s.is_empty());

    // Role-family validation: when the AI named a role, the resolved control must match it
    // (so a bbox that drifted onto the whole TabItem can't satisfy a "button" request).
    if let Some(want) = desired_ct {
        if !ct_family_matches(ct, want) {
            probe.detail = format!("role mismatch: {ct:?} ≠ {want:?}");
            return (None, probe);
        }
    }

    let Ok(rect) = el.get_bounding_rectangle() else {
        probe.detail = "element has no rect".to_string();
        return (None, probe);
    };
    let (left, top) = (rect.get_left(), rect.get_top());
    let (w, h) = (rect.get_width().max(0) as u32, rect.get_height().max(0) as u32);
    if w == 0 || h == 0 || !rect_is_onscreen(left, top) {
        probe.detail = "element rect off-screen/empty".to_string();
        return (None, probe);
    }
    // Size guard: a real control is ~bbox-sized; a far larger rect means the probe walked up
    // into a container (or landed on a big element the AI didn't mean).
    let bbox_area = (ai_bbox.width.max(1) as u64) * (ai_bbox.height.max(1) as u64);
    if (w as u64) * (h as u64) > bbox_area.saturating_mul(20) {
        probe.detail = format!(
            "rect too large ({w}×{h} vs bbox {}×{})",
            ai_bbox.width, ai_bbox.height
        );
        return (None, probe);
    }

    probe.accepted = true;
    probe.detail = if descended {
        "accepted (descended item → close Button)".to_string()
    } else {
        "accepted".to_string()
    };
    let result = LocateResult {
        bbox: Rect {
            x: left,
            y: top,
            width: w,
            height: h,
        },
        name: probe.resolved_name.clone().unwrap_or_default(),
        role: format!("{ct:?}"),
        confidence: 1.0,
    };
    (Some(result), probe)
}

// ---------------------------------------------------------------------------
// S.1 / S.3 — Structured-Context Locator (v0.7 Workstream S)
// ---------------------------------------------------------------------------

/// S.1 cap: more surviving elements than this → the whole block is skipped, never
/// truncated (Decision 4 — a partial list biases the model toward "the answer must
/// be in here"). Raised from 80 (2026-07-05): a live VS Code window with a busy
/// Explorer tree + an extension side panel measured 125 qualifying elements at
/// ~$0.001/request extra cost even at 2-3x that — token cost is negligible here,
/// so the cap is sized for real-world headroom, not budget.
pub const CONTEXT_ELEMENTS_CAP: usize = 200;
/// S.1 hard budget; exceeded → skip the block (the AI call proceeds without it).
/// Raised 300→500 (2026-07-05, VS Code's raw query alone measured ~287 ms) then
/// 500→1000 (2026-07-06): a live SolidWorks session crept from ~400 ms early on to
/// 520/780 ms later as the FeatureManager tree grew with the design, tipping over
/// the 500 ms floor with no engine change — headroom against normal tree growth,
/// not a new failure mode. Still well under the multi-second AI call this precedes.
///
/// This bound is *advisory only* for the general (non-Excel) bulk path below — it's
/// checked after `find_all_build_cache` already returned, so it can decide to keep or
/// discard a result but can never cut the underlying blocking COM call short (Lightroom
/// Classic measured 2.2-5.7s here regardless of this value; see `context_window_is_slow`
/// for the mechanism that actually bounds wall-clock wait time).
pub(crate) const CONTEXT_BUDGET_MS: u128 = 1000;

/// S.1 adaptive skip — windows whose enumeration has already proven itself unproductive
/// this session. Deliberately identity-agnostic: no window class, no app name, no
/// executable check. It reacts only to *observed behaviour* (did the bulk query finish in
/// time on this specific window), so it automatically covers any app with this pathology —
/// discovered or not — and can't be broken by a vendor renaming a window class out from
/// under a hardcoded check the way `is_excel`/`EXCEL_MAIN_WINDOW_CLASS` could be.
///
/// Root cause this exists for (2026-07-07, Lightroom Classic): its ~370 UIA nodes are all
/// `Pane`/`IsControlElement=false` (confirmed live), so `find_all_build_cache`'s control-view
/// filter can only ever return empty — but the underlying COM enumeration still has to
/// physically walk the whole raw tree to determine that, so the cost (2.2-5.7s measured)
/// scales with tree size even though the result never does. Raising `CONTEXT_BUDGET_MS`
/// doesn't help this case (see its doc comment) since the call itself can't be shortened,
/// only abandoned — which is exactly what the timeout in `enumerate_context_snapshot_bounded`
/// (lib.rs) does, recording the outcome here.
static CONTEXT_SLOW_WINDOWS: OnceLock<Mutex<HashMap<usize, u32>>> = OnceLock::new();
/// Consecutive timeouts required before a window is skipped outright. Kept above 1 so a
/// single transient blip (a GC pause, a disk hiccup) on an otherwise-fine window can't
/// permanently blacklist it for the rest of the session — Lightroom Classic timed out 5/5
/// times by a wide margin (2.2-5.7s vs a 1s budget), so a threshold of 2 still catches the
/// real case almost immediately.
const CONTEXT_SLOW_THRESHOLD: u32 = 2;

/// Has `hwnd` already timed out enough times in a row to skip trying again this session?
pub(crate) fn context_window_is_slow(hwnd: usize) -> bool {
    let cell = CONTEXT_SLOW_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    cell.lock().get(&hwnd).copied().unwrap_or(0) >= CONTEXT_SLOW_THRESHOLD
}

/// Record that enumeration against `hwnd` didn't finish inside the wait budget.
pub(crate) fn context_window_mark_slow(hwnd: usize) {
    let cell = CONTEXT_SLOW_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    *cell.lock().entry(hwnd).or_insert(0) += 1;
}

/// Record that enumeration against `hwnd` completed within budget, clearing any prior
/// strikes — a window that's fast now shouldn't stay penalised by an old, unrelated blip.
pub(crate) fn context_window_mark_fast(hwnd: usize) {
    let cell = CONTEXT_SLOW_WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    cell.lock().remove(&hwnd);
}

/// Interactive control types enumerated for the Structured-Context list — the
/// CLICKABLE family from `role_control_type_ids` plus Edit/Slider (inputs a "textbox"/
/// "slider" target selects). Deliberately excludes bulk Text/Pane — they're what
/// blows the cap (S.1).
const CONTEXT_CT_IDS: &[i32] = &[
    50000, // Button
    50019, // TabItem
    50007, // ListItem
    50011, // MenuItem
    50005, // Hyperlink
    50004, // Edit
    50003, // ComboBox
    50002, // CheckBox
    50013, // RadioButton
    50015, // Slider
    50031, // SplitButton
    50024, // TreeItem
];

/// The control types [`CONTEXT_CT_IDS`] enumerates — the walk-up stop set for the
/// live verification. `is_interactive_ct` plus Edit/Slider.
fn is_context_ct(ct: ControlType) -> bool {
    is_interactive_ct(ct) || matches!(ct, ControlType::Edit | ControlType::Slider)
}

/// Accessible name as the snapshot stores/compares it: trimmed, paren-suffix +
/// accelerator stripped (reuses the matcher's normalisers).
fn context_display_name(raw: &str) -> String {
    strip_accelerator(&strip_paren_suffix(raw.trim()))
}

/// S.3 role-family agreement between the live element and the snapshot entry. Exact
/// control type, or both within the lenient button family (web UIs surface buttons
/// inconsistently as Button/SplitButton/Hyperlink/MenuItem — mirrors `ct_family_matches`).
fn context_role_compatible(live: &str, snapshot: &str) -> bool {
    const BUTTON_FAMILY: &[&str] = &["Button", "SplitButton", "Hyperlink", "MenuItem"];
    live == snapshot || (BUTTON_FAMILY.contains(&live) && BUTTON_FAMILY.contains(&snapshot))
}

/// Excel's ribbon/scrollbar automation provider class — its own `FindAll(Descendants)`
/// never terminates correctly. Confirmed live 2026-07-06: sampled "Line up" instances at
/// array positions 0/15/30/45/60 (of 61 total) all shared one identical RuntimeId,
/// ClassName, rect, and zero children — not 61 real siblings, the same element returned 61
/// times. Word and OneNote expose the exact same `NetUI*`/`NUIScrollbar` framework classes
/// with zero duplication, so this is specific to Excel's scrollbar, not the shared Office
/// ribbon chrome.
///
/// It's worse than a single bad branch, though: the scrollbar pane's own container (a small
/// `NetUIHWNDElement`) contains ANOTHER copy of the same `[NUIScrollbar, NUIScrollbar,
/// XLCTL, ExcelGrid]` group nested inside itself — a self-referential structure, not a
/// fixed depth. So a plain `find_all(Descendants)` explodes to 1,200+ duplicate nodes and
/// blows the budget (1.8–4 s). [`excel_collect_candidates`] instead does a pruned collecting
/// walk: it skips the `NUIScrollbar` branch outright and stops recursing into any
/// (ClassName, rect) signature already visited (which breaks the self-nesting loop), while
/// collecting each context-type node as it goes.
pub(crate) const EXCEL_BROKEN_SCROLLBAR_CLASS: &str = "NUIScrollbar";
/// Excel's main window class (`XLMAIN`) — gates the workaround below to Excel specifically,
/// so every other app's enumeration takes the original single-bulk-search path, unchanged.
pub(crate) const EXCEL_MAIN_WINDOW_CLASS: &str = "XLMAIN";
/// The worksheet grid pane's class — a sibling of the broken scrollbar panes under `XLDESK`,
/// not their descendant. Its own children (incl. the sheet-tab strip) are only exposed via a
/// true `Descendants` traversal, not `Children` — see `excel_pruned_walk`.
pub(crate) const EXCEL_GRID_CLASS: &str = "ExcelGrid";
/// Safety bound on the collecting-walk depth, independent of the (class, rect) dedup (which
/// is what actually stops the self-nesting) — just prevents runaway recursion if some other,
/// truly-infinite pattern is ever found. Excel's real tree is < 8 deep.
pub(crate) const SCROLLBAR_SCAN_DEPTH: u32 = 12;
/// Excel gets a larger budget than [`CONTEXT_BUDGET_MS`]: the pruned collecting walk is
/// inherently ~290 ms (measured, cached) vs the sub-100 ms bulk search other apps use, and a
/// PivotTable field pane pushes it higher — so the general 500 ms budget would spuriously
/// skip Excel even though the walk is correct and bounded. This is a one-time cost before the
/// multi-second AI call, so ~1 s of headroom is an acceptable trade for Structured-Context
/// actually working on Excel.
pub(crate) const EXCEL_CONTEXT_BUDGET_MS: u128 = 1500;

/// (ClassName, (left, top, width, height)) — a structural signature for the walk's dedup.
pub(crate) type ClassRectSignature = (String, (i32, i32, i32, i32));

/// Shared pruned walk for Excel: one cached round-trip per container
/// (`find_all_build_cache(Children)`), visiting every descendant self-inclusively while
/// pruning (a) the broken [`EXCEL_BROKEN_SCROLLBAR_CLASS`] branch and (b) any (ClassName,
/// rect) signature already visited (breaks the self-nested `NetUIHWNDElement` loop). The
/// requested `cache` rides along on every element `visit` receives, so callers read cached
/// props with zero further COM. Calls `visit(element, class_name)` for every surviving node
/// before recursing into it.
///
/// `ExcelGrid` gets a special case: it's a sibling of the broken scrollbar panes (not their
/// descendant, confirmed live), but its own children are only exposed via a true
/// `Descendants` COM traversal, not step-by-step `Children` queries — a plain `Children` walk
/// finds 0, silently missing content that lives inside it (e.g. the sheet-tab strip: Sheet1,
/// Add Sheet, Scroll Left/Right). Since `ExcelGrid` itself is outside the broken branch, one
/// bounded `find_all_build_cache(Descendants, gc, cache)` scoped to just that node is safe —
/// verified live (172 ms, no explosion). Pass `grid_cond: Some(cond)` to run that supplement
/// (each match also visited); `None` skips it (e.g. the cell adapter only needs the
/// `ExcelGrid` node itself, not what's inside it).
///
/// `pub(crate)` so both the Structured-Context enumeration (this module) and the Excel cell
/// adapter (`adapters::excel`) share the exact same pruning — they hit the identical
/// broken-scrollbar problem via their own tree walks, and duplicating this logic in two
/// places would only need to drift out of sync once to reintroduce the bug in one of them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn excel_pruned_walk(
    el: &UIElement,
    depth_budget: u32,
    true_cond: &UICondition,
    grid_cond: Option<&UICondition>,
    cache: &uiautomation::core::UICacheRequest,
    seen: &mut std::collections::HashSet<ClassRectSignature>,
    deadline: Instant,
    visit: &mut impl FnMut(&UIElement, &str),
) {
    if depth_budget == 0 || Instant::now() >= deadline {
        return;
    }
    let Ok(children) = el.find_all_build_cache(TreeScope::Children, true_cond, cache) else {
        return;
    };
    for child in children {
        let class_name = child.get_cached_classname().unwrap_or_default();
        if class_name == EXCEL_BROKEN_SCROLLBAR_CLASS {
            continue; // known-broken scrollbar — never recurse into it
        }
        if class_name == EXCEL_GRID_CLASS {
            if let Some(gc) = grid_cond {
                if let Ok(els) = child.find_all_build_cache(TreeScope::Descendants, gc, cache) {
                    for el in &els {
                        let cn = el.get_cached_classname().unwrap_or_default();
                        visit(el, &cn);
                    }
                }
            }
        }
        if let Ok(r) = child.get_cached_bounding_rectangle() {
            let key = (
                class_name.clone(),
                (r.get_left(), r.get_top(), r.get_width(), r.get_height()),
            );
            if !seen.insert(key) {
                continue; // same (class, rect) already visited — the self-nested repeat
            }
        }
        visit(&child, &class_name);
        excel_pruned_walk(
            &child,
            depth_budget - 1,
            true_cond,
            grid_cond,
            cache,
            seen,
            deadline,
            visit,
        );
    }
}

/// S.1 — enumerate the interactive, named, on-screen elements of `hwnd_raw`'s window
/// (+ its owned dialogs/popups) for the Structured-Context prompt block. One batched
/// `find_all_build_cache` per root over [`CONTEXT_CT_IDS`] (Decision 5 — the
/// `deep_role_match` machinery: native OR-condition, cached Name/ControlType/Rect/
/// IsOffscreen, zero per-element COM). `Err(reason)` ⇒ the caller skips the whole
/// block (never truncates): framework `Other` (Lightroom's all-Pane tree, Photoshop's
/// 1-node tree — exactly where the list would be empty/garbage), over-cap, over-budget,
/// or no roots. Rects are virtual-desktop physical pixels.
pub fn enumerate_context_elements(hwnd_raw: usize) -> Result<Vec<super::ContextElement>, String> {
    let started = Instant::now();
    if hwnd_raw == 0 {
        return Err("no target window".to_string());
    }
    let automation = UIAutomation::new().map_err(|e| format!("UIAutomation init: {e}"))?;
    let framework = framework_of(&automation, hwnd_raw);
    if framework == Framework::Other {
        return Err("framework Other".to_string());
    }

    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
    let mut win_rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut win_rect).map_err(|e| format!("GetWindowRect: {e}"))?;
    }
    let win_w = (win_rect.right - win_rect.left).max(1) as i64;
    let win_h = (win_rect.bottom - win_rect.top).max(1) as i64;

    // Roots: owned dialogs/popups first (the active interaction surface), then the
    // main window — the same root set `find_element` searches.
    let mut roots: Vec<UIElement> = Vec::new();
    unsafe {
        for popup in collect_owned_popups(hwnd) {
            if let Ok(el) = automation.element_from_handle(popup.into()) {
                roots.push(el);
            }
        }
        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
            if let Ok(el) = automation.element_from_handle(hwnd.into()) {
                roots.push(el);
            }
        }
    }
    if roots.is_empty() {
        return Err("no search roots".to_string());
    }

    // Excel's scrollbar provider makes a bulk `find_all(Descendants)` explode into 1,200+
    // duplicate nodes (see EXCEL_BROKEN_SCROLLBAR_CLASS) — detect it and use the pruned
    // collecting walk instead, with a larger budget. Every other app keeps the fast bulk path.
    let is_excel = roots
        .iter()
        .any(|r| matches!(r.get_classname(), Ok(c) if c == EXCEL_MAIN_WINDOW_CLASS));
    let budget_ms = if is_excel {
        EXCEL_CONTEXT_BUDGET_MS
    } else {
        CONTEXT_BUDGET_MS
    };

    let cond = {
        let mut cond = None;
        for &id in CONTEXT_CT_IDS {
            let c = automation
                .create_property_condition(UIProperty::ControlType, Variant::from(id), None)
                .map_err(|e| format!("condition: {e}"))?;
            cond = Some(match cond.take() {
                None => c,
                Some(prev) => automation
                    .create_or_condition(prev, c)
                    .map_err(|e| format!("or-condition: {e}"))?,
            });
        }
        cond.ok_or_else(|| "empty condition".to_string())?
    };
    let cache = automation
        .create_cache_request()
        .map_err(|e| format!("cache request: {e}"))?;
    let _ = cache.add_property(UIProperty::Name);
    let _ = cache.add_property(UIProperty::ControlType);
    let _ = cache.add_property(UIProperty::BoundingRectangle);
    let _ = cache.add_property(UIProperty::IsOffscreen);
    let _ = cache.add_property(UIProperty::ClassName); // for the Excel walk's dedup

    // Gather candidate elements (context-type, with the cache attached). Two ways in:
    // Excel → the pruned collecting walk; everything else → the original bulk Descendants
    // search per root. Both yield the SAME 12 context control types, so the filter below
    // is identical for both.
    let gather_started = Instant::now();
    let candidates: Vec<UIElement> = if is_excel {
        let mut out = Vec::new();
        if let Ok(true_cond) = automation.create_true_condition() {
            let mut seen: std::collections::HashSet<ClassRectSignature> =
                std::collections::HashSet::new();
            let deadline = started + Duration::from_millis(budget_ms as u64);
            for root in &roots {
                // grid_cond: None DISABLES the ExcelGrid flat-Descendants escape hatch for
                // enumeration (audit 2026-07-13, confirmed by the excel_enum_histogram_live
                // probe against a live 209-cell CSV). That escape hatch — a raw
                // find_all_build_cache(Descendants) on the ExcelGrid pane — never prunes the
                // broken scrollbar the way this walk's own body does, so on any sheet with an
                // active scrollbar the NUIScrollbar self-nesting explodes it: measured **23,320
                // duplicate NetUIRepeatButton over 138 s**, and zero useful elements (no sheet
                // tabs). Even the foreground-abandoned attempts each leave a 138 s uninterruptible
                // COM task running. The normal recursion below (`excel_pruned_walk` at the child
                // level) already prunes the NUIScrollbar *container*, so it never reaches those
                // buttons — enumeration stays fast (~90 ms measured). Cost: the sheet-tab strip,
                // reachable only via that flat Descendants, drops out of the enumerated list on
                // sheets where it existed — a rare target that falls back to A11y/OCR, an
                // acceptable trade for never exploding. The adapter's find_grid keeps the escape
                // hatch (its grid_cond targets XLSpreadsheetGrid, one element, not the 12 context
                // types, so it can't collect the scrollbar buttons).
                excel_pruned_walk(
                    root,
                    SCROLLBAR_SCAN_DEPTH,
                    &true_cond,
                    None,
                    &cache,
                    &mut seen,
                    deadline,
                    &mut |el, _class_name| {
                        if matches!(el.get_cached_control_type(), Ok(ct) if is_context_ct(ct)) {
                            out.push(el.clone());
                        }
                    },
                );
            }
        }
        log::info!(
            "[context] excel: collecting walk gathered {} candidate(s) in {} ms",
            out.len(),
            gather_started.elapsed().as_millis()
        );
        out
    } else {
        let mut out = Vec::new();
        for root in &roots {
            // A dead popup root must not kill the main window's list.
            if let Ok(els) = root.find_all_build_cache(TreeScope::Descendants, &cond, &cache) {
                out.extend(els);
            }
        }
        out
    };

    if started.elapsed().as_millis() > budget_ms {
        return Err(format!(
            "budget exceeded ({} ms)",
            started.elapsed().as_millis()
        ));
    }

    // Single filter pass over the cached candidates (name / on-screen / geometry / dedup).
    let mut out: Vec<super::ContextElement> = Vec::new();
    let mut seen: std::collections::HashSet<(String, (i32, i32, u32, u32))> =
        std::collections::HashSet::new();
    let mut total_qualifying: usize = 0;
    for el in &candidates {
        // Non-empty display name (a glyph-only control has nothing to select by).
        let Ok(raw_name) = el.get_cached_name() else {
            continue;
        };
        let name = context_display_name(&raw_name);
        if name.is_empty() {
            continue;
        }
        // On-screen: UIA's own flag (scrolled-out list items) + rect ∩ window.
        if el.is_cached_offscreen().unwrap_or(false) {
            continue;
        }
        let Ok(ct) = el.get_cached_control_type() else {
            continue;
        };
        let Ok(rect) = el.get_cached_bounding_rectangle() else {
            continue;
        };
        let (left, top) = (rect.get_left(), rect.get_top());
        let (w, h) = (
            rect.get_width().max(0) as u32,
            rect.get_height().max(0) as u32,
        );
        if w == 0 || h == 0 || !rect_is_onscreen(left, top) {
            continue;
        }
        if left + (w as i32) <= win_rect.left
            || left >= win_rect.right
            || top + (h as i32) <= win_rect.top
            || top >= win_rect.bottom
        {
            continue;
        }
        // Control-sized: >90% of the window in either axis is a container.
        if (w as i64) * 10 > win_w * 9 || (h as i64) * 10 > win_h * 9 {
            continue;
        }
        // Dedupe exact (name, rect) repeats (Chrome doubles names).
        if !seen.insert((name.clone(), (left, top, w, h))) {
            continue;
        }
        // Keep counting past the cap (cheap — the candidates are already cached) so an
        // over-cap skip reports the true qualifying count, not just ">CAP". Stop building
        // `out` at the cap since an over-cap result is always discarded by the caller
        // (Decision 4 — skip the whole block, never truncate).
        total_qualifying += 1;
        if out.len() < CONTEXT_ELEMENTS_CAP {
            out.push(super::ContextElement {
                id: out.len() as u32 + 1,
                name,
                role: format!("{ct:?}"),
                rect: Rect {
                    x: left,
                    y: top,
                    width: w,
                    height: h,
                },
            });
        }
    }
    if total_qualifying > CONTEXT_ELEMENTS_CAP {
        return Err(format!(
            "over cap ({total_qualifying} > {CONTEXT_ELEMENTS_CAP})"
        ));
    }
    Ok(out)
}

/// S.3 — live verification of a selected context element (the `ai_bbox_probe` pattern):
/// `ElementFromPoint` at the snapshot rect's centre must resolve — walking up past text
/// runs — to a context-type control whose role family and name agree with the snapshot
/// (name equal, or one contains the other: badge/suffix drift). Returns the element's
/// **live** rect, so a control that scrolled/moved since capture resolves to something
/// else, fails the name check, and the caller falls back to the normal pipeline —
/// a stale snapshot can never place a pointer ("no pointer beats wrong pointer").
pub fn verify_context_element(snap: &super::ContextElement) -> (Option<Rect>, String) {
    let Ok(automation) = UIAutomation::new() else {
        return (None, "UIAutomation init failed".to_string());
    };
    let cx = snap.rect.x + snap.rect.width as i32 / 2;
    let cy = snap.rect.y + snap.rect.height as i32 / 2;
    let Ok(mut el) = automation.element_from_point(Point::new(cx, cy)) else {
        return (None, "element_from_point failed".to_string());
    };
    let walker = automation.get_control_view_walker().ok();
    let mut found: Option<(ControlType, String)> = None;
    for _ in 0..4 {
        let Ok(ct) = el.get_control_type() else { break };
        if is_context_ct(ct) {
            found = Some((ct, el.get_name().unwrap_or_default()));
            break;
        }
        if is_container_role(ct) {
            break; // reached a window/pane before any context-type ancestor
        }
        match walker.as_ref().and_then(|w| w.get_parent(&el).ok()) {
            Some(parent) => el = parent,
            None => break,
        }
    }
    let Some((ct, live_raw)) = found else {
        return (None, "no interactive control at snapshot point".to_string());
    };
    let live_role = format!("{ct:?}");
    if !context_role_compatible(&live_role, &snap.role) {
        return (
            None,
            format!("role mismatch: live {live_role} ≠ snapshot {}", snap.role),
        );
    }
    let live_name = context_display_name(&live_raw).to_lowercase();
    let snap_name = snap.name.to_lowercase();
    if live_name.is_empty()
        || !(live_name == snap_name
            || live_name.contains(&snap_name)
            || snap_name.contains(&live_name))
    {
        return (
            None,
            format!(
                "name mismatch: live {:?} ≠ snapshot {:?}",
                live_raw, snap.name
            ),
        );
    }
    let Ok(rect) = el.get_bounding_rectangle() else {
        return (None, "live element has no rect".to_string());
    };
    let (left, top) = (rect.get_left(), rect.get_top());
    let (w, h) = (
        rect.get_width().max(0) as u32,
        rect.get_height().max(0) as u32,
    );
    if w == 0 || h == 0 || !rect_is_onscreen(left, top) {
        return (None, "live element rect off-screen/empty".to_string());
    }
    (
        Some(Rect {
            x: left,
            y: top,
            width: w,
            height: h,
        }),
        "verified".to_string(),
    )
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
        target: isize,
        hwnds: Vec<HWND>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let state = &mut *(lparam.0 as *mut State);
        if hwnd.0 as isize == state.target {
            return TRUE; // the main window is added separately
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return TRUE;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != state.pid {
            return TRUE;
        }
        // Two kinds of separate top-level windows the main window's subtree walk can't reach:
        //  - Owned windows (GW_OWNER non-null) — modal dialogs (OK/Cancel, Find, Save As).
        //  - WS_POPUP windows — open menus / dropdowns / combo lists. VLC's Qt menu popups are
        //    WS_POPUP WITHOUT an owner, so the owner check alone missed the open submenu (Speed).
        let owned = GetWindow(hwnd, GW_OWNER)
            .map(|o| !o.0.is_null())
            .unwrap_or(false);
        let is_popup = (GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_POPUP.0) != 0;
        if owned || is_popup {
            state.hwnds.push(hwnd);
        }
        TRUE
    }

    let mut state = State {
        pid: target_pid,
        target: target.0 as isize,
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
    // The window whose UI framework we classify. With a pinned target that's it; in
    // full-screen / no-pin mode it's the window we actually search (set below), so a Chrome
    // browser isn't mislabeled `Other` (which skipped the fast path and timed out).
    let mut framework_hwnd: Option<usize> = opts.target_hwnd;

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
            framework_hwnd = Some(fg_hwnd.0 as usize);
            match automation.element_from_handle(fg_hwnd.into()) {
                Ok(el) => vec![el],
                Err(_) => Vec::new(),
            }
        } else {
            let wins = collect_visible_top_windows(our_pid, 8);
            framework_hwnd = wins.first().map(|h| h.0 as usize);
            wins.into_iter()
                .filter_map(|h| automation.element_from_handle(h.into()).ok())
                .collect()
        }
    };
    trace.search_roots_count = search_roots.len();

    if search_roots.is_empty() {
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    let framework = framework_hwnd
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
                let mut n = 0;
                candidates.extend(deep_role_match(
                    &automation,
                    root,
                    target_text,
                    opts.role.as_deref(),
                    &name_re,
                    &mut n,
                ));
                trace.element_count = Some(trace.element_count.unwrap_or(0).max(n));
                trace.cached = true;
                // Role-family miss → broad retry: the AI's role guess can disagree
                // with the app's actual control type (new Outlook's "To" is a
                // Button; the AI says textbox, so the TEXTUAL find never retrieves
                // it and the anchored name filter never sees it). Deep-find
                // analogue of the matcher's Pass 2.
                if candidates.is_empty() && role_restricts_types(opts.role.as_deref()) {
                    let mut n2 = 0;
                    candidates.extend(deep_role_match(
                        &automation,
                        root,
                        target_text,
                        None,
                        &name_re,
                        &mut n2,
                    ));
                    trace.element_count = Some(trace.element_count.unwrap_or(0).max(n2));
                }
            } else if !opts.icon_target {
                // Eager-tree frameworks (WPF/WinForms/WinUI/Win32): the standard matcher + manual
                // walk are fast and proven here. SKIPPED entirely for icon targets — a glyph has no
                // accessible name on an eager (non-Chrome) surface, and the UIA matcher is a
                // synchronous COM walk that overruns the budget (~1 s on Blender; COM-bound, so a
                // release build won't help), making it pure waste — the pack icon template locates
                // these. Chrome icon targets keep the matcher (their icons often do have names).
                // (lever c of the scoped A11y shorten)
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
                // Fallback: deeply-nested items the depth-12 walk misses (e.g. VLC's Qt menu
                // items far down the tree) — the cached native deep find reaches them. Skipped
                // for known icon-only targets (a pack icon exists): a glyph has no accessible
                // name for this find to match, so it's wasted work — template matching is the path.
                if candidates.is_empty() && !opts.icon_target {
                    let mut n = 0;
                    candidates.extend(deep_role_match(
                        &automation,
                        root,
                        target_text,
                        opts.role.as_deref(),
                        &name_re,
                        &mut n,
                    ));
                    trace.element_count = Some(trace.element_count.unwrap_or(0).max(n));
                    trace.cached = true;
                    // Broad retry on a role-family miss (see the Chrome branch).
                    if candidates.is_empty() && role_restricts_types(opts.role.as_deref()) {
                        let mut n2 = 0;
                        candidates.extend(deep_role_match(
                            &automation,
                            root,
                            target_text,
                            None,
                            &name_re,
                            &mut n2,
                        ));
                        trace.element_count = Some(trace.element_count.unwrap_or(0).max(n2));
                    }
                }
            }
        }
        if !candidates.is_empty() {
            break;
        }
    }

    // Last resort — Pane fallback (raw-view walk) for custom-toolkit apps whose
    // controls only exist in the raw view (Adobe Lightroom family). Only reached
    // when every pass above returned zero candidates, so normal apps never pay
    // for it; skipped on Chromium (its raw tree is huge and its controls are
    // properly typed, so the walk would cost seconds and find nothing new).
    // Skipped for known icon-only targets: a 2.5 s raw-view walk can't find a nameless glyph,
    // and template matching covers it — this is the bulk of the A11y-shorten win on sparse apps.
    if candidates.is_empty() && !is_chrome && !opts.icon_target {
        for root in &search_roots {
            let mut n = 0;
            candidates.extend(pane_fallback_match(&automation, root, &name_re, &mut n));
            trace.element_count = Some(trace.element_count.unwrap_or(0).max(n));
            if !candidates.is_empty() {
                break; // first root with a hit wins — don't pay for the rest
            }
        }
        if !candidates.is_empty() {
            trace.pane_fallback = true;
        }
    }

    // "Wrong spot" memory: drop candidates centred inside the bbox the user just
    // rejected, so the correction retry can surface the second-best match.
    if let Some(av) = opts.avoid_bbox {
        candidates.retain(|c| {
            let cx = c.bbox.x + c.bbox.width as i32 / 2;
            let cy = c.bbox.y + c.bbox.height as i32 / 2;
            !(cx >= av.x
                && cx < av.x + av.width as i32
                && cy >= av.y
                && cy < av.y + av.height as i32)
        });
    }

    if candidates.is_empty() {
        // Name search found nothing. Before giving up, try the AI-bbox probe: ElementFromPoint
        // at the AI's predicted point, verified by role + size (see `ai_bbox_probe`). Gated by
        // per-model bbox trust — not as the safety mechanism (the verification is), but to avoid
        // probing a known-unreliable bbox (free-tier Nemotron) that could land on a coincidental
        // small control. The outcome is recorded either way so the debug drawer shows it.
        // Skipped for icon targets: we have a template for the glyph, and the probe found "no
        // interactive control" on icon-only surfaces anyway — so it's wasted A11y time (part of
        // the scoped A11y shorten).
        if let Some(ai) = opts.ai_bbox {
            if opts.bbox_decisive && !opts.icon_target {
                let (probe_hit, probe) = ai_bbox_probe(&automation, ai, desired_ct);
                trace.bbox_probe = Some(probe);
                if let Some(hit) = probe_hit {
                    trace.candidates.push(A11yCandidate {
                        name: hit.name.clone(),
                        role: hit.role.clone(),
                        bbox: (hit.bbox.x, hit.bbox.y, hit.bbox.width, hit.bbox.height),
                        selected: true,
                        reject_reason: None,
                    });
                    trace.elapsed_ms = started.elapsed().as_millis() as u32;
                    return Ok((Some(hit), trace));
                }
            } else {
                // Two distinct skip reasons share this branch — say which one it was (an icon
                // target skipping the probe was mis-reported as a distrusted model, which sent
                // a live debugging session chasing the trust list).
                let detail = if opts.icon_target {
                    "skipped — icon target (template is the path; the probe can't help a glyph)"
                } else {
                    "skipped — model bbox not trusted (BBOX_DISTRUST_MODELS)"
                };
                trace.bbox_probe = Some(BboxProbe {
                    attempted: false,
                    detail: detail.to_string(),
                    ..Default::default()
                });
            }
        }
        trace.elapsed_ms = started.elapsed().as_millis() as u32;
        return Ok((None, trace));
    }

    // Rank candidates: role agreement first (an Edit named "To" must beat Text
    // tooltips when the AI asked for a textbox), then AI-bbox proximity. Both
    // bboxes are already in virtual-desktop physical pixels. The sort is
    // stable, so without a role or bbox the original pass order is preserved.
    {
        let desired_role = desired_ct.map(|ct| format!("{ct:?}"));
        let ai_center = opts
            .ai_bbox
            .map(|ai| (ai.x as f32 + ai.width as f32 / 2.0, ai.y as f32 + ai.height as f32 / 2.0));
        candidates.sort_by(|a, b| {
            let a_role_miss = desired_role.as_deref().is_some_and(|r| a.role != r);
            let b_role_miss = desired_role.as_deref().is_some_and(|r| b.role != r);
            a_role_miss.cmp(&b_role_miss).then_with(|| {
                let Some((tx, ty)) = ai_center else {
                    return std::cmp::Ordering::Equal;
                };
                let acx = a.bbox.x as f32 + a.bbox.width as f32 / 2.0;
                let acy = a.bbox.y as f32 + a.bbox.height as f32 / 2.0;
                let bcx = b.bbox.x as f32 + b.bbox.width as f32 / 2.0;
                let bcy = b.bbox.y as f32 + b.bbox.height as f32 / 2.0;
                let da = (acx - tx).powi(2) + (acy - ty).powi(2);
                let db = (bcx - tx).powi(2) + (bcy - ty).powi(2);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
    }

    // Trusted-bbox guard: if the best name match sits far outside a trusted AI bbox, it's
    // likely a stray same-named element (a Text "localhost:9876" in the screen corner, nowhere
    // near the tab the AI pointed at). Try the bbox probe and prefer its validated, in-bbox
    // element. Only fires for a trusted model + a name match well outside the bbox, and only
    // wins if the probe actually resolves a valid control there — else the name match stands.
    if opts.bbox_decisive {
        if let Some(ai) = opts.ai_bbox {
            if center_outside_expanded(candidates[0].bbox, ai) {
                let (probe_hit, probe) = ai_bbox_probe(&automation, ai, desired_ct);
                trace.bbox_probe = Some(probe);
                if let Some(hit) = probe_hit {
                    for c in &candidates {
                        trace.candidates.push(A11yCandidate {
                            name: c.name.clone(),
                            role: c.role.clone(),
                            bbox: (c.bbox.x, c.bbox.y, c.bbox.width, c.bbox.height),
                            selected: false,
                            reject_reason: Some("far from AI bbox — probe preferred".to_string()),
                        });
                    }
                    trace.candidates.push(A11yCandidate {
                        name: hit.name.clone(),
                        role: hit.role.clone(),
                        bbox: (hit.bbox.x, hit.bbox.y, hit.bbox.width, hit.bbox.height),
                        selected: true,
                        reject_reason: None,
                    });
                    trace.elapsed_ms = started.elapsed().as_millis() as u32;
                    return Ok((Some(hit), trace));
                }
            }
        }
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
            } else if desired_ct.is_some() || opts.ai_bbox.is_some() {
                Some("ranked lower (role / AI-bbox distance)".to_string())
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
            // Strip a trailing accelerator ("Playback Alt+I" → "Playback") so the anchored
            // regex matches menu items whose UIA name carries the shortcut suffix.
            let normed = strip_accelerator(&norm_dashes(&name));
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

/// Name-filter decision for the deep find, tiered by evidence strength:
/// `Some(true)` = anchored match (the name IS the target, modulo accelerator /
/// paren-suffix / keybinding-annotation decoration); `Some(false)` = loose
/// containment, allowed only when the name is **label-sized** relative to the
/// target; `None` = reject. The label-likeness cap is what keeps short targets
/// safe: "to" must match the field named "To", never the tooltip "Attach a file
/// to this item." — prose containing the target word is not a label (live Outlook
/// false hit).
fn deep_name_filter(name: &str, needle: &str, name_re: &Regex) -> Option<bool> {
    if name.is_empty() {
        return None;
    }
    let normed = strip_accelerator(&norm_dashes(name));
    if name_re.is_match(&normed) || name_re.is_match(&strip_paren_suffix(&normed)) {
        return Some(true);
    }
    // Chromium activity-bar pattern (VS Code): the UIA Name is the label followed by a
    // " (keybinding)" annotation and an optional badge, and is sometimes DOUBLED — the live
    // name is `Extensions (Ctrl+Shift+X) - 4 require restart` repeated twice. The label is
    // the leading token before the first " (" annotation; anchored-match THAT. Still exact,
    // so a short target ("To") can't latch onto a longer leading label ("Tools (Ctrl+T)…").
    if let Some(lead) = normed.split(" (").next() {
        if lead != normed && name_re.is_match(lead) {
            return Some(true);
        }
    }
    let needle_len = needle.chars().count();
    let cap = (needle_len * 3).max(needle_len + 20);
    if normed.chars().count() <= cap && normed.to_ascii_lowercase().contains(needle) {
        return Some(false);
    }
    None
}

/// Deep, role-aware matcher for Chromium/Electron windows: finds far-down items (VS Code's
/// activity bar, search boxes, …) that the standard passes miss. Uses a **native** UIA
/// control-type OR-condition so `find_all` filters *in-process* — only the few matching
/// elements cross the COM boundary, instead of a per-element round-trip over the whole tree
/// (which made the manual approach take seconds). Restricts to the control types appropriate
/// for the AI's role (a "button" target ignores bulk Text content; a "textbox" target keeps
/// Edit/Text), then name-filters via [`deep_name_filter`] — anchored matches win outright;
/// loose containment ("Search Extensions in Marketplace" for "Search Extensions") only
/// counts when no anchored match exists.
fn deep_role_match(
    automation: &UIAutomation,
    root: &UIElement,
    target: &str,
    role: Option<&str>,
    name_re: &Regex,
    // Out: count of role-matching UIA elements seen before name-filtering (0 = tree not built).
    count: &mut usize,
) -> Vec<LocateResult> {
    let needle = norm_dashes(&target.to_ascii_lowercase());
    if needle.chars().count() < 2 {
        return Vec::new();
    }
    let mut cond = None;
    for &id in role_control_type_ids(role) {
        let Ok(c) =
            automation.create_property_condition(UIProperty::ControlType, Variant::from(id), None)
        else {
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
    *count = els.len();
    let mut anchored: Vec<LocateResult> = Vec::new();
    let mut loose: Vec<LocateResult> = Vec::new();
    for el in els {
        let Ok(name) = el.get_cached_name() else {
            continue;
        };
        let Some(is_anchored) = deep_name_filter(&name, &needle, name_re) else {
            continue;
        };
        let Some(r) = element_to_result_cached(&el) else {
            continue;
        };
        if is_anchored {
            anchored.push(r);
        } else {
            loose.push(r);
        }
    }
    if anchored.is_empty() {
        loose
    } else {
        anchored
    }
}

/// Last-resort Pane fallback for custom-toolkit apps (Adobe Lightroom/Photoshop
/// family): their UIA tree types EVERY element as `ControlType.Pane` — layout
/// containers *and* real buttons (Lightroom's Auto button is
/// `Pane "Auto (Bridge View)" 48×19`). The role-family find returns 0 elements
/// there and the matcher rejects every node as a container, so the whole app
/// reads as A11y-opaque. This pass runs only when everything else produced zero
/// candidates and demands stronger evidence than normal elements need:
/// the suffix-stripped name must satisfy the anchored regex AND the rect must
/// be control-sized — a name+size-matched UIA rect is strictly better evidence
/// than the OCR text match we'd otherwise fall back to.
fn pane_fallback_match(
    automation: &UIAutomation,
    root: &UIElement,
    name_re: &Regex,
    // Out: raw-view nodes visited.
    count: &mut usize,
) -> Vec<LocateResult> {
    // Why a RAW-VIEW WALK and not FindAll: Lightroom's real controls are
    // invisible to COM FindAll/FindAllBuildCache — the find returned 273 panes
    // while the raw-view walker saw 603 nodes including the slider rows
    // ('Exposure', 'Vibrance' — verified live 2026-06-12; setting the cache
    // TreeFilter to a true condition changed nothing). Per-node COM is
    // affordable here: pane-world trees are small (~600 nodes ≈ 1.5 s), the
    // pass only runs when everything else returned zero, and a hard time
    // budget caps the worst case.
    const BUDGET_MS: u64 = 2500;
    const MAX_DEPTH: usize = 25;

    let Ok(walker) = automation.get_raw_view_walker() else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_millis(BUDGET_MS);
    let mut out = Vec::new();
    pane_walk(&walker, root, 0, MAX_DEPTH, deadline, name_re, count, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn pane_walk(
    walker: &uiautomation::UITreeWalker,
    el: &UIElement,
    depth: usize,
    max_depth: usize,
    deadline: Instant,
    name_re: &Regex,
    count: &mut usize,
    out: &mut Vec<LocateResult>,
) {
    // Control-sized caps in physical pixels: tall enough for a 200%-DPI button
    // (~4% of a 2160-px screen ≈ 86), wide enough for a long tab label, and a
    // floor that rejects degenerate slivers. Containers (panels: 266×163,
    // 1920×770…) fail the height cap.
    const PANE_MIN: u32 = 6;
    const PANE_MAX_W: u32 = 600;
    const PANE_MAX_H: u32 = 96;

    if depth > max_depth || Instant::now() > deadline {
        return;
    }
    *count += 1;
    if let Ok(name) = el.get_name() {
        let stripped = strip_paren_suffix(&name);
        if !stripped.is_empty() && name_re.is_match(&norm_dashes(&stripped)) {
            if let Ok(rect) = el.get_bounding_rectangle() {
                let left = rect.get_left();
                let top = rect.get_top();
                let width = rect.get_width().max(0) as u32;
                let height = rect.get_height().max(0) as u32;
                if (PANE_MIN..=PANE_MAX_W).contains(&width)
                    && (PANE_MIN..=PANE_MAX_H).contains(&height)
                    && rect_is_onscreen(left, top)
                {
                    let role = el
                        .get_control_type()
                        .map(|ct| format!("{ct:?}"))
                        .unwrap_or_else(|_| "Pane".to_string());
                    out.push(LocateResult {
                        bbox: Rect {
                            x: left,
                            y: top,
                            width,
                            height,
                        },
                        name,
                        role,
                        confidence: 1.0,
                    });
                }
            }
        }
    }
    if let Ok(child) = walker.get_first_child(el) {
        let mut cur = child;
        loop {
            pane_walk(
                walker, &cur, depth + 1, max_depth, deadline, name_re, count, out,
            );
            if Instant::now() > deadline {
                break;
            }
            match walker.get_next_sibling(&cur) {
                Ok(next) => cur = next,
                Err(_) => break,
            }
        }
    }
}

/// True when the AI's role maps to a RESTRICTED control-type family (so a
/// broad retry is worthwhile on a miss). Roles that already map to the broad
/// set would just repeat the identical query.
fn role_restricts_types(role: Option<&str>) -> bool {
    matches!(
        role.map(|r| r.to_ascii_lowercase()).as_deref(),
        Some(
            "textbox"
                | "searchbox"
                | "combobox"
                | "button"
                | "tab"
                | "menuitem"
                | "checkbox"
                | "radio"
                | "link"
                | "listitem"
                | "treeitem"
        )
    )
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

#[cfg(test)]
mod tests {
    use super::{build_name_regex, norm_dashes, strip_accelerator};

    #[test]
    fn accelerator_suffix_is_stripped() {
        assert_eq!(strip_accelerator("Playback Alt+I"), "Playback");
        assert_eq!(strip_accelerator("Media Alt+M"), "Media");
        assert_eq!(strip_accelerator("Save\tCtrl+S"), "Save");
        assert_eq!(strip_accelerator("Zoom In Ctrl++"), "Zoom In");
        assert_eq!(strip_accelerator("文件(&F)"), "文件");
    }

    #[test]
    fn ordinary_labels_are_never_truncated() {
        // "Alt" as a word without "+key" must not trigger the strip.
        assert_eq!(strip_accelerator("Alt text label"), "Alt text label");
        assert_eq!(strip_accelerator("Cut"), "Cut");
        assert_eq!(strip_accelerator("Control Panel"), "Control Panel");
    }

    #[test]
    fn anchored_regex_rejects_partial_label() {
        let re = build_name_regex("insert").unwrap();
        assert!(re.is_match("Insert"));
        assert!(re.is_match("← Insert")); // leading non-word chars allowed
        assert!(!re.is_match("Insert Space")); // extra word → reject
        assert!(!re.is_match("InsertedText"));
    }

    #[test]
    fn truncated_target_becomes_prefix_match() {
        // Model copied a clipped "…" label; UIA names are never truncated,
        // so the core must prefix-match the full accessible name.
        let re = build_name_regex("Sum of Output USD per…").unwrap();
        assert!(re.is_match("Sum of Output USD per 1M tokens"));
        assert!(!re.is_match("Total Output"));
    }

    #[test]
    fn unicode_dashes_normalise_to_ascii() {
        assert_eq!(norm_dashes("a\u{2014}b"), "a-b"); // em dash
        assert_eq!(norm_dashes("a\u{2013}b"), "a-b"); // en dash
        assert_eq!(norm_dashes("a-b"), "a-b");
    }

    #[test]
    fn paren_suffix_strips_one_trailing_group() {
        use super::strip_paren_suffix;
        // Adobe-style view-class suffix (Lightroom's Pane names).
        assert_eq!(strip_paren_suffix("Auto (Bridge View)"), "Auto");
        // Only ONE trailing group — internal parens survive.
        assert_eq!(strip_paren_suffix("Copy (1) (Bridge View)"), "Copy (1)");
        // No suffix → unchanged.
        assert_eq!(strip_paren_suffix("Develop_BasicView"), "Develop_BasicView");
        assert_eq!(strip_paren_suffix("Auto"), "Auto");
    }

    // Live diagnostic: WebView2-hybrid detection (e.g. new Outlook). Pass the
    // target window handle as decimal in NAVISUAL_TEST_HWND.
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; cargo test --lib chromium_detection_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn chromium_detection_live() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        assert!(
            super::window_class_is_chromium(hwnd),
            "expected a Chromium (child) window to be detected"
        );
    }

    // Live diagnostic against a running Lightroom — not part of CI.
    // Run: cargo test --lib pane_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pane_live_lightroom() {
        use uiautomation::UIAutomation;
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        let hwnd = unsafe { FindWindowW(w!("AgWinMainFrame"), None) }
            .expect("Lightroom not running?");
        let automation = UIAutomation::new().unwrap();
        let lr = automation.element_from_handle(hwnd.into()).unwrap();
        let re = build_name_regex("Vibrance").unwrap();
        let mut n = 0;
        let started = std::time::Instant::now();
        let hits = super::pane_fallback_match(&automation, &lr, &re, &mut n);
        eprintln!(
            "pane_fallback: {} panes scanned in {} ms, {} matched",
            n,
            started.elapsed().as_millis(),
            hits.len()
        );
        for h in &hits {
            eprintln!("  HIT name='{}' bbox={:?}", h.name, h.bbox);
        }
        assert!(!hits.is_empty(), "expected Pane 'Vibrance' to match");
    }

    // Live diagnostic: dump how a Chromium app (e.g. VS Code) exposes every element
    // whose UIA Name contains "ext" — the real Name / ControlType / tree-view of the
    // "Extensions" activity-bar item, which the deep find reports as 0 candidates.
    // `control-view` is exactly what the deep find scans; `raw-view` also catches
    // IsControlElement=false items the deep find can't see. Pass VS Code's window
    // handle (decimal) in NAVISUAL_TEST_HWND — e.g. PowerShell:
    //   (Get-Process code | ? { $_.MainWindowHandle -ne 0 } | select -First 1).MainWindowHandle
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; cargo test --lib vscode_extensions_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn vscode_extensions_live() {
        use uiautomation::types::{TreeScope, UIProperty};
        use uiautomation::variants::Variant;
        use uiautomation::UIAutomation;
        use windows::Win32::Foundation::HWND;

        let hwnd_raw: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        let automation = UIAutomation::new().unwrap();
        let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
        let root = automation.element_from_handle(hwnd.into()).unwrap();
        eprintln!("framework = {:?}", super::framework_of(&automation, hwnd_raw));

        // --- Control view: the BROAD clickable+text type set the deep find scans ---
        let mut cond = None;
        for &id in super::role_control_type_ids(None) {
            let c = automation
                .create_property_condition(UIProperty::ControlType, Variant::from(id), None)
                .unwrap();
            cond = Some(match cond.take() {
                None => c,
                Some(prev) => automation.create_or_condition(prev, c).unwrap(),
            });
        }
        let cache = automation.create_cache_request().unwrap();
        let _ = cache.add_property(UIProperty::Name);
        let _ = cache.add_property(UIProperty::ControlType);
        let els = root
            .find_all_build_cache(TreeScope::Descendants, &cond.unwrap(), &cache)
            .unwrap_or_default();
        eprintln!("control-view scanned = {}", els.len());
        let mut ctrl = 0;
        for el in &els {
            let name = el.get_cached_name().unwrap_or_default();
            if name.to_ascii_lowercase().contains("ext") {
                ctrl += 1;
                eprintln!(
                    "  [control] {:<11} name='{}'",
                    el.get_cached_control_type()
                        .map(|c| format!("{c:?}"))
                        .unwrap_or_default(),
                    name
                );
            }
        }
        eprintln!("control-view 'ext' hits = {ctrl}");

        // --- Raw view: catches IsControlElement=false items the deep find can't see ---
        fn walk(
            w: &uiautomation::UITreeWalker,
            el: &uiautomation::UIElement,
            depth: usize,
            hits: &mut usize,
        ) {
            if depth > 40 {
                return;
            }
            let name = el.get_name().unwrap_or_default();
            if name.to_ascii_lowercase().contains("ext") {
                *hits += 1;
                eprintln!(
                    "  [raw d{:<2}] {:<11} name='{}'",
                    depth,
                    el.get_control_type()
                        .map(|c| format!("{c:?}"))
                        .unwrap_or_default(),
                    name
                );
            }
            if let Ok(child) = w.get_first_child(el) {
                let mut cur = child;
                loop {
                    walk(w, &cur, depth + 1, hits);
                    match w.get_next_sibling(&cur) {
                        Ok(next) => cur = next,
                        Err(_) => break,
                    }
                }
            }
        }
        let walker = automation.get_raw_view_walker().unwrap();
        let mut raw = 0;
        walk(&walker, &root, 0, &mut raw);
        eprintln!("raw-view 'ext' hits = {raw}");
    }

    // Live diagnostic for S.1 — dump the Structured-Context element list for a window,
    // with timing (the p50 < 50 ms warm target) and the skip reason when over cap /
    // framework Other. Pass the window handle (decimal) in NAVISUAL_TEST_HWND.
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; cargo test --lib context_enumeration_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn context_enumeration_live() {
        let hwnd_raw: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        let started = std::time::Instant::now();
        let result = super::enumerate_context_elements(hwnd_raw);
        let ms = started.elapsed().as_millis();
        match result {
            Ok(els) => {
                eprintln!("{} elements in {ms} ms", els.len());
                for e in &els {
                    eprintln!(
                        "  {:>3} | {:<12} | {:?} @ ({}, {}) {}x{}",
                        e.id, e.role, e.name, e.rect.x, e.rect.y, e.rect.width, e.rect.height
                    );
                }
            }
            Err(reason) => eprintln!("skipped in {ms} ms: {reason}"),
        }
    }

    // Diagnose WHAT the Excel ExcelGrid-Descendants search returns (2026-07-13). A live log
    // showed a plain 209-cell data sheet (no PivotTable) yielding 559 context-type candidates
    // in ~7 s; this probe auto-finds the open Excel window and prints a (control_type,
    // class_name) histogram of that search plus its count/time in isolation. It confirmed the
    // real cause: 23,320 NetUIRepeatButton (scrollbar arrow-repeat buttons) across self-nested
    // ExcelGrid panes over 138 s, zero useful elements — the escape-hatch's flat Descendants
    // never prunes the broken scrollbar. Motivated switching enumeration to grid_cond=None
    // (see enumerate_context_elements above). Kept lean to re-verify any future regression.
    // Run (Excel open on the sheet): cargo test --lib excel_enum_histogram_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn excel_enum_histogram_live() {
        use super::{CONTEXT_CT_IDS, EXCEL_GRID_CLASS, EXCEL_MAIN_WINDOW_CLASS};
        use std::collections::BTreeMap;
        use std::time::Instant;
        use uiautomation::types::{TreeScope, UIProperty};
        use uiautomation::variants::Variant;
        use uiautomation::UIAutomation;
        use windows::Win32::Foundation::HWND;
        // Auto-find the Excel window by title; override with NAVISUAL_TEST_HWND if needed.
        let hwnd_raw: usize = std::env::var("NAVISUAL_TEST_HWND")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| crate::capture::find_window_by_title("excel", ""))
            .or_else(|| crate::capture::find_window_by_title(".csv", ""))
            .or_else(|| crate::capture::find_window_by_title(".xlsx", ""))
            .expect("no Excel window found (open the sheet, or set NAVISUAL_TEST_HWND)");
        eprintln!("Excel hwnd = {hwnd_raw:#x}");

        let automation = UIAutomation::new().expect("uia init");
        let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
        let root = automation.element_from_handle(hwnd.into()).expect("root");

        // Same 12 context control-type OR-condition enumerate_context_elements builds.
        let mut cond = None;
        for &id in CONTEXT_CT_IDS {
            let c = automation
                .create_property_condition(UIProperty::ControlType, Variant::from(id), None)
                .unwrap();
            cond = Some(match cond.take() {
                None => c,
                Some(prev) => automation.create_or_condition(prev, c).unwrap(),
            });
        }
        let cond = cond.unwrap();
        let cache = automation.create_cache_request().unwrap();
        let _ = cache.add_property(UIProperty::Name);
        let _ = cache.add_property(UIProperty::ControlType);
        let _ = cache.add_property(UIProperty::BoundingRectangle);
        let _ = cache.add_property(UIProperty::ClassName);

        // Histogram every context node the ExcelGrid Descendants search returns by
        // (control_type, class_name), and time the call. This is the exact query the
        // (now-disabled) escape-hatch made; it reveals the NetUIRepeatButton scrollbar
        // explosion that motivated switching enumeration's excel_pruned_walk to grid_cond=None.
        let _ = EXCEL_MAIN_WINDOW_CLASS; // root IS the XLMAIN window; no child lookup needed
        let mut hist: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut grid_ms = 0u128;
        let mut grid_count = 0usize;
        // Find every ExcelGrid pane under the root, and time each one's context-type
        // Descendants search in isolation (this is the exact call the real walk makes).
        let grid_cond = automation
            .create_property_condition(
                UIProperty::ClassName,
                Variant::from(EXCEL_GRID_CLASS),
                None,
            )
            .unwrap();
        let grids = root
            .find_all_build_cache(TreeScope::Descendants, &grid_cond, &cache)
            .unwrap_or_default();
        eprintln!("ExcelGrid panes found: {}", grids.len());
        for g in &grids {
            let t0 = Instant::now();
            if let Ok(els) = g.find_all_build_cache(TreeScope::Descendants, &cond, &cache) {
                grid_ms += t0.elapsed().as_millis();
                grid_count += els.len();
                for el in &els {
                    let ct = el
                        .get_cached_control_type()
                        .map(|c| format!("{c:?}"))
                        .unwrap_or_default();
                    let ecn = el.get_cached_classname().unwrap_or_default();
                    *hist.entry((ct, ecn)).or_insert(0) += 1;
                }
            }
        }
        eprintln!(
            "ExcelGrid Descendants(context-types): {grid_count} elements in {grid_ms} ms"
        );
        eprintln!("Histogram (control_type | class_name | count), sorted:");
        let mut rows: Vec<_> = hist.clone().into_iter().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        for ((ct, cn), n) in rows {
            eprintln!("  {n:>5} | {ct:<12} | {cn}");
        }
    }

    #[test]
    fn context_role_family_and_name_strip() {
        use super::{context_display_name, context_role_compatible};
        // Exact role always agrees; the lenient button family cross-matches.
        assert!(context_role_compatible("Button", "Button"));
        assert!(context_role_compatible("Hyperlink", "Button"));
        assert!(context_role_compatible("SplitButton", "MenuItem"));
        // Outside the family, roles must match exactly (a textbox pick can't verify
        // against a live TabItem).
        assert!(!context_role_compatible("TabItem", "Button"));
        assert!(!context_role_compatible("Edit", "ComboBox"));
        // Display name reuses the matcher's paren-suffix + accelerator strips.
        assert_eq!(context_display_name("Auto (Bridge View)"), "Auto");
        assert_eq!(context_display_name("Playback Alt+I"), "Playback");
        assert_eq!(context_display_name("  Save\u{00a0}Ctrl+S "), "Save");
    }

    #[test]
    fn deep_name_filter_tiers_short_targets_safely() {
        use super::{deep_name_filter, norm_dashes};
        let needle = norm_dashes("to");
        let re = build_name_regex("To").unwrap();
        // The field actually named "To" → anchored.
        assert_eq!(deep_name_filter("To", &needle, &re), Some(true));
        // Prose/tooltips containing the word "to" must be rejected outright —
        // the live new-Outlook false hit.
        assert_eq!(
            deep_name_filter("Attach a file to this item.", &needle, &re),
            None
        );
        assert_eq!(
            deep_name_filter("Restrict permission to this item.", &needle, &re),
            None
        );
    }

    #[test]
    fn restrictive_roles_warrant_broad_retry() {
        use super::role_restricts_types;
        // Restricted families → a broad retry can find what the family missed.
        assert!(role_restricts_types(Some("textbox")));
        assert!(role_restricts_types(Some("button")));
        assert!(role_restricts_types(Some("Tab")));
        // Already-broad roles → a retry would repeat the identical query.
        assert!(!role_restricts_types(None));
        assert!(!role_restricts_types(Some("other")));
        assert!(!role_restricts_types(Some("heading")));
        assert!(!role_restricts_types(Some("slider")));
    }

    #[test]
    fn deep_name_filter_keeps_known_loose_matches() {
        use super::{deep_name_filter, norm_dashes};
        // VS Code activity bar: accelerator-suffixed name → anchored via the
        // paren-suffix strip.
        let needle = norm_dashes("extensions");
        let re = build_name_regex("Extensions").unwrap();
        assert_eq!(
            deep_name_filter("Extensions (Ctrl+Shift+X)", &needle, &re),
            Some(true)
        );
        // Marketplace search box: longer name, label-sized → loose containment.
        let needle = norm_dashes("search extensions");
        let re = build_name_regex("Search Extensions").unwrap();
        assert_eq!(
            deep_name_filter("Search Extensions in Marketplace", &needle, &re),
            Some(false)
        );

        // Live probe 2026-06-13: VS Code's real activity-bar Name is badged AND doubled.
        // The leading-label split (before the " (" keybinding) still anchors to "Extensions".
        let needle = norm_dashes("extensions");
        let re = build_name_regex("Extensions").unwrap();
        assert_eq!(
            deep_name_filter(
                "Extensions (Ctrl+Shift+X) - 4 require restart Extensions (Ctrl+Shift+X) - 4 require restart",
                &needle,
                &re,
            ),
            Some(true)
        );
        // The leading-label tier stays exact: a short target must NOT prefix-latch a
        // longer leading label.
        let needle = norm_dashes("to");
        let re = build_name_regex("To").unwrap();
        assert_eq!(deep_name_filter("Tools (Ctrl+T) - 2 issues", &needle, &re), None);
    }

    #[test]
    fn pane_name_matches_target_after_suffix_strip() {
        // End-to-end name check the Pane fallback performs: the suffix-stripped
        // Lightroom name must satisfy the anchored target regex.
        use super::strip_paren_suffix;
        let re = build_name_regex("Auto").unwrap();
        assert!(re.is_match(&norm_dashes(&strip_paren_suffix("Auto (Bridge View)"))));
        assert!(!re.is_match(&norm_dashes(&strip_paren_suffix("Auto Tone (Bridge View)"))));
        assert!(!re.is_match(&norm_dashes(&strip_paren_suffix("AgDevelop_navigatorPanel"))));
    }

    // Distinct, arbitrary hwnd values per test — CONTEXT_SLOW_WINDOWS is one process-wide
    // static, and Rust runs tests in parallel within a binary, so two tests sharing an hwnd
    // could interfere with each other.
    #[test]
    fn slow_window_tracking_requires_consecutive_strikes() {
        use super::{context_window_is_slow, context_window_mark_slow};
        let hwnd = 0xDEAD_0001;
        assert!(!context_window_is_slow(hwnd), "fresh hwnd starts clean");
        context_window_mark_slow(hwnd);
        assert!(
            !context_window_is_slow(hwnd),
            "one strike alone must not blacklist — a single blip shouldn't be permanent"
        );
        context_window_mark_slow(hwnd);
        assert!(
            context_window_is_slow(hwnd),
            "two consecutive strikes (CONTEXT_SLOW_THRESHOLD) should skip the window"
        );
    }

    #[test]
    fn a_fast_result_clears_prior_strikes() {
        use super::{context_window_is_slow, context_window_mark_fast, context_window_mark_slow};
        let hwnd = 0xDEAD_0002;
        context_window_mark_slow(hwnd);
        context_window_mark_fast(hwnd);
        context_window_mark_slow(hwnd);
        assert!(
            !context_window_is_slow(hwnd),
            "mark_fast must reset the counter, not just decrement it — otherwise an old, \
             unrelated blip plus one new strike could wrongly cross the threshold"
        );
    }
}
