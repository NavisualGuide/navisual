//! Locator trace — Phase 0.1.
//!
//! Records what each tier of the locator considered and which candidate was
//! returned. Surfaced in two places:
//!  - The frontend DebugDrawer (when debug toggle is on).
//!  - A rolling JSONL log at `%LOCALAPPDATA%\com.navisual.app\locate_log.jsonl`.

use crate::capture::Rect;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LocateTrace {
    pub timestamp_ms: u64,
    pub target_text: String,
    pub target_role: Option<String>,
    pub nearby_text: Option<String>,
    /// AI-predicted target bbox in virtual-desktop physical pixels (was: `grid_cell`).
    pub ai_bbox: Option<Rect>,
    /// Pass 0 — app-specific locator adapter (Excel cells, …). `None` when no adapter
    /// claimed the focused app + target, so the standard A11y → OCR path ran untouched.
    pub adapter: Option<AdapterTrace>,
    pub a11y: A11yTrace,
    pub ocr: OcrTrace,
    /// Pass 3 — icon template matching (Workstream B). `None` when not reached (A11y/OCR hit,
    /// or no pack icon candidates for the target).
    pub template: Option<TemplateTrace>,
    pub final_decision: FinalDecision,
    pub final_bbox: Option<Rect>,
    pub elapsed_ms: u32,
}

/// Pass-0 adapter outcome. An adapter "claims" a locate when it recognises the focused app
/// *and* the target shape (e.g. Excel + a cell ref like "Q34"); a claimed locate either
/// produces a deterministic geometry hit or falls through to A11y → OCR (recorded here so
/// the debug drawer shows why the adapter didn't resolve — e.g. an off-screen cell).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AdapterTrace {
    /// Adapter that claimed the target ("excel", …).
    pub name: String,
    /// The adapter produced a `LocateResult` (vs. claimed-but-fell-through).
    pub hit: bool,
    /// Human-readable outcome: accepted, or why it fell through to A11y/OCR.
    pub detail: String,
}

/// Pass-3 icon template-matching outcome (Workstream B). Runs only after A11y + OCR miss and
/// the active pack supplies icon candidates for the target. Surfaced in the debug drawer.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TemplateTrace {
    /// Number of candidate icon crops matched against the capture.
    pub templates_tried: usize,
    /// Filename stem of the best-scoring icon, if any cleared decoding.
    pub best_icon: Option<String>,
    /// Best NCC score seen across all candidates/scales ([-1, 1]).
    pub best_score: f32,
    /// Template scale factor that produced the best match.
    pub best_scale: f32,
    /// The best match cleared the acceptance threshold and was used as the located target.
    pub accepted: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct A11yTrace {
    pub ran: bool,
    pub regex_used: String,
    pub search_roots_count: usize,
    pub candidates: Vec<A11yCandidate>,
    pub timed_out: bool,
    /// A second walk was run after a short wait because the first returned 0 on a
    /// Chromium/Electron window (lazy a11y tree — the first query wakes the build).
    pub retried: bool,
    /// UI framework of the target window ("Chrome" / "Eager" / "Other").
    pub framework: Option<String>,
    /// The Chrome path used the cached `find_all_build_cache` (batched property reads).
    pub cached: bool,
    /// The last-resort Pane fallback produced the candidates: custom-toolkit apps
    /// (Adobe Lightroom family) expose real buttons as ControlType.Pane with
    /// suffixed names ("Auto (Bridge View)"), invisible to the role-family passes.
    pub pane_fallback: bool,
    /// UIA elements the cached find returned BEFORE name-filtering (`None` if the cached find
    /// didn't run). `Some(0)` = the tree wasn't built (lazy app); `Some(n>0)` with no candidates
    /// = the elements were there but none matched the name.
    pub element_count: Option<usize>,
    /// Outcome of the AI-bbox `ElementFromPoint` probe — the name-agnostic fallback tried when
    /// the name search found nothing and a trusted AI bbox is present. `None` = not reached
    /// (name search succeeded, or no bbox).
    pub bbox_probe: Option<BboxProbe>,
    pub elapsed_ms: u32,
}

