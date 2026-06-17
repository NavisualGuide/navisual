use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

// ── Google OAuth PKCE ────────────────────────────────────────────────────────

pub struct OAuthPkce {
    pub verifier: String,
    pub challenge: String, // base64url(SHA-256(verifier))
    pub redirect_uri: String,
    pub port: u16,
}

pub fn generate_pkce(port: u16) -> OAuthPkce {
    // Use two UUIDs as entropy source (no rand crate needed; uuid v4 is CSPRNG-backed).
    let raw = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let verifier = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hash);
    OAuthPkce {
        verifier,
        challenge,
        redirect_uri: format!("http://localhost:{}/callback", port),
        port,
    }
}

/// Build the Supabase Google OAuth URL for PKCE flow.
pub fn google_oauth_url(supabase_url: &str, pkce: &OAuthPkce) -> String {
    format!(
        "{}/auth/v1/authorize?provider=google\
         &response_type=code\
         &code_challenge={}\
         &code_challenge_method=S256\
         &redirect_to={}",
        supabase_url,
        pct_encode(&pkce.challenge),
        pct_encode(&pkce.redirect_uri),
    )
}

fn pct_encode(s: &str) -> String {
    s.bytes().flat_map(|b| {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            vec![b as char]
        } else {
            format!("%{:02X}", b).chars().collect()
        }
    }).collect()
}

/// Spin up a minimal TCP listener, serve a redirect page, return the OAuth code.
/// Times out after 120 s so we don't block forever if the user closes the browser.
pub async fn wait_for_oauth_code(port: u16) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        // First line: "GET /callback?code=XXX&... HTTP/1.1"
        let first_line = request.lines().next().unwrap_or("");
        let path = first_line.split_whitespace().nth(1).unwrap_or("");
        let query = path.split('?').nth(1).unwrap_or("");
        let code = query.split('&')
            .find(|p| p.starts_with("code="))
            .and_then(|p| p.strip_prefix("code="))
            .map(|s| s.to_string());
        // Send a minimal success page so the browser tab shows something.
        let body = "<html><body style='font-family:sans-serif;padding:40px'>\
            <h2>Signed in — you can close this tab and return to Navisual.</h2></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        stream.write_all(response.as_bytes()).await?;
        code.ok_or_else(|| anyhow!("no code in callback"))
    })
    .await
    .map_err(|_| anyhow!("OAuth timed out (no browser response within 120 s)"))??;
    Ok(result)
}

/// Exchange the PKCE auth code for a session.
pub async fn exchange_pkce_code(
    supabase_url: &str,
    anon_key: &str,
    code: &str,
    verifier: &str,
) -> Result<SupabaseSession> {
    let client = Client::new();
    let url = format!("{}/auth/v1/token?grant_type=pkce", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "auth_code": code, "code_verifier": verifier }))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("PKCE exchange failed ({}): {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    let body: serde_json::Value = resp.json().await?;
    parse_session(&body)
}

// ── Stripe Checkout ──────────────────────────────────────────────────────────

/// Call the `create-checkout` Edge Function. Returns the Stripe Checkout URL.
pub async fn create_checkout_session(
    supabase_url: &str,
    access_token: &str,
    amount_usd: f64,
) -> Result<String> {
    let client = Client::new();
    let url = format!("{}/functions/v1/create-checkout", supabase_url);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "amount_usd": amount_usd }))
        .send()
        .await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        let err = body["error"].as_str().unwrap_or("unknown");
        let msg = body["message"].as_str().unwrap_or("");
        bail!("{}: {}", err, msg);
    }
    body["url"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("create-checkout returned no url"))
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
