//! Per-model list pricing for the Settings → Usage **cost estimate**.
//!
//! USD per 1M tokens (input, output), from each provider's published list pricing
//! (see `navisual-internal/docs/model-comparison.csv`, May–Jun 2026). These are
//! ESTIMATES ONLY — they go stale when providers change pricing, so the UI shows a
//! "provider-set, subject to change" disclosure. Update this table as prices move.

/// (input_per_1m, output_per_1m) USD for a known model, else None.
fn price_for(model: &str) -> Option<(f64, f64)> {
    let p = match model {
        // Anthropic
        m if m.starts_with("claude-haiku") => (1.0, 5.0),
        "claude-sonnet-4-6" => (3.0, 15.0),
        m if m.starts_with("claude-opus") => (5.0, 25.0),
        // Gemini
        "gemini-2.5-flash-lite" => (0.10, 0.40),
        "gemini-2.5-flash" => (0.30, 2.50),
        "gemini-3.5-flash" | "gemini-3-flash-preview" => (0.50, 3.00),
        m if m.starts_with("gemini-3.1-pro") || m.starts_with("gemini-3-pro") => (2.0, 12.0),
        // OpenAI
        "gpt-5.4-mini" => (0.75, 4.50),
        "gpt-5.4" => (2.50, 15.0),
        "gpt-5.5" => (5.0, 30.0),
        // DeepSeek (text-only)
        "deepseek-v4-flash" => (0.14, 0.28),
        "deepseek-v4-pro" => (0.435, 0.87),
        // Qwen
        "qwen3.6-plus" => (0.16, 2.87),
        "qwen3.6-flash" => (0.10, 1.00),
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
