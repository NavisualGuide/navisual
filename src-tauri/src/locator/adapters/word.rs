//! Word document-text adapter — Office COM generalisation (2026-07-18).
//!
//! "Show me where it says X" inside a Word document was previously OCR-only, and the
//! corroboration gate rightly distrusts OCR words inside running prose (isolation fails
//! by construction on paragraph text — see the "model" rejection, locator-testing.md
//! 2026-07-18). The object model gives ground truth instead of a guess: `Find` proves
//! the text EXISTS at an exact range, and `Window.GetPoint` returns its screen-pixel
//! rect. No isolation heuristic needed — a COM Find hit is not a coincidental look-alike.
//!
//! Occurrence disambiguation mirrors the OCR anchor, but on real text: when the target
//! appears multiple times, the AI's `nearby_text` is matched against each occurrence's
//! surrounding characters; exactly one corroborated occurrence wins. Multiple matches
//! with no disambiguator fall through — no wrong pointer.
//!
//! Hijack guard: only claims role text/other (the AI labels ribbon controls
//! button/tab/…, and document-prose targets "other"/"text" — live-observed), and never
//! runs for short targets ("OK" would Find-match everywhere).

use super::office_com::{
    as_bool, as_dispatch, as_i32, as_string, call_byref, get, get_active_object, get_indexed,
    get_path, v_bool, v_byref_i32, v_dispatch, v_i32, v_str,
};
use super::{window_class_lower, window_exe_stem_lower, Adapter, AdapterHit, AdapterQuery};
use crate::capture::Rect;
use crate::locator::LocateResult;
use anyhow::{Context, Result};
use windows::Win32::System::Com::IDispatch;

pub struct WordAdapter;

/// Characters of document text kept on each side of an occurrence for the nearby check.
const CONTEXT_CHARS: i32 = 80;
/// Occurrence scan cap — a common word in a long document is ambiguous long before this.
const MAX_OCCURRENCES: usize = 30;
/// wdFindStop — Find must not wrap back to the start (would loop the scan).
const WD_FIND_STOP: i32 = 0;

fn role_is_prose_like(role: Option<&str>) -> bool {
    // "heading" observed live 2026-07-18: the AI labels Word document headings
    // ("Count on Word to count your words") role=heading — document prose too.
    matches!(role, Some("text") | Some("other") | Some("heading"))
}

