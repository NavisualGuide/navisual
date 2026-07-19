//! Blender tool-shelf adapter — the first script-channel adapter (script-adapters-plan.md
//! §3.5/§3.5b, built 2026-07-19).
//!
//! Blender's OpenGL surface has no UIA and no OCR-able text; the 46-icon template pack
//! covers it today. This adapter upgrades tool targets from template *search* to
//! **derived geometry**: the pack-shipped `navisual_bridge.py` addon (user-enabled — the
//! add-on checkbox is the consent) answers read-only localhost queries, and its `tools`
//! query returns the shelf's ordered slots with rects computed from order × widget unit ×
//! scroll (calibrated live on 5.1.2 at ui_scale 1.0 AND 2.0 — scale-multiplicative, so
//! DPI/theme drift dissolves: no pixels are compared for position at all).
//!
//! Bridge not running (addon not installed/enabled, Blender closed) → connection refused
//! in ~0 ms → fall through to the unchanged pipeline (the template pack still works).
//! Coordinates: bpy regions are window-relative with a BOTTOM-UP Y axis; conversion is
//! `screen_y = client_top + (win_h − (y + h))` against the hwnd's client origin.

use super::{window_exe_stem_lower, Adapter, AdapterHit, AdapterQuery};
use crate::capture::Rect;
use crate::locator::LocateResult;
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const BRIDGE_ADDR: &str = "127.0.0.1:47611";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
const READ_TIMEOUT: Duration = Duration::from_millis(700);

pub struct BlenderAdapter;

impl Adapter for BlenderAdapter {
    fn name(&self) -> &'static str {
        "blender"
    }

    fn matches(&self, hwnd: usize, query: &AdapterQuery) -> bool {
        query.target_text.trim().len() >= 2 && window_exe_stem_lower(hwnd) == "blender"
    }

    fn locate(&self, hwnd: usize, query: &AdapterQuery) -> Result<AdapterHit> {
        let tools = match bridge_query(r#"{"q":"tools"}"#) {
            Ok(v) => v,
            Err(e) => {
                return Ok(AdapterHit::fell_through(format!(
                    "bridge not reachable ({e}) — template pack path"
                )))
            }
        };
        if let Some(err) = tools.get("error").and_then(|v| v.as_str()) {
            return Ok(AdapterHit::fell_through(format!("bridge: {err}")));
        }
        let win_h = tools
            .get("window")
            .and_then(|w| w.get(1))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow!("bridge tools reply missing window size"))? as i32;
        let slots = tools
            .get("slots")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("bridge tools reply missing slots"))?;

        let target_tokens = tokens(query.target_text);
        if target_tokens.is_empty() {
            return Ok(AdapterHit::fell_through("target has no matchable tokens"));
        }

        let Some(client) = client_origin(hwnd) else {
            return Ok(AdapterHit::fell_through("client origin unavailable"));
        };

        let mut matched: Vec<(Rect, String)> = Vec::new();
        for slot in slots {
            let Some(members) = slot.get("members").and_then(|v| v.as_array()) else {
                continue;
            };
            let hit_member = members.iter().find_map(|m| {
                let label = m.get("label").and_then(|v| v.as_str()).unwrap_or("");
                let idname = m.get("idname").and_then(|v| v.as_str()).unwrap_or("");
                member_matches(&target_tokens, label, idname).then(|| {
                    if label.is_empty() { idname } else { label }.to_string()
                })
            });
            let Some(name) = hit_member else { continue };
            let Some(rect_arr) = slot.get("rect").and_then(|v| v.as_array()) else {
                continue;
            };
            let vals: Vec<i32> = rect_arr.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect();
            let [x, y_bu, w, h] = vals[..] else { continue };
            let rect = to_screen_rect(client, win_h, x, y_bu, w, h);
            if crate::locator::orchestrator::rejected_by_avoid(&rect, query.avoid_bboxes) {
                continue; // user already rejected this spot for this target
            }
            matched.push((rect, name));
        }

        match matched.len() {
            0 => {
                // No tool-shelf slot — try the Properties nav-bar tabs (second derived
                // surface; fixes the constraint/data wrong-icon class: the pack has no
                // icon for every tab, and generic stems like `object` cross-match).
                if let Some(hit) = self.try_tabs(hwnd, query)? {
                    return Ok(hit);
                }
                Ok(AdapterHit::fell_through(format!(
                    "no tool-shelf slot or nav-bar tab matches {:?} ({} slots reported)",
                    query.target_text,
                    slots.len()
                )))
            }
            1 => {
                let (bbox, name) = matched.remove(0);
                Ok(AdapterHit {
                    result: Some(LocateResult {
                        bbox,
                        name,
                        role: "BlenderTool".to_string(),
                        confidence: 1.0,
                    }),
                    detail: format!("bridge tools → derived rect for {:?}", query.target_text),
                    ambiguous: Vec::new(),
                })
            }
            n => Ok(AdapterHit::ambiguous(
                format!("{n} tool-shelf slots match {:?} — ambiguous", query.target_text),
                matched.into_iter().map(|(r, _)| r).collect(),
            )),
        }
    }
}

