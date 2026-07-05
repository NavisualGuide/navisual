//! Rolling JSONL log of every prompt sent to the AI — a developer diagnostic distinct
//! from the per-call `prompt_<ts>.txt` dumps under `debug_screenshot_enabled` (those
//! live alongside a matching `screenshot_<ts>.jpg`; this is a single running history,
//! gated by its own toggle so a developer can log prompt text without also saving
//! every screenshot). Covers every AI call site (guide/reply/requery/correction) —
//! `debug_screenshot_enabled`'s dump only ever covered `guide()`. The system prompt is
//! static (`src-tauri/src/ai/prompts.rs`) and never logged; entries hold only the
//! per-call dynamic text (task / window context / pack context / Screen Elements block).

use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptLogEntry {
    pub timestamp_ms: u64,
    pub session_id: String,
    /// "task" | "reply" | "requery" | "resume" | "correction".
    pub call_kind: String,
    pub provider: String,
    pub model: String,
    pub has_screenshot: bool,
    /// The exact dynamic text sent as the user message (window context / pack context /
    /// Screen Elements block included when present; system prompt excluded).
    pub prompt: String,
}

impl PromptLogEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: &str,
        call_kind: &str,
        provider: &str,
        model: &str,
        has_screenshot: bool,
        prompt: &str,
    ) -> Self {
        Self {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            session_id: session_id.to_string(),
            call_kind: call_kind.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            has_screenshot,
            prompt: prompt.to_string(),
        }
    }
}

/// Append an entry as one JSON line. Rotates at ~5 MB (mirrors `locator::trace::append_jsonl`).
pub fn append_jsonl(path: &Path, entry: &PromptLogEntry) -> std::io::Result<()> {
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
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    writeln!(file, "{line}")?;
    Ok(())
}
