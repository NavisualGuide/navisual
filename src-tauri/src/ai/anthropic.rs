use anyhow::{anyhow, bail, Result};
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{Message, NavigateStepResponse, Role};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    client: Client,
    #[allow(dead_code)]
    api_key: String,
    pub model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String, timeout_sec: u64) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert("x-api-key", header::HeaderValue::from_str(&api_key)?);
        headers.insert(
            "anthropic-version",
            header::HeaderValue::from_str(ANTHROPIC_VERSION)?,
        );
        headers.insert(
            "anthropic-beta",
            header::HeaderValue::from_static("prompt-caching-2024-07-31"),
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

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

    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        // (delta, steps_seen): the new instruction text, and how many steps have started
        // streaming so far (count_streamed_steps) — the panel shows "Step 1 of ~N" live.
        on_chunk: &mut impl FnMut(&str, usize),
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
                                "overlay_type": {
                                    "type": "string",
                                    "enum": ["arrow", "highlight", "circle", "none"]
                                },
                                "clipboard": {"type": "string"},
                                "checkpoint": {"type": "boolean"},
                                "target_bbox": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 4,
                                    "maxItems": 4,
                                    "description": "Bounding box of the target element as [ymin, xmin, ymax, xmax] in NORMALIZED 0-1000 coordinates: 0 = top/left edge of the image, 1000 = bottom/right edge, regardless of pixel size. Tightly wrap the element. Omit when no target_text."
                                },
                                "target_element_id": {
                                    "type": "integer",
                                    "description": "Id of the target element from the [Screen Elements] list in the message, when the target appears there. Only ids from the list — never invent one. Omit when the target is not listed or no list is present. Still fill target_text."
                                }
                            }
                        }
                    },
                    "state_summary": {"type": "string"},
                    "needs_input": {"type": "boolean"},
                    "suggested_tasks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 3,
                        "description": "Up to 3 short next-task suggestions the user might ask for, ONLY when the current task looks complete or no task is in progress. Each under 80 characters, in the user's language. Omit mid-sequence."
                    }
                }
            }
        });

        let payload = json!({
            "model": effective_model,
            "max_tokens": 1024,
            "stream": true,
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

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            bail!("Anthropic API error ({}): {}", status, body_text);
        }

        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let mut stream = response.bytes_stream().eventsource();

        let mut accumulated_json = String::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut in_instruction = false;
        let mut emitted_instruction_len = 0;

        while let Some(event_result) = stream.next().await {
            let event = event_result?;
            if event.event == "content_block_delta" {
                let data: Value = serde_json::from_str(&event.data).unwrap_or_default();
                if let Some(delta) = data.get("delta") {
                    if delta.get("type").and_then(|t| t.as_str()) == Some("input_json_delta") {
                        if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
                            accumulated_json.push_str(partial);

                            let instruction_prefix = r#""instruction":""#;
                            let instruction_prefix_spaced = r#""instruction": ""#;

                            if !in_instruction
                                && (accumulated_json.contains(instruction_prefix)
                                    || accumulated_json.contains(instruction_prefix_spaced))
                            {
                                in_instruction = true;
                            }

                            if in_instruction {
                                let (delta, new_len) = crate::ai::streaming::instruction_delta(
                                    &accumulated_json,
                                    emitted_instruction_len,
                                );
                                if !delta.is_empty() {
                                    on_chunk(
                                        &delta,
                                        crate::ai::streaming::count_streamed_steps(
                                            &accumulated_json,
                                        ),
                                    );
                                }
                                emitted_instruction_len = new_len;
                            }
                        }
                    }
                }
            } else if event.event == "message_start" {
                let data: Value = serde_json::from_str(&event.data).unwrap_or_default();
                if let Some(msg) = data.get("message") {
                    if let Some(usage) = msg.get("usage") {
                        input_tokens = usage
                            .get("input_tokens")
                            .and_then(|t| t.as_u64())
                            .unwrap_or(0);
                    }
                }
            } else if event.event == "message_delta" {
                let data: Value = serde_json::from_str(&event.data).unwrap_or_default();
                if let Some(usage) = data.get("usage") {
                    output_tokens = usage
                        .get("output_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(output_tokens);
                }
            }
        }

        let step_response: NavigateStepResponse = serde_json::from_str(&accumulated_json)
            .map_err(|e| anyhow!("Failed to parse tool JSON: {e}\n{accumulated_json}"))?;

        Ok((step_response, input_tokens, output_tokens))
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
