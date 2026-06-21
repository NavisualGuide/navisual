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

/// A stable, privacy-preserving per-machine identifier used to enforce the free
/// quota **per device** server-side. The relay keys the 50-request free cap on
/// this hash (sent as the `X-Device-Hash` header) rather than on the throwaway
/// anonymous user id, so signing out / deleting the account / re-anonymizing
/// can't farm a fresh 50 — a new anon on the same machine shares the same device
/// pool and sees the real remaining count.
///
/// Derived from the Windows `MachineGuid` (survives app reinstall + clear-app-data
/// because it lives in the registry), SHA-256'd so the raw machine id never
/// leaves the device. `None` if the source can't be read — the relay then falls
/// back to per-user enforcement (the pre-device-binding behaviour). Computed once
/// and cached. This is a device fingerprint and must be disclosed in the privacy
/// policy / first-run modal.
pub fn device_hash() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            raw_machine_guid().map(|g| {
                let digest = Sha256::digest(g.as_bytes());
                URL_SAFE_NO_PAD.encode(digest)
            })
        })
        .clone()
}

#[cfg(windows)]
fn raw_machine_guid() -> Option<String> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW — don't flash a console window for the one-time query.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Output line looks like: "    MachineGuid    REG_SZ    <guid>"
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(idx) = line.find("REG_SZ") {
            let val = line[idx + "REG_SZ".len()..].trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn raw_machine_guid() -> Option<String> {
    None
}

pub async fn get_balance(supabase_url: &str, access_token: &str) -> Result<BalanceResponse> {
    let client = Client::new();
    let url = format!("{}/functions/v1/relay", supabase_url);
    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token));
    if let Some(dh) = device_hash() {
        req = req.header("X-Device-Hash", dh);
    }
    let resp = req.send().await?;
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

/// The outcome parsed from the OAuth loopback callback: either the PKCE auth
/// `code`, or an `error` GoTrue redirected back with (e.g. the identity is
/// already linked to a different account during an in-place link attempt).
pub enum OAuthCallback {
    Code(String),
    Error { error: String, description: String },
}

