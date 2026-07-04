use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{GuidanceStep, Message, NavigateStepResponse, OverlayType, Role};

/// Same schema instruction as Ollama — DeepSeek doesn't support function-calling
/// for vision models, so we use prompt engineering + response_format:json_object.
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
- target_text: 1-5 words visible on screen (optional)
- target_role: button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other (optional)
- overlay_type: "arrow" for clickable targets, "subtitle" for keyboard/scroll steps with no target (default arrow)
- checkpoint: true = wait for user confirmation, false = auto-advance (required)
- clipboard: text to copy to clipboard (optional)
- target_bbox: [ymin, xmin, ymax, xmax] as NORMALIZED 0-1000 coordinates (0 = top/left edge, 1000 = bottom/right edge of the image, regardless of pixel size; NOT pixels) (optional, omit when no target_text)
- target_element_id: integer id from the [Screen Elements] list in the message when your target appears there — only ids from the list, never invented; still fill target_text (optional, omit when the target is not listed or no list is present)

Top-level fields (outside "steps", required):
- state_summary: one sentence describing what was just accomplished
- needs_input: true only if you must ask the user a question before continuing"#;

pub struct DeepSeekClient {
    client: Client,
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    name: String,
}

impl DeepSeekClient {
    pub fn new(
        api_key: String,
        model: String,
        timeout_sec: u64,
        base_url: Option<String>,
        name: Option<String>,
    ) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .default_headers(headers)
            .build()?;
        let base_url =
            base_url.unwrap_or_else(|| "https://api.deepseek.com/v1/chat/completions".to_string());
        let name = name.unwrap_or_else(|| "DeepSeek".to_string());
        Ok(Self {
            client,
            api_key,
            model,
            base_url,
            name,
        })
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        // include_usage: OpenAI (and DashScope) only put a `usage` field in the
        // stream when asked — without it the final chunk never carries token
        // counts and the usage display records 0. DeepSeek sends usage by
        // default and accepts the option. The extra final chunk it produces has
        // an empty `choices` array, which the parser below already tolerates.
        let mut payload = json!({
            "model": effective_model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        // `response_format: json_object` improves reliability on the hosted
        // OpenAI-compat providers (DeepSeek / OpenAI / Qwen), but the Custom
        // provider points at arbitrary local servers whose support varies —
        // LM Studio rejects `json_object` outright ("must be 'json_schema' or
        // 'text'"). The prompt already mandates JSON-only output, so for Custom
        // we omit the field and let the prompt carry it.
        if self.name != "Custom" {
            payload["response_format"] = json!({ "type": "json_object" });
        }

        // DeepSeek V4 is a reasoning model and intermittently ends a stream having
        // emitted only `reasoning_content` and no answer `content` — surfaced as an
        // "empty response". The empties are non-deterministic on identical input,
        // so retry once before giving up. `stream_once` also salvages the answer
        // out of reasoning_content when the model put its JSON there.
        for attempt in 0..2 {
            if let Some(out) = self.stream_once(&payload, &mut *on_chunk).await? {
                return Ok(out);
            }
            if attempt == 0 {
                log::warn!("{}: empty content from stream, retrying once", self.name);
            }
        }
        bail!("{} returned an empty response", self.name);
    }

    /// One streamed request. Returns `Ok(None)` when the model produced no usable
    /// answer (empty `content` and no recoverable JSON in `reasoning_content`), so
    /// the caller can retry. Propagates transport / non-2xx HTTP errors.
    async fn stream_once(
        &self,
        payload: &Value,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<Option<(NavigateStepResponse, u64, u64)>> {
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(payload)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            bail!("{} API error ({}): {}", self.name, status, body);
        }

        let mut accumulated_text = String::new();
        let mut reasoning_text = String::new();
        let mut finish_reason: Option<String> = None;
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
                if line.is_empty() {
                    continue;
                }

                let data_str = if let Some(s) = line.strip_prefix("data: ") {
                    s.trim()
                } else {
                    continue;
                };

                if data_str == "[DONE]" {
                    break;
                }

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

                if let Some(fr) = data
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|v| v.as_str())
                {
                    finish_reason = Some(fr.to_string());
                }

                let delta = data
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"));

                // Reasoning models stream the chain-of-thought in a separate
                // `reasoning_content` field; capture it so the answer can be
                // salvaged if no `content` ever arrives.
                if let Some(rc) = delta
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(|c| c.as_str())
                {
                    reasoning_text.push_str(rc);
                }

                if let Some(content) = delta
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

        // Primary path — parse the JSON answer out of the content stream.
        if !text.is_empty() {
            if let Some(resp) = parse_first_nav_response(&text) {
                if emitted_instruction_len == 0 {
                    if let Some(step) = resp.steps.first() {
                        on_chunk(&step.instruction);
                    }
                }
                return Ok(Some((resp, input_tokens, output_tokens)));
            }
            // Content present but not parseable into the schema — wrap the raw
            // text as a single instruction so the user still gets guidance.
            let fallback = wrap_as_single_step(&text);
            if emitted_instruction_len == 0 {
                on_chunk(&fallback.steps[0].instruction);
            }
            return Ok(Some((fallback, input_tokens, output_tokens)));
        }

        // Salvage — reasoning models sometimes embed the JSON answer inside
        // `reasoning_content` and never emit a `content` delta. Recover it.
        if !reasoning_text.is_empty() {
            if let Some(start) = reasoning_text.find('{') {
                if let Some(resp) = parse_first_nav_response(&reasoning_text[start..]) {
                    if let Some(step) = resp.steps.first() {
                        on_chunk(&step.instruction);
                    }
                    log::info!("{}: recovered answer from reasoning_content", self.name);
                    return Ok(Some((resp, input_tokens, output_tokens)));
                }
            }
        }

        // Empty-response diagnostic. finish_reason=="stop" with content_chars==0
        // and only reasoning means the reasoning model "answered" inside its CoT
        // then stopped without emitting an answer — common for text-only DeepSeek
        // on continuation prompts that reference a screen it can't see.
        log::warn!(
            "{}: empty answer — finish_reason={:?}, content_chars={}, reasoning_chars={}",
            self.name,
            finish_reason,
            accumulated_text.len(),
            reasoning_text.len()
        );

        Ok(None)
    }
}

