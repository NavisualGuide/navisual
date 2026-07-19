//! Flow A — candidate hints (feature/candidate-hints, 2026-07-18).
//!
//! After "Wrong spot", instead of silently re-pointing at the single next-best match,
//! the retry collects the top 2–3 surviving candidates and shows them all as numbered
//! boxes. **The user is never asked to choose** — we're assisting, not quizzing: their
//! next real click in the target app IS the answer. This module holds:
//!
//!   - the pure helpers (IoU dedupe for collection, point/IoU matching for resolution),
//!   - the pending-candidates record armed when boxes are shown,
//!   - the **state-readback resolution**: at the next natural backend event we read
//!     which candidate the user acted on from the APP'S OWN STATE — Word's Selection
//!     (a prose click moves the caret), PowerPoint's shape Selection (a shape click
//!     selects it), else the UIA focused element. No input hooks, no click listening:
//!     we look at what the action *changed*, through the same channels the candidates
//!     came from. The resolved pick is banked to the local training mirror
//!     (`training/feedback.jsonl`) as a ground-truth label.

use crate::capture::Rect;

/// A user-rejected spot, SCOPED to the target it was rejected for. A "wrong spot" is
/// the statement "this rect is not `target`" — not "never point anywhere in this rect
/// again for anything". Live incident (2026-07-18 Word 'Save' test): the rejection of a
/// whole-heading rect (586×100) for target "Save this for later…" blanket-excluded the
/// heading's own "Save" when the AI re-targeted to just 'Save' — the pointer then
/// contradicted the instruction ("the very first word in the large heading") by sliding
/// to a body-text occurrence. Entries are filtered by [`scoped_avoid`] against the
/// CURRENT step's target before any locate; when the AI renames the same spot ("I meant
/// the Save button"), the pointer may legitimately return to a rejected rect — correct,
/// because the claim about it changed.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AvoidEntry {
    pub bbox: Rect,
    pub target: String,
}

/// The avoid rects that apply to a locate for `target_text`: entries whose recorded
/// target matches (trimmed, case-insensitive). No target → nothing applies.
pub fn scoped_avoid(entries: &[AvoidEntry], target_text: Option<&str>) -> Vec<Rect> {
    let Some(t) = target_text.map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty())
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|e| e.target.trim().to_lowercase() == t)
        .map(|e| e.bbox)
        .collect()
}

/// Pending state armed when candidate boxes are shown; resolved (or expired) at the
/// next backend event. Lives in `GuidanceState`.
#[derive(Clone, Debug)]
pub struct PendingCandidates {
    /// The AI request whose step these candidates belong to (training join key).
    pub request_id: Option<String>,
    pub target_text: String,
    /// Ranked candidate boxes exactly as shown (virtual-desktop pixels).
    pub boxes: Vec<Rect>,
    pub target_hwnd: Option<usize>,
    pub armed_ms: i64,
    /// App-state readback taken AT ARM TIME. Without it, stale state produces false
    /// labels: e.g. the user clicked the (wrong) pointed spot earlier, pressed
    /// ✗ Wrong, then ignored the boxes and typed a new task — the caret still sits
    /// inside a candidate, and a naive readback would record a "chosen" that never
    /// happened. Resolution only trusts a readback that CHANGED from this baseline.
    pub baseline: Option<Rect>,
}

/// What actually happened to a shown candidate set, decided by [`resolution_outcome`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    /// The readback changed from baseline and landed on a candidate.
    Chosen(usize),
    /// The user pressed ✗ Wrong again (correction/retry trigger) — an explicit
    /// rejection of the whole shown set; any readback is disregarded as a label.
    Escalated,
    /// The readback changed from baseline but matches no candidate — the user acted
    /// somewhere else. The resolved rect is the gold negative label: the TRUE target,
    /// never shown.
    ActedElsewhere,
    /// The readback is identical to the arm-time baseline — the user never acted in
    /// the app (typed in the panel, moved on). No label.
    NoAction,
    /// No readback channel produced a rect.
    NoReadback,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Chosen(_) => "chosen",
            Outcome::Escalated => "escalated",
            Outcome::ActedElsewhere => "acted_elsewhere",
            Outcome::NoAction => "no_action",
            Outcome::NoReadback => "no_readback",
        }
    }
}

/// Pure resolution decision. `escalation` = the resolving event is itself a rejection
/// (another ✗ Wrong → correction/retry) rather than neutral progress (guide/next_step).
pub fn resolution_outcome(
    escalation: bool,
    baseline: Option<&Rect>,
    readback: Option<&Rect>,
    candidates: &[Rect],
) -> Outcome {
    if escalation {
        return Outcome::Escalated;
    }
    let Some(rect) = readback else {
        return Outcome::NoReadback;
    };
    if baseline.is_some_and(|b| b == rect) {
        return Outcome::NoAction;
    }
    match match_candidate(rect, candidates) {
        Some(i) => Outcome::Chosen(i),
        None => Outcome::ActedElsewhere,
    }
}

