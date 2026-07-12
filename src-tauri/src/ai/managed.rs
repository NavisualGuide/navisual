use anyhow::{anyhow, bail, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{GuidanceStep, Message, NavigateStepResponse, OverlayType, Role};
use crate::server::{
    load_session, refresh_session, save_session, sign_in_anonymously, SupabaseSession,
};

pub struct ManagedClient {
    client: Client,
    pub supabase_url: String,
    pub anon_key: String,
    pub model: String,
    pub tier: String, // "speed" | "regular" | "smart" — sent to the relay on paid requests
    pub session: Option<SupabaseSession>,
    session_path: Option<PathBuf>,
    free_remaining: AtomicI64, // -1 = unknown
    coin_balance_micro: AtomicI64, // -1 = unknown; µ$ after the last paid request
    // Which billing tier the relay serves this session: -1 = unknown, 0 = free, 1 = paid.
    // Learned from the balance GET (`tier`) and from each relay response's headers
    // (X-Free-Remaining ⇒ free, X-Coin-Balance ⇒ paid). No reader left as of
    // 2026-07-11 (used to drive the now-removed is_free_tier(), which gated
    // Structured-Context off for the free tier — see router::structured_context_enabled
    // for why that gate was lifted) — kept written in case a future feature needs it.
    billing_paid: AtomicI64,
    // The model OpenRouter actually routed to on the last request (the relay sends
    // `openrouter/free`, a router; the response `model` names the concrete model used).
    last_model: parking_lot::Mutex<Option<String>>,
    // Set only when the LAST request billed real coins despite a "Free" quality-tier
    // preference (X-Tier-Auto-Selected / X-Tier-Auto-Selected-Price headers — see
    // relay/index.ts's handlePaid, wasFreePreference). (tier name, price in µ$). None
    // on every other request, including a "Free"-preference request that was still
    // actually free — this exists specifically to surface the one moment billing
    // silently kicks in, not to track billing state generally (coin_balance_micro
    // already does that).
    tier_auto_selected: parking_lot::Mutex<Option<(String, i64)>>,
}

impl ManagedClient {
    pub fn new(
        supabase_url: String,
        anon_key: String,
        model: String,
        tier: String,
        session_path: Option<PathBuf>,
        timeout_sec: u64,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .build()?;

        let session = session_path.as_deref().and_then(load_session);

        Ok(Self {
            client,
            supabase_url,
            anon_key,
            model,
            tier,
            session,
            session_path,
            free_remaining: AtomicI64::new(-1),
            coin_balance_micro: AtomicI64::new(-1),
            billing_paid: AtomicI64::new(-1),
            last_model: parking_lot::Mutex::new(None),
            tier_auto_selected: parking_lot::Mutex::new(None),
        })
    }

    /// (tier name, price in µ$) if the request that just completed billed real coins
    /// despite a "Free" quality-tier preference — see the field's doc comment. Cleared
    /// (returns None) on every other request, so lib.rs can check this once per call
    /// without separately tracking "did this change since last time."
    pub fn take_tier_auto_selected(&self) -> Option<(String, i64)> {
        self.tier_auto_selected.lock().take()
    }

    pub fn free_remaining(&self) -> Option<u32> {
        let v = self.free_remaining.load(Ordering::Relaxed);
        if v < 0 {
            None
        } else {
            Some(v as u32)
        }
    }

    /// µ$ coin balance reported by the relay on the last paid request (None if
    /// no paid request has run this session).
    pub fn coin_balance_micro(&self) -> Option<i64> {
        let v = self.coin_balance_micro.load(Ordering::Relaxed);
        if v < 0 {
            None
        } else {
            Some(v)
        }
    }

    /// Record the billing tier the relay reported ("paid" ⇒ paid, anything else ⇒ free).
    /// Called from the balance GET and inferred from each relay response's headers.
    pub fn set_billing_tier(&self, tier: &str) {
        self.billing_paid
            .store(if tier == "paid" { 1 } else { 0 }, Ordering::Relaxed);
    }

    /// The concrete model OpenRouter routed to on the most recent request.
    pub fn last_routed_model(&self) -> Option<String> {
        self.last_model.lock().clone()
    }

    pub async fn ensure_token(&mut self) -> Result<()> {
        match &self.session {
            Some(s) if !s.is_expired() => return Ok(()),
            Some(s) => {
                let refresh_token = s.refresh_token.clone();
                match refresh_session(&self.supabase_url, &self.anon_key, &refresh_token).await {
                    Ok(new_session) => {
                        if let Some(ref path) = self.session_path {
                            save_session(path, &new_session);
                        }
                        self.session = Some(new_session);
                        return Ok(());
                    }
                    Err(e) => {
                        log::warn!("Session refresh failed ({e}), signing in fresh");
                    }
                }
            }
            None => {}
        }
        let new_session = sign_in_anonymously(&self.supabase_url, &self.anon_key).await?;
        if let Some(ref path) = self.session_path {
            save_session(path, &new_session);
        }
        self.session = Some(new_session);
        Ok(())
    }

    pub async fn send_message(
        &self,
        messages: Vec<Value>,
        on_chunk: &mut impl FnMut(&str),
    ) -> Result<(NavigateStepResponse, u64, u64)> {
        let access_token = self
            .session
            .as_ref()
            .map(|s| s.access_token.clone())
            .ok_or_else(|| anyhow!("no active Supabase session"))?;

        let tool = navigate_step_tool();
        let payload = json!({
            "model": self.model,
            // Paid-tier hint. The relay ignores it for free users (tier='free' in
            // their profile) and uses it to pick the model + price for paid users.
            "tier": self.tier,
            "max_tokens": 1024,
            "messages": messages,
            "tools": [tool],
            "tool_choice": {"type": "function", "function": {"name": "navigate_step"}},
        });

        let relay_url = format!("{}/functions/v1/relay", self.supabase_url);
        let mut req = self
            .client
            .post(&relay_url)
            .header("Authorization", format!("Bearer {}", access_token));
        // Per-device free-quota key (see server::device_hash). The relay enforces
        // the 50-request free cap on this so re-anonymizing can't reset to a fresh
        // 50; absent (old client / unreadable) → relay falls back to per-user.
        if let Some(dh) = crate::server::device_hash() {
            req = req.header("X-Device-Hash", dh);
        }
        let resp = req.json(&payload).send().await.map_err(|e| {
            log::warn!("[managed] relay request failed to send: {e}");
            e
        })?;

        let status = resp.status();

        // The relay returns 402 for two distinct reasons — free requests used up
        // (free_trial_exhausted) or a paid tier selected without enough coins
        // (insufficient_coins, e.g. the "Free" quality-tier preference degraded
        // to a paid tier once free ran out, or the user picked Speed/Regular/
        // Smart directly). Treating every 402 as free_trial_exhausted (as this
        // used to) showed a genuine paying customer low on coins the "free
        // trial used" message, which is simply wrong for them — read the body's
        // `error` field to tell the two apart instead of trusting the status
        // code alone.
        if status.as_u16() == 402 {
            let body_text = resp.text().await.unwrap_or_default();
            let is_insufficient_coins = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .is_some_and(|e| e == "insufficient_coins");
            if is_insufficient_coins {
                bail!("insufficient_coins");
            }
            bail!("free_trial_exhausted");
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("relay error ({}): {}", status, text);
        }

        // Capture balance headers before consuming the response body.
        // Free tier → X-Free-Remaining; paid tier → X-Coin-Balance (µ$).
        let remaining = resp
            .headers()
            .get("x-free-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        if let Some(r) = remaining {
            self.free_remaining.store(r as i64, Ordering::Relaxed);
        }

        let coin_balance = resp
            .headers()
            .get("x-coin-balance")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());

        if let Some(c) = coin_balance {
            self.coin_balance_micro.store(c, Ordering::Relaxed);
        }

        // The two balance headers are mutually exclusive per relay branch: handleFree
        // sends X-Free-Remaining, handlePaid sends X-Coin-Balance. Use whichever appeared
        // to keep the billing tier fresh per request. No current reader (see the field's
        // doc comment) — kept written for a future feature.
        if remaining.is_some() {
            self.billing_paid.store(0, Ordering::Relaxed);
        } else if coin_balance.is_some() {
            self.billing_paid.store(1, Ordering::Relaxed);
        }

        // Only present when this request billed real coins despite a "Free"
        // preference (see the tier_auto_selected field doc comment). Always
        // overwritten (not just set-if-present) so a request that DIDN'T trigger
        // this correctly clears out a stale value from an earlier one, rather than
        // take_tier_auto_selected() picking up something that already happened.
        let auto_tier = resp
            .headers()
            .get("x-tier-auto-selected")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let auto_tier_price = resp
            .headers()
            .get("x-tier-auto-selected-price")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        *self.tier_auto_selected.lock() = match (auto_tier, auto_tier_price) {
            (Some(t), Some(p)) => Some((t, p)),
            _ => None,
        };

        let body: Value = resp.json().await?;

        // The relay always picks the real model server-side (Gemini/Qwen for free,
        // tier-based for paid) and echoes it back in the response body — that's the
        // only meaningful signal here. `self.model` (sent as `payload.model`) is
        // NOT what actually gets requested from any upstream: the relay overwrites
        // it unconditionally on every path, so logging it as "requested=" would
        // just be restating whatever's in `managed_model` config, not what
        // happened. Record only what's real, for the UI/feedback/debug drawer.
        let routed = body["model"].as_str().map(str::to_string);
        log::info!("[managed] routed={routed:?}");
        *self.last_model.lock() = routed;

        // Token usage. The relay forwards the upstream body verbatim, and both
        // OpenRouter (free) and Gemini/OpenAI (paid) include an OpenAI-style `usage`
        // object — read it so the debug meta shows real counts instead of a
        // misleading "0 in · 0 out". (Managed rows are still filtered out of the
        // BYOK token table by provider name, so this never affects billing.)
        let in_tokens = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let out_tokens = body["usage"]["completion_tokens"].as_u64().unwrap_or(0);

        let message = &body["choices"][0]["message"];
        let nav_response: NavigateStepResponse =
            match message["tool_calls"][0]["function"]["arguments"].as_str() {
                // Free models occasionally emit malformed tool args (leaked </think>,
                // whitespace runaway → truncated JSON). Surface a friendly retry, keep
                // the detail in the log.
                Some(json_str) => serde_json::from_str(json_str).map_err(|e| {
                    log::warn!("[managed] navigate_step parse error: {e}\njson: {json_str}");
                    anyhow!("The free model returned an unreadable response. Please try again.")
                })?,
                // No tool call at all. Two distinct causes share this branch: (a) weak free
                // models answering a greeting/general question ("hi", "what can you do?") as
                // plain text, and (b) a capable model (observed 2026-07-04: gemini-3.5-flash on
                // the paid relay path) answering with a fully-formed navigate_step JSON blob —
                // just never through the tool-call channel, because the relay forwards the
                // client's OpenAI-style forced tool_choice unchanged to Gemini's OpenAI-compat
                // endpoint, which doesn't reliably honour that specific "force this named
                // function" shape (`navisual-internal/supabase/functions/relay/index.ts`
                // `callProvider` — only `max_tokens` is translated per-provider, `tool_choice`
                // is not). Try to recover (b) before falling back to (a)'s raw-text treatment.
                None => {
                    let content = message["content"].as_str().unwrap_or("").trim();
                    if content.is_empty() {
                        log::warn!(
                            "[managed] no tool_calls and no content: {}",
                            serde_json::to_string(&body).unwrap_or_default()
                        );
                        return Err(anyhow!(
                            "The free model returned an unreadable response. Please try again."
                        ));
                    }
                    if let Some(recovered) = try_recover_leaked_json(content) {
                        log::info!(
                            "[managed] no tool_call, but content parsed as valid navigate_step JSON ({} step(s)) — recovered",
                            recovered.steps.len()
                        );
                        recovered
                    } else {
                        log::info!("[managed] no tool_call; surfacing plain message as a reply");
                        NavigateStepResponse {
                            steps: vec![GuidanceStep {
                                instruction: content.to_string(),
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
                            state_summary: String::new(),
                            needs_input: true,
                            request_full_screen: false,
                            suggested_tasks: Vec::new(),
                        }
                    }
                }
            };

        // Emit the first instruction as a single chunk (managed tier is non-streaming).
        if let Some(step) = nav_response.steps.first() {
            on_chunk(&step.instruction);
        }

        Ok((nav_response, in_tokens, out_tokens))
    }
}

/// Recover a `navigate_step` response when the model answered with well-formed JSON as plain
/// assistant text instead of using the tool-call channel (see the `None =>` arm above for why
/// this happens). Strips a markdown code fence if the model wrapped it (observed: ` ```json …
/// ``` `), then attempts a strict parse. Rejects a degenerate parse (empty steps / blank
/// instruction) as not a real recovery. `None` on any failure — the caller falls through to
/// treating `content` as a plain conversational reply; this is a strict improvement, never a
/// new failure mode.
fn try_recover_leaked_json(content: &str) -> Option<NavigateStepResponse> {
    let trimmed = content.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let unfenced = unfenced
        .strip_suffix("```")
        .map(str::trim_end)
        .unwrap_or(unfenced);
    let parsed: NavigateStepResponse = serde_json::from_str(unfenced).ok()?;
    let has_real_instruction = parsed
        .steps
        .first()
        .is_some_and(|s| !s.instruction.trim().is_empty());
    has_real_instruction.then_some(parsed)
}

pub fn build_messages(
    user_text: &str,
    screenshot_b64: Option<&str>,
    state_summary: Option<&str>,
    conversation_history: &[Message],
) -> Vec<Value> {
    let mut messages = Vec::new();

    // System prompt as a system role message (OpenAI format).
    messages.push(json!({"role": "system", "content": SYSTEM_PROMPT}));

    for turn in conversation_history {
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "user",
        };
        messages.push(json!({"role": role, "content": turn.content}));
    }

    let mut content: Vec<Value> = Vec::new();

    if let Some(summary) = state_summary {
        content.push(json!({"type": "text", "text": format!("[Context] {}", summary)}));
    }

    if let Some(b64) = screenshot_b64 {
        content.push(json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/jpeg;base64,{}", b64)}
        }));
    }

    content.push(json!({"type": "text", "text": user_text}));

    messages.push(json!({"role": "user", "content": content}));

    messages
}

