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
            api_timeout_sec: 30,
            api_max_retries: 3,
            managed_api_key: None,
            managed_provider: "gemini".to_string(),
            managed_token_cap: 500_000,
            daily_token_cap: 100_000,
            monthly_token_cap: 5_000_000,
            cost_safety_margin: 2.5,
        }
    }
}

impl Config {
    /// Loads configuration from environment variables (and .env file if present).
    pub fn load() -> Self {
        // Try to load .env, but ignore error if it doesn't exist
        let _ = dotenvy::dotenv();

        let mut config = Config::default();

        if let Ok(v) = env::var("API_PROVIDER") { config.api_provider = v; }
        if let Ok(v) = env::var("ANTHROPIC_API_KEY") { config.anthropic_api_key = Some(v); }
        if let Ok(v) = env::var("ANTHROPIC_MODEL") { config.anthropic_model = v; }
        if let Ok(v) = env::var("ANTHROPIC_FAST_MODEL") { config.anthropic_fast_model = v; }
        
        if let Ok(v) = env::var("GEMINI_API_KEY") { config.gemini_api_key = Some(v); }
        if let Ok(v) = env::var("GEMINI_MODEL") { config.gemini_model = v; }
        if let Ok(v) = env::var("GEMINI_FAST_MODEL") { config.gemini_fast_model = v; }

        if let Ok(v) = env::var("OLLAMA_BASE_URL") { config.ollama_base_url = v; }
        if let Ok(v) = env::var("OLLAMA_MODEL") { config.ollama_model = v; }
        
        if let Ok(v) = env::var("OPENAI_API_KEY") { config.openai_api_key = Some(v); }
        if let Ok(v) = env::var("OPENAI_MODEL") { config.openai_model = v; }

        if let Ok(v) = env::var("MANAGED_API_KEY") { config.managed_api_key = Some(v); }
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
