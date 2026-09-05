use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_provider: String,

    // Anthropic
    pub anthropic_api_key: Option<String>,
    pub anthropic_model: String,
    pub anthropic_fast_model: String,

    // Gemini
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    pub gemini_fast_model: String,

    // Ollama
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub ollama_timeout_sec: u64,

    // OpenAI
    pub openai_api_key: Option<String>,
    pub openai_model: String,

    // DeepSeek
    pub deepseek_api_key: Option<String>,
    pub deepseek_model: String,

    // Qwen (Alibaba DashScope)
    pub qwen_api_key: Option<String>,
    pub qwen_model: String,
    pub qwen_base_url: String,

    // Custom — any OpenAI-compatible endpoint (local LM Studio / llama.cpp / vLLM,
    // a DashScope workspace URL, or another cloud). Reuses the DeepSeek client.
    pub custom_api_key: Option<String>,
    pub custom_model: String,
    pub custom_base_url: String,

    // Supabase managed relay (S.1 free trial + paid tiers)
    pub supabase_url: Option<String>,
    pub supabase_anon_key: Option<String>,
    // Display-only placeholder shown before the first managed response (e.g. the
    // "Navisual ready — using {model}" first message) — the relay always decides
    // the real model server-side (Gemini primary / Qwen fallback for free,
    // tier-based for paid) and that gets surfaced via router::get_managed_routed_model()
    // once a real request has happened. Sent as `payload.model` to the relay too,
    // but every relay path (free and paid) overwrites it unconditionally, so it
    // has no bearing on what actually answers — keep this generic rather than
    // naming a specific model, so it can't go stale the way "openrouter/free"
    // did once the free tier stopped routing through OpenRouter (2026-07-11).
    pub managed_model: String,
    // Paid-tier selection sent to the relay: "speed" | "regular" | "smart".
    // Ignored on the free tier (relay routes free users direct to Gemini, Qwen on
    // fallback — see relay/index.ts's handleFreeDirect in navisual-internal).
    pub managed_tier: String,

    // Shared
    pub api_timeout_sec: u64,

    // v0.7 Workstream P — prefilled task suggestions: surface the AI's suggested_tasks
    // (piggybacked on navigate_step) + the local cold-start prefill. Display-only UI
    // sugar with no wrong-pointer risk, so it defaults ON; the Settings → Screen Guide
    // toggle turns it off.
    pub task_suggestions: bool,

    // Locator — comma-separated, case-insensitive substrings of model names whose AI
    // `target_bbox` is NOT trusted to corroborate (rescue) a borderline OCR match. Trust
    // is default-ON for every model; only models matching this list are muted. Default is
    // the managed free-tier chain (weak / degenerate grounders). Frontier models — current
    // and future — are trusted without a code change; mute a newly-bad one by adding it to
    // BBOX_DISTRUST_MODELS in .env (no rebuild/release). Empty string = trust all.
    pub bbox_distrust_models: String,

    // Overlay appearance
    pub overlay_color: String,
    pub overlay_thickness: u32,

    // Behavior
    pub subtitle_enabled: bool,
    pub auto_advance: bool,
    /// Autopilot sensitivity: how many downsampled cells (of 1024, SIG_LEN) must change for a
    /// screen change to trigger an auto-advance. Lower = more sensitive. Default 16 (~1.6%).
    pub autopilot_min_cells: u32,

    // Audio output (TTS)
    pub tts_enabled: bool,
    pub tts_voice: String, // SAPI token ID; empty = system default

    // Audio input (voice)
    pub voice_input_enabled: bool,
    pub voice_language: String,

    // Hotkeys (Tauri accelerator format, e.g. "Alt+KeyE")
    pub hotkey_next: String,
    pub hotkey_wrong: String,
    pub hotkey_pause: String,
    pub hotkey_icon: String,
    pub hotkey_talk: String,

    // ── Developer / testing ──────────────────────────────────────────────────
    //
    // Consolidated 2026-09-04 from seven independent switches to four. The seven
    // were not seven decisions: nobody wants the locate drawer without the
    // response info, or one JSONL log without the other. They were one decision
    // each time they were used, spread across seven checkboxes.
    //
    // The grouping is by CONSEQUENCE, not by subsystem — what turning it on costs
    // you is what a reader of the Settings page is actually deciding about:
    //   diagnostics  — changes what is on screen. Costs nothing, writes nothing.
    //   log files    — appends text to disk. Small, and readable by the tools/.
    //   screenshots  — writes PICTURES OF YOUR SCREEN to disk. Deliberately its
    //                  own switch; see below.
    //   training     — accumulates a joinable corpus, exempt from cleanup.
    /// On-screen diagnostics: the locate-trace drawer, the per-response info line,
    /// and the AI's `target_bbox` drawn on the overlay. Merged because all three
    /// answer the same question — "what did the locator just do?" — and were
    /// invariably turned on together.
    pub debug_diagnostics_enabled: bool,
    /// Append diagnostics to `locate_log.jsonl` and `prompt_log.jsonl`.
    ///
    /// One switch for both, because a locate trace without the prompt that caused
    /// it answers half a question, and `tools/analyze-*.ps1` want both anyway.
    pub debug_log_files_enabled: bool,
    /// Save AI screenshots, OCR inputs, and per-request prompt text to `debug\`.
    ///
    /// **Deliberately NOT merged into `debug_log_files_enabled`.** The other logs
    /// are text; this one writes pictures of whatever was on screen, at a few
    /// hundred KB each. Coupling them would mean that turning on locate
    /// diagnostics silently starts capturing the user's screen to disk, which is
    /// exactly the kind of surprise a privacy-sensitive write must never be.
    pub debug_screenshot_enabled: bool,
    /// Training-data banking (llm-finetuning-eval.md §5b) — one switch for the whole
    /// bundle a future fine-tune needs as COMPLETE, JOINABLE triples: the exact AI-sent
    /// JPEG saved per request (training/shot_<request_id>.jpg), prompt+response entries
    /// in prompt_log.jsonl (forced on even if that toggle is off — a triple without the
    /// response is worthless), rotated jsonl logs ARCHIVED to training/logs/ instead of
    /// deleted, and feedback rows mirrored (with request_id) to training/feedback.jsonl.
    /// The training/ dir is exempt from the 7-day debug cleanup — it exists only when
    /// this is deliberately on, and its whole point is accumulation.
    pub training_capture_enabled: bool,
    /// Session export — the ✗/💾 "Save this session" flow (`session_export.rs`).
    ///
    /// Developer-gated for now at the founder's request: they are the only user of
    /// it, and it is the one feature that deliberately writes screenshots of a real
    /// screen to disk. Keeping it behind the flag means the ring buffer is the only
    /// part running for everyone else, and that is memory-only.
    ///
    /// **The buffer keeps filling regardless.** Gating the UI, not the capture, is
    /// what preserves the whole point of §3.1 — that "this one was worth keeping"
    /// stays a decision you make afterwards. Turning the toggle on mid-session must
    /// find the session already recorded, not start from empty.
    pub session_export_enabled: bool,

    /// Include the *text* of the paragraph the cursor is in, in the `[App State — Word]`
    /// block. Default on — it is what lets the AI say "you're in the Outlook heading"
    /// rather than "you're on line 1".
    ///
    /// Separated from the rest of the block because it is the only field that transmits
    /// document **content** rather than **position**. Page/section/line/style are metadata
    /// the AI cannot get any other way; the paragraph text is prose the user may not want
    /// leaving the machine in structured, greppable form — the screenshot already carries
    /// it, but as pixels, not as a loggable string. Turning this off keeps every positional
    /// field, so the feature still works; it just stops quoting the document.
    pub word_state_paragraph_text: bool,

    /// Gemini reasoning budget, in tokens. `None` (default) omits `thinkingConfig` so the
    /// provider applies its own dynamic policy — measured at 447 thinking tokens against
    /// 156 tokens of visible output. `Some(0)` disables thinking (Flash only); `Some(n)`
    /// caps it. Exists to measure the latency/quality trade before deciding whether the
    /// Speed/Regular/Smart tiers should drive it.
    pub gemini_thinking_budget: Option<i32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_provider: "managed".to_string(),
            anthropic_api_key: None,
            anthropic_model: "claude-sonnet-5".to_string(),
            anthropic_fast_model: "claude-haiku-4-5-20251001".to_string(),
            gemini_api_key: None,
            gemini_model: "gemini-3.7-flash".to_string(),
            gemini_fast_model: "gemini-2.5-flash-lite".to_string(),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.2-vision".to_string(),
            ollama_timeout_sec: 120,
            openai_api_key: None,
            openai_model: "gpt-5.6-terra".to_string(),
            deepseek_api_key: None,
            deepseek_model: "deepseek-v4-flash".to_string(),
            qwen_api_key: None,
            qwen_model: "qwen3.8-max".to_string(),
            qwen_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            custom_api_key: None,
            custom_model: String::new(),
            custom_base_url: String::new(),
            supabase_url: Some("https://gwekzberpfuxsoddwwqj.supabase.co".to_string()),
            supabase_anon_key: Some("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6Imd3ZWt6YmVycGZ1eHNvZGR3d3FqIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzgxMTUxMjEsImV4cCI6MjA5MzY5MTEyMX0.gCXLsnFq3NMv8_JvZGcR9TB9bAfyjCnEnj4u0RZnRbg".to_string()),
            managed_model: "managed".to_string(),
            managed_tier: "regular".to_string(),
            api_timeout_sec: 90,
            task_suggestions: true,
            bbox_distrust_models: "nemotron,gemma,kimi".to_string(),
            overlay_color: "#FF6B35".to_string(),
            overlay_thickness: 4,
            autopilot_min_cells: 16,
            subtitle_enabled: true,
            auto_advance: false,
            tts_enabled: true,
            tts_voice: String::new(),
            voice_input_enabled: true,
            voice_language: "auto".to_string(),
            hotkey_next:  "Ctrl+Backquote".to_string(),
            hotkey_wrong: "Ctrl+KeyE".to_string(),
            hotkey_pause: String::new(),
            hotkey_icon:  String::new(),
            hotkey_talk:  "Ctrl+KeyD".to_string(),
            debug_diagnostics_enabled: false,
            debug_log_files_enabled: false,
            debug_screenshot_enabled: false,
            training_capture_enabled: false,
            session_export_enabled: false,
            word_state_paragraph_text: true,
            gemini_thinking_budget: None,
        }
    }
}