fn navigate_step_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "navigate_step",
            "description": "Provide navigation instructions for the user. Return one or more steps.",
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
                                    "enum": ["button","tab","link","textbox","menuitem",
                                             "checkbox","radio","combobox","slider",
                                             "image","heading","other"]
                                },
                                "target_region": {
                                    "type": "string",
                                    "enum": ["top-left","top-center","top-right",
                                             "center-left","center","center-right",
                                             "bottom-left","bottom-center","bottom-right"]
                                },
                                "target_nearby_text": {"type": "string"},
                                "overlay_type": {
                                    "type": "string",
                                    "enum": ["arrow","highlight","circle","none"]
                                },
                                "clipboard": {"type": "string"},
                                "checkpoint": {"type": "boolean"},
                                "target_bbox": {
                                    "type": "array",
                                    "items": {"type": "number"},
                                    "minItems": 4,
                                    "maxItems": 4,
                                    "description": "Bounding box of the target element as [ymin, xmin, ymax, xmax] in NORMALIZED 0-1000 coordinates (0 = top/left edge, 1000 = bottom/right edge of the image, regardless of pixel size). Omit when no target_text."
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
        }
    })
}

#[cfg(test)]
mod tests {
    use super::try_recover_leaked_json;

    #[test]
    fn recovers_fenced_json() {
        // The live 2026-07-04 case: gemini-3.5-flash answered with a fenced JSON blob
        // instead of a tool call.
        let content = "```json\n{ \"needs_input\": false, \"state_summary\": \"tour\", \"steps\": [ { \"checkpoint\": true, \"instruction\": \"Press Ctrl+B to open the sidebar.\", \"overlay_type\": \"none\" } ] }\n```";
        let recovered = try_recover_leaked_json(content).expect("should recover");
        assert_eq!(recovered.steps.len(), 1);
        assert_eq!(recovered.steps[0].instruction, "Press Ctrl+B to open the sidebar.");
        assert!(!recovered.needs_input);
    }

    #[test]
    fn recovers_unfenced_json() {
        let content = r#"{ "needs_input": false, "state_summary": "x", "steps": [ { "checkpoint": true, "instruction": "Click Save." } ] }"#;
        let recovered = try_recover_leaked_json(content).expect("should recover");
        assert_eq!(recovered.steps[0].instruction, "Click Save.");
    }

    #[test]
    fn rejects_plain_conversational_text() {
        // The original (a)-case this branch was written for: a genuine chit-chat reply
        // must NOT be treated as recoverable JSON — it isn't JSON at all.
        assert!(try_recover_leaked_json("Hi! I can help you navigate this app.").is_none());
    }

    #[test]
    fn rejects_degenerate_parse() {
        // Valid JSON, valid shape, but no real instruction — not a genuine recovery.
        let content = r#"{ "needs_input": true, "state_summary": "", "steps": [] }"#;
        assert!(try_recover_leaked_json(content).is_none());
        let blank_instruction =
            r#"{ "needs_input": false, "state_summary": "", "steps": [ { "checkpoint": true, "instruction": "   " } ] }"#;
        assert!(try_recover_leaked_json(blank_instruction).is_none());
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(try_recover_leaked_json("```json\n{ not valid json\n```").is_none());
    }
}