impl BlenderAdapter {
    /// Properties nav-bar tab resolution via the bridge's `tabs` query. Returns
    /// `Ok(None)` when tabs aren't available/matching (caller reports its own
    /// fall-through); `Some(hit)` on a match (unique or ambiguous).
    fn try_tabs(&self, hwnd: usize, query: &AdapterQuery) -> Result<Option<AdapterHit>> {
        // Role gate (live 2026-07-19): the AI marks top-bar menu entries role=menuitem
        // ("Render" → Render menu, "Render Image" inside it) — the tab matcher's
        // subset rule would hijack those onto the Render Properties TAB (it did, twice;
        // both needed a ✗ Wrong retry to recover via OCR). Tabs may claim tab-ish and
        // unspecified roles only.
        if matches!(query.target_role, Some("menuitem")) {
            return Ok(None);
        }
        let Ok(tabs) = bridge_query(r#"{"q":"tabs"}"#) else {
            return Ok(None);
        };
        if tabs.get("error").is_some() {
            return Ok(None);
        }
        let win_h = tabs
            .get("window")
            .and_then(|w| w.get(1))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let Some(list) = tabs.get("tabs").and_then(|v| v.as_array()) else {
            return Ok(None);
        };
        let Some(client) = client_origin(hwnd) else {
            return Ok(None);
        };
        let target = tokens(query.target_text);
        // "Properties"/"tab" are generic filler both sides use inconsistently —
        // compare on the distinguishing tokens.
        let strip = |ts: &[String]| -> Vec<String> {
            ts.iter()
                .filter(|t| t.as_str() != "properties" && t.as_str() != "tab")
                .cloned()
                .collect()
        };
        let target_core = strip(&target);
        if target_core.is_empty() {
            return Ok(None);
        }
        let plural_eq = |a: &str, b: &str| -> bool {
            if a == b {
                return true;
            }
            let (long, short) = if a.len() > b.len() { (a, b) } else { (b, a) };
            long.len() > 3 && long.len() == short.len() + 1 && long.strip_suffix('s') == Some(short)
        };
        let set_contains = |set: &[String], t: &str| set.iter().any(|s| plural_eq(s, t));

        // Tier 1: the tab's core tokens EQUAL the target's. Tier 2: unique
        // subset either way (2+ → ambiguous, no guess).
        let mut exact: Vec<(Rect, String)> = Vec::new();
        let mut subset: Vec<(Rect, String)> = Vec::new();
        for tab in list {
            let name = tab.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let id = tab.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut name_core = strip(&tokens(name));
            if name_core.is_empty() {
                name_core = tokens(id);
            }
            let eq = name_core.len() == target_core.len()
                && target_core.iter().all(|t| set_contains(&name_core, t));
            let sub = target_core.iter().all(|t| set_contains(&name_core, t))
                || name_core.iter().all(|t| set_contains(&target_core, t));
            if !eq && !sub {
                continue;
            }
            let Some(rect_arr) = tab.get("rect").and_then(|v| v.as_array()) else {
                continue;
            };
            let vals: Vec<i32> = rect_arr.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect();
            let [x, y_bu, w, h] = vals[..] else { continue };
            let rect = to_screen_rect(client, win_h, x, y_bu, w, h);
            if crate::locator::orchestrator::rejected_by_avoid(&rect, query.avoid_bboxes) {
                continue;
            }
            if eq {
                exact.push((rect, name.to_string()));
            } else {
                subset.push((rect, name.to_string()));
            }
        }
        let pool = if !exact.is_empty() { exact } else { subset };
        match pool.len() {
            0 => Ok(None),
            1 => {
                let (bbox, name) = pool.into_iter().next().unwrap();
                Ok(Some(AdapterHit {
                    result: Some(LocateResult {
                        bbox,
                        name,
                        role: "BlenderTab".to_string(),
                        confidence: 1.0,
                    }),
                    detail: format!("bridge tabs → derived rect for {:?}", query.target_text),
                    ambiguous: Vec::new(),
                }))
            }
            n => Ok(Some(AdapterHit::ambiguous(
                format!("{n} nav-bar tabs match {:?} — ambiguous", query.target_text),
                pool.into_iter().map(|(r, _)| r).collect(),
            ))),
        }
    }
}

/// One newline-delimited JSON round trip to the bridge.
fn bridge_query(payload: &str) -> Result<serde_json::Value> {
    let addr = BRIDGE_ADDR.parse().context("bridge addr")?;
    let mut stream =
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).context("bridge connect")?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(READ_TIMEOUT)).ok();
    stream.write_all(payload.as_bytes()).context("bridge send")?;
    stream.write_all(b"\n").context("bridge send nl")?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .context("bridge read")?;
    serde_json::from_str(&line).context("bridge reply parse")
}

