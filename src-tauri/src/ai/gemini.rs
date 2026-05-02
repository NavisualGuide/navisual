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
        on_chunk: &mut impl FnMut(&str),
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
                                "checkpoint": {"type": "boolean"},
                                "grid_cell": {
                                    "type": "string",
                                    "description": "Grid test only: cell label (e.g. 'D7') containing the target element. Cols 1-16 left-right, rows A-I top-bottom."
                                }
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

        // Use streamGenerateContent with SSE (alt=sse)
        let url = format!("{}/{}:streamGenerateContent?alt=sse&key={}", GEMINI_API_BASE, effective_model, self.api_key);
        
        let response = self.client.post(&url)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            bail!("Gemini API error ({}): {}", status, body_text);
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
            
            // Gemini sends data events
            let data_str = event.data;
            if data_str.is_empty() { continue; }
            
            let data: Value = serde_json::from_str(&data_str).unwrap_or_default();
            
            // Extract usage if present
            if let Some(usage) = data.get("usageMetadata") {
                input_tokens = usage.get("promptTokenCount").and_then(|t| t.as_u64()).unwrap_or(input_tokens);
                output_tokens = usage.get("candidatesTokenCount").and_then(|t| t.as_u64()).unwrap_or(output_tokens);
            }
            
            if let Some(candidates) = data.get("candidates").and_then(|c| c.as_array()) {
                if let Some(first_candidate) = candidates.first() {
                    if let Some(parts) = first_candidate.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                        for part in parts {
                            if let Some(fn_call) = part.get("functionCall") {
                                if fn_call.get("name").and_then(|n| n.as_str()) == Some("navigate_step") {
                                    if let Some(args) = fn_call.get("args") {
                                        // Gemini might stream partial args as JSON object or a string fragment?
                                        // Wait, Gemini function calling streaming behaviour: it returns partial args!
                                        // But the args might be partially constructed JSON object.
                                        // Wait, actually Gemini streamGenerateContent with tools returns the full args object 
                                        // progressively in chunks. Let's just convert it to string to extract the instruction.
                                        let partial = serde_json::to_string(args).unwrap_or_default();
                                        
                                        // We just replace accumulated_json with the latest `args` state
                                        // because Gemini sends the cumulative args so far, not diffs!
                                        accumulated_json = partial;
                                        
                                        let instruction_prefix = r#""instruction":""#;
                                        let instruction_prefix_spaced = r#""instruction": ""#;
                                        
                                        if !in_instruction && (accumulated_json.contains(instruction_prefix) || accumulated_json.contains(instruction_prefix_spaced)) {
                                            in_instruction = true;
                                        }
                                        
                                        if in_instruction {
                                            let visible = crate::ai::streaming::extract_visible_instruction(&accumulated_json);
                                            if visible.len() > emitted_instruction_len {
                                                let new_text = &visible[emitted_instruction_len..];
                                                on_chunk(new_text);
                                                emitted_instruction_len = visible.len();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let step_response: NavigateStepResponse = serde_json::from_str(&accumulated_json)
            .map_err(|e| anyhow!("Failed to parse Gemini tool JSON: {e}\n{accumulated_json}"))?;
            
        Ok((step_response, input_tokens, output_tokens))
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
