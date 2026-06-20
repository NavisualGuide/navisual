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

/// Bind the loopback callback port. Done BEFORE opening the browser so a busy
/// port (a previous attempt still waiting) fails fast with a clear message
/// instead of after the user has already been sent to Google.
pub async fn bind_callback_listener(port: u16) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                anyhow!("A sign-in is already in progress — finish it in your browser, or wait a moment and try again.")
            } else {
                anyhow!("Could not start the sign-in listener: {e}")
            }
        })
}

/// Accept one connection on the pre-bound listener, parse the OAuth code from
/// the redirect, and serve a minimal success page. 120 s timeout.
pub async fn accept_oauth_code(listener: tokio::net::TcpListener) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        // Must declare charset=utf-8 — the em-dash is multi-byte UTF-8 and
        // browsers default to Latin-1/Windows-1252 without it (mojibake "â€"").
        let body = "<html><head><meta charset=\"utf-8\"></head>\
            <body style='font-family:sans-serif;padding:40px'>\
            <h2>Signed in — you can close this tab and return to Navisual.</h2></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

// ── Email / password auth + account management (S.2.1) ───────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct AccountInfo {
    /// `None` for an anonymous account; the confirmed email otherwise.
    pub email: Option<String>,
    pub is_anonymous: bool,
    /// Auth providers on the account, e.g. `["email"]`, `["google"]`, or
    /// `["email","google"]`. Lets the UI hide "Change password" for an
    /// OAuth-only (Google) account — its password is managed by the provider,
    /// not by us.
    pub providers: Vec<String>,
}

/// Add an email + password to the CURRENT (anonymous) user, upgrading the
/// account **in place** — the user id and its `user_profiles` row (free-request
/// count + any coins) are preserved. Triggers a confirmation email carrying the
/// 6-digit OTP. The session stays anonymous until the OTP is verified.
pub async fn sign_up_email(
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
    email: &str,
    password: &str,
) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/auth/v1/user", supabase_url);
    let resp = client
        .put(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let msg = friendly_auth_error(resp).await;
        let low = msg.to_lowercase();
        // Repeat of an unverified sign-up: the first attempt already set this
        // password on the anonymous user and queued the email-change OTP, so a
        // second identical submit 422s on "password should be different from the
        // old". The email is already pending and the code was already emailed —
        // treat it as success so the UI advances to code entry instead of a
        // dead-end error. (A genuinely new password would have been accepted.)
        if low.contains("should be different") || low.contains("same password") {
            return Ok(());
        }
        // The email belongs to a different, already-registered account.
        if low.contains("already") && (low.contains("regist") || low.contains("exist")) {
            bail!("This email already has an account — sign in instead.");
        }
        bail!("Sign-up failed ({}): {}", status, msg);
    }
    Ok(())
}

/// Resend the email-confirmation OTP for a pending sign-up. Our anonymous→email
/// upgrade is tracked by GoTrue as an `email_change` (validated live), so that
/// type is tried first, with `signup` as a version fallback. Requires the user's
/// Bearer token (the email change is pending on that exact user). A fresh code is
/// emailed; the previous one is invalidated.
pub async fn resend_signup_otp(
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
    email: &str,
) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/auth/v1/resend", supabase_url);
    let mut last = String::from("resend failed");
    for otp_type in ["email_change", "signup"] {
        let resp = client
            .post(&url)
            .header("apikey", anon_key)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "type": otp_type, "email": email }))
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(());
        }
        last = friendly_auth_error(resp).await;
    }
    bail!("Couldn't resend the code: {}", last)
}

/// Verify an email OTP. `otp_type` is one of `signup` / `email` / `email_change`
/// (which GoTrue uses for an anonymous→email upgrade varies by version — the
/// caller tries them in order; a failed verify does NOT consume the token).
/// Returns the now-confirmed session.
pub async fn verify_email_otp(
    supabase_url: &str,
    anon_key: &str,
    email: &str,
    token: &str,
    otp_type: &str,
) -> Result<SupabaseSession> {
    let client = Client::new();
    let url = format!("{}/auth/v1/verify", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "type": otp_type, "email": email, "token": token }))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "OTP verification failed ({}): {}",
            resp.status(),
            friendly_auth_error(resp).await
        );
    }
    let body: serde_json::Value = resp.json().await?;
    parse_session(&body)
}