/// The AI-bbox hit-test probe: when the name search misses, `ElementFromPoint` at the AI's
/// predicted point reaches on-screen controls the role-family find_all can miss (a browser
/// tab's close button) and sidesteps name mismatches ("Close" vs "close tab"). The bbox is
/// not trusted blindly — the element it lands on is verified by role family + size.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BboxProbe {
    /// The probe actually ran `ElementFromPoint` (vs skipped — e.g. an untrusted model bbox).
    pub attempted: bool,
    /// Control type resolved under the bbox (after walking up to an interactive ancestor).
    pub resolved_role: Option<String>,
    /// Accessible name of that element — often differs from `target_text`, which is the point.
    pub resolved_name: Option<String>,
    /// The probed element passed validation and was used as the located target.
    pub accepted: bool,
    /// Human-readable outcome: accepted, why rejected, or why skipped.
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct A11yCandidate {
    pub name: String,
    pub role: String,
    pub bbox: (i32, i32, u32, u32),
    pub selected: bool,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OcrTrace {
    pub ran: bool,
    pub line_count: usize,
    pub word_count: usize,
    /// Up to 30 OCR text samples for at-a-glance debugging.
    pub sample_texts: Vec<String>,
    /// Strategy that produced the winner ("exact" | "substring" | "fuzzy").
    pub strategy_used: Option<String>,
    /// Tier reached in the relaxed-threshold cascade (Phase 1 D2). Tier 0 = strict.
    pub tier_reached: u8,
    /// Notable candidates considered at the matching strategies, with reject reasons.
    pub candidates: Vec<OcrCandidate>,
    /// Corroboration outcome for the winner (only when A11y was empty and OCR matched).
    pub corroboration: Option<Corroboration>,
    /// E2: the region-cropped upscaled re-OCR rescue was attempted (full-frame OCR missed and a
    /// trusted AI bbox was present). When it rescued a hit, `final_decision` is `HitOcr` and the
    /// result's role is `OcrRegion`.
    pub region_ocr_attempted: bool,
    pub elapsed_ms: u32,
}

/// Why the OCR winner was accepted or hard-rejected. When A11y is empty, an OCR text
/// match must be corroborated by ≥1 signal, else it's a content false-match ("no pointer
/// beats wrong pointer"). Surfaced in the debug drawer.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Corroboration {
    /// UIA ControlType under the winner (`ElementFromPoint`), if resolvable.
    pub uia_control_type: Option<String>,
    /// The UIA element is an interactive control (corroborates).
    pub uia_interactive: bool,
    /// len(target) / len(containing OCR line).
    pub isolation: f32,
    pub isolation_line_len: usize,
    pub isolation_ok: bool,
    pub near_anchor: bool,
    /// Raw geometric proximity of the winner to the AI bbox. Only counts as a
    /// corroboration vote when `bbox_decisive` (a strong-grounding model) — a weak
    /// model's bbox is recorded here but does not rescue the match.
    pub near_ai_bbox: bool,
    /// The answering model is a strong grounder, so `near_ai_bbox` is allowed to vote.
    pub bbox_decisive: bool,
    /// Final verdict (`uia_interactive || isolation_ok || near_anchor ||
    /// (near_ai_bbox && bbox_decisive)`).
    pub accepted: bool,
    /// The accepted pointer was snapped from the OCR text span to the true UIA element
    /// rect under it (`ElementFromPoint` resolved an interactive control) — so the box
    /// covers the whole clickable control, not just the matched word/line.
    pub snapped_to_uia: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OcrCandidate {
    pub text: String,
    pub bbox: (i32, i32, u32, u32),
    pub confidence: f32,
    pub strategy: String,
    pub score: Option<f32>,
    pub selected: bool,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FinalDecision {
    #[default]
    Miss,
    /// Pass 0 — an app-specific adapter (Excel cells, …) resolved the target by
    /// deterministic geometry, no A11y/OCR needed.
    HitAdapter,
    HitA11y,
    HitOcr,
    /// Pass 3 — a nav-pack icon template matched (A11y + OCR both missed).
    HitTemplate,
    /// Phase 1 C5: WindowFromPoint hit-test rejected the locate.
    RejectedByHitTest {
        leaf_class: String,
    },
    /// OCR matched but no corroborator held (likely content text, not the control).
    RejectedUncorroborated {
        detail: String,
    },
    Error {
        message: String,
    },
}

impl LocateTrace {
    pub fn new(target_text: &str) -> Self {
        Self {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            target_text: target_text.to_string(),
            ..Default::default()
        }
    }
}

/// Append a trace as one JSON line. Rotates at ~5 MB.
pub fn append_jsonl(path: &Path, trace: &LocateTrace) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > 5 * 1024 * 1024 {
            let backup = path.with_extension("jsonl.1");
            let _ = std::fs::remove_file(&backup);
            let _ = std::fs::rename(path, &backup);
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(trace).map_err(std::io::Error::other)?;
    writeln!(file, "{line}")?;
    Ok(())
}