impl Adapter for WordAdapter {
    fn name(&self) -> &'static str {
        "word"
    }

    fn matches(&self, hwnd: usize, query: &AdapterQuery) -> bool {
        if !role_is_prose_like(query.target_role) || query.target_text.trim().len() < 3 {
            return false;
        }
        window_class_lower(hwnd) == "opusapp" || window_exe_stem_lower(hwnd) == "winword"
    }

    fn locate(&self, hwnd: usize, query: &AdapterQuery) -> Result<AdapterHit> {
        let app = match get_active_object("Word.Application") {
            Ok(a) => a,
            Err(e) => return Ok(AdapterHit::fell_through(format!("no COM instance: {e}"))),
        };
        // Word is SDI — each document owns a top-level window. Match the COM window to
        // OUR hwnd so a pinned-but-not-active document is never searched through (or
        // pointed at with) another window's geometry.
        let window = match super::office_com::resolve_app_window(&app, hwnd) {
            Ok(w) => w,
            Err(e) => return Ok(AdapterHit::fell_through(format!("{e}"))),
        };
        let doc = get_path(&window, &["Document"]).context("Window.Document")?;

        let target = query.target_text.trim();
        let occurrences = find_occurrences(&doc, target)?;
        if occurrences.is_empty() {
            return Ok(AdapterHit::fell_through(format!(
                "Find: {target:?} not present in the document text"
            )));
        }
        let total = occurrences.len();

        // Flow A — occurrence-level avoid awareness: with rejected spots on record,
        // resolve every occurrence's screen rect first, drop the rejected (and the
        // unresolvable — can't be pointed at anyway), then choose among survivors.
        // This is what makes the adapter yield "the NEXT occurrence" on a Wrong-spot
        // retry instead of re-hitting the rejected one and dying at the Pass-0 veto.
        // The no-avoid path is unchanged (choose first, one GetPoint).
        let (chosen, chosen_rect, avoided_note) = if query.avoid_bboxes.is_empty() {
            let Some(chosen) = choose_occurrence(&occurrences, query.nearby_text) else {
                return Ok(AdapterHit::fell_through(format!(
                    "{total} occurrences of {target:?}, none singled out by nearby_text — ambiguous"
                )));
            };
            (chosen.clone(), None, String::new())
        } else {
            let resolved: Vec<(Occurrence, Rect)> = occurrences
                .iter()
                .filter_map(|o| {
                    range_screen_rect(&window, &doc, o.start, o.end).map(|r| (o.clone(), r))
                })
                .collect();
            let surviving: Vec<(Occurrence, Rect)> = resolved
                .into_iter()
                .filter(|(_, r)| {
                    !crate::locator::orchestrator::rejected_by_avoid(r, query.avoid_bboxes)
                })
                .collect();
            if surviving.is_empty() {
                return Ok(AdapterHit::fell_through(format!(
                    "all resolvable occurrences of {target:?} were already rejected"
                )));
            }
            let survivors: Vec<Occurrence> =
                surviving.iter().map(|(o, _)| o.clone()).collect();
            let Some(chosen) = choose_occurrence(&survivors, query.nearby_text) else {
                return Ok(AdapterHit::fell_through(format!(
                    "{} non-rejected occurrences of {target:?} remain — ambiguous",
                    survivors.len()
                )));
            };
            let rect = surviving
                .iter()
                .find(|(o, _)| o.start == chosen.start)
                .map(|(_, r)| *r);
            (chosen.clone(), rect, ", after avoid filtering".to_string())
        };

        let rect = match chosen_rect {
            Some(r) => r,
            None => match range_screen_rect(&window, &doc, chosen.start, chosen.end) {
                Some(r) => r,
                None => {
                    return Ok(AdapterHit::fell_through(format!(
                        "found {target:?} but its rect is unavailable (scrolled out of view?)"
                    )))
                }
            },
        };

        Ok(AdapterHit {
            result: Some(LocateResult {
                bbox: rect,
                name: target.to_string(),
                role: "WordText".to_string(),
                confidence: 1.0,
            }),
            detail: format!(
                "Find hit {} of {total} at chars {}..{} ({}{avoided_note})",
                chosen.ordinal,
                chosen.start,
                chosen.end,
                if query.nearby_text.is_some() {
                    "nearby-corroborated"
                } else {
                    "sole occurrence"
                }
            ),
        })
    }
}

/// Screen-pixel rect of a document char range via `Window.GetPoint`, or None when the
/// range isn't rendered (scrolled out) or reports a degenerate/off-screen rect.
fn range_screen_rect(
    window: &IDispatch,
    doc: &IDispatch,
    start: i32,
    end: i32,
) -> Option<Rect> {
    let range = as_dispatch(&get_indexed(doc, "Range", vec![v_i32(start), v_i32(end)]).ok()?).ok()?;
    let (mut left, mut top, mut width, mut height) = (0i32, 0i32, 0i32, 0i32);
    let mut args = [
        v_byref_i32(&mut left),
        v_byref_i32(&mut top),
        v_byref_i32(&mut width),
        v_byref_i32(&mut height),
        v_dispatch(&range),
    ];
    call_byref(window, "GetPoint", &mut args).ok()?;
    drop(args);
    if width <= 0 || height <= 0 || !super::rect_is_onscreen(left, top) {
        return None;
    }
    Some(Rect {
        x: left,
        y: top,
        width: width as u32,
        height: height as u32,
    })
}

#[derive(Clone)]
pub(crate) struct Occurrence {
    /// 1-based position in document order (for the trace).
    pub ordinal: usize,
    pub start: i32,
    pub end: i32,
    /// Surrounding document text (±CONTEXT_CHARS), for the nearby check.
    pub context: String,
    /// Byte offset of the target within `context` — the anchor-distance origin.
    pub target_offset: usize,
}

