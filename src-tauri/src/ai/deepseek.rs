use anyhow::{bail, Result};
use reqwest::{header, Client};
use serde_json::{json, Value};
use std::time::Duration;

use crate::ai::managed::navigate_step_tool;
use crate::ai::prompts::{add_expected_result, system_prompt, with_expected_result_field};
use crate::ai::types::{GuidanceStep, Message, NavigateStepResponse, Role, UsageDetail};

/// Same schema instruction as Ollama — DeepSeek doesn't support function-calling
/// for vision models, so we use prompt engineering + response_format:json_object.
pub(crate) const JSON_FORMAT_INSTRUCTION: &str = r#"

IMPORTANT: Respond ONLY with a single valid JSON object — no markdown, no explanation. The top-level object has exactly three keys: "steps", "state_summary", "needs_input".

Example (copy this structure exactly):
{
  "steps": [
    {
      "instruction": "Click the Layout tab at the top of the ribbon.",
      "target_text": "Layout",
      "target_role": "tab",
      "checkpoint": true
    }
  ],
  "goal": "Add page numbers starting at 1 on page 3, centred, none on the title or contents pages",
  "state_summary": "GOAL: add page numbers starting at 1 on page 3. CONSTRAINTS: title and contents pages must have none. DONE: opened the Layout tab. FAILED: nothing yet.",
  "needs_input": false
}

Step fields (inside "steps" array only):
- instruction: what the user should do (required)
- target_text: 1-5 words visible on screen (optional)
- target_role: button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other (optional)
- checkpoint: true = wait for user confirmation, false = auto-advance (required)
- clipboard: text to copy to clipboard (optional)
- target_bbox: [ymin, xmin, ymax, xmax] as NORMALIZED 0-1000 coordinates (0 = top/left edge, 1000 = bottom/right edge of the image, regardless of pixel size; NOT pixels) (REQUIRED whenever target_text is set, even if target_element_id is also set — the only time to omit it is a step with no target_text at all)
- target_element_id: integer id from the [Screen Elements] list in the message when your target appears there — only ids from the list, never invented; still fill target_text (optional, omit when the target is not listed or no list is present)

Top-level fields (outside "steps", required):
- goal: the user's overall objective, carried across turns. MERGE later detail into it rather than replacing it ('centre them at the bottom' refines the goal, it is not a new goal); replace it outright only on a genuine task switch; omit to leave it unchanged
- state_summary: your ONLY memory between turns (earlier turns are truncated away). Rewrite it in full each turn carrying the user's GOAL in their own words, any CONSTRAINTS they stated, what is DONE, anything TRIED THAT FAILED, and anything you have SEEN on an earlier screen that the user may still need once it is gone, copied exactly
- needs_input: true only if you must ask the user a question before continuing

