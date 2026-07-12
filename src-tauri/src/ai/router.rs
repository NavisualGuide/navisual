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
    /// Any OpenAI-compatible endpoint the user configures (local LM Studio /
    /// llama.cpp / vLLM, a DashScope workspace URL, or another cloud). Reuses
    /// the DeepSeek client and the OpenAI-compat message builder.
    Custom(DeepSeekClient),
    Managed(ManagedClient),
}

pub struct AiRouter {
    pub config: Config,
    pub cost_tracker: CostTracker,
    pub session_manager: SessionManager,
    client: Option<ApiClient>,
    managed_session_path: Option<PathBuf>,
    /// (input, output) token counts from the most recent guidance/correction call,
    /// surfaced to the debug Response-info drawer. (0, 0) before the first call.
    last_usage: (u64, u64),
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
            last_usage: (0, 0),
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
            "custom" => self.config.custom_model.clone(),
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

    /// µ$ coin balance from the relay's last paid request (None for non-managed
    /// or before any paid request this session).
    pub fn get_managed_coin_balance(&self) -> Option<i64> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.coin_balance_micro()
        } else {
            None
        }
    }

    /// (tier name, price in µ$) if the request that just completed billed real coins
    /// despite a "Free" quality-tier preference — see ManagedClient's field doc
    /// comment. One-shot: calling this consumes the value, so it's None again even
    /// if called twice for the same request.
    pub fn take_managed_tier_auto_selected(&self) -> Option<(String, i64)> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.take_tier_auto_selected()
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

    /// Record the relay-reported billing tier on the managed client (from a balance GET).
    /// No-op for non-managed providers.
    pub fn set_managed_billing_tier(&self, tier: &str) {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.set_billing_tier(tier);
        }
    }

    /// Whether to build the Structured-Context `[Screen Elements]` block for this request.
    /// Re-enabled for the managed FREE tier 2026-07-11 — was false there from 2026-07-10
    /// through the free-tier routing change: the OLD free chain's weak OpenRouter vision
    /// models (Gemma/Nemotron) hung past the client timeout on the big element list AND
    /// were bbox-denylisted so they couldn't use element selection well anyway (confirmed
    /// live 2026-07-10 — a 90-element block turned a 2.4 s free response into a >120 s
    /// hang). That reasoning no longer applies: free now routes direct to Gemini 3.1
    /// Flash-Lite / Qwen3.5-flash (see relay/index.ts's handleFreeDirect,
    /// navisual-internal), not the old weak chain. Confirmed via real BYOK usage before
    /// flipping this back on, not just the model swap alone — 21/21 logged BYOK requests
    /// across 5 provider/model pairs had the block active with zero hangs, worst case
    /// 20.4 s (qwen3.6-plus); critically, gemini-3.1-flash-lite (1.96–2.43 s) and
    /// qwen3.5-flash (7.3–11.2 s) are the *exact* models the free tier now uses, not just
    /// similar ones. If this needs reverting: the gate was `!c.is_free_tier()`, reading
    /// `ManagedClient.billing_paid` (still tracked, just unread now — see
    /// `set_billing_tier`/`send_message`'s header parsing in managed.rs, both left in
    /// place) via a since-removed `is_free_tier()` one-liner
    /// (`self.billing_paid.load(Ordering::Relaxed) != 1`).
    pub fn structured_context_enabled(&self) -> bool {
        true
    }

    /// (input, output) token counts from the most recent guidance/correction call.
    pub fn get_last_usage(&self) -> (u64, u64) {
        self.last_usage
    }

    /// Called by lib.rs after sign_in_anon to seed the session into the client.
    pub fn set_managed_session(&mut self, session: crate::server::SupabaseSession) {
        if let Some(ApiClient::Managed(ref mut c)) = self.client {
            c.session = Some(session);
        }
    }

    /// Drop the in-memory managed session (sign-out). The caller deletes the
    /// on-disk session file and seeds a fresh anonymous session afterwards so
    /// the free tier keeps working. No-op for other providers.
    ///
    /// Also wipes the cached per-account state (balance/tier/routed-model) —
    /// see ManagedClient::reset_account_state for the sign-out bleed this
    /// fixes. Every reachable account-switch flow in the UI passes through
    /// here first (sign-out / delete-account → reset_to_anonymous; there is
    /// no switch-account-while-signed-in path), and the anonymous→signed-in
    /// direction can't bleed (an anon session never populated coin state),
    /// so this single reset point covers all real paths.
    pub fn clear_managed_session(&mut self) {
        if let Some(ApiClient::Managed(ref mut c)) = self.client {
            c.session = None;
            c.reset_account_state();
        }
    }

    // NOTE: `client_access_token`, `ensure_managed_token`, and `has_managed_session`
    // were removed — account management now uses the provider-independent
    // `AppState::supabase_session` (via `acct_session_token` in lib.rs) so it works
    // regardless of which `API_PROVIDER` is active.

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
            // Any OpenAI-compatible endpoint. No API-key gate — local servers
            // (LM Studio / llama.cpp) accept an empty or dummy key; gate on the
            // Base URL being set instead.
            "custom" if !self.config.custom_base_url.trim().is_empty() => {
                let chat_url = format!(
                    "{}/chat/completions",
                    self.config.custom_base_url.trim_end_matches('/')
                );
                let key = self.config.custom_api_key.clone().unwrap_or_default();
                match DeepSeekClient::new(
                    key,
                    self.config.custom_model.clone(),
                    self.config.api_timeout_sec,
                    Some(chat_url),
                    Some("Custom".to_string()),
                ) {
                    Ok(client) => self.client = Some(ApiClient::Custom(client)),
                    Err(e) => log::error!("Custom client init failed: {e}"),
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
                    self.config.managed_tier.clone(),
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
            Some(ApiClient::OpenAI(c)) | Some(ApiClient::Qwen(c)) | Some(ApiClient::Custom(c)) => {
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

        // Record usage, attributed to the model that actually handled it (for managed,
        // the concrete model OpenRouter routed to; else the configured one).
        let provider = self.config.api_provider.clone();
        let model = self
            .get_managed_routed_model()
            .unwrap_or_else(|| self.active_model());
        self.last_usage = (in_tokens, out_tokens);
        self.cost_tracker
            .record_usage(&provider, &model, in_tokens, out_tokens);
        if let Some(session) = &mut self.session_manager.current_session {
            session.record_tokens(in_tokens, out_tokens);
        }

        Ok(response)
    }
}
