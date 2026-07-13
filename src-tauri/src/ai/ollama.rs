use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{GuidanceStep, Message, NavigateStepResponse, OverlayType, Role};

/// Appended to the system prompt so the model knows to return JSON.
/// Vision models in Ollama don't support the tools/function-calling API,
/// so we use prompt engineering instead and parse JSON from the text response.
const JSON_FORMAT_INSTRUCTION: &str = r#"

IMPORTANT: Respond ONLY with a single valid JSON object — no markdown, no explanation. The top-level object has exactly three keys: "steps", "state_summary", "needs_input".

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
  "needs_input": false
}

Step fields (inside "steps" array only):
- instruction: what the user should do (required)
- target_text: the EXACT visible label of the element to point at, 1-5 words (REQUIRED — this is what locates the on-screen element). For a step with no on-screen target (scrolling, pressing a key), use an empty string "".
- target_role: button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other (optional)
- overlay_type: "arrow" for clickable targets, "subtitle" for keyboard/scroll steps with no target (default arrow)
- checkpoint: true = wait for user confirmation, false = auto-advance (required)
- clipboard: text to copy to clipboard (optional)
- target_bbox: [ymin, xmin, ymax, xmax] as NORMALIZED 0-1000 coordinates (0 = top/left edge, 1000 = bottom/right edge of the image, regardless of pixel size; NOT pixels) (optional, omit when no target_text)
- target_element_id: integer id from the [Screen Elements] list in the message when your target appears there — only ids from the list, never invented; still fill target_text (optional, omit when the target is not listed or no list is present)

Top-level fields (outside "steps", required):
- state_summary: one sentence describing what was just accomplished
- needs_input: true only if you must ask the user a question before continuing

Optional top-level field:
- suggested_tasks: up to 3 short next-task suggestions the user might ask for (each under 80 characters, in the user's language) — ONLY when the current task looks complete or no task is in progress; omit mid-sequence"#;

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
        Ok(Self {
            client,
            model,
            base_url,
        })
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        // (delta, steps_seen) — see streaming::count_streamed_steps.
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        // Constrain output to the navigate_step JSON *schema* (Ollama structured
        // outputs), not just "any JSON". `format:"json"` only forces *valid* JSON,
        // so weak vision models emit valid-but-wrong shapes — re-encoding the
        // window-context block, adding an extra wrapper key, or `target_bbox` as a
        // string — which fail to parse and fall through to the raw-text fallback.
        // A schema grammar-constrains sampling to the exact shape the parser expects.
        // Falls back to "json" if the Ollama build is too old to accept a schema.
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let build_payload = |fmt: Value| {
            json!({
                "model": effective_model,
                "messages": messages.clone(),
                "stream": true,
                "format": fmt,
                // Bound generation with a hard token cap. Weak vision models can run
                // away inside a string field — repeating a phrase and never closing
                // the quote — which otherwise hangs until the request times out
                // (surfacing as "error decoding response body"). A navigate_step
                // response is well under 768 tokens, so this never truncates valid
                // output; it only stops a runaway. (No repeat_penalty — under a
                // grammar constraint it skewed some models toward an empty reply.)
                "options": {
                    "num_predict": 768
                }
            })
        };

        let mut response = self
            .client
            .post(&url)
            .json(&build_payload(navigate_step_schema()))
            .send()
            .await?;
        if !response.status().is_success() {
            log::warn!(
                "Ollama rejected the JSON-schema format (status {}); retrying with format=json. Update Ollama for structured-output support.",
                response.status()
            );
            response = self
                .client
                .post(&url)
                .json(&build_payload(json!("json")))
                .send()
                .await?;
        }

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
                if line.is_empty() {
                    continue;
                }

                let data: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Surface server/grammar errors Ollama streams as {"error": ...}
                // (an unsupported `format` schema, a bad option, OOM, a model still
                // loading). Without this the stream just ends with no content and the
                // user only sees the generic "empty response" message.
                if let Some(err) = data.get("error").and_then(|e| e.as_str()) {
                    bail!("Ollama error: {}", err);
                }

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
                            let (delta, new_len) = crate::ai::streaming::instruction_delta(
                                &accumulated_text,
                                emitted_instruction_len,
                            );
                            if !delta.is_empty() {
                                on_chunk(
                                    &delta,
                                    crate::ai::streaming::count_streamed_steps(&accumulated_text),
                                );
                            }
                            emitted_instruction_len = new_len;
                        }
                    }
                }
            }
        }

        let text = accumulated_text.trim().to_string();
        if text.is_empty() {
            bail!(
                "Ollama returned no output (the server replied but the model generated nothing). \
                 This usually means the model stalled under the JSON-schema constraint or is \
                 still loading — retry, or try a different vision model."
            );
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
                    on_chunk(&step_response.steps[0].instruction, step_response.steps.len());
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
                target_bbox: None,
                target_element_id: None,
            }],
            state_summary: "Continuing task...".to_string(),
            needs_input: false,
            suggested_tasks: Vec::new(),
        };
        if emitted_instruction_len == 0 {
            on_chunk(&fallback.steps[0].instruction, 1);
        }
        Ok((fallback, input_tokens, output_tokens))
    }
}

/// JSON Schema for the navigate_step response, passed to Ollama as `format` so
/// the model's output is grammar-constrained to exactly the shape the parser
/// (`NavigateStepResponse`) expects — a non-empty `steps` array plus the three
/// top-level flags. This stops weak vision models from emitting valid-but-wrong
/// JSON (extra wrapper keys, `target_bbox` as a string, the window-context block
/// re-encoded). Optional step fields are omitted from `required` so scroll-only
/// steps can leave them out; `target_bbox` is constrained to a numeric array so
/// it can never come back as a string.
fn navigate_step_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "steps": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        // maxLength + enum stop weak models from running away inside a
                        // string field (e.g. target_role looping "-like-textbox-like-…"
                        // until the token cap, which leaves the JSON unparseable). The
                        // grammar forces the closing quote at the limit, so output stays
                        // valid. Limits are generous — they never truncate legit content.
                        "instruction": { "type": "string", "maxLength": 400 },
                        "target_text": { "type": "string", "maxLength": 80 },
                        "target_role": {
                            "type": "string",
                            "enum": [
                                "button", "tab", "link", "textbox", "menuitem", "checkbox",
                                "radio", "combobox", "slider", "image", "heading", "other"
                            ]
                        },
                        "overlay_type": { "type": "string", "maxLength": 16 },
                        "clipboard": { "type": "string", "maxLength": 2000 },
                        "target_bbox": { "type": "array", "items": { "type": "number" } },
                        "target_element_id": { "type": "integer" },
                        "checkpoint": { "type": "boolean" }
                    },
                    "required": ["instruction", "target_text", "checkpoint"]
                }
            },
            "state_summary": { "type": "string", "maxLength": 300 },
            "needs_input": { "type": "boolean" },
            "suggested_tasks": {
                "type": "array",
                "maxItems": 3,
                "items": { "type": "string", "maxLength": 80 }
            }
        },
        "required": ["steps", "state_summary", "needs_input"]
    })
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
