//! Per-model list pricing for the Settings → Usage **cost estimate**.
//!
//! USD per 1M tokens (input, output), from each provider's published list pricing
//! (see `navisual-internal/docs/model-comparison.csv`, May–Jun 2026). These are
//! ESTIMATES ONLY — they go stale when providers change pricing, so the UI shows a
//! "provider-set, subject to change" disclosure. Update this table as prices move.
//!
//! **Two things the flat (input, output) pair cannot express, handled upstream:**
//!
//! * **Cache tiers.** Anthropic bills input at three rates (uncached 1×, cache write
//!   1.25×, cache read 0.1×). `anthropic.rs` folds that into a *billing-equivalent*
//!   input count before it reaches here, so the flat rate lands correctly. No other
//!   provider currently exposes a cache split — and on the default path (Gemini via the
//!   relay) caching was measured as **not happening at all** (2026-08-13: no
//!   `cachedContentTokenCount` in the response), so input is paid at full rate.
//! * **Reasoning tokens.** Providers bill hidden reasoning as OUTPUT. `gemini.rs` adds
//!   `thoughtsTokenCount` to the output count — it was ~447 against 156 visible tokens on
//!   a measured request, so ignoring it under-reported output roughly 4×.

/// (input_per_1m, output_per_1m) USD for a known model, else None.
fn price_for(model: &str) -> Option<(f64, f64)> {
    let p = match model {
        // Anthropic
        m if m.starts_with("claude-haiku") => (1.0, 5.0),
        // Sonnet 5 (2026-08-16 refresh) is a real price CUT from Sonnet 4.6's 3.0/15.0 —
        // $2/$10 became Anthropic's PERMANENT rate 2026-08-10 (was introductory pricing
        // through 2026-08-31, a scheduled increase to 3.0/15.0 was cancelled). Kept as an
        // exact match, not a `starts_with`, because the OLD 4.6 rate must not silently apply
        // to a differently-priced new generation — see model-comparison.md's 2026-08-16
        // changelog for the source.
        "claude-sonnet-5" => (2.0, 10.0),
        "claude-sonnet-4-6" => (3.0, 15.0), // superseded; kept for historical logs
        m if m.starts_with("claude-opus") => (5.0, 25.0), // opus-5 same rate as opus-4-7
        // Gemini
        "gemini-2.5-flash-lite" => (0.10, 0.40),
        "gemini-2.5-flash" => (0.30, 2.50),
        // 1.50/9.00 per model-comparison.csv. Was (0.50, 3.00) — a third of list, which
        // under-reported the most likely BYOK Gemini model by 3x (caught 2026-08-13 when a
        // recomputed per-request cost came out LOWER despite measurably more tokens).
        // Cached input would be 0.15, but the default path is measurably not caching.
        "gemini-3.5-flash" | "gemini-3-flash-preview" => (1.50, 9.00),
        // ⚠️ PROMOTIONAL PRICING, ends 2026-12-31 — reverts to (1.50, 7.50) on 2027-01-01.
        // Half 3.5-flash's input and ~42% of its output while it lasts. Cached input 0.075
        // (0.15 after), though our ~2,924-token prefix is under Gemini's 4,096 cache minimum
        // so no discount applies today. Revisit this row before January 2027.
        "gemini-3.7-flash" => (0.75, 3.75),
        m if m.starts_with("gemini-3.1-pro") || m.starts_with("gemini-3-pro") => (2.0, 12.0),
        // OpenAI — 5.6 family (2026-08-16 refresh) replaces 5.4/5.5, ~86% cheaper at the
        // fast tier (Luna vs old 5.4-mini's 0.75/4.50). Old rows kept for historical logs.
        "gpt-5.6-luna" => (0.10, 0.60),
        "gpt-5.6-terra" => (2.50, 15.0),
        "gpt-5.6-sol" => (5.0, 30.0),
        "gpt-5.4-mini" => (0.75, 4.50), // superseded by gpt-5.6-luna
        "gpt-5.4" => (2.50, 15.0),      // superseded by gpt-5.6-terra
        "gpt-5.5" => (5.0, 30.0),       // superseded by gpt-5.6-sol
        // DeepSeek (text-only)
        "deepseek-v4-flash" => (0.14, 0.28),
        "deepseek-v4-pro" => (0.435, 0.87),
        // Qwen — 3.8/3.7 (2026-08-16 refresh) supersede 3.6; 3.8-max is the first -max
        // generation that's natively vision-capable, 3.7-flash is the cheapest vision pick
        // in the whole table.
        "qwen3.8-max" => (2.00, 6.00),
        "qwen3.7-flash" => (0.03, 0.13),
        "qwen3.6-plus" => (0.16, 2.87),  // superseded by qwen3.8-max
        "qwen3.6-flash" => (0.10, 1.00), // superseded by qwen3.7-flash
        _ => return None,
    };
    Some(p)
}

/// Estimated USD cost for the given token counts. Semantics:
/// - `ollama` (local) → `Some(0.0)` (free)
/// - `managed` → `None` (not token-priced for the user — the requests/coins balance
///   covers it; shown separately in the UI)
/// - a BYOK model in the table → `Some(cost)`
/// - an unknown model → `None` (UI shows tokens only, cost "—")
pub fn estimate_cost(provider: &str, model: &str, in_tokens: u64, out_tokens: u64) -> Option<f64> {
    match provider {
        "ollama" => Some(0.0),
        "managed" => None,
        _ => {
            let (pin, pout) = price_for(model)?;
            Some(in_tokens as f64 / 1_000_000.0 * pin + out_tokens as f64 / 1_000_000.0 * pout)
        }
    }
}