/// Accept the OAuth loopback callback on the pre-bound listener and parse the
/// PKCE `code` (or `error`) from the redirect. 120 s timeout. Borrows the
/// listener (`accept` takes `&self`) so the caller can reuse the same bound port
/// for a follow-up round-trip — e.g. the in-place-link → replace fallback —
/// without rebinding (and racing TIME_WAIT).
///
/// Loops rather than handling a single connection because:
/// (1) GoTrue returns OAuth **errors** in the URL **fragment** (`#error=...`),
///     which a browser never sends to a server. The first hit (bare `/callback`,
///     fragment withheld) is answered with a tiny page whose JS copies
///     `location.hash` into the query and reloads, so the follow-up request
///     carries the error where we can read it — this is what lets the "identity
///     already linked" conflict reach the replace fallback instead of dying as
///     "no code in callback". PKCE **success** arrives as `?code=` in the query
///     and returns on the first hit, before the bounce page is ever served.
/// (2) Stray requests (favicon, browser liveness probes) must not be mistaken
///     for the callback — non-`/callback` paths are answered 204 and skipped.
pub async fn accept_oauth_callback(
    listener: &tokio::net::TcpListener,
) -> Result<OAuthCallback> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Charset must be declared — the em-dash is multi-byte UTF-8 and browsers
    // otherwise default to Latin-1/Windows-1252 (mojibake "â€"").
    let close_page = "<html><head><meta charset=\"utf-8\"></head>\
        <body style='font-family:sans-serif;padding:40px'>\
        <h2>You can close this tab and return to Navisual.</h2></body></html>";
    // Served on a fragment-only callback: move `location.hash` into the query and
    // reload so the next request carries the params (GoTrue puts errors there).
    let bounce_page = "<html><head><meta charset=\"utf-8\"></head>\
        <body style='font-family:sans-serif;padding:40px'>\
        <h2>You can close this tab and return to Navisual.</h2>\
        <script>(function(){var h=location.hash?location.hash.slice(1):'';\
        if(h){location.replace(location.pathname+'?'+h);}})();</script></body></html>";

    let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let mut bounced = false;
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await?;
            let request = String::from_utf8_lossy(&buf[..n]);
            // First line: "GET /callback?code=XXX&... HTTP/1.1"
            let first_line = request.lines().next().unwrap_or("");
            let target = first_line.split_whitespace().nth(1).unwrap_or("");
            let (path, query) = target.split_once('?').unwrap_or((target, ""));

            // Ignore stray requests (favicon, liveness probes) so they aren't
            // mistaken for the callback — keep listening for the real redirect.
            if !path.starts_with("/callback") {
                let _ = stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }

            // Pull `code` / `error` / `error_description` out of the callback query.
            let (mut code, mut error, mut description) = (None, None, String::new());
            for pair in query.split('&') {
                let Some((k, v)) = pair.split_once('=') else { continue };
                match k {
                    "code" => code = Some(url_decode(v)),
                    "error" => error = Some(url_decode(v)),
                    // Fallback only — prefer the human-readable `error` over the code.
                    "error_code" if error.is_none() => error = Some(url_decode(v)),
                    "error_description" => description = url_decode(v),
                    _ => {}
                }
            }

            if let Some(code) = code {
                let _ = stream.write_all(http_html(close_page).as_bytes()).await;
                return Ok(OAuthCallback::Code(code));
            }
            if let Some(error) = error {
                let _ = stream.write_all(http_html(close_page).as_bytes()).await;
                return Ok(OAuthCallback::Error { error, description });
            }

            // Neither in the query — the params are probably in the URL fragment.
            // Bounce once to surface them; if the retry is still empty, give up.
            if !bounced {
                bounced = true;
                let _ = stream.write_all(http_html(bounce_page).as_bytes()).await;
                continue;
            }
            let _ = stream.write_all(http_html(close_page).as_bytes()).await;
            return Err(anyhow!("no code in callback"));
        }
    })
    .await
    .map_err(|_| anyhow!("OAuth timed out (no browser response within 120 s)"))??;
    Ok(result)
}

/// Build a `200 OK` HTTP/1.1 response with an HTML body and `Connection: close`.
fn http_html(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Percent-decode an `application/x-www-form-urlencoded` query value (`%XX`
/// escapes + `+` for space). Byte-wise so it never panics on a multi-byte UTF-8
/// sequence split across escapes.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Begin an **in-place** OAuth identity link for the current signed-in user.
/// Hits GoTrue's manual-linking authorize endpoint WITH the user's Bearer token,
/// so the provider identity attaches to *this* user id — preserving the
/// `user_profiles` row (free-request count + coins) instead of minting a brand-new
/// account the way the plain `/authorize` sign-in does. Requires "Manual linking"
/// enabled in the Supabase dashboard (Authentication → Sign In / Providers).
/// Returns the provider consent URL to open in the system browser; the PKCE `code`
/// comes back to the loopback callback and is redeemed with `exchange_pkce_code`,
/// identical to the sign-in flow. Mirrors the auth-js `linkIdentity` request
/// (`GET …/user/identities/authorize?…&skip_http_redirect=true`).
pub async fn link_identity_url(
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
    provider: &str,
    pkce: &OAuthPkce,
) -> Result<String> {
    let client = Client::new();
    let url = format!(
        "{}/auth/v1/user/identities/authorize?provider={}\
         &code_challenge={}\
         &code_challenge_method=S256\
         &redirect_to={}\
         &skip_http_redirect=true",
        supabase_url,
        pct_encode(provider),
        pct_encode(&pkce.challenge),
        pct_encode(&pkce.redirect_uri),
    );
    let resp = client
        .get(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "identity-link init failed ({}): {}",
            resp.status(),
            friendly_auth_error(resp).await
        );
    }
    let body: serde_json::Value = resp.json().await?;
    body["url"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("identity-link authorize returned no url"))
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