/// Parse the first complete `NavigateStepResponse` from `text`, tolerating code
/// fences and trailing duplicate JSON / explanatory prose.
///
/// Accepts a non-empty plan, OR any valid no-step response (the model asking via
/// `needs_input`, or signalling the task is complete). For the no-step case a clean
/// instruction is synthesized so the user never sees raw JSON — the old behaviour
/// fell through to `wrap_as_single_step`, which leaked the literal
/// `{ "steps": [], "state_summary": "...task complete", ... }` object as the guidance
/// text (observed on Qwen after a finished task). Returns `None` only when nothing
/// parses into the schema at all (genuinely unparseable output, still wrapped at the
/// call site).
fn parse_first_nav_response(text: &str) -> Option<NavigateStepResponse> {
    let stripped = text
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let mut stream =
        serde_json::Deserializer::from_str(stripped).into_iter::<NavigateStepResponse>();
    if let Some(Ok(mut resp)) = stream.next() {
        if !resp.steps.is_empty() {
            return Some(resp);
        }
        // Valid object, no steps — still a legitimate response. Synthesize a clean
        // instruction instead of falling through to the raw-JSON wrap:
        //   • needs_input → the model is asking the user something
        //   • otherwise   → the model is signalling the task is finished
        let instruction = if resp.needs_input {
            "Tell me what you'd like to do and which app or window you're in, and I'll guide you."
                .to_string()
        } else {
            "✓ That looks complete — let me know if there's anything else you'd like help with."
                .to_string()
        };
        resp.steps.push(GuidanceStep {
            instruction,
            target_text: None,
            target_role: None,
            target_region: None,
            target_nearby_text: None,
            overlay_type: OverlayType::None,
            clipboard: None,
            checkpoint: true,
            target_bbox: None,
            target_element_id: None,
        });
        return Some(resp);
    }
    None
}

/// Wrap arbitrary text as a single-step response so non-empty but unparseable
/// model output still surfaces as guidance instead of an error.
fn wrap_as_single_step(text: &str) -> NavigateStepResponse {
    NavigateStepResponse {
        steps: vec![GuidanceStep {
            instruction: text.to_string(),
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
        request_full_screen: false,
    }
}

/// Appended to the system prompt on the **text-only** path so a screen-blind model
/// (DeepSeek) doesn't falsely claim to see the screen and hallucinate UI elements.
/// Deliberately NOT used by `build_openai_messages` (OpenAI/Qwen) — those send the
/// screenshot, so the model genuinely can see it.
const TEXT_ONLY_NOTICE: &str = r#"

IMPORTANT — YOU CANNOT SEE THE SCREEN. No screenshot is provided to you (this provider is text-only). Never say or imply that you can see the user's screen. Base your guidance on the [Current Window Info] (the focused app's title and class), the user's words, and your general knowledge of how that application's UI is normally laid out. If you are unsure what is currently on screen, ask a short clarifying question (set needs_input=true) rather than guessing — do NOT invent specific on-screen elements you cannot confirm. ALWAYS return at least one step whose "instruction" is your reply to the user; when you are answering a question or asking for clarification, put that reply/question in the instruction (e.g. "I can't see your screen — tell me which app you're in and what you want to do"). Never return an empty "steps" list."#;

/// Text-only message builder (no screenshot) for the literal DeepSeek API.
/// CONFIRMED 2026-05-24: api.deepseek.com rejects `image_url` content parts with
/// HTTP 400 ("unknown variant `image_url`, expected `text`") on deepseek-v4-flash
/// and deepseek-v4-pro — DeepSeek V4 has no vision via the official API. The
/// screenshot is dropped here; DeepSeek guidance is inferred from text only.
pub fn build_messages(
    user_text: &str,
    _screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    let system_with_format = format!(
        "{}{}{}",
        SYSTEM_PROMPT, JSON_FORMAT_INSTRUCTION, TEXT_ONLY_NOTICE
    );
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

/// Build messages for OpenAI-compatible vision endpoints — used by both the
/// real OpenAI API (api.openai.com) and Qwen's DashScope OpenAI-compat
/// endpoint. Both accept the standard `image_url` content part with a base64
/// data URL, so the screenshot is included.
///
/// Do NOT use this for the literal DeepSeek API (api.deepseek.com), which
/// rejects image_url with HTTP 400 — that one needs `build_messages` instead.
pub fn build_openai_messages(
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