/// Lowercased alphanumeric tokens of a name ("builtin.primitive_cube_add" →
/// ["builtin","primitive","cube","add"]).
fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Target matches a slot member when its tokens equal the LABEL's tokens, or are a
/// subset of the idname's ("Add Cube" ⊆ builtin.primitive_cube_add). Subset against the
/// label too ("Annotate" should hit "Annotate Line"'s slot? NO — that would multi-match
/// every annotate variant; label equality keeps slot matching precise, and the group's
/// first member carries the plain name).
fn member_matches(target: &[String], label: &str, idname: &str) -> bool {
    let label_t = tokens(label);
    if !label_t.is_empty() && label_t == target {
        return true;
    }
    let id_t = tokens(idname);
    !id_t.is_empty() && target.iter().all(|t| id_t.contains(t))
}

/// bpy window-relative bottom-up rect → virtual-desktop screen rect.
fn to_screen_rect(client: (i32, i32), win_h: i32, x: i32, y_bu: i32, w: i32, h: i32) -> Rect {
    let top_down_y = win_h - (y_bu + h);
    Rect {
        x: client.0 + x,
        y: client.1 + top_down_y,
        width: w.max(0) as u32,
        height: h.max(0) as u32,
    }
}

/// Screen coordinates of the window's client-area origin. bpy's window size is the
/// CLIENT area (verified live: bpy 1600×950 == GetClientRect), so all conversion is
/// client-relative — never the outer window rect (title bar would skew Y).
#[cfg(windows)]
fn client_origin(hwnd: usize) -> Option<(i32, i32)> {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    unsafe {
        let h = HWND(hwnd as *mut _);
        let mut pt = POINT { x: 0, y: 0 };
        ClientToScreen(h, &mut pt).as_bool().then_some((pt.x, pt.y))
    }
}

#[cfg(not(windows))]
fn client_origin(_hwnd: usize) -> Option<(i32, i32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matching_rules() {
        let t = |s: &str| tokens(s);
        // Label equality (case/punctuation-insensitive).
        assert!(member_matches(&t("Move"), "Move", "builtin.move"));
        assert!(member_matches(&t("select box"), "Select Box", "builtin.select_box"));
        // Idname subset: the AI says "Add Cube", idname is primitive_cube_add.
        assert!(member_matches(
            &t("Add Cube"),
            "Add Cube",
            "builtin.primitive_cube_add"
        ));
        assert!(member_matches(&t("cube"), "", "builtin.primitive_cube_add"));
        // The raw idname works too.
        assert!(member_matches(&t("builtin.measure"), "Measure", "builtin.measure"));
        // No cross-tool leakage: "Move" must not match Rotate.
        assert!(!member_matches(&t("Move"), "Rotate", "builtin.rotate"));
        // Label subsets do NOT match (Annotate vs Annotate Line stays distinct).
        assert!(!member_matches(&t("Annotate Line"), "Annotate", "builtin.annotate"));
        assert!(member_matches(
            &t("Annotate Line"),
            "Annotate Line",
            "builtin.annotate_line"
        ));
    }

    #[test]
    fn bottom_up_conversion() {
        // Live calibration numbers: client origin (100,50), bpy window 1600×950,
        // Move slot rect [2,765,56,32] → client top-down y = 950−(765+32) = 153.
        let r = to_screen_rect((100, 50), 950, 2, 765, 56, 32);
        assert_eq!((r.x, r.y, r.width, r.height), (102, 203, 56, 32));
    }

    // Live: Blender running with the navisual_bridge addon enabled. Resolves TARGET
    // (default "Move") via the bridge and prints the screen rect.
    // Run: $env:NAVISUAL_TEST_HWND=<hwnd>; $env:TARGET="Rotate";
    //      cargo test --lib blender_bridge_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn blender_bridge_live() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND")
            .parse()
            .expect("decimal hwnd");
        let target = std::env::var("TARGET").unwrap_or_else(|_| "Move".into());
        let adapter = BlenderAdapter;
        let query = AdapterQuery {
            target_text: &target,
            target_role: None,
            nearby_text: None,
            avoid_bboxes: &[],
        };
        assert!(adapter.matches(hwnd, &query), "adapter should claim Blender");
        let started = std::time::Instant::now();
        let hit = adapter.locate(hwnd, &query).expect("locate errored");
        eprintln!(
            "blender_bridge_live: target={target:?} in {}ms detail={}",
            started.elapsed().as_millis(),
            hit.detail
        );
        match hit.result {
            Some(r) => eprintln!("  HIT name={:?} bbox={:?}", r.name, r.bbox),
            None => eprintln!("  fell through (ambiguous={})", hit.ambiguous.len()),
        }
    }
}
