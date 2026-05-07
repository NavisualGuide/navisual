use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

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

    // Supabase managed relay (S.1 free trial + paid tiers)
    pub supabase_url: Option<String>,
    pub supabase_anon_key: Option<String>,
    pub managed_model: String,

    // Shared
    pub api_timeout_sec: u64,
    pub api_max_retries: u32,

    // Managed Key
    pub managed_api_key: Option<String>,
    pub managed_provider: String,
    pub managed_token_cap: u64,

    // Token Budget
    pub daily_token_cap: u64,
    pub monthly_token_cap: u64,
    pub cost_safety_margin: f64,

    // Overlay appearance
    pub overlay_color: String,
    pub overlay_thickness: u32,

    // Behavior
    pub subtitle_enabled: bool,
    pub auto_advance: bool,

    // Audio output (TTS)
    pub tts_enabled: bool,

    // Audio input (voice)
    pub voice_input_enabled: bool,
    pub voice_language: String,

    // Hotkeys (Tauri accelerator format, e.g. "Alt+KeyE")
    pub hotkey_next: String,
    pub hotkey_wrong: String,
    pub hotkey_pause: String,
    pub hotkey_icon: String,

    // Developer / testing
    pub grid_test_enabled: bool,
    pub debug_screenshot_enabled: bool,
    pub debug_show_response_info: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_provider: "anthropic".to_string(),
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
            openai_model: "gpt-4o".to_string(),
            supabase_url: Some("https://gwekzberpfuxsoddwwqj.supabase.co".to_string()),
            supabase_anon_key: Some("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6Imd3ZWt6YmVycGZ1eHNvZGR3d3FqIiwicm9sZSI6ImFub24iLCJpYXQiOjE3NzgxMTUxMjEsImV4cCI6MjA5MzY5MTEyMX0.gCXLsnFq3NMv8_JvZGcR9TB9bAfyjCnEnj4u0RZnRbg".to_string()),
            managed_model: "nvidia/nemotron-nano-12b-v2-vl:free".to_string(),
            api_timeout_sec: 90,
            api_max_retries: 3,
            managed_api_key: None,
            managed_provider: "gemini".to_string(),
            managed_token_cap: 500_000,
            daily_token_cap: 100_000,
            monthly_token_cap: 5_000_000,
            cost_safety_margin: 2.5,
            overlay_color: "#FF6B35".to_string(),
            overlay_thickness: 4,
            subtitle_enabled: true,
            auto_advance: false,
            tts_enabled: true,
            voice_input_enabled: true,
            voice_language: "en-US".to_string(),
            hotkey_next:  "Ctrl+Backquote".to_string(),
            hotkey_wrong: "Ctrl+KeyE".to_string(),
            hotkey_pause: "Ctrl+KeyS".to_string(),
            hotkey_icon:  "Ctrl+KeyQ".to_string(),
            grid_test_enabled: false,
            debug_screenshot_enabled: false,
            debug_show_response_info: false,
        }
    }
}

