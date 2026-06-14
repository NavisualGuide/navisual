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
    pub managed_model: String,

    // Shared
    pub api_timeout_sec: u64,

    // Overlay appearance
    pub overlay_color: String,
    pub overlay_thickness: u32,

    // Behavior
    pub subtitle_enabled: bool,
    pub auto_advance: bool,

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

    // Developer / testing
    pub debug_screenshot_enabled: bool,
    pub debug_show_response_info: bool,
    /// Render the locator-trace drawer in the panel (Phase 0.1).
    pub debug_locate_trace_enabled: bool,
    /// Append every locate trace to %APPDATA%\com.navisual.app\locate_log.jsonl.
    pub debug_locate_log_file_enabled: bool,
    /// Draw the AI-returned target_bbox on the overlay (developer / comparison).
    pub debug_show_ai_bbox: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_provider: "managed".to_string(),
            anthropic_api_key: None,
            anthropic_model: "claude-sonnet-4-6".to_string(),
            anthropic_fast_model: "claude-haiku-4-5-20251001".to_string(),
            gemini_api_key: None,
            gemini_model: "gemini-2.5-flash".to_string(),
            gemini_fast_model: "gemini-2.5-flash-lite".to_string(),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.2-vision".to_string(),
            ollama_timeout_sec: 120,
            openai_api_key: None,
            openai_model: "gpt-5.5".to_string(),
            deepseek_api_key: None,
            deepseek_model: "deepseek-v4-flash".to_string(),
            qwen_api_key: None,
            qwen_model: "qwen3.6-plus".to_string(),
            qwen_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            custom_api_key: None,
            custom_model: String::new(),
            custom_base_url: String::new(),
            supabase_url: Some("https://gwekzberpfuxsoddwwqj.supabase.co".to_string()),
            supabase_anon_key: Some("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6Imd3ZWt6YmVycGZ1eHNvZGR3d3FqIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzgxMTUxMjEsImV4cCI6MjA5MzY5MTEyMX0.gCXLsnFq3NMv8_JvZGcR9TB9bAfyjCnEnj4u0RZnRbg".to_string()),
            managed_model: "openrouter/free".to_string(),
            api_timeout_sec: 90,
            overlay_color: "#FF6B35".to_string(),
            overlay_thickness: 4,
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
            debug_screenshot_enabled: false,
            debug_show_response_info: false,
            debug_locate_trace_enabled: false,
            debug_locate_log_file_enabled: false,
            debug_show_ai_bbox: false,
        }
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
        if let Ok(v) = env::var("HOTKEY_NEXT") {
            if !v.is_empty() {
                config.hotkey_next = v;
            }
        }
        if let Ok(v) = env::var("HOTKEY_WRONG") {
            if !v.is_empty() {
                config.hotkey_wrong = v;
            }
        }
        if let Ok(v) = env::var("HOTKEY_PAUSE") {
            if !v.is_empty() {
                config.hotkey_pause = v;
            }
        }
        if let Ok(v) = env::var("HOTKEY_ICON") {
            if !v.is_empty() {
                config.hotkey_icon = v;
            }
        }
        if let Ok(v) = env::var("HOTKEY_TALK") {
            if !v.is_empty() {
                config.hotkey_talk = v;
            }
        }
        if let Ok(v) = env::var("DEBUG_SCREENSHOT_ENABLED") {
            config.debug_screenshot_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("DEBUG_SHOW_RESPONSE_INFO") {
            config.debug_show_response_info = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("DEBUG_LOCATE_TRACE_ENABLED") {
            config.debug_locate_trace_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("DEBUG_LOCATE_LOG_FILE_ENABLED") {
            config.debug_locate_log_file_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = env::var("DEBUG_SHOW_AI_BBOX") {
            config.debug_show_ai_bbox = v == "true" || v == "1";
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
