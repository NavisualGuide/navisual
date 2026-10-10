use anyhow::{anyhow, bail, Result};
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::prompts::{add_expected_result, system_prompt};
use crate::ai::types::{Message, NavigateStepResponse, Role, UsageDetail};

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiClient {
    client: Client,
    api_key: String,
    pub model: String,
    /// `None` = omit thinkingConfig entirely (provider default: dynamic).
    /// `Some(0)` = thinking off. `Some(n)` = cap at ~n tokens. **Gemini 2.5-era only.**
    thinking_budget: Option<i32>,
    /// Shared `reasoning_effort` word, clamped to what Gemini accepts. Takes precedence
    /// over `thinking_budget`: a 3.x model ignores the budget, and every model this app
    /// ships with is 3.x.
    thinking_level: Option<String>,
    /// The raw usage split of the last call -- see `UsageDetail`. A side channel rather than
    /// a wider return tuple; `take_last_usage` reads it.
    last_usage: parking_lot::Mutex<Option<UsageDetail>>,
    /// H1 switch, set by the router from `Config::expected_result` (see `prompts::system_prompt`).
    pub(crate) expected_result: bool,
    /// H2: the frame `inspect` crops from, set per request by the router; `None` = no tool.
    pub(crate) inspect_frame: Option<std::sync::Arc<image::RgbImage>>,
    pub(crate) inspect_mode: crate::ai::inspect::Mode,
    pub(crate) inspect_tools: crate::ai::inspect::Tools,
    last_inspects: parking_lot::Mutex<Vec<crate::ai::inspect::Look>>,
    /// `modelVersion` of the last response: the model that answered, which is not always the one
    /// named (Google redirected gemini-3.7-flash to 3.8 on 2026-10-09).
    last_served: parking_lot::Mutex<Option<String>>,
}

impl GeminiClient {
    pub fn new(
        api_key: String,
        model: String,
        timeout_sec: u64,
        thinking_budget: Option<i32>,
        thinking_level: Option<String>,
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

        Ok(Self {
            client,
            api_key,
            model,
            thinking_budget,
            thinking_level,
            last_usage: parking_lot::Mutex::new(None),
            expected_result: false,
            inspect_frame: None,
            inspect_mode: crate::ai::inspect::Mode::Off,
            inspect_tools: crate::ai::inspect::Tools::Zoom,
            last_inspects: parking_lot::Mutex::new(Vec::new()),
            last_served: parking_lot::Mutex::new(None),
        })
    }

    /// The raw usage split of this client's last call, taken -- so a later call that reports
    /// nothing can never be read as this one.
    pub fn take_last_usage(&self) -> Option<UsageDetail> {
        self.last_usage.lock().take()
    }

    #[cfg(test)]
    pub fn take_inspects(&self) -> Vec<crate::ai::inspect::Look> {
        std::mem::take(&mut *self.last_inspects.lock())
    }

    #[cfg(test)]
    pub fn take_served(&self) -> Option<String> {
        self.last_served.lock().take()
    }

    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        // (delta, steps_seen) — see streaming::count_streamed_steps.
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        let mut tool_schema = json!({
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
                                "clipboard": {"type": "string"},
                                "checkpoint": {"type": "boolean"},
                                "target_bbox": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 4,
                                    "maxItems": 4,
                                    "description": "Bounding box of the target element as [ymin, xmin, ymax, xmax] in your native object-detection coordinate system (normalized 0-1000). The box should tightly wrap the target element. Omit when no target_text."
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
                    "state_summary": {"type": "string", "description": "Your ONLY memory between turns — earlier turns are truncated away. Rewrite it in full each turn, carrying: the user's GOAL in their own words (verbatim until they change it), any CONSTRAINTS they stated, what is DONE, and anything TRIED THAT FAILED. Never assume an earlier turn is still visible."},
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

        let mut payload = json!({
            "contents": messages,
            "systemInstruction": {
                "parts": [{"text": system_prompt(self.expected_result)}]
            },
            "tools": [{"function_declarations": [tool_schema]}],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": ["navigate_step"]
                }
            }
        });