Optional top-level fields:
- suggested_tasks: up to 3 short next-task suggestions the user might ask for (each under 80 characters, in the user's language) — ONLY when the current task looks complete or no task is in progress; omit mid-sequence
- keep: facts the user may still need after they leave the screen, each copied exactly as shown; kept notes come back on every later turn and are never rewritten, so add each once and never repeat one already kept (omit when there is nothing new)
- app_note: only when the prompt asks for a learned note, that one line; otherwise omit
- plan_outline: a short route overview toward the goal — like a map app's route overview, not turn-by-turn (that's steps/instruction). 2-8 short plain-language milestones, e.g. ["Open the Insert tab", "Add page numbers", "Set them to start at page 3"]. Shown to the user when they ask to see the plan. REVISE the whole list (replace it, don't append) whenever your understanding of the route changes. Omit to leave the previously shown plan unchanged; omit entirely on a simple one-step task
- plan_completed_count: how many of plan_outline's milestones (counting from the first) are already done — 0 means still on the first one, 1 means the first is done and you're on the second, etc. Update as milestones finish, and again whenever you revise plan_outline (count against the NEW list). Omit to leave the previous count unchanged; meaningless without plan_outline, so omit both together"#;

pub struct DeepSeekClient {
    client: Client,
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    name: String,
    /// Shared `reasoning_effort` word. Only reaches the wire on the OpenAI `/v1/responses`
    /// path -- every other model on this client uses prompted JSON, where there is no
    /// reasoning parameter to set.
    reasoning_effort: Option<String>,
    /// The raw usage split of the last call -- see `UsageDetail`. A side channel rather than
    /// a wider return tuple; `take_last_usage` reads it.
    last_usage: parking_lot::Mutex<Option<UsageDetail>>,
    /// H1 switch, set by the router from `Config::expected_result` (see `prompts::system_prompt`).
    pub(crate) expected_result: bool,
    /// H2: the frame `inspect` crops from (OpenAI via Responses only); `None` = no tool.
    pub(crate) inspect_frame: Option<std::sync::Arc<image::RgbImage>>,
    pub(crate) inspect_mode: crate::ai::inspect::Mode,
    pub(crate) inspect_tools: crate::ai::inspect::Tools,
    /// H6: earlier screens `recall` can return (OpenAI via Responses only).
    pub(crate) recall_screens: Option<std::sync::Arc<Vec<crate::ai::inspect::EarlierScreen>>>,
    pub(crate) recall_max: usize,
    /// H3: the accessibility snapshot `ui_tree` reads, set per request by the router.
    pub(crate) ui_snapshot: Option<std::sync::Arc<crate::ai::inspect::UiSource>>,
    last_inspects: parking_lot::Mutex<Vec<crate::ai::inspect::Look>>,
    /// `response.model` of the last Responses API call: the model that answered.
    last_served: parking_lot::Mutex<Option<String>>,
}

impl DeepSeekClient {
    pub fn new(
        api_key: String,
        model: String,
        timeout_sec: u64,
        base_url: Option<String>,
        name: Option<String>,
        reasoning_effort: Option<String>,
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
            reasoning_effort,
            last_usage: parking_lot::Mutex::new(None),
            expected_result: false,
            inspect_frame: None,
            inspect_mode: crate::ai::inspect::Mode::Off,
            inspect_tools: crate::ai::inspect::Tools::Zoom,
            recall_screens: None,
            recall_max: 1,
            ui_snapshot: None,
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

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        model_override: Option<&str>,
        // (delta, steps_seen) — see streaming::count_streamed_steps.
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let effective_model = model_override.unwrap_or(&self.model);

        // TEST SPIKE (2026-08-19): all OpenAI BYOK models via /v1/responses instead
        // of /v1/chat/completions. Originally scoped to gpt-5.6* only (the one
        // family where /v1/chat/completions outright rejects reasoning + forced
        // function tools together — see the relay's reasoning_effort:'none' fix,
        // same day, model-comparison.md), widened to every OpenAI model per
        // explicit instruction — real tool-calling should be strictly better than
        // the prompt-JSON path for any model that supports it, not just the ones
        // /v1/chat/completions structurally blocks. BYOK only — validate here
        // before touching the relay's harder passthrough-response constraint.
        if self.name == "OpenAI" {
            return self
                .send_message_responses_api(messages, effective_model, on_chunk)
                .await;
        }

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

        // Qwen is hybrid-thinking, and DashScope wants `enable_thinking` stated explicitly on
        // reasoning-capable models. The shared effort word maps onto its boolean: `none` and
        // `minimal` mean off, anything else means on. Unset leaves it absent, which is the
        // provider's own default -- deliberately, because every Qwen number in
        // model-comparison.md was measured with thinking FORCED off by the relay (which must
        // do that: DashScope rejects tool_choice object while thinking is on), so whether
        // Qwen's 0/8 grounding is partly self-inflicted is an open question this makes
        // askable rather than answering by assumption.
        if self.name == "Qwen" {
            if let Some(effort) = self.reasoning_effort.as_deref() {
                payload["enable_thinking"] = json!(!matches!(effort, "none" | "minimal"));
            }
        }

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

    /// TEST SPIKE (2026-08-19) — all OpenAI BYOK models via `/v1/responses`. Real
    /// tool-calling (`tools`+`tool_choice`, flattened per this endpoint's shape —
    /// no nested `function` wrapper, unlike Chat Completions) instead of the
    /// prompted-JSON approach every other BYOK model on this client uses, so the
    /// model gets to reason (`reasoning_effort` when set, else the provider default)
    /// without the function-tools/reasoning_effort conflict Chat Completions
    /// raises for the gpt-5.6 family specifically. Streamed — see
    /// `stream_once_responses_api` for the event-accumulation logic. See
    /// `messages_to_responses_input` for the request-shape conversion.
    async fn send_message_responses_api(
        &self,
        messages: Vec<Value>,
        effective_model: &str,
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let (instructions, input) = messages_to_responses_input(&messages);

        let mut tool = navigate_step_tool();
        if self.expected_result {
            add_expected_result(&mut tool);
        }
        let flat_tool = json!({
            "type": "function",
            "name": tool["function"]["name"],
            "description": tool["function"]["description"],
            "parameters": tool["function"]["parameters"],
        });

        let mut payload = json!({
            "model": effective_model,
            "instructions": instructions,
            "input": input,
            "tools": [flat_tool.clone()],
            "tool_choice": {"type": "function", "name": "navigate_step"},
            "stream": true,
        });
        // Passed through unclamped: the Responses API accepts all seven words, and this is
        // the endpoint on which reasoning and forced function tools can coexist at all --
        // /v1/chat/completions rejects the combination outright, which is why every OpenAI
        // BYOK model moved here on 2026-08-19. Omitted when unset, leaving OpenAI's own
        // default (`medium` on gpt-5.6).
        if let Some(effort) = self.reasoning_effort.as_deref() {
            payload["reasoning"] = json!({ "effort": effort });
            log_effort_once(&format!("reasoning.effort={effort}"));
        } else {
            log_effort_once("provider default (no reasoning field sent)");
        }

        // H2 (`inspect.rs`): with a frame offered, `tool_choice: required` lets the model zoom
        // first; each zoom continues the same response (`previous_response_id`) with the crop
        // inside the function_call_output. After MAX_INSPECTS the original payload forces
        // navigate_step again. Only the navigate_step round streams a caption.
        // H6 `recall` joins the same loop when earlier screens are offered.
        let sources = crate::ai::inspect::Sources {
            frame: self.inspect_frame.clone(),
            earlier: self.recall_screens.clone(),
            recall_max: self.recall_max,
            ui: self.ui_snapshot.clone(),
        };
        self.last_inspects.lock().clear();
        let look_names = sources.names(self.inspect_tools);
        let mut declared = vec![flat_tool.clone()];
        for (name, description, parameters) in
            crate::ai::inspect::declarations(self.inspect_mode, &look_names, self.recall_max)
        {
            declared.push(json!({"type": "function", "name": name,
                                 "description": description, "parameters": parameters}));
        }
        if !look_names.is_empty() {
            // Says the tools reached the request; an "[inspect]" line follows each one the model calls.
            log::info!("[looks] {} declared: {}", self.name, look_names.join(", "));
        }
        let mut total = UsageDetail::default();
        let mut zooms = 0usize;
        let mut follow_up: Option<(String, Value)> = None;
        loop {
            let zoom_open = !look_names.is_empty() && zooms < crate::ai::inspect::MAX_INSPECTS;
            let mut p = payload.clone();
            if zoom_open {
                p["tools"] = Value::Array(declared.clone());
                // `First`: the opening round must look -- with both tools offered, it is forced
                // to the first (the zoom), since a forced choice names one function. Recall is
                // never forced.
                p["tool_choice"] = if zooms == 0
                    && self.inspect_mode == crate::ai::inspect::Mode::First
                    && sources.frame.is_some()
                {
                    json!({"type": "function", "name": look_names[0]})
                } else {
                    json!("required")
                };
                p["parallel_tool_calls"] = json!(false);
            }
            if let Some((id, input)) = &follow_up {
                p["previous_response_id"] = json!(id);
                p["input"] = input.clone();
            }
            let round = self.stream_round_responses_api(&p, on_chunk).await?;
            total.input += round.usage.input;
            total.cached_input += round.usage.cached_input;
            total.output += round.usage.output;
            total.reasoning += round.usage.reasoning;
            if look_names.contains(&round.name.as_str()) && zoom_open {
                {
                    zooms += 1;
                    let args: Value =
                        serde_json::from_str(&round.arguments).unwrap_or_else(|_| json!({}));
                    let (answer, look) = crate::ai::inspect::answer(&round.name, &sources, args).await;
                    if let Some(l) = look {
                        log::info!("[inspect] {} {} {zooms}: {:?} turns {:?}", self.name, l.tool, l.region, l.turns);
                        self.last_inspects.lock().push(l);
                    }
                    let output = match answer {
                        // A zoom's crop goes back alone, as H2 was measured; a recalled screen
                        // says which turn it is.
                        crate::ai::inspect::Reply::Images(images, caption) if round.name == crate::ai::inspect::RECALL => {
                            let mut parts = vec![json!({"type": "input_text", "text": caption})];
                            parts.extend(images.iter().map(|b64| json!({"type": "input_image",
                                "image_url": format!("data:image/jpeg;base64,{b64}")})));
                            Value::Array(parts)
                        }
                        crate::ai::inspect::Reply::Images(images, _) => Value::Array(images.iter().map(|b64| json!({
                            "type": "input_image", "image_url": format!("data:image/jpeg;base64,{b64}")})).collect()),
                        crate::ai::inspect::Reply::Text(text) => json!(text),
                        crate::ai::inspect::Reply::Refused(why) => json!(why),
                    };
                    follow_up = Some((
                        round.response_id.clone(),
                        json!([{"type": "function_call_output", "call_id": round.call_id, "output": output}]),
                    ));
                    continue;
                }
            }
            *self.last_usage.lock() = Some(total);
            let resp = self.parse_navigate(&round.arguments, round.emitted_instruction_len, on_chunk)?;
            return Ok((resp, total.input, total.output));
        }
    }

    /// SSE parser for one `/v1/responses` round. Genuinely different event vocabulary from
    /// Chat Completions (see `stream_once`) — no `choices[].delta`, instead typed `type`-tagged
    /// events: `response.output_item.added` names the function being called,
    /// `response.function_call_arguments.delta` carries its arguments (accumulated —
    /// `parallel_tool_calls: false` or a forced tool means one function_call item per round),
    /// and `response.completed` carries the response id and `usage`. Live captions reuse the
    /// `instruction_delta` scan `stream_once` uses on Chat Completions' `content`, for
    /// navigate_step only — an `inspect` call has no instruction to show.
    async fn stream_round_responses_api(
        &self,
        payload: &Value,
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<ResponsesRound> {
        let response = self
            .client
            .post("https://api.openai.com/v1/responses")
            .bearer_auth(&self.api_key)
            .json(payload)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            bail!("{} Responses API error ({}): {}", self.name, status, body);
        }

        let mut r = ResponsesRound::default();
        let mut in_instruction = false;
        let mut line_buf = String::new();
        let mut usage_logged = false;

        let mut stream = response.bytes_stream();
        let mut progress = crate::ai::streaming::AnswerProgress::default();
        // What the stream did, for the one line logged if it fails. Two "stalls" on the
        // verification runs (2026-10-12) ran the full 90 s with the answer begun and no stall
        // line, so either the answer kept growing (a runaway) or the wait was elsewhere; this
        // says which.
        let mut diag = StreamDiag::new();
        loop {
            let next = crate::ai::streaming::next_or_stall(&mut stream, progress.update(r.arguments.len()), &self.name).await;
            let chunk_result = match next {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    diag.log_failure(&self.name, &e, r.arguments.len(), &progress);
                    return Err(e);
                }
            };
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    let e = anyhow::Error::from(e);
                    diag.log_failure(&self.name, &e, r.arguments.len(), &progress);
                    return Err(e);
                }
            };
            diag.bytes += chunk.len();
            line_buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(nl) = line_buf.find('\n') {
                let line = line_buf[..nl].trim().to_string();
                line_buf = line_buf[nl + 1..].to_string();
                if line.is_empty() {
                    continue;
                }
                let data_str = match line.strip_prefix("data: ") {
                    Some(s) => s.trim(),
                    None => continue,
                };
                if data_str == "[DONE]" {
                    break;
                }
                let data: Value = match serde_json::from_str(data_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                diag.event(data["type"].as_str().unwrap_or("?"));

                match data["type"].as_str() {
                    Some("response.created") => {
                        if let Some(id) = data["response"]["id"].as_str() {
                            r.response_id = id.to_string();
                        }
                    }
                    Some("response.output_item.added")
                        if data["item"]["type"].as_str() == Some("function_call") =>
                    {
                        r.name = data["item"]["name"].as_str().unwrap_or_default().to_string();
                        r.call_id = data["item"]["call_id"].as_str().unwrap_or_default().to_string();
                    }
                    Some("response.function_call_arguments.delta") => {
                        if let Some(delta) = data["delta"].as_str() {
                            r.arguments.push_str(delta);
                            if crate::ai::inspect::is_look(&r.name) {
                                continue;
                            }

                            let prefix = r#""instruction":""#;
                            let prefix_sp = r#""instruction": ""#;
                            if !in_instruction
                                && (r.arguments.contains(prefix) || r.arguments.contains(prefix_sp))
                            {
                                in_instruction = true;
                            }
                            if in_instruction {
                                let (d, new_len) = crate::ai::streaming::instruction_delta(
                                    &r.arguments,
                                    r.emitted_instruction_len,
                                );
                                if !d.is_empty() {
                                    on_chunk(&d, crate::ai::streaming::count_streamed_steps(&r.arguments));
                                }
                                r.emitted_instruction_len = new_len;
                            }
                        }
                    }
                    Some("error") => {
                        bail!("{} Responses API stream error: {}", self.name, data);
                    }
                    Some("response.completed") => {
                        if let Some(id) = data["response"]["id"].as_str() {
                            r.response_id = id.to_string();
                        }
                        if let Some(m) = data["response"]["model"].as_str() {
                            *self.last_served.lock() = Some(m.to_string());
                        }
                        if let Some(usage) = data["response"]["usage"].as_object() {
                            // Responses API names: cache hits under input_tokens_details,
                            // reasoning under output_tokens_details (both already included in
                            // the totals).
                            let top = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                            let detail = |group: &str, key: &str| {
                                usage
                                    .get(group)
                                    .and_then(|d| d.get(key))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0)
                            };
                            r.usage = UsageDetail {
                                input: top("input_tokens"),
                                cached_input: detail("input_tokens_details", "cached_tokens"),
                                cache_write: 0,
                                output: top("output_tokens"),
                                reasoning: detail("output_tokens_details", "reasoning_tokens"),
                            };
                            if !usage_logged {
                                log::info!("[tokens] responses-api raw usage (once): {}", data["response"]["usage"]);
                                usage_logged = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(r)
    }

    /// The navigate_step answer of the last round, parsed, with the first step shown as the
    /// caption if nothing streamed.
    fn parse_navigate(
        &self,
        arguments: &str,
        emitted_instruction_len: usize,
        on_chunk: &mut impl FnMut(&str, usize),
    ) -> Result<NavigateStepResponse> {
        if arguments.trim().is_empty() {
            bail!("{}: Responses API stream produced no function_call_arguments", self.name);
        }
        let resp: NavigateStepResponse = serde_json::from_str(arguments).map_err(|e| {
            anyhow::anyhow!(
                "{}: function_call arguments didn't match the schema: {} — raw: {}",
                self.name,
                e,
                arguments
            )
        })?;
        if emitted_instruction_len == 0 {
            if let Some(step) = resp.steps.first() {
                on_chunk(&step.instruction, resp.steps.len().max(1));
            }
        }
        Ok(resp)
    }

    /// One streamed request. Returns `Ok(None)` when the model produced no usable
    /// answer (empty `content` and no recoverable JSON in `reasoning_content`), so
    /// the caller can retry. Propagates transport / non-2xx HTTP errors.
    async fn stream_once(
        &self,
        payload: &Value,
        on_chunk: &mut impl FnMut(&str, usize),
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
        let mut progress = crate::ai::streaming::AnswerProgress::default();
        while let Some(chunk_result) =
            crate::ai::streaming::next_or_stall(&mut stream, progress.update(accumulated_text.len()), &self.name).await?
        {
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
                    // Prompt caching on this path is AUTOMATIC — nothing here requests it.
                    // Two field shapes because this one client serves several providers:
                    // OpenAI/Qwen report `prompt_tokens_details.cached_tokens`, DeepSeek reports
                    // `prompt_cache_hit_tokens`. Reported only so we can see whether the
                    // discount is arriving; our prefix (the constant ~2.4k-token SYSTEM_PROMPT,
                    // sent first) already satisfies every provider's minimum, so a persistent
                    // zero means the provider isn't caching, not that we asked wrongly.
                    let cached = usage
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(|v| v.as_u64()))
                        .unwrap_or(0);
                    if cached > 0 {
                        log::info!("[tokens] in={input_tokens} cached={cached}");
                    }
                    let reasoning = usage
                        .get("completion_tokens_details")
                        .and_then(|d| d.get("reasoning_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    *self.last_usage.lock() = Some(UsageDetail {
                        input: input_tokens,
                        cached_input: cached,
                        cache_write: 0,
                        output: output_tokens,
                        reasoning,
                    });
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

        // Primary path — parse the JSON answer out of the content stream.
        if !text.is_empty() {
            if let Some(resp) = parse_first_nav_response(&text) {
                if emitted_instruction_len == 0 {
                    if let Some(step) = resp.steps.first() {
                        on_chunk(&step.instruction, resp.steps.len().max(1));
                    }
                }
                return Ok(Some((resp, input_tokens, output_tokens)));
            }
            // Content present but not parseable into the schema — wrap the raw
            // text as a single instruction so the user still gets guidance.
            let fallback = wrap_as_single_step(&text);
            if emitted_instruction_len == 0 {
                on_chunk(&fallback.steps[0].instruction, 1);
            }
            return Ok(Some((fallback, input_tokens, output_tokens)));
        }

        // Salvage — reasoning models sometimes embed the JSON answer inside
        // `reasoning_content` and never emit a `content` delta. Recover it.
        if !reasoning_text.is_empty() {
            if let Some(start) = reasoning_text.find('{') {
                if let Some(resp) = parse_first_nav_response(&reasoning_text[start..]) {
                    if let Some(step) = resp.steps.first() {
                        on_chunk(&step.instruction, resp.steps.len().max(1));
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

/// Converts the Chat-Completions-shaped `messages` this client normally builds
/// (via `build_openai_messages`) into `/v1/responses`' shape: the leading
/// `role:"system"` message becomes the top-level `instructions` string, and
/// every remaining turn becomes an `input` item with its content items
/// renamed (`text`->`input_text`, `image_url`->`input_image` with the URL
/// flattened out of its nested `{url:...}` wrapper). Plain-string content
/// (text-only turns) is normalized to a one-item array — Responses' examples
/// only ever show array content, not bothering to confirm the string form is
/// also accepted.
///
/// Assistant-role content items use a DIFFERENT type vocabulary than
/// user-role ones (`output_text`/`refusal` vs `input_text`/`input_image`) —
/// live 400 caught 2026-08-19 on the relay's copy of this same function, on
/// the second call of any conversation ("Invalid value: 'input_text'.
/// Supported values are: 'output_text' and 'refusal'.", param
/// input[1].content[0]): every single-turn validation this session used a
/// fresh session with no prior assistant turn, so this never surfaced until
/// real multi-turn history hit it live. Mirror the relay's fix here too —
/// keep the two in sync.
/// Counts kept while a Responses stream runs, logged in one line only if it fails.
struct StreamDiag {
    started: std::time::Instant,
    bytes: usize,
    events: std::collections::BTreeMap<String, u32>,
    last_event: String,
}

impl StreamDiag {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            bytes: 0,
            events: std::collections::BTreeMap::new(),
            last_event: String::new(),
        }
    }

    fn event(&mut self, kind: &str) {
        *self.events.entry(kind.to_string()).or_default() += 1;
        self.last_event = kind.to_string();
    }

    fn log_failure(
        &self,
        provider: &str,
        err: &anyhow::Error,
        answer_chars: usize,
        progress: &crate::ai::streaming::AnswerProgress,
    ) {
        let grew = progress
            .grew_at()
            .map(|t| format!("{} ms ago", t.elapsed().as_millis()))
            .unwrap_or_else(|| "never".to_string());
        log::warn!(
            "[stream] {provider} failed after {} ms: {err}; answer {answer_chars} chars, last grew {grew}; \
             {} bytes; events {:?}; last event {:?}",
            self.started.elapsed().as_millis(),
            self.bytes,
            self.events,
            self.last_event
        );
    }
}

/// One `/v1/responses` round: which function the model called, its arguments, and what to
/// continue from.
#[derive(Default)]
struct ResponsesRound {
    name: String,
    call_id: String,
    response_id: String,
    arguments: String,
    emitted_instruction_len: usize,
    usage: UsageDetail,
}

fn messages_to_responses_input(messages: &[Value]) -> (String, Vec<Value>) {
    let mut instructions = String::new();
    let mut input = Vec::new();

    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        if role == "system" {
            if let Some(s) = msg["content"].as_str() {
                instructions = s.to_string();
            }
            continue;
        }
        let is_assistant = role == "assistant";
        let text_type = if is_assistant { "output_text" } else { "input_text" };

        let content = &msg["content"];
        let items: Vec<Value> = if let Some(s) = content.as_str() {
            vec![json!({"type": text_type, "text": s})]
        } else if let Some(arr) = content.as_array() {
            arr.iter()
                .map(|part| match part["type"].as_str() {
                    Some("text") => json!({
                        "type": text_type,
                        "text": part["text"].as_str().unwrap_or("")
                    }),
                    Some("image_url") if !is_assistant => json!({
                        "type": "input_image",
                        "image_url": part["image_url"]["url"].as_str().unwrap_or("")
                    }),
                    _ => part.clone(),
                })
                .collect()
        } else {
            vec![]
        };

        input.push(json!({ "role": role, "content": items }));
    }

    (instructions, input)
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
            clipboard: None,
            checkpoint: true,
            target_bbox: None,
            target_element_id: None,
            expected_result: None,
        });
        return Some(resp);
    }
    None
}

/// Wrap arbitrary text as a single-step response so non-empty but unparseable
/// model output still surfaces as guidance instead of an error.
fn wrap_as_single_step(text: &str) -> NavigateStepResponse {
    NavigateStepResponse {
        goal: String::new(),
        plan_outline: Vec::new(),
            keep: Vec::new(),
            app_note: String::new(),
        plan_completed_count: None,
        steps: vec![GuidanceStep {
            instruction: text.to_string(),
            target_text: None,
            target_role: None,
            target_region: None,
            target_nearby_text: None,
            clipboard: None,
            checkpoint: true,
            target_bbox: None,
            target_element_id: None,
            expected_result: None,
        }],
        state_summary: "Continuing task...".to_string(),
        needs_input: false,
        suggested_tasks: Vec::new(),
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
    expected_result: bool,
) -> Vec<Value> {
    let mut messages = Vec::new();

    let system_with_format = format!(
        "{}{}{}",
        system_prompt(expected_result),
        with_expected_result_field(JSON_FORMAT_INSTRUCTION, expected_result),
        TEXT_ONLY_NOTICE
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
    expected_result: bool,
) -> Vec<Value> {
    let mut messages = Vec::new();

    let system_with_format = format!(
        "{}{}",
        system_prompt(expected_result),
        with_expected_result_field(JSON_FORMAT_INSTRUCTION, expected_result)
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

/// One log line per CHANGE of the reasoning effort actually put on the wire.
///
/// Rule 1: when a fix cannot be observed from outside, ship the log line with it.
/// Edge-triggered, so a long session cannot flood the log. The Gemini path carries the
/// same helper for the same reason.
fn log_effort_once(what: &str) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if last.as_deref() != Some(what) {
            log::info!("[reasoning] openai {what}");
            *last = Some(what.to_string());
        }
    }
}
