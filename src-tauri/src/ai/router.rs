use anyhow::{Result, bail};
use std::path::PathBuf;

use crate::ai::config::Config;
use crate::ai::cost_tracker::CostTracker;
use crate::ai::session::SessionManager;
use crate::ai::types::NavigateStepResponse;
use crate::ai::anthropic::{AnthropicClient, build_messages as build_anthropic};
use crate::ai::gemini::{GeminiClient, build_messages as build_gemini};
use crate::ai::managed::{ManagedClient, build_messages as build_managed};

pub enum ApiClient {
    Anthropic(AnthropicClient),
    Gemini(GeminiClient),
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

    /// Returns the number of free requests remaining for the managed provider,
    /// or None if the provider is not managed or no request has been made yet.
    pub fn get_managed_free_remaining(&self) -> Option<u32> {
        if let Some(ApiClient::Managed(ref c)) = self.client {
            c.free_remaining()
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
            "managed" => {
                let url = match &self.config.supabase_url {
                    Some(u) => u.clone(),
                    None => { log::error!("SUPABASE_URL not configured"); return; }
                };
                let key = match &self.config.supabase_anon_key {
                    Some(k) => k.clone(),
                    None => { log::error!("SUPABASE_ANON_KEY not configured"); return; }
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
            Some(ApiClient::Managed(c)) => {
                c.ensure_token().await?;
                let msgs = build_managed(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, &mut on_chunk).await?
            }
            None => {
                bail!("No API client configured for provider '{}'", self.config.api_provider);
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
