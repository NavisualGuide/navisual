use serde_json::{json, Value};
use reqwest::{Client, header};
use anyhow::{Result, bail, anyhow};
use std::time::Duration;
use futures_util::StreamExt;

use crate::ai::types::{NavigateStepResponse, GuidanceStep, OverlayType, Message, Role};
use crate::ai::prompts::SYSTEM_PROMPT;

pub struct OllamaClient {
    client: Client,
    pub model: String,
    pub base_url: String,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String, timeout_sec: u64) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .default_headers(headers)
            .build()?;
        Ok(Self { client, model, base_url })
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        let tool = json!({
            "type": "function",
            "function": {
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
                                    "overlay_type": {
                                        "type": "string",
                                        "enum": ["arrow", "highlight", "circle", "none"]
                                    },
                                    "clipboard": {"type": "string"},
                                    "checkpoint": {"type": "boolean"},
                                    "grid_cell": {"type": "string"}
                                }
                            }
                        },
                        "state_summary": {"type": "string"},
                        "needs_input": {"type": "boolean"},
                        "request_full_screen": {"type": "boolean"}
                    }
                }
            }
        });

        let payload = json!({
            "model": effective_model,
            "messages": messages,
            "stream": true,
            "tools": [tool]
        });

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let response = self.client.post(&url).json(&payload).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            bail!("Ollama API error ({}): {}", status, body);
        }

        let mut accumulated_text = String::new();
        let mut tool_call_args: Option<Value> = None;
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut in_instruction = false;
        let mut emitted_instruction_len = 0usize;
        let mut line_buf = String::new();

        let mut stream = response.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            line_buf.push_str(&String::from_utf8_lossy(&chunk));

            // Ollama streams NDJSON — process one complete line at a time.
            while let Some(nl) = line_buf.find('\n') {
                let line = line_buf[..nl].trim().to_string();
                line_buf = line_buf[nl + 1..].to_string();
                if line.is_empty() { continue; }

                let data: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Token usage appears on the final done=true line.
                if let Some(n) = data.get("prompt_eval_count").and_then(|v| v.as_u64()) {
                    input_tokens = n;
                }
                if let Some(n) = data.get("eval_count").and_then(|v| v.as_u64()) {
                    output_tokens = n;
                }

                if let Some(message) = data.get("message") {
                    // Tool call returned by the model.
                    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            if let Some(func) = tc.get("function") {
                                if func.get("name").and_then(|n| n.as_str()) == Some("navigate_step") {
                                    if let Some(args) = func.get("arguments") {
                                        tool_call_args = Some(args.clone());
                                    }
                                }
                            }
                        }
                    }

                    // Content chunks — streamed text (used when model returns plain text
                    // instead of a tool call, or when it narrates before calling the tool).
                    if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            accumulated_text.push_str(content);

                            let prefix = r#""instruction":""#;
                            let prefix_sp = r#""instruction": ""#;
                            if !in_instruction
                                && (accumulated_text.contains(prefix)
                                    || accumulated_text.contains(prefix_sp))
                            {
                                in_instruction = true;
                            }
                            if in_instruction {
                                let visible = crate::ai::streaming::extract_visible_instruction(
                                    &accumulated_text,
                                );
                                if visible.len() > emitted_instruction_len {
                                    on_chunk(&visible[emitted_instruction_len..]);
                                    emitted_instruction_len = visible.len();
                                }
                            }
                        }
                    }
                }
            }
        }

        // Prefer a structured tool call if the model produced one.
        if let Some(args) = tool_call_args {
            let step_response: NavigateStepResponse = serde_json::from_value(args.clone())
                .map_err(|e| anyhow!("Failed to parse Ollama tool args: {e}\n{args}"))?;
            if emitted_instruction_len == 0 {
                if let Some(s) = step_response.steps.first() {
                    on_chunk(&s.instruction);
                }
            }
            return Ok((step_response, input_tokens, output_tokens));
        }

        // Fall back to plain-text response (llama models sometimes ignore tool schemas).
        let text = accumulated_text.trim().to_string();
        if text.is_empty() {
            bail!("Ollama returned an empty response (check that the model is running: ollama serve)");
        }

        // Try to extract a JSON object embedded in the text.
        let json_text = {
            let s = text.find('{');
            let e = text.rfind('}');
            match (s, e) {
                (Some(s), Some(e)) if e > s => &text[s..=e],
                _ => text.as_str(),
            }
        };
        if let Ok(step_response) = serde_json::from_str::<NavigateStepResponse>(json_text) {
            if !step_response.steps.is_empty() {
                if emitted_instruction_len == 0 {
                    on_chunk(&step_response.steps[0].instruction);
                }
                return Ok((step_response, input_tokens, output_tokens));
            }
        }

        // Plain text — wrap it as a single checkpoint step.
        let fallback = NavigateStepResponse {
            steps: vec![GuidanceStep {
                instruction: text.clone(),
                target_text: None,
                target_role: None,
                target_region: None,
                target_nearby_text: None,
                overlay_type: OverlayType::None,
                clipboard: None,
                checkpoint: true,
                grid_cell: None,
            }],
            state_summary: "Continuing task...".to_string(),
            needs_input: false,
            request_full_screen: false,
        };
        if emitted_instruction_len == 0 {
            on_chunk(&fallback.steps[0].instruction);
        }
        Ok((fallback, input_tokens, output_tokens))
    }
}

pub fn build_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    messages.push(json!({ "role": "system", "content": SYSTEM_PROMPT }));

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        messages.push(json!({ "role": role, "content": turn.content }));
    }

    let mut content = String::new();
    if let Some(summary) = state_summary {
        content.push_str(&format!("[Context] {}\n", summary));
    }
    content.push_str(user_text);

    let mut user_msg = json!({ "role": "user", "content": content });

    // Ollama native vision: base64 images go in the top-level "images" array.
    if let Some(b64) = screenshot_b64 {
        user_msg["images"] = json!([b64]);
    }

    messages.push(user_msg);
    messages
}
