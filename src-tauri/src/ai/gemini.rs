use serde_json::{json, Value};
use reqwest::{Client, header};
use anyhow::{Result, bail, anyhow};
use std::time::Duration;

use crate::ai::types::{NavigateStepResponse, Message, Role};
use crate::ai::prompts::SYSTEM_PROMPT;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiClient {
    client: Client,
    api_key: String,
    pub model: String,
}

impl GeminiClient {
    pub fn new(api_key: String, model: String, timeout_sec: u64) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
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
            "contents": messages,
            "systemInstruction": {
                "parts": [{"text": SYSTEM_PROMPT}]
            },
            "tools": [{"function_declarations": [tool_schema]}],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": ["navigate_step"]
                }
            }
        });

        let url = format!("{}/{}:generateContent?key={}", GEMINI_API_BASE, effective_model, self.api_key);
        
        let response = self.client.post(&url)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            bail!("Gemini API error ({}): {}", status, body_text);
        }

        let data: Value = serde_json::from_str(&body_text)?;
        
        let usage = data.get("usageMetadata");
        let input_tokens = usage.and_then(|u| u.get("promptTokenCount")).and_then(|t| t.as_u64()).unwrap_or(0);
        let output_tokens = usage.and_then(|u| u.get("candidatesTokenCount")).and_then(|t| t.as_u64()).unwrap_or(0);

        let candidates = data.get("candidates").and_then(|c| c.as_array()).ok_or_else(|| anyhow!("Missing candidates array"))?;
        
        if let Some(first_candidate) = candidates.first() {
            let parts = first_candidate.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()).ok_or_else(|| anyhow!("Missing parts array"))?;
            for part in parts {
                if let Some(fn_call) = part.get("functionCall") {
                    if fn_call.get("name").and_then(|n| n.as_str()) == Some("navigate_step") {
                        let args = fn_call.get("args").ok_or_else(|| anyhow!("Missing function args"))?;
                        let step_response: NavigateStepResponse = serde_json::from_value(args.clone())?;
                        return Ok((step_response, input_tokens, output_tokens));
                    }
                }
            }
        }

        bail!("No navigate_step function call in response")
    }
}

pub fn build_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut contents = Vec::new();

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "model",
            Role::System => "user", // fallback
        };
        contents.push(json!({
            "role": role,
            "parts": [{"text": turn.content}]
        }));
    }

    let mut parts = Vec::new();

    if let Some(summary) = state_summary {
        parts.push(json!({"text": format!("[Context] {}", summary)}));
    }

    if let Some(b64) = screenshot_b64 {
        parts.push(json!({
            "inlineData": {
                "mimeType": "image/jpeg",
                "data": b64
            }
        }));
    }

    parts.push(json!({"text": user_text}));

    contents.push(json!({
        "role": "user",
        "parts": parts
    }));

    contents
}