/// Candidates older than this are dropped unresolved — the screen has almost
/// certainly changed too much for the app-state readback to mean anything.
pub const PENDING_EXPIRY_MS: i64 = 10 * 60 * 1000;

/// Intersection-over-union of two rects.
pub fn iou(a: &Rect, b: &Rect) -> f64 {
    let ax2 = a.x + a.width as i32;
    let ay2 = a.y + a.height as i32;
    let bx2 = b.x + b.width as i32;
    let by2 = b.y + b.height as i32;
    let ix = (ax2.min(bx2) - a.x.max(b.x)).max(0) as f64;
    let iy = (ay2.min(by2) - a.y.max(b.y)).max(0) as f64;
    let inter = ix * iy;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = (a.width as f64 * a.height as f64) + (b.width as f64 * b.height as f64) - inter;
    inter / union
}

/// Successive-retry collection can return the "next-best" as a slightly shifted copy
/// of an earlier box (the avoid veto is centre-based, so a large overlapping rect can
/// survive it). Collapse near-duplicates before showing.
pub fn dedupe_candidates(mut boxes: Vec<Rect>) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for b in boxes.drain(..) {
        if out.iter().all(|kept| iou(kept, &b) < 0.5) {
            out.push(b);
        }
    }
    out
}

/// Which candidate did the user's action land on? `resolved` is the rect (or caret
/// point as a zero/small rect) the app reports for the acted-on element. Containment
/// of the resolved centre wins; else the best IoU ≥ 0.3. None → unresolved (honest).
pub fn match_candidate(resolved: &Rect, candidates: &[Rect]) -> Option<usize> {
    let cx = resolved.x + resolved.width as i32 / 2;
    let cy = resolved.y + resolved.height as i32 / 2;
    for (i, c) in candidates.iter().enumerate() {
        if cx >= c.x
            && cx < c.x + c.width as i32
            && cy >= c.y
            && cy < c.y + c.height as i32
        {
            return Some(i);
        }
    }
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (i, iou(c, resolved)))
        .filter(|(_, v)| *v >= 0.3)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Read the rect of whatever the user last acted on in the target app, via the best
