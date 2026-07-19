//! Excel cell adapter — v0.6 Workstream A.
//!
//! Cells are the worst case for visual AI grounding: a dense uniform grid with no per-cell
//! text to read, so only the strongest grounders hit them (model-comparison.md). This adapter
//! sidesteps grounding entirely — the AI emits a cell ref ("Q34") and we resolve the exact
//! pixels deterministically via UIA `GridPattern`, making cell-pointing work on *every* model.
//!
//! Resolution: `"Q34"` → column 17, row 34 (both 1-based) → `GridPattern::GetItem(34, 17)`
//! → the cell element's `BoundingRectangle`. Excel's UIA grid reserves index 0 for the
//! header row/column (Select-All corner at (0,0)), so a 1-based cell ref maps **directly** to
//! `GetItem(row, col)` with no offset — verified live (`GetItem(1,1)`=A1, `GetItem(34,17)`=Q34).
//!
//! Caveats:
//!   - **Virtualized off-screen cells.** A cell scrolled out of view may be absent from the
//!     grid (or report an off-screen rect). We *fall through* (no wrong pointer) rather than
//!     guess; emitting a scroll / Ctrl+G step is a later enhancement (v0.6.x).
//!   - **Office COM** (`Window.PointsToScreenPixelsX`) is the bulletproof second cut for
//!     frozen panes / precision — deferred (late-bound `IDispatch::Invoke` in Rust is painful).

use super::{
    rect_is_onscreen, window_class_lower, window_exe_stem_lower, Adapter, AdapterHit,
    AdapterQuery,
};
use crate::capture::Rect;
use crate::locator::a11y::{excel_pruned_walk, ClassRectSignature, SCROLLBAR_SCAN_DEPTH};
use crate::locator::LocateResult;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use uiautomation::controls::ControlType;
use uiautomation::patterns::UIGridPattern;
use uiautomation::types::UIProperty;
use uiautomation::variants::Variant;
use uiautomation::{UIAutomation, UIElement};
use windows::Win32::Foundation::HWND;

/// Excel column ceiling (XFD) and row ceiling (1,048,576) — refs beyond these aren't cells.
const MAX_COL: i32 = 16_384;
const MAX_ROW: i32 = 1_048_576;

/// UIA ControlType ids for building the property condition passed into `excel_pruned_walk`'s
/// `ExcelGrid` escape hatch (which needs a `&UICondition`, not the enum) — numeric form of
/// the same three types checked via `ControlType` matching in `find_grid`'s `consider`.
const CT_TABLE: i32 = 50_036;
const CT_DATA_GRID: i32 = 50_028;
const CT_CUSTOM: i32 = 50_025;
/// Same budget-scale as the Structured-Context Excel walk (`a11y::EXCEL_CONTEXT_BUDGET_MS`)
/// — this is the same pruned-walk shape, so it should finish in the same ~300 ms ballpark;
/// this is a safety bound against runaway recursion, not an expected duration.
const FIND_GRID_BUDGET_MS: u64 = 1500;

pub struct ExcelAdapter;

impl Adapter for ExcelAdapter {
    fn name(&self) -> &'static str {
        "excel"
    }

    fn matches(&self, hwnd: usize, query: &AdapterQuery) -> bool {
        if parse_cell_ref(query.target_text).is_none() {
            return false;
        }
        // Top-level Excel window class is XLMAIN; exe is EXCEL.EXE. Either gate is enough.
        window_class_lower(hwnd) == "xlmain" || window_exe_stem_lower(hwnd) == "excel"
    }

    fn locate(&self, hwnd: usize, query: &AdapterQuery) -> Result<AdapterHit> {
        let target_text = query.target_text;
        let (row, col) = parse_cell_ref(target_text)
            .ok_or_else(|| anyhow!("not a cell ref: {target_text}"))?;

        let automation = UIAutomation::new().map_err(|e| anyhow!("UIAutomation init: {e}"))?;
        let root = automation
            .element_from_handle(HWND(hwnd as *mut _).into())
            .map_err(|e| anyhow!("element_from_handle: {e}"))?;

        let Some((grid, rows, cols)) = find_grid(&automation, &root) else {
            return Ok(AdapterHit::fell_through(
                "no GridPattern surface found in the Excel window",
            ));
        };

        // Calibration (verified live against Excel): the UIA grid reserves index 0 for the
        // header row (column letters) and index 0 for the header column (row numbers), with
        // the Select-All corner at (0,0). So a data cell (row R, col C, both 1-based) maps
        // DIRECTLY to GetItem(R, C) — GetItem(1,1)=A1, GetItem(34,17)=Q34. No subtraction.
        // Out-of-range usually means the cell is below/right of the grid the tree currently
        // exposes (virtualized) — fall through, don't guess.
        if (rows > 0 && row >= rows) || (cols > 0 && col >= cols) {
            return Ok(AdapterHit::fell_through(format!(
                "{target_text} (row {row}, col {col}) outside the live grid {rows}×{cols} — likely scrolled out"
            )));
        }

        let cell = match grid.get_item(row, col) {
            Ok(c) => c,
            Err(e) => {
                return Ok(AdapterHit::fell_through(format!(
                    "GridPattern.GetItem({row},{col}) failed: {e} — cell likely off-screen"
                )))
            }
        };

        let Ok(rect) = cell.get_bounding_rectangle() else {
            return Ok(AdapterHit::fell_through(format!(
                "{target_text} resolved but has no rect — scrolled out"
            )));
        };
        let (left, top) = (rect.get_left(), rect.get_top());
        let (w, h) = (
            rect.get_width().max(0) as u32,
            rect.get_height().max(0) as u32,
        );
        if w == 0 || h == 0 || !rect_is_onscreen(left, top) {
            return Ok(AdapterHit::fell_through(format!(
                "{target_text} rect off-screen/empty ({left},{top} {w}×{h}) — scroll into view"
            )));
        }

        let name = cell.get_name().unwrap_or_default();
        Ok(AdapterHit {
            result: Some(LocateResult {
                bbox: Rect {
                    x: left,
                    y: top,
                    width: w,
                    height: h,
                },
                name,
                role: "ExcelCell".to_string(),
                confidence: 1.0,
            }),
            detail: format!("{target_text} → GridPattern.GetItem({row},{col})"),
        })
    }
}