/// Scan the document for every occurrence of `target` (case-insensitive, no wildcards,
/// wrap off), capturing each match's range + surrounding context.
fn find_occurrences(doc: &IDispatch, target: &str) -> Result<Vec<Occurrence>> {
    let content = get_path(doc, &["Content"])?;
    let doc_end = as_i32(&get(&content, "End")?)?;
    // `Content` returns a fresh Range; Find repositions THIS range on every Execute.
    let range = content;
    let find = get_path(&range, &["Find"])?;

    let mut out = Vec::new();
    let mut last_start = -1i32;
    for ordinal in 1..=MAX_OCCURRENCES {
        // Execute positional args: FindText, MatchCase, MatchWholeWord, MatchWildcards,
        // MatchSoundsLike, MatchAllWordForms, Forward, Wrap.
        let found = call_execute(&find, target)?;
        if !found {
            break;
        }
        let start = as_i32(&get(&range, "Start")?)?;
        let end = as_i32(&get(&range, "End")?)?;
        if start <= last_start {
            break; // Wrap protection — never trust a backwards Find.
        }
        last_start = start;
        let c_start = (start - CONTEXT_CHARS).max(0);
        let c_end = (end + CONTEXT_CHARS).min(doc_end);
        let context = get_indexed(doc, "Range", vec![v_i32(c_start), v_i32(c_end)])
            .and_then(|v| as_dispatch(&v))
            .and_then(|r| get(&r, "Text"))
            .and_then(|v| as_string(&v))
            .unwrap_or_default();
        out.push(Occurrence {
            ordinal,
            start,
            end,
            context,
            target_offset: (start - c_start).max(0) as usize,
        });
    }
    Ok(out)
}

fn call_execute(find: &IDispatch, target: &str) -> Result<bool> {
    let result = super::office_com::call(
        find,
        "Execute",
        vec![
            v_str(target),
            v_bool(false), // MatchCase
            v_bool(false), // MatchWholeWord
            v_bool(false), // MatchWildcards
            v_bool(false), // MatchSoundsLike
            v_bool(false), // MatchAllWordForms
            v_bool(true),  // Forward
            v_i32(WD_FIND_STOP),
        ],
    )?;
    as_bool(&result)
}

