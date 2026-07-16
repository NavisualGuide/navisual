//! Rolling JSONL log of every prompt sent to the AI — a developer diagnostic distinct
//! from the per-call `prompt_<ts>.txt` dumps under `debug_screenshot_enabled` (those
//! live alongside a matching `screenshot_<ts>.jpg`; this is a single running history,
//! gated by its own toggle so a developer can log prompt text without also saving
//! every screenshot). Covers every AI call site (guide/reply/requery/correction) —
//! `debug_screenshot_enabled`'s dump only ever covered `guide()`. The system prompt is
//! static (`src-tauri/src/ai/prompts.rs`) and never logged; entries hold only the
//! per-call dynamic text (task / window context / pack context / Screen Elements block).
//!
//! Since 2026-07-15 this doubles as the training-data record (llm-finetuning-eval.md
//! §5b): with `training_capture_enabled` on, entries also carry the parsed AI
//! `response` (an entry without the output can't form an SFT pair), a `request_id`
//! that joins the entry to its `LocateTrace`, saved screenshot, and feedback row, a
//! `prev_request_id` chaining correction retries to the response they rejected
//! (natural DPO preference pairs), and the `app_version` so future curation can
//! segment by prompt-format era (e.g. the 23→17 rules consolidation).

use std::path::Path;

/// Log-format version. Bump when fields change meaning (not on additive changes).
const SCHEMA: u32 = 2;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptLogEntry {
    pub timestamp_ms: u64,
    /// See `SCHEMA`. Entries without the field are schema 1 (pre-2026-07-15).
    pub schema: u32,
    pub session_id: String,
    /// Joins this entry to the same request's LocateTrace, training screenshot
    /// (`training/shot_<request_id>.jpg`), and feedback rows. `None` only for
    /// entries written by builds between the field's introduction and its call
    /// sites being wired (shouldn't be observed in practice).
    pub request_id: Option<String>,
    /// The previous AI request in this session, when there was one — for a
    /// `correction` entry this is the response the user just rejected, which
    /// makes (prev, this) a (rejected, chosen) preference pair once this one
    /// is accepted. For task/reply/requery it simply chains the session.
    pub prev_request_id: Option<String>,
    /// App version at write time — prompt formats drift across releases, so
    /// curation must be able to segment by era.
    pub app_version: Option<String>,
    /// "task" | "reply" | "requery" | "resume" | "correction".
    pub call_kind: String,
    pub provider: String,
    pub model: String,
    pub has_screenshot: bool,
    /// Filename (relative to the app-data dir) of the exact AI-sent JPEG, when
    /// training capture saved one — makes the triple self-describing.
    pub screenshot_file: Option<String>,
    /// The exact dynamic text sent as the user message (window context / pack context /
    /// Screen Elements block included when present; system prompt excluded).
    pub prompt: String,
    /// The parsed `NavigateStepResponse`, re-serialized — the canonical navigate_step
    /// output, normalized across providers (raw provider wire formats differ; the
    /// parsed struct is what training should target anyway). `None` when the call
    /// failed — see `response_error`.
    pub response: Option<serde_json::Value>,
    /// Error string when the AI call failed (timeout, 402, unreadable response…).
    pub response_error: Option<String>,
}

pub struct PromptLogFields<'a> {
    pub session_id: &'a str,
    pub request_id: Option<&'a str>,
    pub prev_request_id: Option<&'a str>,
    pub app_version: Option<&'a str>,
    pub call_kind: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub has_screenshot: bool,
    pub screenshot_file: Option<&'a str>,
    pub prompt: &'a str,
    pub response: Option<serde_json::Value>,
    pub response_error: Option<&'a str>,
}

impl PromptLogEntry {
    pub fn new(f: PromptLogFields<'_>) -> Self {
        Self {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            schema: SCHEMA,
            session_id: f.session_id.to_string(),
            request_id: f.request_id.map(str::to_string),
            prev_request_id: f.prev_request_id.map(str::to_string),
            app_version: f.app_version.map(str::to_string),
            call_kind: f.call_kind.to_string(),
            provider: f.provider.to_string(),
            model: f.model.to_string(),
            has_screenshot: f.has_screenshot,
            screenshot_file: f.screenshot_file.map(str::to_string),
            prompt: f.prompt.to_string(),
            response: f.response,
            response_error: f.response_error.map(str::to_string),
        }
    }
}

/// Append an entry as one JSON line. Rotation policy per `jsonl_log::append_line` —
/// `archive` (training capture on) preserves rotated-out history under
/// `training/logs/` instead of deleting it.
pub fn append_jsonl(path: &Path, entry: &PromptLogEntry, archive: bool) -> std::io::Result<()> {
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    crate::jsonl_log::append_line(path, &line, archive)
}
