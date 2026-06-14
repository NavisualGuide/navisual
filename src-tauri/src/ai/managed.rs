use anyhow::{anyhow, bail, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{Message, NavigateStepResponse, Role};
use crate::server::{
    load_session, refresh_session, save_session, sign_in_anonymously, SupabaseSession,
};

pub struct ManagedClient {
    client: Client,
    pub supabase_url: String,
    pub anon_key: String,
    pub model: String,
    pub session: Option<SupabaseSession>,
    session_path: Option<PathBuf>,
    free_remaining: AtomicI64, // -1 = unknown
    // The model OpenRouter actually routed to on the last request (the relay sends
    // `openrouter/free`, a router; the response `model` names the concrete model used).
    last_model: parking_lot::Mutex<Option<String>>,
}

impl ManagedClient {
    pub fn new(
        supabase_url: String,
        anon_key: String,
        model: String,
        session_path: Option<PathBuf>,
        timeout_sec: u64,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .build()?;

        let session = session_path.as_deref().and_then(load_session);

        Ok(Self {
            client,
            supabase_url,
            anon_key,
            model,
            session,
            session_path,
            free_remaining: AtomicI64::new(-1),
            last_model: parking_lot::Mutex::new(None),
        })
    }

    pub fn free_remaining(&self) -> Option<u32> {
        let v = self.free_remaining.load(Ordering::Relaxed);
        if v < 0 {
            None
        } else {
            Some(v as u32)
        }
    }

    /// The concrete model OpenRouter routed to on the most recent request.
    pub fn last_routed_model(&self) -> Option<String> {
        self.last_model.lock().clone()
    }

    pub async fn ensure_token(&mut self) -> Result<()> {
        match &self.session {
            Some(s) if !s.is_expired() => return Ok(()),
            Some(s) => {
                let refresh_token = s.refresh_token.clone();
                match refresh_session(&self.supabase_url, &self.anon_key, &refresh_token).await {
                    Ok(new_session) => {
                        if let Some(ref path) = self.session_path {
                            save_session(path, &new_session);
                        }
                        self.session = Some(new_session);
                        return Ok(());
                    }
                    Err(e) => {
                        log::warn!("Session refresh failed ({e}), signing in fresh");
                    }
                }
            }
            None => {}
        }
        let new_session = sign_in_anonymously(&self.supabase_url, &self.anon_key).await?;
        if let Some(ref path) = self.session_path {
            save_session(path, &new_session);
        }
        self.session = Some(new_session);
        Ok(())
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let access_token = self
            .session
            .as_ref()
            .map(|s| s.access_token.clone())
            .ok_or_else(|| anyhow!("no active Supabase session"))?;

        let tool = navigate_step_tool();
        let payload = json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": messages,
            "tools": [tool],
            "tool_choice": {"type": "function", "function": {"name": "navigate_step"}},
        });

        let relay_url = format!("{}/functions/v1/relay", self.supabase_url);
        let resp = self
            .client
            .post(&relay_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();

        if status.as_u16() == 402 {
            bail!("free_trial_exhausted");
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("relay error ({}): {}", status, text);
        }

        // Capture X-Free-Remaining before consuming the response body.
        let remaining = resp
            .headers()
            .get("x-free-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        if let Some(r) = remaining {
            self.free_remaining.store(r as i64, Ordering::Relaxed);
        }

        let body: Value = resp.json().await?;

        // OpenRouter returns the concrete model it routed to (the relay sends the
        // `openrouter/free` router). Record it so the UI/feedback can show which
        // free model actually handled the request.
        let routed = body["model"].as_str().map(str::to_string);
        log::info!("[managed] requested={} routed={routed:?}", self.model);
        *self.last_model.lock() = routed;

        let json_str = body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .ok_or_else(|| {
                log::warn!(
                    "[managed] no tool_calls in relay response: {}",
                    serde_json::to_string(&body).unwrap_or_default()
                );
                anyhow!("The free model returned an unreadable response. Please try again.")
            })?;

        // Free models occasionally emit malformed tool args (leaked </think>, whitespace
        // runaway → truncated JSON). Surface a friendly retry, keep the detail in the log.
        let nav_response: NavigateStepResponse = serde_json::from_str(json_str).map_err(|e| {
            log::warn!("[managed] navigate_step parse error: {e}\njson: {json_str}");
            anyhow!("The free model returned an unreadable response. Please try again.")
        })?;

        // Emit the first instruction as a single chunk (managed tier is non-streaming).
        if let Some(step) = nav_response.steps.first() {
            on_chunk(&step.instruction);
        }

        Ok((nav_response, 0, 0))
    }
}

pub fn build_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    // System prompt as a system role message (OpenAI format).
    messages.push(json!({"role": "system", "content": SYSTEM_PROMPT}));

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "user",
        };
        messages.push(json!({"role": role, "content": turn.content}));
    }

    let mut content: Vec<Value> = Vec::new();

    if let Some(summary) = state_summary {
        content.push(json!({"type": "text", "text": format!("[Context] {}", summary)}));
    }

    if let Some(b64) = screenshot_b64 {
        content.push(json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/jpeg;base64,{}", b64)}
        }));
    }

    content.push(json!({"type": "text", "text": user_text}));

    messages.push(json!({"role": "user", "content": content}));

    messages
}

fn navigate_step_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "navigate_step",
            "description": "Provide navigation instructions for the user. Return one or more steps.",
            "parameters": {
                "type": "object",
                "required": ["steps", "state_summary", "needs_input"],
                "properties": {
                    "steps": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["instruction", "checkpoint"],
                            "properties": {
                                "instruction": {"type": "string"},
                                "target_text": {"type": "string"},
                                "target_role": {
                                    "type": "string",
                                    "enum": ["button","tab","link","textbox","menuitem",
                                             "checkbox","radio","combobox","slider",
                                             "image","heading","other"]
                                },
                                "target_region": {
                                    "type": "string",
                                    "enum": ["top-left","top-center","top-right",
                                             "center-left","center","center-right",
                                             "bottom-left","bottom-center","bottom-right"]
                                },
                                "target_nearby_text": {"type": "string"},
                                "overlay_type": {
                                    "type": "string",
                                    "enum": ["arrow","highlight","circle","none"]
                                },
                                "clipboard": {"type": "string"},
                                "checkpoint": {"type": "boolean"},
                                "target_bbox": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 4,
                                    "maxItems": 4,
                                    "description": "Bounding box of the target element as [ymin, xmin, ymax, xmax] in NORMALIZED 0-1000 coordinates (0 = top/left edge, 1000 = bottom/right edge of the image, regardless of pixel size). Omit when no target_text."
                                }
                            }
                        }
                    },
                    "state_summary": {"type": "string"},
                    "needs_input": {"type": "boolean"}
                }
            }
        }
    })
}