/// Find the worksheet grid: the descendant container that exposes a `GridPattern` with a
/// non-empty shape (Table / DataGrid / Custom control types; the cell `DataItem`s are
/// excluded by the control-type check, so this stays cheap) and picks the largest grid.
///
/// Uses [`excel_pruned_walk`] rather than a plain `find_all(Subtree, ...)` — the naive form
/// hits the exact same broken-scrollbar problem as the Structured-Context enumeration did
/// (confirmed live 2026-07-06: a real `A1` request reported "no GridPattern surface found"
/// after the walk apparently stalled inside the self-nested `NUIScrollbar` branch — same
/// root cause, different call site). Sharing the walker means this can't drift out of sync
/// with that fix.
///
/// The real grid is NOT the `ExcelGrid`-classed pane itself (that's just `ControlType::Pane`,
/// no pattern support) — it's a `DataGrid`-typed grandchild (class `XLSpreadsheetGrid`, live
/// name "Grid") one level further in. Like the sheet-tab strip, it's only reachable via a
/// true `Descendants` search scoped to `ExcelGrid`, so this passes the Table/DataGrid/Custom
/// condition as `grid_cond` to use that escape hatch — confirmed live: 173 ms, resolves A1
/// correctly (rect 26,239 64×20).
fn find_grid(
    automation: &UIAutomation,
    root: &UIElement,
) -> Option<(UIGridPattern, i32, i32)> {
    let mut best: Option<(UIGridPattern, i32, i32)> = None;

    let mut consider = |el: &UIElement| {
        let Ok(ct) = el.get_cached_control_type().or_else(|_| el.get_control_type()) else {
            return;
        };
        if !matches!(
            ct,
            ControlType::Table | ControlType::DataGrid | ControlType::Custom
        ) {
            return;
        }
        let Ok(grid) = el.get_pattern::<UIGridPattern>() else {
            return;
        };
        let rows = grid.get_row_count().unwrap_or(0);
        let cols = grid.get_column_count().unwrap_or(0);
        if rows <= 0 || cols <= 0 {
            return;
        }
        let area = (rows as i64) * (cols as i64);
        let better = best
            .as_ref()
            .map(|(_, r, c)| area > (*r as i64) * (*c as i64))
            .unwrap_or(true);
        if better {
            best = Some((grid, rows, cols));
        }
    };
    // Test root itself first, in case the window element is the grid (mirrors the old
    // TreeScope::Subtree behaviour, which included the search root).
    consider(root);

    let grid_type_cond = {
        let mut acc: Option<uiautomation::core::UICondition> = None;
        for id in [CT_TABLE, CT_DATA_GRID, CT_CUSTOM] {
            let c = automation
                .create_property_condition(UIProperty::ControlType, Variant::from(id), None)
                .ok()?;
            acc = Some(match acc {
                None => c,
                Some(prev) => automation.create_or_condition(prev, c).ok()?,
            });
        }
        acc?
    };
    let true_cond = automation.create_true_condition().ok()?;
    let cache = automation.create_cache_request().ok()?;
    let _ = cache.add_property(UIProperty::ClassName);
    let _ = cache.add_property(UIProperty::ControlType);
    let _ = cache.add_property(UIProperty::BoundingRectangle);
    let mut seen: HashSet<ClassRectSignature> = HashSet::new();
    let deadline = Instant::now() + Duration::from_millis(FIND_GRID_BUDGET_MS);

    excel_pruned_walk(
        root,
        SCROLLBAR_SCAN_DEPTH,
        &true_cond,
        Some(&grid_type_cond), // the ExcelGrid escape hatch — reaches XLSpreadsheetGrid
        &cache,
        &mut seen,
        deadline,
        &mut |el, _class_name| consider(el),
    );
    best
}