/// Resolve a developer switch that replaced several older ones (2026-09-04).
///
/// The new key wins whenever it is set. Only when it is absent do the retired
/// keys fill it in, and then generously: any of them being on means the merged
/// group was in use. Reading a retired key once beats silently resetting an
/// existing developer setup to off, because that failure is invisible -- it looks
/// like diagnostics simply stopped working, with nothing tying it to an upgrade.
fn merged_switch(new_key: Option<bool>, legacy: &[Option<bool>]) -> bool {
    match new_key {
        Some(v) => v,
        None => legacy.iter().any(|l| l.unwrap_or(false)),
    }
}

impl Config {
    /// Loads configuration from a specific .env file path, or falls back to
    /// the working-directory .env (dev mode). Missing file is silently ignored.
    ///
    /// For the explicit-path case we use a hand-rolled parser instead of
    /// `dotenvy::from_path` because our values can contain backslashes (Windows
    /// registry paths in `TTS_VOICE`, e.g. `HKEY_LOCAL_MACHINE\SOFTWARE\...`).
    /// dotenvy treats unquoted backslashes as escape-sequence starts, which
    /// fails the parse on that line and silently drops every later line in the
    /// file — so a SAPI token ID near the bottom of .env would also break
    /// `DEBUG_SHOW_AI_BBOX`, `MANAGED_PROVIDER`, etc.
    pub fn load(env_file: Option<&std::path::Path>) -> Self {
        match env_file {
            Some(path) => load_env_file_simple(path),
            None => {
                let _ = dotenvy::dotenv();
            }
        }

        let mut config = Config::default();

        if let Ok(v) = env::var("API_PROVIDER") {
            config.api_provider = v;
        }
        if let Ok(v) = env::var("ANTHROPIC_API_KEY") {
            if !v.is_empty() {
                config.anthropic_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("ANTHROPIC_MODEL") {
            config.anthropic_model = v;
        }
        if let Ok(v) = env::var("ANTHROPIC_FAST_MODEL") {
            config.anthropic_fast_model = v;
        }

        if let Ok(v) = env::var("GEMINI_API_KEY") {
            if !v.is_empty() {
                config.gemini_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("GEMINI_MODEL") {
            config.gemini_model = v;
        }
        if let Ok(v) = env::var("GEMINI_FAST_MODEL") {
            config.gemini_fast_model = v;
        }

        if let Ok(v) = env::var("OLLAMA_BASE_URL") {
            config.ollama_base_url = v;
        }
        if let Ok(v) = env::var("OLLAMA_MODEL") {
            config.ollama_model = v;
        }

        if let Ok(v) = env::var("OPENAI_API_KEY") {
            if !v.is_empty() {
                config.openai_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("OPENAI_MODEL") {
            config.openai_model = v;
        }

        if let Ok(v) = env::var("DEEPSEEK_API_KEY") {
            if !v.is_empty() {
                config.deepseek_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("DEEPSEEK_MODEL") {
            if !v.is_empty() {
                config.deepseek_model = v;
            }
        }

        if let Ok(v) = env::var("QWEN_API_KEY") {
            if !v.is_empty() {
                config.qwen_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("QWEN_MODEL") {
            if !v.is_empty() {
                config.qwen_model = v;
            }
        }
        if let Ok(v) = env::var("QWEN_BASE_URL") {
            if !v.is_empty() {
                config.qwen_base_url = v;
            }
        }
        if let Ok(v) = env::var("CUSTOM_API_KEY") {
            if !v.is_empty() {
                config.custom_api_key = Some(v);
            }
        }
        if let Ok(v) = env::var("CUSTOM_MODEL") {
            if !v.is_empty() {
                config.custom_model = v;
            }
        }
        if let Ok(v) = env::var("CUSTOM_BASE_URL") {
            if !v.is_empty() {
                config.custom_base_url = v;
            }
        }

        if let Ok(v) = env::var("SUPABASE_URL") {
            if !v.is_empty() {
                config.supabase_url = Some(v);
            }
        }
        if let Ok(v) = env::var("SUPABASE_ANON_KEY") {
            if !v.is_empty() {
                config.supabase_anon_key = Some(v);
            }
        }
        if let Ok(v) = env::var("MANAGED_MODEL") {
            if !v.is_empty() {
                config.managed_model = v;
            }
        }
        if let Ok(v) = env::var("MANAGED_TIER") {
            let v = v.trim().to_lowercase();
            // "free" added 2026-07-11 alongside the Quality Tier dropdown's Free
            // option — missed here originally, so a saved "free" preference was
            // silently rejected on the next read and reverted to whatever this
            // struct's in-memory default was ("regular"), even though it had
            // written correctly to .env. Not a validated paid-tier key on the
            // relay either way (see relay/index.ts's PAID_TIERS) — it degrades
            // safely there regardless of whether it round-trips here.
            if v == "free" || v == "speed" || v == "regular" || v == "smart" {
                config.managed_tier = v;
            }
        }
        if let Ok(v) = env::var("TASK_SUGGESTIONS") {
            config.task_suggestions = v == "true" || v == "1";
        }
        // No is_empty guard: an explicit empty value means "trust every model".
        if let Ok(v) = env::var("BBOX_DISTRUST_MODELS") {
            config.bbox_distrust_models = v;
        }

        if let Ok(v) = env::var("OVERLAY_COLOR") {
            config.overlay_color = v;
        }
        if let Ok(v) = env::var("OVERLAY_THICKNESS") {
            if let Ok(n) = v.parse::<u32>() {
                config.overlay_thickness = n;
            }
        }
        if let Ok(v) = env::var("SUBTITLE_ENABLED") {
            config.subtitle_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("AUTO_ADVANCE") {
            config.auto_advance = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("AUTOPILOT_MIN_CELLS") {
            if let Ok(n) = v.parse::<u32>() {
                // Clamp to a sane band: below ~4 fires on caret-scale noise; above ~200 (of 1024)
                // needs most of the window to change. Guards a hand-edited .env, too.
                config.autopilot_min_cells = n.clamp(4, 200);
            }
        }
        if let Ok(v) = env::var("TTS_ENABLED") {
            config.tts_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("TTS_VOICE") {
            if !v.is_empty() {
                config.tts_voice = v;
            }
        }
        if let Ok(v) = env::var("VOICE_INPUT_ENABLED") {
            config.voice_input_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("VOICE_LANGUAGE") {
            if !v.is_empty() {
                config.voice_language = v;
            }
        }
        // Hotkeys: NO is_empty guard, unlike most string settings above. An empty
        // value here is a real user decision — the Settings clear (×) button writes
        // `HOTKEY_NEXT=` to unset a binding — and the guard used to resurrect the
        // struct default on the next load, silently re-registering the "cleared"
        // hotkey after a restart (audit 2026-07-12 F4; only visible on the three
        // keys with non-empty defaults — Next/Wrong/Talk). A missing key (fresh
        // install, hand-edited .env) still gets the default via Config::default().
        // Platform note: this relies on an empty-valued env var reading back as
        // Ok("") rather than Err(NotPresent) — pinned by env_empty_value_roundtrip
        // in the tests below; if that assumption ever breaks, absent and cleared
        // become indistinguishable and this needs a sentinel value instead.
        if let Ok(v) = env::var("HOTKEY_NEXT") {
            config.hotkey_next = v;
        }
        if let Ok(v) = env::var("HOTKEY_WRONG") {
            config.hotkey_wrong = v;
        }
        if let Ok(v) = env::var("HOTKEY_PAUSE") {
            config.hotkey_pause = v;
        }
        if let Ok(v) = env::var("HOTKEY_ICON") {
            config.hotkey_icon = v;
        }
        if let Ok(v) = env::var("HOTKEY_TALK") {
            config.hotkey_talk = v;
        }
        let truthy = |v: &str| v == "true" || v == "1";

        if let Ok(v) = env::var("DEBUG_SCREENSHOT_ENABLED") {
            config.debug_screenshot_enabled = truthy(&v);
        }
        if let Ok(v) = env::var("SESSION_EXPORT_ENABLED") {
            config.session_export_enabled = truthy(&v);
        }

        // Merged-switch migration (see `merged_switch`).
        let flag = |k: &str| env::var(k).ok().map(|v| truthy(&v));
        config.debug_diagnostics_enabled = merged_switch(
            flag("DEBUG_DIAGNOSTICS_ENABLED"),
            &[
                flag("DEBUG_SHOW_RESPONSE_INFO"),
                flag("DEBUG_LOCATE_TRACE_ENABLED"),
                flag("DEBUG_SHOW_AI_BBOX"),
            ],
        );
        config.debug_log_files_enabled = merged_switch(
            flag("DEBUG_LOG_FILES_ENABLED"),
            &[
                flag("DEBUG_LOCATE_LOG_FILE_ENABLED"),
                flag("DEBUG_PROMPT_LOG_FILE_ENABLED"),
            ],
        );

        if let Ok(v) = env::var("TRAINING_CAPTURE_ENABLED") {
            config.training_capture_enabled = v == "true" || v == "1";
        }
        // Defaults ON, so this one reads as an opt-OUT (unlike the toggles above).
        if let Ok(v) = env::var("GEMINI_THINKING_BUDGET") {
            config.gemini_thinking_budget = v.trim().parse::<i32>().ok();
        }
        if let Ok(v) = env::var("WORD_STATE_PARAGRAPH_TEXT") {
            config.word_state_paragraph_text = !(v == "false" || v == "0");
        }

        // BYOK keys stored in the Windows Credential Manager are referenced from
        // .env by a sentinel value (see credvault.rs) — resolve them to the real
        // secrets here, so every caller downstream sees a normal key. A vault miss
        // (credential deleted by hand) degrades to "no key configured", the same
        // as an empty .env line.
        for (field, env_name) in [
            (&mut config.anthropic_api_key, "ANTHROPIC_API_KEY"),
            (&mut config.gemini_api_key, "GEMINI_API_KEY"),
            (&mut config.openai_api_key, "OPENAI_API_KEY"),
            (&mut config.deepseek_api_key, "DEEPSEEK_API_KEY"),
            (&mut config.qwen_api_key, "QWEN_API_KEY"),
            (&mut config.custom_api_key, "CUSTOM_API_KEY"),
        ] {
            if field.as_deref() == Some(crate::credvault::SENTINEL) {
                *field = crate::credvault::read(env_name);
            }
        }

        config
    }
}

/// Minimal `.env` reader: one `KEY=VALUE` per line, `#` comments, blank lines
/// ignored. Values are taken literally up to end of line — no quoting, no
/// escape processing, no continuation. Sets process env vars so the existing
/// `env::var(...)` reads in `Config::load` pick them up. Existing env vars are
/// preserved (matches `dotenvy::from_path` semantics).
fn load_env_file_simple(path: &std::path::Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let key = trimmed[..eq].trim();
        let value = &trimmed[eq + 1..];
        if key.is_empty() {
            continue;
        }
        if env::var_os(key).is_none() {
            env::set_var(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merged_switch;
    use std::env;

    /// Pins the platform assumption the hotkey-clear round-trip relies on (see the
    /// hotkey block in Config::load): an env var set to the EMPTY string must read
    /// back as Ok("") — i.e. present-but-empty is distinguishable from absent. If
    /// this ever fails (a platform where setting an empty value deletes the var),
    /// a cleared hotkey would silently resurrect its default on the next load, and
    /// the clear feature needs a sentinel value ("none") instead of "".
    /// The 2026-09-04 seven-to-four merge. Tested through the pure helper rather
    /// than by setting process env vars: the suite runs in parallel, so mutating
    /// shared env is how a test becomes intermittently wrong for reasons that have
    /// nothing to do with what it is checking.
    #[test]
    fn a_legacy_switch_carries_its_group_forward() {
        // Any old key in the group being on means the group was in use.
        assert!(merged_switch(None, &[Some(true), None, None]));
        assert!(merged_switch(None, &[None, Some(true), Some(false)]));
        assert!(merged_switch(None, &[None, None, Some(true)]));
    }

    #[test]
    fn absent_everywhere_stays_off() {
        assert!(!merged_switch(None, &[None, None, None]));
        assert!(!merged_switch(None, &[Some(false), Some(false)]));
        assert!(!merged_switch(None, &[]));
    }

    #[test]
    fn an_explicit_new_key_beats_every_legacy_one() {
        // Silently resetting a developer's setup would surface only as diagnostics
        // quietly not appearing, so the legacy fill is generous -- but it must
        // never override a decision the user actually made on the new key.
        assert!(!merged_switch(Some(false), &[Some(true), Some(true)]));
        assert!(merged_switch(Some(true), &[Some(false), None]));
    }

    #[test]
    fn env_empty_value_roundtrip() {
        const KEY: &str = "NAVISUAL_TEST_EMPTY_ENV_VALUE";
        env::set_var(KEY, "");
        assert_eq!(
            env::var(KEY).as_deref(),
            Ok(""),
            "empty-valued env var must read back as Ok(\"\") — hotkey clearing depends on it"
        );
        env::remove_var(KEY);
        assert!(env::var(KEY).is_err(), "removed var must read as absent");
    }
}
