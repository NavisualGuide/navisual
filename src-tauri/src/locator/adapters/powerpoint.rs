//! PowerPoint slide-canvas adapter — Office COM generalisation (2026-07-18).
//!
//! The slide editing canvas is **UIA-opaque** (probed live: zero A11y candidates for
//! placeholders, hit-test `uia=unknown`), so before this adapter the only channel was
//! OCR — which breaks exactly when it matters most: an empty placeholder's prompt text
//! ("Click to add title") **vanishes the moment the user clicks into it**, and a live
//! session watched the gate then correctly refuse a fuzzy match on the *other*
//! placeholder (locator-testing.md, PowerPoint session 2026-07-17).
//!
//! The object model doesn't care about any of that: shapes exist with exact geometry in
//! every state. `Application.ActiveWindow.View.Slide.Shapes` → match the target against
//! placeholder prompts / shape text / shape names → `PointsToScreenPixelsX/Y` converts
//! point coordinates to screen pixels (DPI- and zoom-correct, per Office).
//!
//! Hijack guard: the adapter only claims **content-like** targets (role textbox/text/
//! other). A ribbon target ("Design", role tab) never reaches it, and a matched shape
//! must be unique at its match tier — two candidates → fall through, no wrong pointer.

use super::office_com::{
    as_bool, as_dispatch, as_f64, as_i32, as_string, call, get, get_indexed, get_active_object,
    get_path, v_f32, v_i32,
};
use super::{window_class_lower, window_exe_stem_lower, Adapter, AdapterHit, AdapterQuery};
use crate::capture::Rect;
use crate::locator::LocateResult;
use anyhow::{Context, Result};

pub struct PowerPointAdapter;

/// Roles this adapter may claim. Control roles (button/tab/menuitem/…) are excluded so a
/// ribbon target can never be hijacked by a same-named shape on the slide. A missing role
/// is also excluded — weak models omit it too casually to treat as "content".
fn role_is_content_like(role: Option<&str>) -> bool {
    matches!(role, Some("textbox") | Some("text") | Some("other"))
}

impl Adapter for PowerPointAdapter {
    fn name(&self) -> &'static str {
        "powerpoint"
    }

    fn matches(&self, hwnd: usize, query: &AdapterQuery) -> bool {
        if !role_is_content_like(query.target_role) || query.target_text.trim().len() < 2 {
            return false;
        }
        window_class_lower(hwnd) == "pptframeclass" || window_exe_stem_lower(hwnd) == "powerpnt"
    }

    fn locate(&self, hwnd: usize, query: &AdapterQuery) -> Result<AdapterHit> {
        let app = match get_active_object("PowerPoint.Application") {
            Ok(a) => a,
            Err(e) => return Ok(AdapterHit::fell_through(format!("no COM instance: {e}"))),
        };
        // Match the COM window to OUR hwnd — never trust ActiveWindow blindly (a pinned
        // but not-active presentation would resolve the wrong window's geometry).
        let window = match super::office_com::resolve_app_window(&app, hwnd) {
            Ok(w) => w,
            Err(e) => return Ok(AdapterHit::fell_through(format!("{e}"))),
        };
        // Slide of the current editing view. Fails outside normal view (slideshow,
        // sorter) — fall through rather than guess.
        let slide = match get_path(&window, &["View", "Slide"]) {
            Ok(s) => s,
            Err(e) => {
                return Ok(AdapterHit::fell_through(format!(
                    "no editable slide in view: {e}"
                )))
            }
        };

        let shapes = get_path(&slide, &["Shapes"])?;
        let count = as_i32(&get(&shapes, "Count")?)?;
        let mut candidates: Vec<ShapeInfo> = Vec::new();
        for i in 1..=count {
            let Ok(item_v) = get_indexed(&shapes, "Item", vec![v_i32(i)]) else {
                continue;
            };
            let Ok(shape) = as_dispatch(&item_v) else {
                continue;
            };
            let name = get(&shape, "Name")
                .ok()
                .and_then(|v| as_string(&v).ok())
                .unwrap_or_default();
            let has_text = get(&shape, "HasTextFrame")
                .ok()
                .and_then(|v| as_bool(&v).ok())
                .unwrap_or(false);
            let text = if has_text {
                get_path(&shape, &["TextFrame", "TextRange"])
                    .ok()
                    .and_then(|tr| get(&tr, "Text").ok())
                    .and_then(|v| as_string(&v).ok())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            candidates.push(ShapeInfo {
                index: i,
                name,
                text,
            });
        }

        let Some(pick) = pick_shape(query.target_text, &candidates) else {
            return Ok(AdapterHit::fell_through(format!(
                "no unique shape match among {} shapes",
                candidates.len()
            )));
        };
        let picked = &candidates[pick.index_in_candidates];

        // Re-fetch the picked shape and convert its point geometry to screen pixels.
        let shape = as_dispatch(&get_indexed(&shapes, "Item", vec![v_i32(picked.index)])?)?;
        let left = as_f64(&get(&shape, "Left")?)?;
        let top = as_f64(&get(&shape, "Top")?)?;
        let width = as_f64(&get(&shape, "Width")?)?;
        let height = as_f64(&get(&shape, "Height")?)?;
        let (x1, y1, x2, y2) =
            match convert_rect_to_pixels(&window, left, top, left + width, top + height) {
                Ok(r) => r,
                Err(e) => {
                    return Ok(AdapterHit::fell_through(format!(
                        "PointsToScreenPixels failed: {e}"
                    )))
                }
            };
        let (w, h) = ((x2 - x1).max(0) as u32, (y2 - y1).max(0) as u32);
        if w == 0 || h == 0 || !super::rect_is_onscreen(x1, y1) {
            return Ok(AdapterHit::fell_through(format!(
                "shape rect off-screen/empty ({x1},{y1} {w}×{h})"
            )));
        }

        Ok(AdapterHit {
            result: Some(LocateResult {
                bbox: Rect {
                    x: x1,
                    y: y1,
                    width: w,
                    height: h,
                },
                name: picked.name.clone(),
                role: "PptShape".to_string(),
                confidence: 1.0,
            }),
            detail: format!(
                "shape {} ({:?}) via {}",
                picked.index, picked.name, pick.tier
            ),
        })
    }
}

