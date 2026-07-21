use serde::{Deserialize, Serialize};

/// Lax `Option<T>` deserializer used on AI-response fields that models
/// commonly emit as `""` or as a non-enum string instead of omitting them.
///
/// Behaviour: JSON `null` → `None`; JSON `""` (empty / whitespace) → `None`;
/// any other value that fails to deserialize → `None` (swallowed, never
/// errors); anything that parses cleanly → `Some(T)`.
///
/// This stops a single malformed field (e.g. `"target_role": ""` instead of
/// a valid enum variant) from blowing up the whole response and dropping the
/// user into the raw-JSON-as-instruction fallback path.
fn lax_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(ref s) if s.trim().is_empty() => Ok(None),
        v => Ok(serde_json::from_value::<T>(v).ok()),
    }
}

/// Lax bbox deserializer. The prompt requests `[ymin, xmin, ymax, xmax]` (four
/// numbers), but some models (notably GPT) return a 4-corner polygon
/// `[[a,b],[a,b],[a,b],[a,b]]` instead. Accept either — normalising a polygon to
/// its bounding box while preserving the model's axis order — and fall back to
/// `None` on anything malformed, so a weird bbox never drops the whole response
/// into the raw-JSON-as-instruction fallback path (see [lax_option]).
fn lax_bbox<'de, D>(deserializer: D) -> Result<Option<[f64; 4]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(bbox_from_value(&value))
}

/// Best-effort `[f64; 4]` from arbitrary JSON; `None` (never an error) when the
/// value can't be read as a box.
fn bbox_from_value(value: &serde_json::Value) -> Option<[f64; 4]> {
    let arr = value.as_array()?;
    // Canonical form: four plain numbers, used as-is.
    if arr.len() == 4 && arr.iter().all(serde_json::Value::is_number) {
        let n: Vec<f64> = arr.iter().filter_map(serde_json::Value::as_f64).collect();
        if n.len() == 4 {
            return Some([n[0], n[1], n[2], n[3]]);
        }
    }
    // Polygon form: corner points → bounding box. A polygon means the model IGNORED
    // the requested flat `[ymin,xmin,ymax,xmax]` and fell back to its native detection
    // format — and in every vision convention (COCO, image/canvas/SVG coords, GPT
    // point output) a corner point is `[x, y]`, x-first. So read p[0]=x, p[1]=y and
    // emit in OUR y-first order. (Audit 2026-07-12 C9: this used to assume corners
    // matched the flat form's y-first order, transposing a real GPT polygon — width
    // and height swapped. Bounded impact — the bbox is only a locator tiebreaker/hint
    // — but a transposed box actively mis-aims the "look here" cue.)
    let mut xs = Vec::with_capacity(arr.len());
    let mut ys = Vec::with_capacity(arr.len());
    for pt in arr {
        let p = pt.as_array()?;
        if p.len() < 2 {
            return None;
        }
        xs.push(p[0].as_f64()?);
        ys.push(p[1].as_f64()?);
    }
    if xs.is_empty() {
        return None;
    }
    let xmin = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let xmax = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let ymin = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let ymax = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some([ymin, xmin, ymax, xmax])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayType {
    Arrow,
    Highlight,
    Circle,
    Subtitle,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetRole {
    Button,
    Tab,
    Link,
    Textbox,
    Menuitem,
    Checkbox,
    Radio,
    Combobox,
    Slider,
    Image,
    Heading,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetRegion {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidanceStep {
    pub instruction: String,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_text: Option<String>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_role: Option<TargetRole>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_region: Option<TargetRegion>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_nearby_text: Option<String>,
    #[serde(default = "default_overlay", deserialize_with = "lax_overlay")]
    pub overlay_type: OverlayType,
    #[serde(default, deserialize_with = "lax_option")]
    pub clipboard: Option<String>,
    #[serde(default = "default_true")]
    pub checkpoint: bool,
    /// Bounding box returned by the AI as `[ymin, xmin, ymax, xmax]`. Drives
    /// the locator (A11y proximity sort + OCR overlap filter) and the
    /// developer "show AI bbox" overlay.
    ///
    /// All providers are instructed to use normalized 0–1000 coordinates
    /// (resolution-independent). Converted to virtual-desktop screen coords by
    /// the backend before use.
    #[serde(default, deserialize_with = "lax_bbox")]
    pub target_bbox: Option<[f64; 4]>,
    /// Structured-Context selection (v0.7 Workstream S): the id of the element the
    /// model picked from the [Screen Elements] list, when one was present. A
    /// per-request index into the snapshot in `GuidanceState` — verified against the
    /// live tree before use; never replaces `target_text` (four-pass fallback).
    #[serde(default, deserialize_with = "lax_element_id")]
    pub target_element_id: Option<u32>,
}

/// Lax element-id deserializer: models emit `12`, `"12"`, or `12.0`; anything else
/// (null, garbage, negative, fractional) becomes `None` — never a parse failure
/// (follows the [`lax_bbox`] / [`lax_overlay`] precedent).
fn lax_element_id<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                    .map(|f| f as u64)
            })
            .and_then(|v| u32::try_from(v).ok()),
        serde_json::Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    })
}

