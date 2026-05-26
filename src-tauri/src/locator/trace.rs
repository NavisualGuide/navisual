//! Locator trace — Phase 0.1.
//!
//! Records what each tier of the locator considered and which candidate was
//! returned. Surfaced in two places:
//!  - The frontend DebugDrawer (when debug toggle is on).
//!  - A rolling JSONL log at `%APPDATA%\com.navisual.app\locate_log.jsonl`.

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
    pub a11y: A11yTrace,
    pub ocr: OcrTrace,
    pub final_decision: FinalDecision,
    pub final_bbox: Option<Rect>,
    pub elapsed_ms: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct A11yTrace {
    pub ran: bool,
    pub regex_used: String,
    pub search_roots_count: usize,
    pub candidates: Vec<A11yCandidate>,
    pub timed_out: bool,
    pub elapsed_ms: u32,
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
    pub elapsed_ms: u32,
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
    HitA11y,
    HitOcr,
    /// Phase 1 C5: WindowFromPoint hit-test rejected the locate.
    RejectedByHitTest {
        leaf_class: String,
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
