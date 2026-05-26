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

fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}