fn default_overlay() -> OverlayType {
    OverlayType::Arrow
}

/// Lax `OverlayType` deserializer: an unrecognised value (e.g. a model inventing
/// `"pointer"`) falls back to the default instead of failing the whole response
/// parse — which on the json-object providers (OpenAI/DeepSeek/Qwen) would drop
/// into the raw-JSON-as-instruction path. Mirrors [`lax_option`]; closes the
/// last un-lax field on [`GuidanceStep`].
fn lax_overlay<'de, D>(deserializer: D) -> Result<OverlayType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<OverlayType>(value).unwrap_or_else(|_| default_overlay()))
}

fn default_true() -> bool {
    true
}

/// Prompt-Rule-14 defense in depth (2026-07-18): models leak `[Screen Elements]`
/// ids into user-facing instruction text despite the rule forbidding it — observed
/// twice, from two Gemini models, in two formats ("(id 30)" flash-lite 2026-07-12,
/// which prompted the rule; "(ID: 108)" 3.5-flash 2026-07-18, which ignored it).
/// The instruction is rendered verbatim and read aloud by TTS, so the reliable
/// layer must be deterministic, not prompted. Strips parenthesized/bracketed id
/// references and markdown emphasis (`**…**`/`__…__` render as literal asterisks
/// in the panel), then tidies the whitespace the removals leave behind.
///
/// RECOVERY: a leaked id is the model's element selection in the wrong slot — the
/// 3.5-flash incident carried "(ID: 108)" in prose while `target_element_id` was
/// null. When the field is empty and the prose ids are unambiguous (exactly one
/// distinct value), move it there. Pass 0.5's live verification still gates every
/// id (role-family + name agreement at the snapshot rect), so a wrong recovered id
/// dies exactly like a wrong directly-emitted id always did — recovery adds
/// signal, never risk.
pub fn sanitize_steps(steps: &mut [GuidanceStep]) {
    for step in steps.iter_mut() {
        // Fenced-JSON leak (live 2026-07-19, gemini-3.5-flash): the model nested a
        // WHOLE response JSON inside the instruction field as a ```json block — the
        // outer parse succeeds, so no recovery path fires and the raw block renders
        // (and would be read aloud). Recover the inner instruction (and any target
        // fields the outer step left empty) before the normal cleaning below.
        recover_fenced_json_instruction(step);
        let (cleaned, ids) = sanitize_instruction_text(&step.instruction);
        if cleaned != step.instruction {
            log::info!("[sanitize] instruction cleaned (leaked ids: {ids:?})");
            step.instruction = cleaned;
        }
        if step.target_element_id.is_none() {
            let mut distinct = ids;
            distinct.sort_unstable();
            distinct.dedup();
            if let [only] = distinct[..] {
                log::info!("[sanitize] recovered target_element_id={only} from instruction prose");
                step.target_element_id = Some(only);
            }
        }
    }
}

