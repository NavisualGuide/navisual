use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Token totals for one (provider, model). Daily + monthly, each input/output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub daily_in: u64,
    pub daily_out: u64,
    pub monthly_in: u64,
    pub monthly_out: u64,
}

/// Persisted usage, bucketed per `"provider|model"`. The old single-aggregate
/// schema (daily_input/…) is silently discarded on load — serde ignores the
/// unknown fields and `models` defaults to empty, so we just start fresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub date: String,  // YYYY-MM-DD — daily reset boundary
    pub month: String, // YYYY-MM    — monthly reset boundary
    #[serde(default)]
    pub models: BTreeMap<String, ModelUsage>, // key: "provider|model"
    pub last_updated: String,
}

impl Default for TokenUsage {
    fn default() -> Self {
        let now = Local::now();
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            month: now.format("%Y-%m").to_string(),
            models: BTreeMap::new(),
            last_updated: now.to_rfc3339(),
        }
    }
}

pub struct CostTracker {
    storage_path: Option<PathBuf>,
    usage: TokenUsage,
}

impl CostTracker {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut tracker = Self {
            storage_path,
            usage: TokenUsage::default(),
        };
        tracker.load();
        tracker
    }

    fn load(&mut self) {
        if let Some(path) = &self.storage_path {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(path) {
                    if let Ok(data) = serde_json::from_str::<TokenUsage>(&content) {
                        self.usage = data;
                    }
                }
            }
        }
        self.roll_over_if_needed();
    }

    /// Zero the daily (and monthly) buckets when the calendar day/month has rolled
    /// over since the last write — so a long-running session resets correctly, not
    /// only at startup.
    fn roll_over_if_needed(&mut self) {
        let now = Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let this_month = now.format("%Y-%m").to_string();
        if self.usage.date != today {
            for m in self.usage.models.values_mut() {
                m.daily_in = 0;
                m.daily_out = 0;
            }
            self.usage.date = today;
        }
        if self.usage.month != this_month {
            for m in self.usage.models.values_mut() {
                m.monthly_in = 0;
                m.monthly_out = 0;
            }
            self.usage.month = this_month;
        }
    }

    fn save(&self) {
        if let Some(path) = &self.storage_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&self.usage) {
                let _ = fs::write(path, json);
            }
        }
    }

    pub fn record_usage(&mut self, provider: &str, model: &str, input_tokens: u64, output_tokens: u64) {
        self.roll_over_if_needed();
        let entry = self.usage.models.entry(format!("{provider}|{model}")).or_default();
        entry.daily_in += input_tokens;
        entry.daily_out += output_tokens;
        entry.monthly_in += input_tokens;
        entry.monthly_out += output_tokens;
        self.usage.last_updated = Local::now().to_rfc3339();
        self.save();
    }

    /// Per-(provider, model) usage snapshot for the Settings → Usage panel.
    /// Rolls daily/monthly buckets over first so the displayed totals are current.
    /// Each tuple is (`"provider|model"`, totals).
    pub fn breakdown(&mut self) -> Vec<(String, ModelUsage)> {
        self.roll_over_if_needed();
        self.usage
            .models
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Clear all recorded usage (Settings → Usage → Reset).
    pub fn reset(&mut self) {
        self.usage = TokenUsage::default();
        self.save();
    }
}
