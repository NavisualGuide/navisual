use anyhow::{anyhow, bail, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupabaseSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64, // Unix timestamp (seconds)
}

impl SupabaseSession {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        now >= self.expires_at - 60 // 60s buffer
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BalanceResponse {
    pub tier: String,
    pub free_remaining: u32,
    pub coin_balance_microdollars: u64,
}

pub async fn sign_in_anonymously(supabase_url: &str, anon_key: &str) -> Result<SupabaseSession> {
    let client = Client::new();
    let url = format!("{}/auth/v1/signup", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .body(r#"{"data":{}}"#)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Supabase sign-in failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    parse_session(&body)
}

pub async fn refresh_session(
    supabase_url: &str,
    anon_key: &str,
    refresh_token: &str,
) -> Result<SupabaseSession> {
    let client = Client::new();
    let url = format!("{}/auth/v1/token?grant_type=refresh_token", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({"refresh_token": refresh_token}))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Session refresh failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    parse_session(&body)
}

fn parse_session(body: &serde_json::Value) -> Result<SupabaseSession> {
    let access_token = body["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("no access_token in response"))?
        .to_string();
    let refresh_token = body["refresh_token"]
        .as_str()
        .ok_or_else(|| anyhow!("no refresh_token in response"))?
        .to_string();
    let expires_in = body["expires_in"].as_i64().unwrap_or(3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(SupabaseSession {
        access_token,
        refresh_token,
        expires_at: now + expires_in,
    })
}

pub async fn get_balance(supabase_url: &str, access_token: &str) -> Result<BalanceResponse> {
    let client = Client::new();
    let url = format!("{}/functions/v1/relay", supabase_url);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "get_balance failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(resp.json().await?)
}

/// Insert one feedback row into the Supabase `feedback` table via PostgREST.
/// Uses the user's JWT when signed in (so `auth.uid()` fills `user_id`), and
/// falls back to the anon key (anon role) for BYOK / offline users.
pub async fn submit_feedback(
    supabase_url: &str,
    anon_key: &str,
    access_token: Option<&str>,
    row: &serde_json::Value,
) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/rest/v1/feedback", supabase_url);
    let bearer = access_token.unwrap_or(anon_key);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", bearer))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=minimal")
        .json(row)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "submit_feedback failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

pub fn load_session(path: &Path) -> Option<SupabaseSession> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_session(path: &Path, session: &SupabaseSession) {
    if let Ok(json) = serde_json::to_string_pretty(session) {
        let _ = std::fs::write(path, json);
    }
}
