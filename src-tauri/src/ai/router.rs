use anyhow::{Result, bail};

use crate::ai::config::Config;
use crate::ai::cost_tracker::CostTracker;
use crate::ai::session::SessionManager;
use crate::ai::types::NavigateStepResponse;
use crate::ai::anthropic::{AnthropicClient, build_messages as build_anthropic};
use crate::ai::gemini::{GeminiClient, build_messages as build_gemini};

pub enum ApiClient {
    Anthropic(AnthropicClient),
    Gemini(GeminiClient),
}

pub struct AiRouter {
    pub config: Config,
    pub cost_tracker: CostTracker,
    pub session_manager: SessionManager,
    client: Option<ApiClient>,
}

impl AiRouter {
    pub fn new(config: Config, cost_tracker: CostTracker, session_manager: SessionManager) -> Self {
        let mut router = Self {
            config: config.clone(),
            cost_tracker,
            session_manager,
            client: None,
        };
        router.init_client();
        router
    }

    pub fn reload_config(&mut self, config: Config) {
        self.config = config;
        self.client = None;
        self.init_client();
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

        let result = match &self.client {
            Some(ApiClient::Anthropic(c)) => {
                let msgs = build_anthropic(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
            }
            Some(ApiClient::Gemini(c)) => {
                let msgs = build_gemini(user_text, screenshot_b64, state_summary, &conversation);
                c.send_message(msgs, None, &mut on_chunk).await?
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
