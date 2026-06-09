//! C5 — WindowFromPoint hit-test verification.
//!
//! After OCR locates a candidate, call `WindowFromPoint` at the bbox centre to
//! confirm the leaf HWND belongs to an interactive control class.  Non-interactive
//! Win32 classes (labels, column headers, scrollbars, status bars) are rejected
//! so the pointer doesn't land on an inert area.
//!
//! Web-renderer classes (Chromium, Firefox, WebView2) are exempt — inside those
//! surfaces the DOM — not Win32 class names — governs interactivity, and the
//! OCR candidate is valid regardless of what the leaf HWND class says.
//!
//! Only applied to OCR hits.  A11y results carry UIA control-type information
//! that already filters non-interactive roles.

use uiautomation::controls::ControlType;
use uiautomation::types::Point;
use uiautomation::UIAutomation;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, WindowFromPoint};

/// Outcome of a hit-test against a screen pixel.
#[derive(Debug)]
pub enum HitTestOutcome {
    /// The leaf HWND class is acceptable — proceed with this candidate.
    Pass,
    /// The leaf HWND class is on the denylist — skip this candidate.
    Rejected { leaf_class: String },
    /// The leaf HWND is a web-renderer surface — skip class check, always pass.
    WebRenderer,
}

/// Win32 classes that are never a user-clickable control.
const DENYLIST: &[&str] = &[
    "Static",             // Win32 label / static-text control
    "SysHeader32",        // ListView / TreeView column header
    "ScrollBar",          // scrollbar track
    "msctls_statusbar32", // status bar at the bottom of windows
];

/// Classes that host a web-content rendering surface.
/// The DOM controls interactivity inside these windows, not the Win32 class.
const WEB_RENDERERS: &[&str] = &[
    "Chrome_RenderWidgetHostHWND", // Chromium, Edge, WebView2, Electron
    "MozillaWindowClass",          // Firefox
    "GeckoPluginWindow",           // Firefox legacy plugin surface
];

/// Check whether the virtual-desktop pixel at `(cx, cy)` sits on an
/// interactive Win32 control.
///
/// Returns `Pass` when the leaf HWND is `NULL` (rare — don't block on error),
/// when the class is a web renderer, or when the class is not on the denylist.
pub fn verify_hit(cx: i32, cy: i32) -> HitTestOutcome {
    let pt = POINT { x: cx, y: cy };
    let hwnd: HWND = unsafe { WindowFromPoint(pt) };

    // NULL hwnd — no window at this point, don't block.
    if hwnd.0.is_null() {
        return HitTestOutcome::Pass;
    }

    let class = get_class_name(hwnd);
    if class.is_empty() {
        return HitTestOutcome::Pass;
    }

    // Web renderer — skip denylist entirely.
    if WEB_RENDERERS
        .iter()
        .any(|&wc| class.eq_ignore_ascii_case(wc))
    {
        return HitTestOutcome::WebRenderer;
    }

    // Denylist check.
    if DENYLIST.iter().any(|&dc| class.eq_ignore_ascii_case(dc)) {
        return HitTestOutcome::Rejected { leaf_class: class };
    }

    HitTestOutcome::Pass
}

/// UIA role hit-test: classify the control under a screen pixel. The **primary**
/// corroborator for an OCR match — works on native apps *and* primed Chromium/Electron
/// (where `WindowFromPoint` is blind because all web content is one HWND). The prime
/// (`a11y::prime`) makes `ElementFromPoint` return the real web element with its role.
pub enum RoleHit {
    /// An interactive control (button/link/menuitem/tab/…) — corroborates the match.
    Interactive(String),
    /// Content (Document/Text/Edit/terminal) — does NOT corroborate.
    Content(String),
    /// UIA couldn't resolve a usable element/type (tree cold, or non-UIA surface).
    Unknown,
}

/// Resolve the control type under `(cx, cy)` (virtual-desktop pixels). `ElementFromPoint`
/// returns the *deepest* element — a button's deepest node is often a Text run — so we walk
/// up a few ancestors looking for an interactive control before concluding "content".
pub fn verify_role(cx: i32, cy: i32) -> RoleHit {
    let Ok(automation) = UIAutomation::new() else {
        return RoleHit::Unknown;
    };
    let mut el = match automation.element_from_point(Point::new(cx, cy)) {
        Ok(e) => e,
        Err(_) => return RoleHit::Unknown,
    };
    let walker = automation.get_control_view_walker().ok();
    for _ in 0..3 {
        match el.get_control_type() {
            Ok(ct) if is_interactive(ct) => return RoleHit::Interactive(format!("{ct:?}")),
            Ok(ct) if is_content(ct) => return RoleHit::Content(format!("{ct:?}")),
            Ok(_) => {
                // Neutral container (Group/Pane/Custom/Text-run) — try the parent.
                match walker.as_ref().and_then(|w| w.get_parent(&el).ok()) {
                    Some(parent) => el = parent,
                    None => return RoleHit::Unknown,
                }
            }
            Err(_) => return RoleHit::Unknown,
        }
    }
    RoleHit::Unknown
}

fn is_interactive(ct: ControlType) -> bool {
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

fn is_content(ct: ControlType) -> bool {
    matches!(
        ct,
        ControlType::Document | ControlType::Text | ControlType::Edit
    )
}

fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}