impl Config {
    /// Loads configuration from a specific .env file path, or falls back to
    /// the working-directory .env (dev mode). Missing file is silently ignored.
    pub fn load(env_file: Option<&std::path::Path>) -> Self {
        match env_file {
            Some(path) => { let _ = dotenvy::from_path(path); }
            None       => { let _ = dotenvy::dotenv(); }
        }

        let mut config = Config::default();

        if let Ok(v) = env::var("API_PROVIDER") { config.api_provider = v; }
        if let Ok(v) = env::var("ANTHROPIC_API_KEY") { if !v.is_empty() { config.anthropic_api_key = Some(v); } }
        if let Ok(v) = env::var("ANTHROPIC_MODEL") { config.anthropic_model = v; }
        if let Ok(v) = env::var("ANTHROPIC_FAST_MODEL") { config.anthropic_fast_model = v; }
        
        if let Ok(v) = env::var("GEMINI_API_KEY") { if !v.is_empty() { config.gemini_api_key = Some(v); } }
        if let Ok(v) = env::var("GEMINI_MODEL") { config.gemini_model = v; }
        if let Ok(v) = env::var("GEMINI_FAST_MODEL") { config.gemini_fast_model = v; }

        if let Ok(v) = env::var("OLLAMA_BASE_URL") { config.ollama_base_url = v; }
        if let Ok(v) = env::var("OLLAMA_MODEL") { config.ollama_model = v; }
        
        if let Ok(v) = env::var("OPENAI_API_KEY") { if !v.is_empty() { config.openai_api_key = Some(v); } }
        if let Ok(v) = env::var("OPENAI_MODEL") { config.openai_model = v; }

        if let Ok(v) = env::var("SUPABASE_URL") { if !v.is_empty() { config.supabase_url = Some(v); } }
        if let Ok(v) = env::var("SUPABASE_ANON_KEY") { if !v.is_empty() { config.supabase_anon_key = Some(v); } }
        if let Ok(v) = env::var("MANAGED_MODEL") { if !v.is_empty() { config.managed_model = v; } }

        if let Ok(v) = env::var("MANAGED_API_KEY") { if !v.is_empty() { config.managed_api_key = Some(v); } }
        if let Ok(v) = env::var("OVERLAY_COLOR") { config.overlay_color = v; }
        if let Ok(v) = env::var("OVERLAY_THICKNESS") {
            if let Ok(n) = v.parse::<u32>() { config.overlay_thickness = n; }
        }
        if let Ok(v) = env::var("SUBTITLE_ENABLED") { config.subtitle_enabled = v == "true" || v == "1"; }
        if let Ok(v) = env::var("AUTO_ADVANCE") { config.auto_advance = v == "true" || v == "1"; }
        if let Ok(v) = env::var("TTS_ENABLED") { config.tts_enabled = v == "true" || v == "1"; }
        if let Ok(v) = env::var("VOICE_INPUT_ENABLED") { config.voice_input_enabled = v == "true" || v == "1"; }
        if let Ok(v) = env::var("VOICE_LANGUAGE") { if !v.is_empty() { config.voice_language = v; } }
        if let Ok(v) = env::var("HOTKEY_NEXT")  { if !v.is_empty() { config.hotkey_next  = v; } }
        if let Ok(v) = env::var("HOTKEY_WRONG") { if !v.is_empty() { config.hotkey_wrong = v; } }
        if let Ok(v) = env::var("HOTKEY_PAUSE") { if !v.is_empty() { config.hotkey_pause = v; } }
        if let Ok(v) = env::var("HOTKEY_ICON")  { if !v.is_empty() { config.hotkey_icon  = v; } }
        if let Ok(v) = env::var("GRID_TEST_ENABLED") { config.grid_test_enabled = v == "true" || v == "1"; }
        if let Ok(v) = env::var("DEBUG_SCREENSHOT_ENABLED") { config.debug_screenshot_enabled = v == "true" || v == "1"; }
        if let Ok(v) = env::var("DEBUG_SHOW_RESPONSE_INFO") { config.debug_show_response_info = v == "true" || v == "1"; }
        if let Ok(v) = env::var("MANAGED_PROVIDER") { config.managed_provider = v; }
        if let Ok(v) = env::var("MANAGED_TOKEN_CAP") { 
            if let Ok(n) = v.parse() { config.managed_token_cap = n; }
        }

        if let Ok(v) = env::var("DAILY_TOKEN_CAP") { 
            if let Ok(n) = v.parse() { config.daily_token_cap = n; }
        }
        if let Ok(v) = env::var("MONTHLY_TOKEN_CAP") { 
            if let Ok(n) = v.parse() { config.monthly_token_cap = n; }
        }
        if let Ok(v) = env::var("COST_SAFETY_MARGIN") { 
            if let Ok(n) = v.parse() { config.cost_safety_margin = n; }
        }

        config
    }
}