/// Convert two point-space corners to screen pixels via the window's
/// `PointsToScreenPixelsX/Y`.
///
/// PPT quirk (probed live 2026-07-18): the conversion throws "Illegal value"
/// (0x80020009, any argument) unless the window's active pane is the SLIDE pane — e.g.
/// after the user clicked a slide thumbnail, the thumbnail pane holds pane focus and the
/// conversion is simply unavailable. Recovery: remember the active pane, activate the
/// slide pane (Panes(2) in normal view), convert, then restore the original pane — net
/// zero state change. The direct attempt runs first so the common case (slide pane
/// already active) has no side effects at all.
fn convert_rect_to_pixels(
    window: &windows::Win32::System::Com::IDispatch,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
) -> Result<(i32, i32, i32, i32)> {
    let px = |name: &str, pts: f64| -> Result<i32> {
        as_i32(&call(window, name, vec![v_f32(pts as f32)])?)
    };
    let convert = |_: ()| -> Result<(i32, i32, i32, i32)> {
        Ok((
            px("PointsToScreenPixelsX", left)?,
            px("PointsToScreenPixelsY", top)?,
            px("PointsToScreenPixelsX", right)?,
            px("PointsToScreenPixelsY", bottom)?,
        ))
    };
    if let Ok(r) = convert(()) {
        return Ok(r);
    }

    // Pane dance. Identify the currently active pane by COM identity so it can be
    // restored afterwards (Pane has no Index property).
    use windows::core::Interface;
    let panes = get_path(window, &["Panes"])?;
    let count = as_i32(&get(&panes, "Count")?)?;
    if count < 2 {
        return convert(()); // not normal view — report the original error
    }
    let active_raw = get_path(window, &["ActivePane"])
        .ok()
        .and_then(|p| p.cast::<windows::core::IUnknown>().ok())
        .map(|u| u.as_raw() as usize);
    let mut original: Option<windows::Win32::System::Com::IDispatch> = None;
    if let Some(active_raw) = active_raw {
        for i in 1..=count {
            let Ok(pane) = get_indexed(&panes, "Item", vec![v_i32(i)]).and_then(|v| as_dispatch(&v))
            else {
                continue;
            };
            let same = pane
                .cast::<windows::core::IUnknown>()
                .map(|u| u.as_raw() as usize == active_raw)
                .unwrap_or(false);
            if same && i != 2 {
                original = Some(pane);
            }
        }
    }
    let slide_pane = as_dispatch(&get_indexed(&panes, "Item", vec![v_i32(2)])?)?;
    call(&slide_pane, "Activate", vec![]).context("Panes(2).Activate")?;
    let result = convert(());
    if let Some(orig) = original {
        let _ = call(&orig, "Activate", vec![]); // best-effort restore
    }
    result
}

pub(crate) struct ShapeInfo {
    /// 1-based `Shapes.Item` index.
    pub index: i32,
    pub name: String,
    pub text: String,
}

pub(crate) struct ShapePick {
    pub index_in_candidates: usize,
    pub tier: &'static str,
}

