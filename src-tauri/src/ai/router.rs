use anyhow::{bail, Result};
use std::path::PathBuf;

use crate::ai::anthropic::{build_messages as build_anthropic, AnthropicClient};
use crate::ai::config::Config;
use crate::ai::cost_tracker::CostTracker;
use crate::ai::deepseek::{
    build_messages as build_deepseek, build_openai_messages as build_openai, DeepSeekClient,
};
use crate::ai::gemini::{build_messages as build_gemini, GeminiClient};
use crate::ai::managed::{build_messages as build_managed, ManagedClient};
use crate::ai::ollama::{build_messages as build_ollama, OllamaClient};
use crate::ai::session::SessionManager;
use crate::ai::types::NavigateStepResponse;

pub enum ApiClient {
    Anthropic(AnthropicClient),
    Gemini(GeminiClient),
    Ollama(OllamaClient),
    DeepSeek(DeepSeekClient),
    OpenAI(DeepSeekClient),
    Qwen(DeepSeekClient),
    Managed(ManagedClient),
}

pub struct AiRouter {
    pub config: Config,
    pub cost_tracker: CostTracker,
    pub session_manager: SessionManager,
    client: Option<ApiClient>,
    managed_session_path: Option<PathBuf>,
}

impl AiRouter {
    pub fn new(
        config: Config,
        cost_tracker: CostTracker,
        session_manager: SessionManager,
        managed_session_path: Option<PathBuf>,
    ) -> Self {
        let mut router = Self {
            config: config.clone(),
            cost_tracker,
            session_manager,
            client: None,
            managed_session_path,
        };
        router.init_client();
        router
    }

    pub fn reload_config(&mut self, config: Config) {
        self.config = config;
        // Preserve the managed client's session across config reloads.
        let existing_session = if let Some(ApiClient::Managed(ref c)) = self.client {
            c.session.clone()
        } else {
            None
        };
        self.client = None;
        self.init_client();
        if let (Some(session), Some(ApiClient::Managed(ref mut c))) =
            (existing_session, &mut self.client)
        {
            c.session = Some(session);
        }
    }

    /// The model string the active provider will use — for the latency/telemetry
    /// log. For `managed` this is the client-sent hint; the relay may override it
    /// server-side, so treat managed rows as "what the app requested".
    pub fn active_model(&self) -> String {
        match self.config.api_provider.as_str() {
            "anthropic" => self.config.anthropic_model.clone(),
            "gemini" => self.config.gemini_model.clone(),
            "ollama" => self.config.ollama_model.clone(),
            "openai" => self.config.openai_model.clone(),
            "deepseek" => self.config.deepseek_model.clone(),
            "qwen" => self.config.qwen_model.clone(),
            "managed" => self.config.managed_model.clone(),
            other => other.to_string(),
        }
    }

