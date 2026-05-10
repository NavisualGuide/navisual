use serde::{Deserialize, Serialize};
use chrono::Local;
use std::path::PathBuf;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub date: String,
    pub daily_input: u64,
    pub daily_output: u64,
    pub monthly_input: u64,
    pub monthly_output: u64,
    pub month: String,
    pub last_updated: String,
}

impl Default for TokenUsage {
    fn default() -> Self {
        let now = Local::now();
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            daily_input: 0,
            daily_output: 0,
            monthly_input: 0,
            monthly_output: 0,
            month: now.format("%Y-%m").to_string(),
            last_updated: now.to_rfc3339(),
        }
    }
}

pub struct CostTracker {
    pub daily_cap: u64,
    pub monthly_cap: u64,
    pub safety_margin: f64,
    storage_path: Option<PathBuf>,
    usage: TokenUsage,
}

impl CostTracker {
    pub fn new(daily_cap: u64, monthly_cap: u64, safety_margin: f64, storage_path: Option<PathBuf>) -> Self {
        let mut tracker = Self {
            daily_cap,
            monthly_cap,
            safety_margin,
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
                        let now = Local::now();
                        let today = now.format("%Y-%m-%d").to_string();
                        if self.usage.date != today {
                            self.usage.daily_input = 0;
                            self.usage.daily_output = 0;
                            self.usage.date = today;
                        }
                        let current_month = now.format("%Y-%m").to_string();
                        if self.usage.month != current_month {
                            self.usage.monthly_input = 0;
                            self.usage.monthly_output = 0;
                            self.usage.month = current_month;
                        }
                    }
                }
            }
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

    pub fn daily_total(&self) -> u64 {
        self.usage.daily_input + self.usage.daily_output
    }

    pub fn monthly_total(&self) -> u64 {
        self.usage.monthly_input + self.usage.monthly_output
    }

    pub fn can_spend(&self, estimated_tokens: u64) -> bool {
        let adjusted = (estimated_tokens as f64 * self.safety_margin) as u64;
        let within_daily = self.daily_total() + adjusted <= self.daily_cap;
        let within_monthly = self.monthly_total() + adjusted <= self.monthly_cap;
        within_daily && within_monthly
    }

    pub fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.usage.daily_input += input_tokens;
        self.usage.daily_output += output_tokens;
        self.usage.monthly_input += input_tokens;
        self.usage.monthly_output += output_tokens;
        self.usage.last_updated = Local::now().to_rfc3339();
        self.save();
    }
}