/// Normalise for matching: lowercase, collapse whitespace (PowerPoint text uses \r for
/// paragraph breaks), trim.
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Map a placeholder PROMPT target ("Click to add title") to the substring its shape's
/// NAME carries ("Title 1" / "Subtitle 2" / "Content Placeholder 2" / "Text Placeholder").
/// The prompt text is rendered by PowerPoint but is NOT part of the shape's text — an
/// empty placeholder has empty `TextRange.Text` — so this is the only way to resolve it.
fn prompt_keyword(target_norm: &str) -> Option<&'static [&'static str]> {
    if !target_norm.starts_with("click to add") {
        return None;
    }
    if target_norm.contains("subtitle") {
        Some(&["subtitle"])
    } else if target_norm.contains("title") {
        Some(&["title"])
    } else if target_norm.contains("text") {
        // Body placeholders are named "Content Placeholder N" or "Text Placeholder N".
        Some(&["content placeholder", "text placeholder"])
    } else {
        None
    }
}

/// Pick the target shape, or None when nothing matches uniquely. Tiers, strongest first:
/// 1. placeholder prompt ("click to add title" → empty shape named like a Title)
/// 2. exact text (the shape's whole text equals the target)
/// 3. containment (target within the shape text) — unique only
/// 4. exact name ("Title 1")
///
/// Any tier with 2+ candidates falls through — no wrong pointer.
pub(crate) fn pick_shape(target: &str, shapes: &[ShapeInfo]) -> Option<ShapePick> {
    let t = norm(target);
    if t.is_empty() {
        return None;
    }

    if let Some(keywords) = prompt_keyword(&t) {
        // Subtlety: "title" is a substring of "subtitle", so a title prompt must not
        // claim a subtitle placeholder. Handle by excluding subtitle names for the
        // bare-"title" keyword.
        let matches: Vec<usize> = shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let n = norm(&s.name);
                s.text.trim().is_empty()
                    && keywords.iter().any(|k| n.contains(k))
                    && !(keywords == ["title"] && n.contains("subtitle"))
            })
            .map(|(i, _)| i)
            .collect();
        if let [only] = matches[..] {
            return Some(ShapePick {
                index_in_candidates: only,
                tier: "placeholder-prompt",
            });
        }
        if matches.len() > 1 {
            return None;
        }
        // No empty placeholder of that kind — fall through to the text tiers (the
        // model may be echoing stale prompt text for a now-filled placeholder).
    }

    let exact: Vec<usize> = shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| norm(&s.text) == t)
        .map(|(i, _)| i)
        .collect();
    if let [only] = exact[..] {
        return Some(ShapePick {
            index_in_candidates: only,
            tier: "exact-text",
        });
    }
    if exact.len() > 1 {
        return None;
    }

    let contains: Vec<usize> = shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.text.trim().is_empty() && norm(&s.text).contains(&t))
        .map(|(i, _)| i)
        .collect();
    if let [only] = contains[..] {
        return Some(ShapePick {
            index_in_candidates: only,
            tier: "text-contains",
        });
    }
    if contains.len() > 1 {
        return None;
    }

    let by_name: Vec<usize> = shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| norm(&s.name) == t)
        .map(|(i, _)| i)
        .collect();
    if let [only] = by_name[..] {
        return Some(ShapePick {
            index_in_candidates: only,
            tier: "shape-name",
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes() -> Vec<ShapeInfo> {
        vec![
            ShapeInfo {
                index: 1,
                name: "Title 1".into(),
                text: "".into(),
            },
            ShapeInfo {
                index: 2,
                name: "Content Placeholder 2".into(),
                text: "Day 1: know your friends\r".into(),
            },
        ]
    }

    #[test]
    fn prompt_resolves_empty_title_placeholder() {
        let s = shapes();
        let pick = pick_shape("Click to add title", &s).expect("should match");
        assert_eq!(pick.index_in_candidates, 0);
        assert_eq!(pick.tier, "placeholder-prompt");
    }

    #[test]
    fn prompt_for_text_resolves_content_placeholder_only_when_empty() {
        let mut s = shapes();
        // Filled content placeholder — the prompt text is gone on screen, and the
        // prompt tier must not claim it; the exact-text tier still can via its text.
        assert!(pick_shape("Click to add text", &s).is_none());
        s[1].text = String::new();
        let pick = pick_shape("Click to add text", &s).expect("empty body should match");
        assert_eq!(pick.index_in_candidates, 1);
    }

    #[test]
    fn title_prompt_does_not_claim_subtitle() {
        let s = vec![
            ShapeInfo {
                index: 1,
                name: "Subtitle 2".into(),
                text: "".into(),
            },
            ShapeInfo {
                index: 2,
                name: "Title 1".into(),
                text: "".into(),
            },
        ];
        let pick = pick_shape("Click to add title", &s).expect("should match the title");
        assert_eq!(pick.index_in_candidates, 1);
        let sub = pick_shape("Click to add subtitle", &s).expect("should match the subtitle");
        assert_eq!(sub.index_in_candidates, 0);
    }

    #[test]
    fn exact_and_containment_text_tiers() {
        let s = shapes();
        let pick = pick_shape("Day 1: know your friends", &s).expect("exact text");
        assert_eq!(pick.index_in_candidates, 1);
        assert_eq!(pick.tier, "exact-text");
        let pick = pick_shape("know your friends", &s).expect("containment");
        assert_eq!(pick.tier, "text-contains");
    }

    #[test]
    fn ambiguity_falls_through() {
        let s = vec![
            ShapeInfo {
                index: 1,
                name: "TextBox 1".into(),
                text: "Sales".into(),
            },
            ShapeInfo {
                index: 2,
                name: "TextBox 2".into(),
                text: "Sales".into(),
            },
        ];
        assert!(pick_shape("Sales", &s).is_none());
    }

    #[test]
    fn role_gate_blocks_control_roles() {
        assert!(role_is_content_like(Some("textbox")));
        assert!(role_is_content_like(Some("other")));
        assert!(!role_is_content_like(Some("button")));
        assert!(!role_is_content_like(Some("tab")));
        assert!(!role_is_content_like(None));
    }

    // Probe: what argument form does PointsToScreenPixelsX accept, and what does the
    // window/view state look like? (First live run rejected VT_R4 100.0 with
    // "Illegal value" 0x80020009.)
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; cargo test --lib ppt_p2p_probe_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ppt_p2p_probe_live() {
        use super::super::office_com::{call, get, get_active_object, get_path, v_f32, v_i32};
        let app = get_active_object("PowerPoint.Application").expect("connect");
        let window = get_path(&app, &["ActiveWindow"]).expect("ActiveWindow");
        for (label, probe) in [
            ("ViewType", get(&window, "ViewType").and_then(|v| as_i32(&v))),
            ("Active", get(&window, "Active").and_then(|v| as_i32(&v))),
            ("WindowState", get(&window, "WindowState").and_then(|v| as_i32(&v))),
            ("HWND", get(&window, "HWND").and_then(|v| as_i32(&v))),
        ] {
            eprintln!("{label}: {probe:?}");
        }
        for (label, arg) in [("VT_R4 100.0", v_f32(100.0)), ("VT_I4 100", v_i32(100))] {
            let r = call(&window, "PointsToScreenPixelsX", vec![arg]).and_then(|v| as_i32(&v));
            eprintln!("PointsToScreenPixelsX({label}) = {r:?}");
        }
        // ActivePane theory: the classic VBA fix for "Illegal value" here is activating
        // the slide pane (Panes(2) in normal view) first — our earlier probe clicks
        // activated the THUMBNAIL pane.
        let pane_idx = get_path(&window, &["ActivePane"])
            .and_then(|p| get(&p, "Index"))
            .and_then(|v| as_i32(&v));
        eprintln!("ActivePane.Index = {pane_idx:?}");
        if let Ok(panes) = get_path(&window, &["Panes"]) {
            use super::super::office_com::get_indexed;
            let n = get(&panes, "Count").and_then(|v| as_i32(&v));
            eprintln!("Panes.Count = {n:?}");
            if let Ok(pane2) = get_indexed(&panes, "Item", vec![v_i32(2)]).and_then(|v| {
                super::super::office_com::as_dispatch(&v)
            }) {
                let act = call(&pane2, "Activate", vec![]);
                eprintln!("Panes(2).Activate = {:?}", act.is_ok());
                let r = call(&window, "PointsToScreenPixelsX", vec![v_f32(100.0)])
                    .and_then(|v| as_i32(&v));
                eprintln!("after activate: PointsToScreenPixelsX(100.0) = {r:?}");
            }
        }
    }

    // Live: PowerPoint open on a slide in normal view. Resolves TARGET (default
    // "Click to add title") against the active slide's shapes and prints the rect.
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; $env:TARGET="Welcome";
    //      cargo test --lib ppt_shape_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ppt_shape_live() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        let target = std::env::var("TARGET").unwrap_or_else(|_| "Click to add title".into());
        let adapter = PowerPointAdapter;
        let query = AdapterQuery {
            target_text: &target,
            target_role: Some("textbox"),
            nearby_text: None,
        };
        assert!(adapter.matches(hwnd, &query), "adapter should claim PowerPoint");
        let started = std::time::Instant::now();
        let hit = adapter.locate(hwnd, &query).expect("locate errored");
        eprintln!(
            "ppt_shape_live: target={target:?} in {}ms detail={}",
            started.elapsed().as_millis(),
            hit.detail
        );
        match hit.result {
            Some(r) => eprintln!("  HIT name={:?} role={} bbox={:?}", r.name, r.role, r.bbox),
            None => eprintln!("  fell through (no pointer)"),
        }
    }
}