    /// Returns the number of free requests remaining for the managed provider,
    /// or None if the provider is not managed or no request has been made yet.
    pub fn get_managed_free_remaining(&self) -> Option<u32> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.free_remaining()
        } else {
            None
        }
    }

    /// The concrete model OpenRouter routed the last managed request to (the relay
    /// sends the `openrouter/free` router). None for non-managed providers or before
    /// the first request.
    pub fn get_managed_routed_model(&self) -> Option<String> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.last_routed_model()
        } else {
            None
        }
    }

    /// Called by lib.rs after sign_in_anon to seed the session into the client.
    pub fn set_managed_session(&mut self, session: crate::server::SupabaseSession) {
        if let Some(ApiClient::Managed(ref mut c)) = self.client {
            c.session = Some(session);
        }
    }

    /// Returns the current access token if the managed client has a valid session.
    pub fn client_access_token(&self) -> Option<String> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.session.as_ref().map(|s| s.access_token.clone())
        } else {
            None
        }
    }

    /// Returns true if the managed client has a session (even if expired — ensure_token refreshes it).
    pub fn has_managed_session(&self) -> bool {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.session.is_some()
        } else {
            false
        }
    }

    fn init_client(&mut self) {
        let provider = self.config.api_provider.as_str();
        match provider {
            "anthropic" => {
                if let Some(key) = &self.config.anthropic_api_key {
                    match AnthropicClient::new(
                        key.clone(),
                        self.config.anthropic_model.clone(),
                        self.config.api_timeout_sec,
                    ) {
                        Ok(client) => self.client = Some(ApiClient::Anthropic(client)),
                        Err(e) => log::error!("AnthropicClient init failed: {e}"),
                    }
                }
            }
            "gemini" => {
                if let Some(key) = &self.config.gemini_api_key {
                    match GeminiClient::new(
                        key.clone(),
                        self.config.gemini_model.clone(),
                        self.config.api_timeout_sec,
                    ) {
                        Ok(client) => self.client = Some(ApiClient::Gemini(client)),
                        Err(e) => log::error!("GeminiClient init failed: {e}"),
                    }
                }
            }
            "ollama" => {
                let timeout = self.config.ollama_timeout_sec;
                match OllamaClient::new(
                    self.config.ollama_base_url.clone(),
                    self.config.ollama_model.clone(),
                    timeout,
                ) {
                    Ok(client) => self.client = Some(ApiClient::Ollama(client)),
                    Err(e) => log::error!("OllamaClient init failed: {e}"),
                }
            }
            "deepseek" => {
                if let Some(key) = &self.config.deepseek_api_key {
                    match DeepSeekClient::new(
                        key.clone(),
                        self.config.deepseek_model.clone(),
                        self.config.api_timeout_sec,
                        None,
                        Some("DeepSeek".to_string()),
                    ) {
                        Ok(client) => self.client = Some(ApiClient::DeepSeek(client)),
                        Err(e) => log::error!("DeepSeekClient init failed: {e}"),
                    }
                }
            }
            "openai" => {
                if let Some(key) = &self.config.openai_api_key {
                    match DeepSeekClient::new(
                        key.clone(),
                        self.config.openai_model.clone(),
                        self.config.api_timeout_sec,
                        Some("https://api.openai.com/v1/chat/completions".to_string()),
                        Some("OpenAI".to_string()),
                    ) {
                        Ok(client) => self.client = Some(ApiClient::OpenAI(client)),
                        Err(e) => log::error!("OpenAI client init failed: {e}"),
                    }
                }
            }
            "qwen" => {
                if let Some(key) = &self.config.qwen_api_key {
                    let chat_url = format!(
                        "{}/chat/completions",
                        self.config.qwen_base_url.trim_end_matches('/')
                    );
                    match DeepSeekClient::new(
                        key.clone(),
                        self.config.qwen_model.clone(),
                        self.config.api_timeout_sec,
                        Some(chat_url),
                        Some("Qwen".to_string()),
                    ) {
                        Ok(client) => self.client = Some(ApiClient::Qwen(client)),
                        Err(e) => log::error!("Qwen client init failed: {e}"),
                    }
                }
            }
            "managed" => {
                let url = match &self.config.supabase_url {
                    Some(u) => u.clone(),
                    None => {
                        log::error!("SUPABASE_URL not configured");
                        return;
                    }
                };
                let key = match &self.config.supabase_anon_key {
                    Some(k) => k.clone(),
                    None => {
                        log::error!("SUPABASE_ANON_KEY not configured");
                        return;
                    }
                };
                match ManagedClient::new(
                    url,
                    key,
                    self.config.managed_model.clone(),
                    self.managed_session_path.clone(),
                    self.config.api_timeout_sec,
                ) {
                    Ok(client) => self.client = Some(ApiClient::Managed(client)),
                    Err(e) => log::error!("ManagedClient init failed: {e}"),
                }
            }
            _ => {}
        }
    }

    pub async fn send_guidance_request(
        &mut self,
        user_text: &str,
        screenshot_b64: Option<&str>,
        state_summary: Option<&str>,
        mut on_chunk: impl FnMut(&str),
    ) -> Result<NavigateStepResponse> {
        // Pre-check budget
        let estimated_total = 3000; // rough estimate
        if !self.cost_tracker.can_spend(estimated_total) {
            bail!("Token budget would be exceeded.");
        }

        let conversation = if let Some(session) = &self.session_manager.current_session {
            session.get_conversation_for_api(10)
        } else {
            Vec::new()
        };

        let result = match &mut self.client {
            Some(ApiClient::Anthropic(c)) => {
                let msgs = build_anthropic(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::Gemini(c)) => {
                let msgs = build_gemini(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::Ollama(c)) => {
                let msgs = build_ollama(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::DeepSeek(c)) => {
                // CONFIRMED 2026-05-24: api.deepseek.com rejects image_url with HTTP 400
                // ("unknown variant `image_url`, expected `text`") for both
                // deepseek-v4-flash and deepseek-v4-pro. DeepSeek V4 is text-only via
                // the official API — it cannot see the screen, so guidance is inferred
                // from the task text + history. For a China-native VISION option, use
                // Qwen (Qwen3-VL via DashScope), which accepts the screenshot below.
                let msgs = build_deepseek(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::OpenAI(c)) | Some(ApiClient::Qwen(c)) => {
                // OpenAI (api.openai.com) and Qwen (DashScope) are both OpenAI-compat
                // vision endpoints — they accept image_url and need the screenshot.
                let msgs = build_openai(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::Managed(c)) => {
                c.ensure_token().await?;
                let msgs = build_managed(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, &mut on_chunk).await?
            }
            None => {
                bail!(
                    "No API client configured for provider '{}'",
                    self.config.api_provider
                );
            }
        };

        let (response, in_tokens, out_tokens) = result;

        // Record usage
        self.cost_tracker.record_usage(in_tokens, out_tokens);
        if let Some(session) = &mut self.session_manager.current_session {
            session.record_tokens(in_tokens, out_tokens);
        }

        Ok(response)
    }
}