/// Parse an A1-style cell ref into 1-based `(row, col)`. Rejects ranges ("A1:B2"), bare
/// columns/rows, and refs beyond Excel's `XFD1048576` ceiling. Case-insensitive.
fn parse_cell_ref(target: &str) -> Option<(i32, i32)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([A-Za-z]{1,3})([0-9]{1,7})$").unwrap());
    let caps = re.captures(target.trim())?;
    let col = col_letters_to_index(&caps[1])?;
    let row: i32 = caps[2].parse().ok()?;
    if !(1..=MAX_ROW).contains(&row) || !(1..=MAX_COL).contains(&col) {
        return None;
    }
    Some((row, col))
}

/// Bijective base-26 column-letter → 1-based index ("A"→1, "Z"→26, "AA"→27, "Q"→17).
fn col_letters_to_index(letters: &str) -> Option<i32> {
    let mut idx: i64 = 0;
    for ch in letters.chars() {
        let c = ch.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            return None;
        }
        idx = idx * 26 + (c as i64 - 'A' as i64 + 1);
        if idx > MAX_COL as i64 {
            return None;
        }
    }
    (idx > 0).then_some(idx as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_map_to_one_based_index() {
        assert_eq!(col_letters_to_index("A"), Some(1));
        assert_eq!(col_letters_to_index("Z"), Some(26));
        assert_eq!(col_letters_to_index("AA"), Some(27));
        assert_eq!(col_letters_to_index("Q"), Some(17));
        assert_eq!(col_letters_to_index("XFD"), Some(16_384)); // last valid column
        assert_eq!(col_letters_to_index("XFE"), None); // one past the ceiling
    }

    #[test]
    fn parses_valid_cell_refs() {
        assert_eq!(parse_cell_ref("Q34"), Some((34, 17)));
        assert_eq!(parse_cell_ref("A1"), Some((1, 1)));
        assert_eq!(parse_cell_ref("q34"), Some((34, 17))); // case-insensitive
        assert_eq!(parse_cell_ref(" B2 "), Some((2, 2))); // surrounding whitespace tolerated
        assert_eq!(parse_cell_ref("XFD1048576"), Some((1_048_576, 16_384)));
    }

    #[test]
    fn rejects_non_cell_refs() {
        assert_eq!(parse_cell_ref("A1:B2"), None); // range
        assert_eq!(parse_cell_ref("Q"), None); // bare column
        assert_eq!(parse_cell_ref("34"), None); // bare row
        assert_eq!(parse_cell_ref("Performance"), None); // a normal label
        assert_eq!(parse_cell_ref("ABCD1"), None); // 4 letters
        assert_eq!(parse_cell_ref("A12345678"), None); // 8 digits
        assert_eq!(parse_cell_ref("A0"), None); // row 0 invalid
        assert_eq!(parse_cell_ref(""), None);
    }

    // Live: open Excel with a sheet, focus a cell, and run against its window. Resolves the
    // cell ref in TARGET (default "B2") and prints the bbox so the 0-vs-1 / header offset can
    // be calibrated against the real grid. Pass Excel's window handle (decimal) in
    // NAVISUAL_TEST_HWND — e.g. PowerShell:
    //   (Get-Process excel | ? { $_.MainWindowHandle -ne 0 } | select -First 1).MainWindowHandle
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; $env:TARGET="B2";
    //      cargo test --lib excel_cell_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn excel_cell_live() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND to the Excel window handle")
            .parse()
            .expect("NAVISUAL_TEST_HWND must be a decimal handle");
        let target = std::env::var("TARGET").unwrap_or_else(|_| "B2".to_string());
        let adapter = ExcelAdapter;
        let query = AdapterQuery {
            target_text: &target,
            target_role: None,
            nearby_text: None,
            avoid_bboxes: &[],
        };
        assert!(
            adapter.matches(hwnd, &query),
            "adapter should claim Excel + a cell ref (class={:?}, exe={:?})",
            window_class_lower(hwnd),
            window_exe_stem_lower(hwnd)
        );
        let started = std::time::Instant::now();
        let hit = adapter.locate(hwnd, &query).expect("locate errored");
        eprintln!(
            "excel_cell_live: target={target} resolved_in={}ms detail={}",
            started.elapsed().as_millis(),
            hit.detail
        );
        match hit.result {
            Some(r) => eprintln!("  HIT name={:?} role={} bbox={:?}", r.name, r.role, r.bbox),
            None => eprintln!("  fell through (no pointer)"),
        }
    }
}