/// available channel. Returns `(rect, method)`.
#[cfg(windows)]
pub fn read_acted_rect(target_hwnd: Option<usize>) -> Option<(Rect, &'static str)> {
    use crate::locator::adapters::office_com;

    let hwnd = target_hwnd?;
    let exe = crate::locator::adapters::window_exe_stem_lower(hwnd);
    match exe.as_str() {
        // A click in Word prose moves the caret: Selection.Range → GetPoint.
        "winword" => {
            let app = office_com::get_active_object("Word.Application").ok()?;
            let window = office_com::resolve_app_window(&app, hwnd).ok()?;
            let sel = office_com::get_path(&app, &["Selection", "Range"]).ok()?;
            let (mut l, mut t, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
            let mut args = [
                office_com::v_byref_i32(&mut l),
                office_com::v_byref_i32(&mut t),
                office_com::v_byref_i32(&mut w),
                office_com::v_byref_i32(&mut h),
                office_com::v_dispatch(&sel),
            ];
            office_com::call_byref(&window, "GetPoint", &mut args).ok()?;
            drop(args);
            Some((
                Rect {
                    x: l,
                    y: t,
                    width: w.max(0) as u32,
                    height: h.max(0) as u32,
                },
                "word-selection",
            ))
        }
        // A click on a PowerPoint shape selects it: Selection.ShapeRange(1).
        "powerpnt" => {
            let app = office_com::get_active_object("PowerPoint.Application").ok()?;
            let window = office_com::resolve_app_window(&app, hwnd).ok()?;
            let shape = office_com::get_path(&window, &["Selection", "ShapeRange"])
                .and_then(|sr| {
                    office_com::get_indexed(&sr, "Item", vec![office_com::v_i32(1)])
                })
                .and_then(|v| office_com::as_dispatch(&v))
                .ok()?;
            let left = office_com::as_f64(&office_com::get(&shape, "Left").ok()?).ok()?;
            let top = office_com::as_f64(&office_com::get(&shape, "Top").ok()?).ok()?;
            let width = office_com::as_f64(&office_com::get(&shape, "Width").ok()?).ok()?;
            let height = office_com::as_f64(&office_com::get(&shape, "Height").ok()?).ok()?;
            let (x1, y1, x2, y2) = crate::locator::adapters::ppt_points_to_pixels(
                &window,
                left,
                top,
                left + width,
                top + height,
            )
            .ok()?;
            Some((
                Rect {
                    x: x1,
                    y: y1,
                    width: (x2 - x1).max(0) as u32,
                    height: (y2 - y1).max(0) as u32,
                },
                "ppt-shape-selection",
            ))
        }
        // Everything else: the UIA focused element (a click focuses most controls).
        _ => {
            let automation = uiautomation::UIAutomation::new().ok()?;
            let el = automation.get_focused_element().ok()?;
            let r = el.get_bounding_rectangle().ok()?;
            let (w, h) = (r.get_width().max(0) as u32, r.get_height().max(0) as u32);
            if w == 0 || h == 0 {
                return None;
            }
            Some((
                Rect {
                    x: r.get_left(),
                    y: r.get_top(),
                    width: w,
                    height: h,
                },
                "uia-focus",
            ))
        }
    }
}

#[cfg(not(windows))]
pub fn read_acted_rect(_target_hwnd: Option<usize>) -> Option<(Rect, &'static str)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn iou_basics() {
        assert_eq!(iou(&r(0, 0, 10, 10), &r(0, 0, 10, 10)), 1.0);
        assert_eq!(iou(&r(0, 0, 10, 10), &r(20, 20, 10, 10)), 0.0);
        let v = iou(&r(0, 0, 10, 10), &r(5, 0, 10, 10));
        assert!(v > 0.3 && v < 0.4, "half-overlap ≈ 1/3, got {v}");
    }

    #[test]
    fn dedupe_collapses_near_duplicates() {
        let out = dedupe_candidates(vec![
            r(100, 100, 50, 20),
            r(102, 101, 50, 20), // shifted copy — dropped
            r(400, 100, 50, 20), // distinct — kept
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].x, 100);
        assert_eq!(out[1].x, 400);
    }

    #[test]
    fn match_by_center_containment() {
        let candidates = vec![r(100, 100, 50, 20), r(400, 100, 50, 20)];
        // Caret (collapsed range → tiny rect) inside candidate 1.
        assert_eq!(match_candidate(&r(410, 105, 2, 14), &candidates), Some(1));
        assert_eq!(match_candidate(&r(120, 110, 0, 14), &candidates), Some(0));
    }

    #[test]
    fn scoped_avoid_filters_by_target() {
        let entries = vec![
            AvoidEntry {
                bbox: r(100, 100, 586, 100),
                target: "Save this for later, access it anywhere".into(),
            },
            AvoidEntry {
                bbox: r(400, 300, 32, 23),
                target: "Save".into(),
            },
        ];
        // The live incident: re-targeting to 'Save' must NOT inherit the heading
        // rejection — only the entry recorded for 'Save' applies.
        let applied = scoped_avoid(&entries, Some("Save"));
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].x, 400);
        // Case/whitespace-insensitive.
        assert_eq!(scoped_avoid(&entries, Some(" save ")).len(), 1);
        // The original target still sees its own rejection.
        assert_eq!(
            scoped_avoid(&entries, Some("Save this for later, access it anywhere")).len(),
            1
        );
        // No target / unknown target → nothing applies.
        assert!(scoped_avoid(&entries, None).is_empty());
        assert!(scoped_avoid(&entries, Some("Bold")).is_empty());
    }

    #[test]
    fn outcome_decision_table() {
        let candidates = vec![r(100, 100, 50, 20), r(400, 100, 50, 20)];
        let baseline = r(700, 300, 60, 20);
        // Escalation (another Wrong press) disregards even a candidate-matching readback.
        assert_eq!(
            resolution_outcome(true, Some(&baseline), Some(&r(410, 105, 2, 14)), &candidates),
            Outcome::Escalated
        );
        // Readback unchanged from baseline → the user never acted → no label. This is
        // the stale-caret false positive: baseline INSIDE candidate 0, untouched.
        let stale = r(120, 110, 2, 14);
        assert_eq!(
            resolution_outcome(false, Some(&stale), Some(&stale), &candidates),
            Outcome::NoAction
        );
        // Readback moved into a candidate → genuine pick.
        assert_eq!(
            resolution_outcome(false, Some(&baseline), Some(&r(410, 105, 2, 14)), &candidates),
            Outcome::Chosen(1)
        );
        // Readback moved somewhere that matches nothing → the true target was elsewhere.
        assert_eq!(
            resolution_outcome(false, Some(&baseline), Some(&r(900, 500, 40, 20)), &candidates),
            Outcome::ActedElsewhere
        );
        // No readback channel.
        assert_eq!(
            resolution_outcome(false, Some(&baseline), None, &candidates),
            Outcome::NoReadback
        );
    }

    #[test]
    fn match_by_iou_fallback_and_unresolved() {
        let candidates = vec![r(100, 100, 50, 20), r(400, 100, 50, 20)];
        // Focused-element rect larger than the candidate (control vs word) — centre
        // slightly outside, IoU carries it.
        assert_eq!(match_candidate(&r(95, 95, 55, 30), &candidates), Some(0));
        // Far away → honest None.
        assert_eq!(match_candidate(&r(800, 500, 40, 20), &candidates), None);
        assert_eq!(match_candidate(&r(0, 0, 4000, 3000), &candidates), None);
    }
}
