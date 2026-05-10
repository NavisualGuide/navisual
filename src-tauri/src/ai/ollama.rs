use serde_json::{json, Value};
use reqwest::{Client, header};
use anyhow::{Result, bail};
use std::time::Duration;
use futures_util::StreamExt;

use crate::ai::types::{NavigateStepResponse, GuidanceStep, OverlayType, Message, Role};
use crate::ai::prompts::SYSTEM_PROMPT;

/// Appended to the system prompt so the model knows to return JSON.
/// Vision models in Ollama don't support the tools/function-calling API,
/// so we use prompt engineering instead and parse JSON from the text response.
const JSON_FORMAT_INSTRUCTION: &str = r#"

IMPORTANT: You must respond ONLY with a valid JSON object — no markdown fences, no explanation before or after. Use exactly this schema:
{"steps":[{"instruction":"<your instruction>","target_text":"<1-5 words to find on screen>","target_role":"button","overlay_type":"arrow","checkpoint":true}],"state_summary":"<brief state>","needs_input":false}

Fields:
- instruction: what the user should do (required)
- target_text: 1-5 words visible on screen that identify the element (optional)
- target_role: one of button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other (optional)
- target_region: one of top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|bottom-right (optional)
- overlay_type: arrow|highlight|circle|none (default arrow)
- clipboard: text to copy to clipboard (optional)
- checkpoint: true = wait for user, false = auto-advance (required)
- needs_input: true if you need the user to clarify something before proceeding
- request_full_screen: true if you need to see beyond the active window
- state_summary: one sentence describing what was just accomplished"#;

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

        // Vision models in Ollama do not support the tools API — omit it entirely
        // and rely on the JSON format instruction in the system message instead.
        let payload = json!({
            "model": effective_model,
            "messages": messages,
            "stream": true
        });

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let response = self.client.post(&url).json(&payload).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            bail!("Ollama API error ({}): {}", status, body);
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

            // Ollama streams NDJSON — one JSON object per line.
            while let Some(nl) = line_buf.find('\n') {
                let line = line_buf[..nl].trim().to_string();
                line_buf = line_buf[nl + 1..].to_string();
                if line.is_empty() { continue; }

                let data: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Token counts appear on the final done=true line.
                if let Some(n) = data.get("prompt_eval_count").and_then(|v| v.as_u64()) {
                    input_tokens = n;
                }
                if let Some(n) = data.get("eval_count").and_then(|v| v.as_u64()) {
                    output_tokens = n;
                }

                if let Some(content) = data
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !content.is_empty() {
                        accumulated_text.push_str(content);

                        // Stream the instruction field as it arrives.
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
            bail!("Ollama returned an empty response (is ollama serve running?)");
        }

        // Strip optional markdown code fences the model may add despite instructions.
        let stripped = text
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        // Extract the outermost JSON object from the response.
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

        // Last resort: wrap raw text as a single checkpoint step.
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

    // Append the JSON format instruction so the model knows what to return.
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

    let mut content = String::new();
    if let Some(summary) = state_summary {
        content.push_str(&format!("[Context] {}\n", summary));
    }
    content.push_str(user_text);

    let mut user_msg = json!({ "role": "user", "content": content });

    // Ollama native vision: base64 images in the top-level "images" array.
    if let Some(b64) = screenshot_b64 {
        user_msg["images"] = json!([b64]);
    }

    messages.push(user_msg);
    messages
}
