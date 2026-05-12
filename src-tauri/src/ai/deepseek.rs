use serde_json::{json, Value};
use reqwest::{Client, header};
use anyhow::{Result, bail};
use std::time::Duration;
use futures_util::StreamExt;

use crate::ai::types::{NavigateStepResponse, GuidanceStep, OverlayType, Message, Role};
use crate::ai::prompts::SYSTEM_PROMPT;

/// Same schema instruction as Ollama — DeepSeek doesn't support function-calling
/// for vision models, so we use prompt engineering + response_format:json_object.
const JSON_FORMAT_INSTRUCTION: &str = r#"

IMPORTANT: Respond ONLY with a single valid JSON object — no markdown, no explanation. The top-level object has exactly four keys: "steps", "state_summary", "needs_input", "request_full_screen".

Example (copy this structure exactly):
{
  "steps": [
    {
      "instruction": "Click the Layout tab at the top of the ribbon.",
      "target_text": "Layout",
      "target_role": "tab",
      "overlay_type": "arrow",
      "checkpoint": true
    }
  ],
  "state_summary": "User is opening the Layout tab.",
  "needs_input": false,
  "request_full_screen": false
}

Step fields (inside "steps" array only):
- instruction: what the user should do (required)
- target_text: 1-5 words visible on screen (optional)
- target_role: button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other (optional)
- overlay_type: "arrow" for clickable targets, "subtitle" for keyboard/scroll steps with no target (default arrow)
- checkpoint: true = wait for user confirmation, false = auto-advance (required)
- clipboard: text to copy to clipboard (optional)

Top-level fields (outside "steps", required):
- state_summary: one sentence describing what was just accomplished
- needs_input: true only if you must ask the user a question before continuing
- request_full_screen: true only if you need to see beyond the current window"#;

pub struct DeepSeekClient {
    client: Client,
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    name: String,
}

impl DeepSeekClient {
    pub fn new(api_key: String, model: String, timeout_sec: u64, base_url: Option<String>, name: Option<String>) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .default_headers(headers)
            .build()?;
        let base_url = base_url.unwrap_or_else(|| "https://api.deepseek.com/v1/chat/completions".to_string());
        let name = name.unwrap_or_else(|| "DeepSeek".to_string());
        Ok(Self { client, api_key, model, base_url, name })
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        let payload = json!({
            "model": effective_model,
            "messages": messages,
            "stream": true,
            "response_format": { "type": "json_object" }
        });

        let response = self.client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            bail!("{} API error ({}): {}", self.name, status, body);
        }

        let mut accumulated_text = String::new();
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        let mut in_instruction = false;
        let mut emitted_instruction_len = 0usize;
        let mut line_buf = String::new();

        let mut stream = response.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            line_buf.push_str(&String::from_utf8_lossy(&chunk));

            // OpenAI SSE format: "data: {...}\n\n" lines.
            while let Some(nl) = line_buf.find('\n') {
                let line = line_buf[..nl].trim().to_string();
                line_buf = line_buf[nl + 1..].to_string();
                if line.is_empty() { continue; }

                let data_str = if let Some(s) = line.strip_prefix("data: ") {
                    s.trim()
                } else {
                    continue;
                };

                if data_str == "[DONE]" { break; }

                let data: Value = match serde_json::from_str(data_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Token counts appear in the final chunk's usage field.
                if let Some(usage) = data.get("usage") {
                    if let Some(n) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        input_tokens = n;
                    }
                    if let Some(n) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = n;
                    }
                }

                if let Some(content) = data
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
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

        let text = accumulated_text.trim().to_string();
        if text.is_empty() {
            bail!("{} returned an empty response", self.name);
        }

        let stripped = text
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let json_text = match (stripped.find('{'), stripped.rfind('}')) {
            (Some(s), Some(e)) if e > s => &stripped[s..=e],
            _ => stripped,
        };

        if let Ok(step_response) = serde_json::from_str::<NavigateStepResponse>(json_text) {
            if !step_response.steps.is_empty() {
                if emitted_instruction_len == 0 {
                    on_chunk(&step_response.steps[0].instruction);
                }
                return Ok((step_response, input_tokens, output_tokens));
            }
        }

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
    _screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    let system_with_format = format!("{}{}", SYSTEM_PROMPT, JSON_FORMAT_INSTRUCTION);
    messages.push(json!({ "role": "system", "content": system_with_format }));

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        messages.push(json!({ "role": role, "content": turn.content }));
    }

    let mut text_content = String::new();
    if let Some(summary) = state_summary {
        text_content.push_str(&format!("[Context] {}\n", summary));
    }
    text_content.push_str(user_text);

    // DeepSeek's chat completions API (api.deepseek.com) is text-only —
    // image_url content parts are rejected with a 400. Skip the screenshot.
    messages.push(json!({ "role": "user", "content": text_content }));

    messages
}

/// Build messages for Qwen (DashScope OpenAI-compat endpoint).
/// Qwen VL models accept image_url with base64 data URLs — include the screenshot.
pub fn build_qwen_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    let system_with_format = format!("{}{}", SYSTEM_PROMPT, JSON_FORMAT_INSTRUCTION);
    messages.push(json!({ "role": "system", "content": system_with_format }));

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };
        messages.push(json!({ "role": role, "content": turn.content }));
    }

    let mut text_content = String::new();
    if let Some(summary) = state_summary {
        text_content.push_str(&format!("[Context] {}\n", summary));
    }
    text_content.push_str(user_text);

    if let Some(b64) = screenshot_b64 {
        let content = json!([
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/jpeg;base64,{}", b64)
                }
            },
            { "type": "text", "text": text_content }
        ]);
        messages.push(json!({ "role": "user", "content": content }));
    } else {
        messages.push(json!({ "role": "user", "content": text_content }));
    }

    messages
}
