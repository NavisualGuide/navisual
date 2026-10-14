use anyhow::{anyhow, bail, Result};
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::prompts::{add_expected_result, system_prompt};
use crate::ai::types::{Message, NavigateStepResponse, Role, UsageDetail};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    client: Client,
    #[allow(dead_code)]
    api_key: String,
    pub model: String,
    /// The raw usage split of the last call -- see `UsageDetail`. A side channel rather than
    /// a wider return tuple; `take_last_usage` reads it.
    last_usage: parking_lot::Mutex<Option<UsageDetail>>,
    /// H1 switch, set by the router from `Config::expected_result` (see `prompts::system_prompt`).
    pub(crate) expected_result: bool,
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
            last_usage: parking_lot::Mutex::new(None),
            expected_result: false,
        })
    }

    /// The raw usage split of this client's last call, taken -- so a later call that reports
    /// nothing can never be read as this one.
    pub fn take_last_usage(&self) -> Option<UsageDetail> {
        self.last_usage.lock().take()
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

        let mut tool_schema = json!({
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
                    "goal": {"type": "string", "description": "The user's overall objective, carried across turns. Set it from their first request. When they add detail or a constraint later, MERGE it into the existing goal rather than replacing it - 'centre them at the bottom' REFINES 'add page numbers starting on page 3', it is not a new goal. Replace the goal outright only when the user genuinely switches to a different task. Omit or leave empty to keep the current goal unchanged."},
                    "plan_outline": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 8,
                        "description": "A short route overview toward the goal — like a map app's route overview, not turn-by-turn directions (that's steps/instruction). 2-8 short plain-language milestones, e.g. ['Open the Insert tab', 'Add page numbers', 'Set them to start at page 3', 'Skip the title page']. Shown to the user when they ask to see the plan. REVISE the whole list (replace it, don't append to it) whenever your understanding of the route changes — it is expected to change as you learn more. Omit to leave the previously shown plan unchanged; omit entirely on a simple one-step task where a route overview would add nothing."
                    },
                    "plan_completed_count": {"type": "integer", "description": "How many of plan_outline's milestones (counting from the first) are already done — 0 means you're still on the first one, 1 means the first is done and you're on the second, etc. Update it whenever a milestone is finished, and again whenever you revise plan_outline itself (count against the NEW list). Omit to leave the previous count unchanged; meaningless without plan_outline, so omit both together."},
                    "keep": {"type": "array", "items": {"type": "string"}, "description": crate::ai::prompts::KEEP_FIELD},
                    "app_note": {"type": "string", "description": crate::ai::prompts::APP_NOTE_FIELD},
                    "state_summary": {"type": "string", "description": crate::ai::prompts::STATE_SUMMARY_FIELD},
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
        if self.expected_result {
            add_expected_result(&mut tool_schema);
        }

        let payload = json!({
            "model": effective_model,
            "max_tokens": 1024,
            "stream": true,
            "system": [
                {
                    "type": "text",
                    "text": system_prompt(self.expected_result),
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

        let mut stream = response.bytes_stream().eventsource();

        let mut accumulated_json = String::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        // (raw total, cache read, cache write) from message_start, for `UsageDetail`.
        let mut raw_split = (0u64, 0u64, 0u64);
        let mut in_instruction = false;
        let mut emitted_instruction_len = 0;

        let mut progress = crate::ai::streaming::AnswerProgress::default();
        while let Some(event_result) =
            crate::ai::streaming::next_or_stall(&mut stream, progress.update(accumulated_json.len()), "Anthropic").await?
        {
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
                        let u = |k: &str| usage.get(k).and_then(|t| t.as_u64()).unwrap_or(0);
                        // Anthropic splits input three ways and `input_tokens` counts ONLY the
                        // uncached remainder — so reading it alone under-reports total input,
                        // and does so by MORE the better caching works. Both cache buckets are
                        // real input the request paid for (creation at a premium, reads at ~10%
                        // of base), so the cost tracker needs the sum.
                        let uncached = u("input_tokens");
                        let cache_write = u("cache_creation_input_tokens");
                        let cache_read = u("cache_read_input_tokens");
                        let true_total = uncached + cache_write + cache_read;

                        // Anthropic is the only provider whose input tokens are billed at
                        // three different rates: uncached 1x, cache WRITE 1.25x, cache READ
                        // 0.1x. `pricing.rs` has a single flat input rate per model, and
                        // widening the usage tuple through all seven providers to carry the
                        // split would be a lot of churn for a dev-gated estimate — so the
                        // weighting is applied here, at the only place the split exists.
                        //
                        // The reported figure is therefore a BILLING-EQUIVALENT token count,
                        // not a raw one: priced at the flat base rate it yields the right
                        // cost. Summing the three raw buckets instead (what this did before)
                        // over-charges reads by 10x, and does so by MORE the better caching
                        // works — the opposite of the truth. The raw split is logged.
                        let billable = uncached as f64
                            + cache_write as f64 * 1.25
                            + cache_read as f64 * 0.10;
                        input_tokens = billable.round() as u64;
                        raw_split = (true_total, cache_read, cache_write);

                        if cache_write + cache_read > 0 {
                            log::info!(
                                "[tokens] anthropic raw_in={true_total} (uncached={uncached} \
                                 cache_write={cache_write} cache_read={cache_read}) \
                                 billing_equivalent={input_tokens}"
                            );
                        }
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

        // Output arrives on message_delta, after the input split, so the detail is written
        // once the stream is done. Anthropic folds extended thinking into output_tokens and
        // reports no separate count.
        *self.last_usage.lock() = Some(UsageDetail {
            input: raw_split.0,
            cached_input: raw_split.1,
            cache_write: raw_split.2,
            output: output_tokens,
            reasoning: 0,
        });

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
