//! App-specific locator adapters — v0.6 Workstream A.
//!
//! Pass 0 of the locator: before the generic A11y → OCR pipeline runs, an adapter
//! gets a chance to resolve the target by **deterministic local geometry** for an
//! app where AI grounding is weakest. The first (and only, for now) adapter is
//! Excel cells — the AI emits a cell ref ("Q34") and we resolve the exact pixels
//! via UIA `GridPattern`, with **no AI pixel grounding** (only Gemini 3+/Qwen-omni
//! hit dense grid cells visually — see model-comparison.md).
//!
//! Contract:
//!   - `matches(hwnd, target)` — true when the adapter recognises the focused app
//!     *and* the target shape. Cheap (class/exe + a regex), no UIA.
//!   - `locate(hwnd, target)` — resolve, or return `result: None` to say "I claimed
//!     this but couldn't resolve it" (e.g. an off-screen virtualized cell). The
//!     orchestrator then falls through to the untouched A11y → OCR path.
//!
//! Adding an adapter: implement [`Adapter`], push it into [`adapters()`].

use super::LocateResult;
use anyhow::Result;

#[cfg(windows)]
mod blender;
#[cfg(windows)]
mod excel;
#[cfg(windows)]
pub(crate) mod office_com;
#[cfg(windows)]
mod powerpoint;
#[cfg(windows)]
mod word;

/// Re-export for the Flow-A candidate readback (`locator/candidates.rs`), which
/// resolves a PowerPoint shape selection to screen pixels through the same
/// pane-quirk-aware conversion the adapter uses.
#[cfg(windows)]
pub(crate) use powerpoint::convert_rect_to_pixels as ppt_points_to_pixels;

/// Everything an adapter may gate or resolve on. `target_role`/`nearby_text` come from the
/// AI response (both optional): the Office canvas adapters gate on role so they can never
/// hijack a ribbon/control target, and Word uses `nearby_text` to disambiguate repeated
/// occurrences of the target text (ground-truth analogue of the OCR anchor).
/// `avoid_bboxes` (Flow A): spots the user rejected this step — an avoid-aware adapter
/// resolves to the next-best NON-rejected answer (Word: the next occurrence) instead of
/// re-hitting the rejected one and getting vetoed at Pass 0.
pub struct AdapterQuery<'a> {
    pub target_text: &'a str,
    pub target_role: Option<&'a str>,
    pub nearby_text: Option<&'a str>,
    pub avoid_bboxes: &'a [crate::capture::Rect],
}

/// Outcome of an adapter's `locate`. `result: None` means "claimed but couldn't resolve"
/// (caller falls through to A11y/OCR); `detail` is surfaced in the debug drawer either way.
pub struct AdapterHit {
    pub result: Option<LocateResult>,
    pub detail: String,
    /// Flow B: when the adapter fell through because it KNEW 2+ equally-good answers
    /// (occurrence/shape-tier ambiguity), the known rects — screen coords, the pass's
    /// own order. Recorded into the trace; if the WHOLE pipeline then misses, they're
    /// shown as candidate boxes instead of a hint ring (never pre-empting later passes
    /// — the Save/QAT case). Empty everywhere else.
    pub ambiguous: Vec<crate::capture::Rect>,
}

impl AdapterHit {
    /// Claimed the target but couldn't resolve it — caller falls through to A11y/OCR.
    pub fn fell_through(detail: impl Into<String>) -> Self {
        Self {
            result: None,
            detail: detail.into(),
            ambiguous: Vec::new(),
        }
    }

    /// Fell through on a KNOWN ambiguity — the candidate rects ride along (Flow B).
    pub fn ambiguous(detail: impl Into<String>, rects: Vec<crate::capture::Rect>) -> Self {
        Self {
            result: None,
            detail: detail.into(),
            ambiguous: rects,
        }
    }
}