        // Reasoning budget. Left ABSENT by default, which means the provider's own dynamic
        // policy — measured at 447 thinking tokens against 156 of actual output on a
        // "which control do I click" task, i.e. ~74% of generated tokens producing nothing
        // the user ever sees. Sending 0 disables thinking (Flash only; Pro has a floor), a
        // positive value caps it while still allowing reasoning.
        //
        // Deliberately not defaulted to 0: some steps ARE genuine multi-step planning
        // ("restart page numbering at page 3 without touching the title page") and that is
        // where the budget earns its latency. The right home for this is the existing
        // Speed/Regular/Smart dial rather than a global switch — this field exists so the
        // trade can be measured before it is wired to a tier.
        // Level first: Gemini 3.x takes `thinkingLevel` and ignores `thinkingBudget`, so on
        // every model this app ships with the budget is the dead branch. It is kept for a
        // BYOK user pointing at an older 2.5 model, where the reverse is true.
        //
        // Clamped to Gemini's range rather than passed through, because the shared key also
        // serves OpenAI, which has three levels Gemini does not. `none` becomes `minimal`:
        // Gemini 3.x cannot turn reasoning off, so honouring it literally would 400 every
        // request for someone whose only mistake was using a word another provider accepts.
        //
        // `minimal` is NOT a universal Gemini floor. Measured live 2026-09-21 against
        // gemini-3.7-flash, the shipped default: 400 INVALID_ARGUMENT, "Thinking level
        // MINIMAL is not supported for this model." Only the Flash-Lite line accepts it;
        // on Flash and Pro the floor is `low`. So the clamp goes UP, not down -- giving a
        // model more thinking than asked costs a little latency, while asking for a level
        // it does not have breaks every request the user makes.
        let minimal_supported = self.model.contains("flash-lite");
        let level = self.thinking_level.as_deref().map(|l| match l {
            "none" | "minimal" if minimal_supported => "minimal",
            "none" | "minimal" => "low",
            "xhigh" | "max" => "high",
            other => other,
        });
        if let Some(level) = level {
            payload["generationConfig"] = json!({
                "thinkingConfig": { "thinkingLevel": level }
            });
            log_thinking_once(&format!("thinkingLevel={level}"));
        } else if let Some(budget) = self.thinking_budget {
            payload["generationConfig"] = json!({
                "thinkingConfig": { "thinkingBudget": budget }
            });
            log_thinking_once(&format!("thinkingBudget={budget}"));
        } else {
            log_thinking_once("provider default (no thinkingConfig sent)");
        }