/// Extract a JSON string value for `key` from possibly-MALFORMED JSON text (the live
/// leak had trailing garbage — `}\nLoc``` ` — so a full parse can't be relied on).
/// Returns the unescaped value of the FIRST occurrence.
fn extract_json_string(text: &str, key: &str) -> Option<String> {
    let pattern = format!(r#""{key}"\s*:\s*("(?:[^"\\]|\\.)*")"#, key = regex::escape(key));
    let re = regex::Regex::new(&pattern).ok()?;
    let m = re.captures(text)?;
    serde_json::from_str::<String>(&m[1]).ok()
}

/// When an instruction is substantially a fenced/inline JSON blob carrying our own
/// response schema, replace it with the inner instruction and backfill empty target
/// fields from the blob. Conservative trigger: the text must contain both a code fence
/// (or a leading `{`) and a quoted `"instruction"` key — ordinary prose can't trip it.
fn recover_fenced_json_instruction(step: &mut GuidanceStep) {
    let text = step.instruction.trim().to_string();
    let text = text.as_str();
    let fenced = text.contains("```");
    let jsonish = fenced || text.starts_with('{');
    if !jsonish || !text.contains("\"instruction\"") {
        return;
    }
    let Some(inner) = extract_json_string(text, "instruction") else {
        // JSON-shaped but unrecoverable — strip the fences at least.
        if fenced {
            step.instruction = text.replace("```json", " ").replace("```", " ").trim().to_string();
            log::info!("[sanitize] fenced block stripped (no inner instruction found)");
        }
        return;
    };
    log::info!("[sanitize] fenced-JSON instruction recovered ({} chars → {})", text.len(), inner.len());
    step.instruction = inner;
    if step.target_text.as_deref().is_none_or(|t| t.trim().is_empty()) {
        step.target_text = extract_json_string(text, "target_text");
    }
    if step.target_nearby_text.is_none() {
        step.target_nearby_text = extract_json_string(text, "target_nearby_text");
    }
}

fn sanitize_instruction_text(text: &str) -> (String, Vec<u32>) {
    use std::sync::OnceLock;
    static ID_REF: OnceLock<regex::Regex> = OnceLock::new();
    static EMPHASIS: OnceLock<regex::Regex> = OnceLock::new();
    static SPACES: OnceLock<regex::Regex> = OnceLock::new();
    static SPACE_PUNCT: OnceLock<regex::Regex> = OnceLock::new();
    // Bracket classes include the FULLWIDTH / CJK forms （）【】［］〔〕and the fullwidth colon
    // '：', because a model replying in Chinese/Japanese emits CJK punctuation — the ASCII-only
    // form silently missed a real leak ("…编辑框（ID 91）…", live 2026-07-20, gemini-3.1-flash-lite).
    let id_ref = ID_REF.get_or_init(|| {
        regex::Regex::new(r"(?i)[(\[（【［〔]\s*id\s*[:#=：]?\s*(\d{1,5})\s*[)\]）】］〕]").unwrap()
    });
    let ids: Vec<u32> = id_ref
        .captures_iter(text)
        .filter_map(|c| c[1].parse().ok())
        .collect();
    let cleaned = id_ref.replace_all(text, "");
    // One alternation group is always empty, so "$1$2" yields the surviving text.
    let emphasis = EMPHASIS
        .get_or_init(|| regex::Regex::new(r"\*\*([^*\n]+)\*\*|__([^_\n]+)__").unwrap());
    let cleaned = emphasis.replace_all(&cleaned, "$1$2");
    let spaces = SPACES.get_or_init(|| regex::Regex::new(r"[ \t]{2,}").unwrap());
    let cleaned = spaces.replace_all(&cleaned, " ");
    let space_punct = SPACE_PUNCT.get_or_init(|| regex::Regex::new(r" +([.,;:!?])").unwrap());
    let cleaned = space_punct.replace_all(&cleaned, "$1");
    (cleaned.trim().to_string(), ids)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigateStepResponse {
    pub steps: Vec<GuidanceStep>,
    #[serde(default)]
    pub state_summary: String,
    #[serde(default)]
    pub needs_input: bool,
    /// Workstream P (v0.7): up to 3 short next-task suggestions the user might ask
    /// for, offered only when the current task looks complete or none is in
    /// progress. Display-only — the frontend prefills the task box (selected) and
    /// never auto-submits. Absence is the norm mid-task.
    #[serde(default, deserialize_with = "lax_suggestions")]
    pub suggested_tasks: Vec<String>,
    // The old AI-driven full-screen request field was removed 2026-07-12 (audit
    // F12) — the mechanism was deleted at SDD rev 2.17 and the field had been
    // inert ever since. Stray model output still emitting the key is simply
    // ignored (serde's default unknown-field behavior) — no tolerated field needed.
}

/// Lax `suggested_tasks` deserializer (Workstream P): keep only string entries that
/// are non-empty after trimming and within the length cap (an over-long entry is a
/// runaway/garbage string, not a task — dropped, not truncated); case-insensitive
/// dedupe; hard cap 3. A non-array or otherwise malformed value becomes an empty
/// list — never a parse failure (the [`lax_bbox`]/[`lax_overlay`] precedent).
fn lax_suggestions<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    const MAX_SUGGESTIONS: usize = 3;
    const MAX_CHARS: usize = 80;
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(arr) = value.as_array() else {
        return Ok(Vec::new());
    };
    let mut out: Vec<String> = Vec::new();
    for v in arr {
        let Some(s) = v.as_str().map(str::trim) else {
            continue;
        };
        if s.is_empty() || s.chars().count() > MAX_CHARS {
            continue;
        }
        if out.iter().any(|e| e.eq_ignore_ascii_case(s)) {
            continue;
        }
        out.push(s.to_string());
        if out.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(json: &str) -> GuidanceStep {
        serde_json::from_str(json).expect("GuidanceStep must deserialize")
    }

    #[test]
    fn minimal_step_gets_defaults() {
        let s = step(r#"{"instruction": "Click the button"}"#);
        assert!(s.checkpoint, "checkpoint defaults to true");
        assert!(matches!(s.overlay_type, OverlayType::Arrow));
        assert!(s.target_text.is_none());
        assert!(s.target_bbox.is_none());
    }

    #[test]
    fn invented_overlay_type_falls_back_to_arrow() {
        // A model inventing "pointer" must not fail the whole response parse.
        let s = step(r#"{"instruction": "x", "overlay_type": "pointer"}"#);
        assert!(matches!(s.overlay_type, OverlayType::Arrow));
    }

    #[test]
    fn empty_or_invalid_role_becomes_none() {
        let s = step(r#"{"instruction": "x", "target_role": ""}"#);
        assert!(s.target_role.is_none());
        let s = step(r#"{"instruction": "x", "target_role": "button-like-thing"}"#);
        assert!(s.target_role.is_none());
        let s = step(r#"{"instruction": "x", "target_role": "button"}"#);
        assert!(matches!(s.target_role, Some(TargetRole::Button)));
    }

    #[test]
    fn bbox_accepts_canonical_and_polygon_forms() {
        // Flat form is the requested [ymin, xmin, ymax, xmax] — used verbatim.
        let s = step(r#"{"instruction": "x", "target_bbox": [80, 450, 110, 550]}"#);
        assert_eq!(s.target_bbox, Some([80.0, 450.0, 110.0, 550.0]));

        // GPT-style 4-corner polygon → normalized to its bounding box. Corners are
        // [x, y] (the universal vision convention — audit C9), so an element spanning
        // x:450–550, y:80–110 → flat [ymin, xmin, ymax, xmax] = [80, 450, 110, 550].
        // Deliberately non-square (100 wide × 30 tall) so a width/height transpose
        // would fail this assertion.
        let s = step(
            r#"{"instruction": "x", "target_bbox": [[450, 80], [550, 80], [450, 110], [550, 110]]}"#,
        );
        assert_eq!(s.target_bbox, Some([80.0, 450.0, 110.0, 550.0]));
    }

    #[test]
    fn malformed_bbox_becomes_none_not_error() {
        for bad in [
            r#"{"instruction": "x", "target_bbox": "top left"}"#,
            r#"{"instruction": "x", "target_bbox": [1, 2, 3]}"#,
            r#"{"instruction": "x", "target_bbox": null}"#,
        ] {
            let s = step(bad);
            assert!(s.target_bbox.is_none(), "should be None for: {bad}");
        }
    }

    #[test]
    fn element_id_lax_forms() {
        // Canonical integer.
        let s = step(r#"{"instruction": "x", "target_element_id": 12}"#);
        assert_eq!(s.target_element_id, Some(12));
        // String-wrapped and float-integer forms models emit.
        let s = step(r#"{"instruction": "x", "target_element_id": "12"}"#);
        assert_eq!(s.target_element_id, Some(12));
        let s = step(r#"{"instruction": "x", "target_element_id": 12.0}"#);
        assert_eq!(s.target_element_id, Some(12));
        // Garbage never fails the whole response parse.
        for bad in [
            r#"{"instruction": "x", "target_element_id": "the save button"}"#,
            r#"{"instruction": "x", "target_element_id": -3}"#,
            r#"{"instruction": "x", "target_element_id": 12.7}"#,
            r#"{"instruction": "x", "target_element_id": null}"#,
            r#"{"instruction": "x", "target_element_id": [12]}"#,
        ] {
            let s = step(bad);
            assert!(s.target_element_id.is_none(), "should be None for: {bad}");
        }
        // Absent → None.
        let s = step(r#"{"instruction": "x"}"#);
        assert!(s.target_element_id.is_none());
    }

    #[test]
    fn sanitize_strips_id_ref_and_recovers_it() {
        // The live 2026-07-18 gemini-3.5-flash incident: id in prose, field null.
        let mut steps = vec![step(
            r#"{"instruction": "Click the **More** drop-down (ID: 108) to open the list."}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(
            steps[0].instruction,
            "Click the More drop-down to open the list."
        );
        assert_eq!(steps[0].target_element_id, Some(108));
    }

    #[test]
    fn sanitize_handles_flash_lite_format() {
        // The 2026-07-12 flash-lite incident format that prompted prompt Rule 14.
        let mut steps = vec![step(r#"{"instruction": "click the Search box (id 30)"}"#)];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "click the Search box");
        assert_eq!(steps[0].target_element_id, Some(30));
    }

    #[test]
    fn sanitize_strips_fullwidth_cjk_id_refs() {
        // Live 2026-07-20 (gemini-3.1-flash-lite, Chinese reply): the ids leaked with FULLWIDTH
        // parens （）(U+FF08/FF09) that the ASCII-only class missed → shown + spoken verbatim.
        let mut steps = vec![step(
            r#"{"instruction": "在中间的“Editor content”编辑框（ID 91）中输入语句，然后点击“Run”按钮（ID 90）。"}"#,
        )];
        sanitize_steps(&mut steps);
        assert!(!steps[0].instruction.contains("ID"), "{}", steps[0].instruction);
        assert!(!steps[0].instruction.contains('（'), "{}", steps[0].instruction);
        // Two distinct ids → ambiguous → not recovered (matches the ASCII behavior).
        assert!(steps[0].target_element_id.is_none());
    }

    #[test]
    fn sanitize_never_overwrites_an_explicit_id() {
        let mut steps = vec![step(
            r#"{"instruction": "Click Save (ID: 5).", "target_element_id": 12}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "Click Save.");
        assert_eq!(steps[0].target_element_id, Some(12));
    }

    #[test]
    fn sanitize_skips_recovery_when_ids_are_ambiguous() {
        let mut steps = vec![step(
            r#"{"instruction": "Click A (ID: 3) then B (ID: 9)."}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "Click A then B.");
        assert!(steps[0].target_element_id.is_none());
        // The SAME id repeated is not ambiguous.
        let mut steps = vec![step(
            r#"{"instruction": "Click A (ID: 3). I mean A (id 3)."}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].target_element_id, Some(3));
    }

    #[test]
    fn fenced_json_instruction_recovered() {
        // The live 2026-07-19 leak, verbatim shape: a whole response JSON nested in
        // the instruction as a fenced block, MALFORMED tail included.
        let leaked = "```json\n{\n  \"needs_input\": false,\n  \"steps\": [\n    {\n      \"instruction\": \"Click the Show Gizmo icon (the coordinate axes icon with a dropdown arrow) in the top-right header of the 3D viewport.\",\n      \"overlay_type\": \"circle\",\n      \"target_nearby_text\": \"Options\",\n      \"target_role\": \"button\",\n      \"target_text\": \"Show Gizmo\"\n    }\n  ]\n}\n}\nLoc```\n  ]\n}";
        let mut steps = vec![step(&serde_json::json!({ "instruction": leaked }).to_string())];
        sanitize_steps(&mut steps);
        assert_eq!(
            steps[0].instruction,
            "Click the Show Gizmo icon (the coordinate axes icon with a dropdown arrow) in the top-right header of the 3D viewport."
        );
        // Empty outer target fields backfilled from the blob.
        assert_eq!(steps[0].target_text.as_deref(), Some("Show Gizmo"));
        assert_eq!(steps[0].target_nearby_text.as_deref(), Some("Options"));
    }

    #[test]
    fn fenced_json_never_overwrites_outer_targets_and_prose_untouched() {
        // An outer target_text survives recovery.
        let leaked = "```json\n{\"instruction\": \"Inner text.\", \"target_text\": \"Inner\"}```";
        let mut steps = vec![step(
            &serde_json::json!({ "instruction": leaked, "target_text": "Outer" }).to_string(),
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "Inner text.");
        assert_eq!(steps[0].target_text.as_deref(), Some("Outer"));
        // Prose that merely MENTIONS the word instruction is untouched.
        let mut steps = vec![step(
            r#"{"instruction": "Follow the instruction shown in the dialog."}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "Follow the instruction shown in the dialog.");
    }

    #[test]
    fn sanitize_leaves_clean_text_untouched() {
        for clean in [
            "Click the ruler icon near the bottom of the left toolbar.",
            "Type 5 * 3 = 15 into the cell.",
            "Wait for the (idle) indicator, then press Enter.",
            "Open the ID card scanner (see the sidebar).",
        ] {
            let mut steps = vec![step(&format!(r#"{{"instruction": {clean:?}}}"#))];
            sanitize_steps(&mut steps);
            assert_eq!(steps[0].instruction, clean, "should be untouched: {clean}");
            assert!(steps[0].target_element_id.is_none());
        }
    }

    #[test]
    fn sanitize_strips_bracketed_and_underscore_forms() {
        let mut steps = vec![step(
            r#"{"instruction": "Press __Insert__ [id #7] on the ribbon."}"#,
        )];
        sanitize_steps(&mut steps);
        assert_eq!(steps[0].instruction, "Press Insert on the ribbon.");
        assert_eq!(steps[0].target_element_id, Some(7));
    }

    #[test]
    fn response_optional_fields_default() {
        let r: NavigateStepResponse =
            serde_json::from_str(r#"{"steps": [{"instruction": "x"}]}"#).unwrap();
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.state_summary, "");
        assert!(!r.needs_input);
        assert!(r.suggested_tasks.is_empty());
    }

    #[test]
    fn suggested_tasks_lax_forms() {
        let resp = |json: &str| -> NavigateStepResponse {
            serde_json::from_str(json).expect("NavigateStepResponse must deserialize")
        };
        // Well-formed list passes through.
        let r = resp(
            r#"{"steps": [], "suggested_tasks": ["Print this document", "Change the font"]}"#,
        );
        assert_eq!(r.suggested_tasks, vec!["Print this document", "Change the font"]);
        // Trim + drop empties + case-insensitive dedupe + hard cap 3.
        let r = resp(
            r#"{"steps": [], "suggested_tasks": ["  Save the file ", "", "save the FILE", "Print", "Undo", "Redo"]}"#,
        );
        assert_eq!(r.suggested_tasks, vec!["Save the file", "Print", "Undo"]);
        // A runaway string (weak-model failure mode) is dropped, not truncated.
        let long = "a".repeat(200);
        let r = resp(&format!(
            r#"{{"steps": [], "suggested_tasks": ["{long}", "Fine"]}}"#
        ));
        assert_eq!(r.suggested_tasks, vec!["Fine"]);
        // Non-string entries are skipped; non-array values never fail the parse.
        let r = resp(r#"{"steps": [], "suggested_tasks": [1, {"a": 2}, "Real task"]}"#);
        assert_eq!(r.suggested_tasks, vec!["Real task"]);
        for bad in [
            r#"{"steps": [], "suggested_tasks": "explore the app"}"#,
            r#"{"steps": [], "suggested_tasks": 42}"#,
            r#"{"steps": [], "suggested_tasks": null}"#,
        ] {
            assert!(resp(bad).suggested_tasks.is_empty(), "should be empty for: {bad}");
        }
    }
}
