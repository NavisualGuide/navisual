use serde_json::{json, Value};
use reqwest::{Client, header};
use anyhow::{Result, bail, anyhow};
use std::time::Duration;

use crate::ai::types::{NavigateStepResponse, Message, Role};
use crate::ai::prompts::SYSTEM_PROMPT;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    client: Client,
    api_key: String,
    pub model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String, timeout_sec: u64) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert("x-api-key", header::HeaderValue::from_str(&api_key)?);
        headers.insert("anthropic-version", header::HeaderValue::from_str(ANTHROPIC_VERSION)?);
        headers.insert("anthropic-beta", header::HeaderValue::from_static("prompt-caching-2024-07-31"));
        headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .default_headers(headers)
            .build()?;

        Ok(Self {
            client,
            api_key,
            model,
        })
    }

    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);
        
        let tool_schema = json!({
            "name": "navigate_step",
            "description": "Provide navigation instructions for the user. Return one or more steps. Steps with checkpoint=true will wait for the user to complete the action before proceeding.",
            "cache_control": {"type": "ephemeral"},
            "input_schema": {
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
                                    "enum": ["button", "tab", "link", "textbox", "menuitem", "checkbox", "radio", "combobox", "slider", "image", "heading", "other"]
                                },
                                "target_region": {
                                    "type": "string",
                                    "enum": ["top-left", "top-center", "top-right", "center-left", "center", "center-right", "bottom-left", "bottom-center", "bottom-right"]
                                },
                                "target_nearby_text": {"type": "string"},
                                "target_zone_x": {"type": "integer"},
                                "target_zone_y": {"type": "integer"},
                                "overlay_type": {
                                    "type": "string",
                                    "enum": ["arrow", "highlight", "circle", "none"]
                                },
                                "clipboard": {"type": "string"},
                                "checkpoint": {"type": "boolean"}
                            }
                        }
                    },
                    "state_summary": {"type": "string"},
                    "needs_input": {"type": "boolean"},
                    "request_full_screen": {"type": "boolean"}
                }
            }
        });

        let payload = json!({
            "model": effective_model,
            "max_tokens": 1024,
            "system": [
                {
                    "type": "text",
                    "text": SYSTEM_PROMPT,
                    "cache_control": {"type": "ephemeral"}
                }
            ],
            "tools": [tool_schema],
            "tool_choice": {"type": "tool", "name": "navigate_step"},
            "messages": messages,
        });

        let response = self.client.post(ANTHROPIC_API_URL)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            bail!("Anthropic API error ({}): {}", status, body_text);
        }

        let data: Value = serde_json::from_str(&body_text)?;
        
        let usage = data.get("usage");
        let input_tokens = usage.and_then(|u| u.get("input_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);
        let output_tokens = usage.and_then(|u| u.get("output_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);

        let content = data.get("content").and_then(|c| c.as_array()).ok_or_else(|| anyhow!("Missing content array"))?;
        
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") && 
               block.get("name").and_then(|n| n.as_str()) == Some("navigate_step") {
                
                let input = block.get("input").ok_or_else(|| anyhow!("Missing tool input"))?;
                let step_response: NavigateStepResponse = serde_json::from_value(input.clone())?;
                return Ok((step_response, input_tokens, output_tokens));
            }
        }

        bail!("No navigate_step tool_use block in response")
    }
}

pub fn build_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "user", // fallback
        };
        messages.push(json!({
            "role": role,
            "content": turn.content
        }));
    }

    let mut content = Vec::new();

    if let Some(summary) = state_summary {
        content.push(json!({
            "type": "text",
            "text": format!("[Context] {}", summary)
        }));
    }

    if let Some(b64) = screenshot_b64 {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": b64
            }
        }));
    }

    content.push(json!({
        "type": "text",
        "text": user_text
    }));

    messages.push(json!({
        "role": "user",
        "content": content
    }));

    messages
}