        // H2 (`inspect.rs`): when a frame is offered the model may zoom before it answers, up to
        // MAX_INSPECTS times; each zoom is one more round on the same contents, the crop riding
        // back inside the functionResponse. Only the navigate_step round streams a caption.
        let frame = self.inspect_frame.clone();
        self.last_inspects.lock().clear();
        let look_names = self.inspect_tools.names();
        let mut declared = vec![tool_schema.clone()];
        for (name, description, parameters) in
            crate::ai::inspect::declarations(self.inspect_mode, self.inspect_tools)
        {
            declared.push(json!({"name": name, "description": description, "parameters": parameters}));
        }
        let mut contents: Vec<Value> = payload["contents"].as_array().cloned().unwrap_or_default();
        let mut total = UsageDetail::default();
        let mut zooms = 0usize;

        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let (accumulated_json, raw_text) = loop {
            let zoom_open = frame.is_some() && zooms < crate::ai::inspect::MAX_INSPECTS;
            payload["contents"] = Value::Array(contents.clone());
            if zoom_open {
                // `First`: the opening round may only look.
                let mut allowed: Vec<&str> = look_names.to_vec();
                if !(zooms == 0 && self.inspect_mode == crate::ai::inspect::Mode::First) {
                    allowed.insert(0, "navigate_step");
                }
                payload["tools"] = json!([{"function_declarations": declared.clone()}]);
                payload["toolConfig"] = json!({"functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": allowed
                }});
            } else if zooms > 0 {
                payload["tools"] = json!([{"function_declarations": [tool_schema.clone()]}]);
                payload["toolConfig"] = json!({"functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": ["navigate_step"]
                }});
            }

            let response = self
                .client
                .post(stream_url(effective_model))
                .header("x-goog-api-key", &self.api_key)
                .json(&payload)
                .send()
                .await?;

            let status = response.status();
            if !status.is_success() {
                let body_text = response.text().await?;
                bail!("Gemini API error ({}): {}", status, body_text);
            }

            let mut stream = response.bytes_stream().eventsource();

            let mut accumulated_json = String::new();
            let mut in_instruction = false;
            let mut emitted_instruction_len = 0;
            let mut raw_text = String::new();
            let mut round: Option<UsageDetail> = None;
            // The model's turn as it will be echoed back on a zoom: every non-thought part, one
            // functionCall (the latest, carrying the thoughtSignature Gemini 3 requires back).
            let mut model_parts: Vec<Value> = Vec::new();
            let mut inspect_args: Option<Value> = None;
            let mut inspect_id: Option<Value> = None;
            let mut inspect_name = String::new();

            while let Some(event_result) = stream.next().await {
                let event = event_result?;

                // Gemini sends data events
                let data_str = event.data;
                if data_str.is_empty() {
                    continue;
                }

                let data: Value = serde_json::from_str(&data_str).unwrap_or_default();
                if let Some(v) = data.get("modelVersion").and_then(Value::as_str) {
                    *self.last_served.lock() = Some(v.to_string());
                }

                // Extract usage if present
                if let Some(usage) = data.get("usageMetadata") {
                    let input_tokens = usage
                        .get("promptTokenCount")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    // Google bills reasoning as OUTPUT, so the cost estimate needs both.
                    // `candidatesTokenCount` alone under-reported by ~4x on a measured request
                    // (156 visible against 447 thinking) — the Usage tab was showing a quarter
                    // of what was actually charged.
                    let visible = usage
                        .get("candidatesTokenCount")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    let thoughts = usage
                        .get("thoughtsTokenCount")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    // Gemini reports implicit-cache hits here. We never ask for caching, so a
                    // non-zero value means we are getting it free; a persistent zero is the
                    // evidence for wiring explicit context caching on the constant prefix.
                    let c = usage
                        .get("cachedContentTokenCount")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    log::info!("[tokens] gemini in={input_tokens} out={} cached={c}", visible + thoughts);
                    // Usage arrives on more than one chunk, cumulatively; the last one wins.
                    round = Some(UsageDetail {
                        input: input_tokens,
                        cached_input: c,
                        cache_write: 0,
                        output: visible + thoughts,
                        reasoning: thoughts,
                    });
                    // A zero is ambiguous between "not caching" and "not reporting", so put the
                    // real field set on record once rather than guessing from the absence.
                    if c == 0 {
                        static DUMPED: std::sync::atomic::AtomicBool =
                            std::sync::atomic::AtomicBool::new(false);
                        if !DUMPED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            log::info!("[tokens] raw gemini usageMetadata (once): {usage}");
                        }
                    }
                }

                if let Some(candidates) = data.get("candidates").and_then(|c| c.as_array()) {
                    if let Some(first_candidate) = candidates.first() {
                        if let Some(parts) = first_candidate
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if !part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                                    keep_model_part(&mut model_parts, part);
                                }
                                if let Some(fn_call) = part.get("functionCall") {
                                    let name = fn_call.get("name").and_then(|n| n.as_str());
                                    if name.is_some_and(|n| look_names.contains(&n)) {
                                        inspect_name = name.unwrap_or_default().to_string();
                                        inspect_args = fn_call.get("args").cloned();
                                        inspect_id = fn_call.get("id").cloned();
                                    } else if name == Some("navigate_step") {
                                        if let Some(args) = fn_call.get("args") {
                                            // Gemini sends the cumulative args so far, not diffs,
                                            // so the latest state replaces the buffer.
                                            accumulated_json =
                                                serde_json::to_string(args).unwrap_or_default();

                                            let instruction_prefix = r#""instruction":""#;
                                            let instruction_prefix_spaced = r#""instruction": ""#;

                                            if !in_instruction
                                                && (accumulated_json.contains(instruction_prefix)
                                                    || accumulated_json
                                                        .contains(instruction_prefix_spaced))
                                            {
                                                in_instruction = true;
                                            }

                                            if in_instruction {
                                                let (delta, new_len) =
                                                    crate::ai::streaming::instruction_delta(
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
                                } else if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    raw_text.push_str(text);
                                    // Prevent streaming raw text directly as it might be pseudo-JSON
                                }
                            }
                        }
                    }
                }
            }

            if let Some(r) = round {
                total.input += r.input;
                total.cached_input += r.cached_input;
                total.output += r.output;
                total.reasoning += r.reasoning;
            }

            if accumulated_json.trim().is_empty() && zoom_open {
                if let (Some(args), Some(f)) = (inspect_args, frame.as_ref()) {
                    zooms += 1;
                    let (answer, look) = crate::ai::inspect::answer(&inspect_name, f.clone(), args).await;
                    if let Some(l) = look {
                        log::info!("[inspect] gemini {} {zooms}: {:?}", l.tool, l.region);
                        self.last_inspects.lock().push(l);
                    }
                    let reply = match answer {
                        crate::ai::inspect::Reply::Image(b64) => json!({"functionResponse": {
                            "name": inspect_name,
                            "response": {"result": "the region at full resolution"},
                            "parts": [{"inlineData": {"mimeType": "image/jpeg", "data": b64}}]
                        }}),
                        crate::ai::inspect::Reply::Text(text) => json!({"functionResponse": {
                            "name": inspect_name,
                            "response": {"result": text}
                        }}),
                        crate::ai::inspect::Reply::Refused(why) => json!({"functionResponse": {
                            "name": inspect_name,
                            "response": {"error": why}
                        }}),
                    };
                    // The 3.8 migration guide asks every FunctionResponse to carry the call's id
                    // as well as its name; 3.8 accepted it without, so it is echoed when present.
                    let mut reply = reply;
                    if let Some(id) = inspect_id {
                        reply["functionResponse"]["id"] = id;
                    }
                    contents.push(json!({"role": "model", "parts": model_parts}));
                    contents.push(json!({"role": "user", "parts": [reply]}));
                    continue;
                }
            }
            break (accumulated_json, raw_text);
        };
        let (input_tokens, output_tokens) = (total.input, total.output);
        *self.last_usage.lock() = Some(total);

        if accumulated_json.trim().is_empty() {
            if !raw_text.is_empty() {
                let mut clean_instruction = raw_text.trim().to_string();

                // Extract clean instruction if Gemini outputted pseudo-JSON
                if let Some(start_idx) = clean_instruction.find(r#"instruction: ""#) {
                    let after = &clean_instruction[start_idx + 14..];
                    if let Some(end_idx) = after.find('"') {
                        clean_instruction = after[..end_idx].to_string();
                    }
                } else if let Some(start_idx) = clean_instruction.find(r#""instruction": ""#) {
                    let after = &clean_instruction[start_idx + 16..];
                    if let Some(end_idx) = after.find('"') {
                        clean_instruction = after[..end_idx].to_string();
                    }
                }

                let mut checkpoint = true;
                if raw_text.contains("checkpoint: false")
                    || raw_text.contains("\"checkpoint\": false")
                {
                    checkpoint = false;
                }

                let mut needs_input = false;
                if raw_text.contains("needs_input: true")
                    || raw_text.contains("\"needs_input\": true")
                {
                    needs_input = true;
                }

                let mut target_role = None;
                if let Some(idx) = raw_text
                    .find(r#"target_role: ""#)
                    .or_else(|| raw_text.find(r#""target_role": ""#))
                {
                    let offset = if raw_text[idx..].starts_with("\"target_role") {
                        16
                    } else {
                        14
                    };
                    let after = &raw_text[idx + offset..];
                    if let Some(end) = after.find('"') {
                        let role_str = &after[..end];
                        target_role = serde_json::from_str(&format!("\"{}\"", role_str)).ok();
                    }
                }

                let mut state_summary = "Continuing task...".to_string();
                if let Some(idx) = raw_text
                    .find(r#"state_summary: ""#)
                    .or_else(|| raw_text.find(r#""state_summary": ""#))
                {
                    let offset = if raw_text[idx..].starts_with("\"state_summary") {
                        18
                    } else {
                        16
                    };
                    let after = &raw_text[idx + offset..];
                    if let Some(end) = after.find('"') {
                        state_summary = after[..end].to_string();
                    }
                }

                let fallback = NavigateStepResponse {
                    goal: String::new(),
                    plan_outline: Vec::new(),
                    plan_completed_count: None,
                    steps: vec![crate::ai::types::GuidanceStep {
                        instruction: clean_instruction,
                        target_text: None,
                        target_role,
                        target_region: None,
                        target_nearby_text: None,
                        clipboard: None,
                        checkpoint,
                        target_bbox: None,
                        target_element_id: None,
                        expected_result: None,
                    }],
                    state_summary,
                    needs_input,
                    suggested_tasks: Vec::new(),
                };

                // Emit the cleaned instruction to the UI instantly
                on_chunk(&fallback.steps[0].instruction, 1);

                return Ok((fallback, input_tokens, output_tokens));
            } else {
                bail!("Gemini returned an empty response (possible safety filter or API error).");
            }
        }

        let step_response: NavigateStepResponse = serde_json::from_str(&accumulated_json)
            .map_err(|e| anyhow!("Failed to parse Gemini tool JSON: {e}\n{accumulated_json}"))?;

        Ok((step_response, input_tokens, output_tokens))
    }
}

/// The streaming endpoint (SSE). **The key is not in it**: it travels in the `x-goog-api-key`
/// header, which Google documents alongside the `?key=` form. reqwest prints the request URL
/// in every transport error -- "error sending request for url (...)" -- so a key in the query
/// string was one network blip from the console, the log, and anything an error message
/// reaches. Seen 2026-10-07, when a replay-harness run printed a BYOK key in full.
fn stream_url(model: &str) -> String {
    format!("{GEMINI_API_BASE}/{model}:streamGenerateContent?alt=sse")
}

/// One part of a streamed model turn, kept for echoing back on an `inspect` round. A
/// functionCall replaces an earlier one (Gemini may stream cumulative args as several parts), and
/// keeps the earlier part's thoughtSignature if the new one has none.
fn keep_model_part(parts: &mut Vec<Value>, part: &Value) {
    if part.get("functionCall").is_some() {
        if let Some(i) = parts.iter().position(|p| p.get("functionCall").is_some()) {
            let mut merged = part.clone();
            if merged.get("thoughtSignature").is_none() {
                if let Some(sig) = parts[i].get("thoughtSignature").cloned() {
                    merged["thoughtSignature"] = sig;
                }
            }
            parts[i] = merged;
            return;
        }
    }
    parts.push(part.clone());
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

/// One log line per CHANGE of the reasoning setting actually put on the wire.
///
/// Rule 1: when a fix cannot be observed from outside, ship the log line with it.
/// `REASONING_EFFORT` reached the request through three hops (env -> Config ->
/// per-provider clamp) with nothing printing what survived, so a value that failed to
/// load looked exactly like a value that made no difference.
fn log_thinking_once(what: &str) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if last.as_deref() != Some(what) {
            log::info!("[reasoning] gemini {what}");
            *last = Some(what.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_api_key_never_rides_in_the_url() {
        let url = super::stream_url("gemini-3.7-flash");
        assert!(url.ends_with("/gemini-3.7-flash:streamGenerateContent?alt=sse"), "{url}");
        assert!(!url.contains("key"), "{url}");
    }
}