/// An app-specific locator. See the module docs for the contract.
pub trait Adapter {
    /// Stable identifier surfaced in the trace ("excel", …).
    fn name(&self) -> &'static str;
    /// True when this adapter recognises the focused app **and** the target shape.
    /// Must be cheap — runs on every locate before the standard pipeline (window
    /// class / exe / regex / role gates only — no UIA, no COM).
    fn matches(&self, hwnd: usize, query: &AdapterQuery) -> bool;
    /// Resolve the target to exact pixels, or `AdapterHit::fell_through(..)` when it
    /// recognised the target but couldn't resolve it this time.
    fn locate(&self, hwnd: usize, query: &AdapterQuery) -> Result<AdapterHit>;
}

/// What the orchestrator's Pass 0 gets back when an adapter *claimed* the target.
pub struct AdapterOutcome {
    pub name: String,
    pub result: Option<LocateResult>,
    pub detail: String,
    /// Flow B ambiguity set (see [`AdapterHit::ambiguous`]).
    pub ambiguous: Vec<crate::capture::Rect>,
}

/// The registered adapters, in priority order. The first whose `matches` returns true wins.
fn adapters() -> Vec<Box<dyn Adapter>> {
    #[cfg(windows)]
    {
        vec![
            Box::new(excel::ExcelAdapter),
            Box::new(powerpoint::PowerPointAdapter),
            Box::new(word::WordAdapter),
            Box::new(blender::BlenderAdapter),
        ]
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Pass 0 — try the app-specific adapters before A11y. Returns `Some` only when an adapter
/// *claimed* the target (recognised the app + target shape); `None` means the standard
/// A11y → OCR pipeline should run unchanged. A claimed-but-unresolved locate returns
/// `Some` with `result: None` so the orchestrator can record it and still fall through.
pub fn try_locate(target_hwnd: Option<usize>, query: &AdapterQuery) -> Option<AdapterOutcome> {
    let hwnd = resolve_target_hwnd(target_hwnd)?;
    for adapter in adapters() {
        if !adapter.matches(hwnd, query) {
            continue;
        }
        return Some(match adapter.locate(hwnd, query) {
            Ok(hit) => AdapterOutcome {
                name: adapter.name().to_string(),
                result: hit.result,
                detail: hit.detail,
                ambiguous: hit.ambiguous,
            },
            Err(e) => AdapterOutcome {
                name: adapter.name().to_string(),
                result: None,
                detail: format!("error: {e}"),
                ambiguous: Vec::new(),
            },
        });
    }
    None
}

/// Resolve the window the adapter should inspect: the pinned HWND the AI saw, else the
/// current foreground window (skipping our own process). Mirrors a11y's root selection.
#[cfg(windows)]
fn resolve_target_hwnd(target_hwnd: Option<usize>) -> Option<usize> {
    if let Some(h) = target_hwnd.filter(|h| *h != 0) {
        return Some(h);
    }
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() || pid == 0 {
            return None;
        }
        Some(hwnd.0 as usize)
    }
}

#[cfg(not(windows))]
fn resolve_target_hwnd(target_hwnd: Option<usize>) -> Option<usize> {
    target_hwnd.filter(|h| *h != 0)
}

/// Sanity-bound an absolute screen coordinate. A minimized Office window places its
/// elements at the Windows minimized sentinel (~-32000), and scrolled-out/virtualized
/// content reports absurd coords — reject both so adapters fall through instead of
/// pointing into nowhere. Shared by every Office adapter.
#[cfg(windows)]
pub(crate) fn rect_is_onscreen(left: i32, top: i32) -> bool {
    left > -30_000 && top > -30_000 && left.abs() <= 100_000 && top.abs() <= 100_000
}

/// Lowercase class name of `hwnd` ("xlmain", …), or empty on failure. Windows-only helper
/// shared by adapters that gate on window class.
#[cfg(windows)]
pub(crate) fn window_class_lower(hwnd: usize) -> String {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(HWND(hwnd as *mut _), &mut buf);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize]).to_ascii_lowercase()
    }
}

/// Lowercase exe file stem of the process owning `hwnd` ("excel", …), or empty on failure.
#[cfg(windows)]
pub(crate) fn window_exe_stem_lower(hwnd: usize) -> String {
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    unsafe {
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        std::path::Path::new(&String::from_utf16_lossy(&buf[..len as usize]))
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    }
}