/// Convenience wrapper for the password-recovery OTP type.
pub async fn verify_recovery_otp(
    supabase_url: &str,
    anon_key: &str,
    email: &str,
    token: &str,
) -> Result<SupabaseSession> {
    verify_email_otp(supabase_url, anon_key, email, token, "recovery").await
}

/// Sign in with email + password. Returns a session.
pub async fn sign_in_email(
    supabase_url: &str,
    anon_key: &str,
    email: &str,
    password: &str,
) -> Result<SupabaseSession> {
    let client = Client::new();
    let url = format!("{}/auth/v1/token?grant_type=password", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let msg = friendly_auth_error(resp).await;
        // Account exists but its email was never confirmed. Surface a recognizable
        // marker so the caller can route to the verification screen + resend a code
        // instead of dead-ending on a raw "Email not confirmed" error.
        if msg.to_lowercase().contains("not confirmed") {
            bail!("EMAIL_NOT_CONFIRMED: verify your email to finish signing in.");
        }
        bail!("{}", msg);
    }
    let body: serde_json::Value = resp.json().await?;
    parse_session(&body)
}

/// Revoke the current session server-side (best-effort).
pub async fn sign_out(supabase_url: &str, anon_key: &str, access_token: &str) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/auth/v1/logout", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Sign-out failed ({})", resp.status());
    }
    Ok(())
}

/// Send a password-reset email containing the recovery OTP.
pub async fn request_password_reset(
    supabase_url: &str,
    anon_key: &str,
    email: &str,
) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/auth/v1/recover", supabase_url);
    let resp = client
        .post(&url)
        .header("apikey", anon_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Password-reset request failed ({}): {}",
            resp.status(),
            friendly_auth_error(resp).await
        );
    }
    Ok(())
}

/// Change the password of the CURRENT session (signed-in user).
pub async fn change_password(
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
    new_password: &str,
) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/auth/v1/user", supabase_url);
    let resp = client
        .put(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "password": new_password }))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Change password failed ({}): {}",
            resp.status(),
            friendly_auth_error(resp).await
        );
    }
    Ok(())
}

/// Fetch the current user's email + anonymous flag (for the Account UI).
pub async fn get_account_info(
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
) -> Result<AccountInfo> {
    let client = Client::new();
    let url = format!("{}/auth/v1/user", supabase_url);
    let resp = client
        .get(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("get_account_info failed ({})", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let email = body["email"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let is_anonymous = body["is_anonymous"].as_bool().unwrap_or(false);
    // `app_metadata.providers` is the authoritative list; fall back to the
    // singular `provider`, then to scanning `identities[].provider`.
    let mut providers: Vec<String> = body["app_metadata"]["providers"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if providers.is_empty() {
        if let Some(p) = body["app_metadata"]["provider"].as_str() {
            providers.push(p.to_string());
        }
    }
    if providers.is_empty() {
        if let Some(ids) = body["identities"].as_array() {
            for id in ids {
                if let Some(p) = id["provider"].as_str() {
                    providers.push(p.to_string());
                }
            }
        }
    }
    Ok(AccountInfo {
        email,
        is_anonymous,
        providers,
    })
}

/// Permanently delete the current account via the service-role `delete-account`
/// Edge Function (a client can't delete `auth.users` under RLS). The
/// `user_profiles` row is removed by its `ON DELETE CASCADE` FK. Never refunds
/// coins — refunds are manual.
pub async fn delete_account(supabase_url: &str, access_token: &str) -> Result<()> {
    let client = Client::new();
    let url = format!("{}/functions/v1/delete-account", supabase_url);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Account deletion failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

/// Best-effort extraction of GoTrue's human-readable error message
/// (`error_description` / `msg` / `error`), falling back to the raw body.
async fn friendly_auth_error(resp: reqwest::Response) -> String {
    let raw = resp.text().await.unwrap_or_default();
    if let Ok(body) = serde_json::from_str::<serde_json::Value>(&raw) {
        for key in ["error_description", "msg", "message", "error"] {
            if let Some(s) = body[key].as_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    raw
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