/// Choose the occurrence to point at. Exactly one occurrence → it (nearby optional).
/// Multiple → the anchor picks the occurrence it sits CLOSEST to. Plain containment is
/// not enough: occurrences 30 chars apart share most of their ±80-char contexts, so the
/// anchor appears in both (live-observed with the two "model"s of one sentence) — but
/// its distance to each occurrence still separates them cleanly. A distance tie (or no
/// anchor in any context) stays ambiguous — no wrong pointer.
pub(crate) fn choose_occurrence<'a>(
    occurrences: &'a [Occurrence],
    nearby: Option<&str>,
) -> Option<&'a Occurrence> {
    if let [only] = occurrences {
        return Some(only);
    }
    let anchor = nearby?.trim().to_lowercase();
    if anchor.is_empty() {
        return None;
    }
    // Distance = |anchor centre − target centre| within the RAW context (byte space is
    // consistent for comparison; no whitespace collapsing so offsets stay meaningful).
    let mut scored: Vec<(&Occurrence, usize)> = occurrences
        .iter()
        .filter_map(|o| {
            let ctx = o.context.to_lowercase();
            let anchor_off = ctx.find(&anchor)?;
            let anchor_centre = anchor_off + anchor.len() / 2;
            let dist = anchor_centre.abs_diff(o.target_offset);
            Some((o, dist))
        })
        .collect();
    scored.sort_by_key(|(_, d)| *d);
    match scored[..] {
        [(only, _)] => Some(only),
        [(best, d1), (_, d2), ..] if d1 < d2 => Some(best),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occ(ordinal: usize, start: i32, context: &str, target_offset: usize) -> Occurrence {
        Occurrence {
            ordinal,
            start,
            end: start + 5,
            context: context.to_string(),
            target_offset,
        }
    }

    #[test]
    fn sole_occurrence_wins_without_nearby() {
        let occurrences = vec![occ(1, 100, "written to the small/fast model, normally", 26)];
        assert_eq!(choose_occurrence(&occurrences, None).unwrap().ordinal, 1);
    }

    #[test]
    fn nearby_picks_the_nearest_occurrence() {
        // The live 2026-07-18 case: two "model"s only 30 chars apart, so BOTH ±80-char
        // contexts contain both anchors — plain containment was ambiguous (observed);
        // distance separates them. Same context string, different target offsets.
        let ctx = "request to the small/fast model, normally a Haiku-class model. Naming";
        let first = ctx.find("model").unwrap(); // 26
        let second = ctx.rfind("model").unwrap(); // 57
        let occurrences = vec![occ(1, 163, ctx, first), occ(2, 193, ctx, second)];
        let pick = choose_occurrence(&occurrences, Some("small/fast")).unwrap();
        assert_eq!(pick.ordinal, 1);
        let pick = choose_occurrence(&occurrences, Some("Haiku-class")).unwrap();
        assert_eq!(pick.ordinal, 2);
    }

    #[test]
    fn multiple_without_nearby_is_ambiguous() {
        let occurrences = vec![occ(1, 100, "aaa", 0), occ(2, 220, "bbb", 0)];
        assert!(choose_occurrence(&occurrences, None).is_none());
        // An anchor equidistant from identical layouts is a tie — still ambiguous.
        let occurrences = vec![
            occ(1, 100, "the model x", 4),
            occ(2, 220, "the model y", 4),
        ];
        assert!(choose_occurrence(&occurrences, Some("the")).is_none());
        // An anchor found in NO context is ambiguous too.
        assert!(choose_occurrence(&occurrences, Some("ribbon")).is_none());
    }

    #[test]
    fn role_gate_blocks_control_roles() {
        assert!(role_is_prose_like(Some("text")));
        assert!(role_is_prose_like(Some("other")));
        assert!(role_is_prose_like(Some("heading")));
        assert!(!role_is_prose_like(Some("button")));
        assert!(!role_is_prose_like(Some("textbox")));
        assert!(!role_is_prose_like(None));
    }

    // Live: Word open with a document containing TARGET. Resolves it and prints the rect.
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; $env:TARGET="model"; $env:NEARBY="small/fast";
    //      cargo test --lib word_find_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn word_find_live() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        let target = std::env::var("TARGET").unwrap_or_else(|_| "model".into());
        let nearby = std::env::var("NEARBY").ok();
        // Context diagnostics: print every occurrence's captured context so anchor
        // misses are debuggable from the output.
        if let Ok(app) = get_active_object("Word.Application") {
            if let Ok(doc) = get_path(&app, &["ActiveWindow", "Document"]) {
                match find_occurrences(&doc, &target) {
                    Ok(occ) => {
                        for o in &occ {
                            eprintln!(
                                "  occurrence {} @ {}..{} ctx={:?}",
                                o.ordinal, o.start, o.end, o.context
                            );
                        }
                    }
                    Err(e) => eprintln!("  find_occurrences error: {e:#}"),
                }
            }
        }
        // AVOID="x,y,w,h" simulates a rejected spot (Flow A) — the adapter should
        // resolve the NEXT non-rejected occurrence instead of the avoided one.
        let avoid: Vec<Rect> = std::env::var("AVOID")
            .ok()
            .and_then(|s| {
                let v: Vec<i32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                (v.len() == 4).then(|| Rect {
                    x: v[0],
                    y: v[1],
                    width: v[2] as u32,
                    height: v[3] as u32,
                })
            })
            .into_iter()
            .collect();
        let adapter = WordAdapter;
        let query = AdapterQuery {
            target_text: &target,
            target_role: Some("other"),
            nearby_text: nearby.as_deref(),
            avoid_bboxes: &avoid,
        };
        assert!(adapter.matches(hwnd, &query), "adapter should claim Word");
        let started = std::time::Instant::now();
        let hit = adapter.locate(hwnd, &query).expect("locate errored");
        eprintln!(
            "word_find_live: target={target:?} nearby={nearby:?} in {}ms detail={}",
            started.elapsed().as_millis(),
            hit.detail
        );
        match hit.result {
            Some(r) => eprintln!("  HIT name={:?} role={} bbox={:?}", r.name, r.role, r.bbox),
            None => eprintln!("  fell through (no pointer)"),
        }
        // Apartment regression guard — see ppt_shape_live's matching assertion.
        assert!(
            uiautomation::UIAutomation::new().is_ok(),
            "UIAutomation must still initialise on this thread after the adapter ran"
        );
    }
}
