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

/// How long a sign-in waits for the browser to come back, per round-trip.
///
/// This bounds the **wait**, not the port. It is a budget for a *human*, which
/// is why it is minutes rather than seconds: a first-time Google registration is
/// a possibly cold browser launch (Edge shows its first-run page before it ever
/// reaches the consent URL), an account chooser, a password, often an SMS 2FA
/// code, and only then the consent screen. The original 120 s was short enough
/// for a real first registration to overrun — reported live 2026-09-09 and
/// reproduced 2026-09-10.
///
/// What it costs when it expires is now only that the panel stops saying
/// "Signing in…" and frees the button. The port keeps listening, so a redirect
/// arriving after this still gets a page that explains itself — see
/// [`OAuthCallbackServer`].
///
/// **It must stay under GoTrue's own state expiry, and that is the binding
/// constraint** — not the user's patience. GoTrue signs the `state` it hands
/// Google and rejects it once expired; the evidence (2026-09-10, a deliberate
/// five-minute wait) is a redirect carrying
/// `error_code=bad_oauth_state&error_description=OAuth+state+has+expired`,
/// which means the sign-in was already unrecoverable at that point. A first
/// pass set this to 300 s, level with that expiry, which is the wrong
/// ordering: the app went on saying "Signing in…" over a flow that could no
/// longer succeed. It now expires first, so the panel tells the user to retry
/// while a retry still works.
///
/// Note what this does *not* fix: on an expired state GoTrue has no verified
/// `redirect_to` to honour, so it falls back to the project's **Site URL** —
/// the browser lands there, not on this port, and no amount of listening here
/// can answer it. That one is a dashboard setting; see `server-plan.md`.
const OAUTH_CALLBACK_TIMEOUT_SECS: u64 = 240;

/// The outcome parsed from the OAuth loopback callback: either the PKCE auth
/// `code`, or an `error` GoTrue redirected back with (e.g. the identity is
/// already linked to a different account during an in-place link attempt).
pub enum OAuthCallback {
    Code(String),
    Error { error: String, description: String },
}

/// What a browser hitting `/callback` is told, which depends entirely on
/// whether a sign-in is actually waiting for it.
enum CallbackState {
    /// No sign-in has been started, or the last one gave up waiting. A redirect
    /// arriving now is a late one from an attempt that already timed out.
    Idle,
    /// A sign-in is waiting; the next callback is handed to it over `tx`.
    /// `bounced` is per attempt — see [`decide_callback_response`].
    Armed {
        tx: tokio::sync::oneshot::Sender<OAuthCallback>,
        bounced: bool,
    },
    /// The waiting sign-in got its callback. A second hit (a refresh, a
    /// duplicate tab) must not be reported to the user as an expiry.
    Handled,
}

/// The loopback HTTP server that receives the Google OAuth redirect.
///
/// **It outlives any single sign-in, deliberately.** The listener used to be a
/// local in `start_google_oauth`, so the port died when the command returned —
/// and a redirect arriving even a second later hit a closed port, which the
/// browser renders as `ERR_CONNECTION_REFUSED`: a page that neither names
/// Navisual nor says what to do, on the one surface the user is actually
/// looking at. Reported live 2026-09-09 and reproduced 2026-09-10.
///
/// Two separate things are needed for a real page to appear there, and
/// conflating them is easy: **binding** the port is what stops the connection
/// being refused, and an **always-on acceptor** is what actually answers it. A
/// bound socket with nobody sitting in `accept()` only converts a fast, honest
/// refusal into a browser spinner ending in `ERR_EMPTY_RESPONSE`, and leaks the
/// accept backlog while it does it. So this owns both.
///
/// The port is claimed once and never handed back, which also closes a small
/// hole: between attempts, nothing else on the machine can squat on 9876 and
/// receive the next real callback.
pub struct OAuthCallbackServer {
    state: parking_lot::Mutex<CallbackState>,
}

impl OAuthCallbackServer {
    /// Claim the next callback for a sign-in that is about to open the browser.
    /// Fails when one is already waiting — two concurrent sign-ins would race
    /// for the same redirect, and the loser would silently steal it.
    pub fn arm(&self) -> Result<tokio::sync::oneshot::Receiver<OAuthCallback>> {
        let mut st = self.state.lock();
        // Liveness, not just state: a claim whose receiver has gone (its command
        // was dropped without waiting) has nobody behind it, and treating that as
        // "in progress" would wedge every later sign-in for the life of the
        // process. Found by the test below, not by reasoning.
        if let CallbackState::Armed { tx, .. } = &*st {
            if !tx.is_closed() {
                bail!("A sign-in is already in progress — finish it in your browser, or wait a moment and try again.");
            }
            log::info!("[oauth] taking over an abandoned sign-in claim");
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        *st = CallbackState::Armed { tx, bounced: false };
        Ok(rx)
    }

    /// Wait for the armed callback, up to [`OAUTH_CALLBACK_TIMEOUT_SECS`].
    ///
    /// On timeout the attempt is disarmed but the server keeps listening, so a
    /// late redirect still lands on a page rather than a connection error.
    pub async fn wait(
        &self,
        pending: tokio::sync::oneshot::Receiver<OAuthCallback>,
    ) -> Result<OAuthCallback> {
        let budget = std::time::Duration::from_secs(OAUTH_CALLBACK_TIMEOUT_SECS);
        match tokio::time::timeout(budget, pending).await {
            Ok(Ok(cb)) => Ok(cb),
            // The sender was dropped: the callback arrived carrying neither a
            // code nor an error, even after the fragment bounce.
            Ok(Err(_)) => Err(anyhow!("no code in callback")),
            Err(_) => {
                self.disarm();
                log::warn!(
                    "[oauth] no callback within {OAUTH_CALLBACK_TIMEOUT_SECS} s; the sign-in \
                     stopped waiting (the port stays open and will explain itself)"
                );
                Err(anyhow!(
                    "Sign-in timed out — the browser did not come back within {} minutes, \
                     and a Google sign-in stops being valid around that point. \
                     Press \"Continue with Google\" to start a fresh one.",
                    OAUTH_CALLBACK_TIMEOUT_SECS / 60
                ))
            }
        }
    }

    /// Give up the claim without having received a callback. Never clobbers
    /// `Handled`, so a callback that landed in the same instant still counts.
    pub fn disarm(&self) {
        let mut st = self.state.lock();
        if matches!(*st, CallbackState::Armed { .. }) {
            *st = CallbackState::Idle;
        }
    }
}

static CALLBACK_SERVER: std::sync::OnceLock<
    parking_lot::Mutex<Option<std::sync::Arc<OAuthCallbackServer>>>,
> = std::sync::OnceLock::new();

/// The process-wide callback server, binding the port on first use.
///
/// **Lazily, not at startup.** `9876` is a number this app picked, and most
/// users never sign in with Google at all (BYOK keys, Ollama, the anonymous
/// free tier) — there is no reason for them to hold the port, or to lose it to
/// whatever else on the machine wanted it first. Binding on the first sign-in
/// also surfaces a genuine conflict at a moment the user can make sense of,
/// instead of silently at launch.
pub fn oauth_callback_server(port: u16) -> Result<std::sync::Arc<OAuthCallbackServer>> {
    let slot = CALLBACK_SERVER.get_or_init(|| parking_lot::Mutex::new(None));
    let mut guard = slot.lock();
    if let Some(existing) = guard.as_ref() {
        return Ok(existing.clone());
    }

    // Bound through `std` so this stays a sync function: it runs under a
    // process-wide lock, and an `.await` inside one is how deadlocks start.
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!(
                "Port {port} is already in use, so Google sign-in cannot start. \
                 Close whatever is using it, or sign in with an email address instead."
            )
        } else {
            anyhow!("Could not start the sign-in listener: {e}")
        }
    })?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let server = std::sync::Arc::new(OAuthCallbackServer {
        state: parking_lot::Mutex::new(CallbackState::Idle),
    });
    let acceptor = server.clone();
    tokio::spawn(async move { accept_callbacks(listener, acceptor).await });
    log::info!("[oauth] callback server listening on 127.0.0.1:{port} for the life of the process");

    *guard = Some(server.clone());
    Ok(server)
}

/// Accept forever.
///
/// Each connection is handled on its own task: a browser that opens a socket
/// and sends nothing (a speculative preconnect, a liveness probe) must never be
/// able to stall the one connection that carries the code.
async fn accept_callbacks(
    listener: tokio::net::TcpListener,
    server: std::sync::Arc<OAuthCallbackServer>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let server = server.clone();
                tokio::spawn(async move { handle_callback_conn(stream, server).await });
            }
            Err(e) => {
                // Don't spin hot on a persistent accept failure.
                log::warn!("[oauth] callback accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

/// Read one request, answer it, hang up.
async fn handle_callback_conn(
    mut stream: tokio::net::TcpStream,
    server: std::sync::Arc<OAuthCallbackServer>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // A client that connects and then says nothing is dropped rather than held.
    let mut buf = [0u8; 4096];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream.read(&mut buf),
    )
    .await;
    let n = match read {
        Ok(Ok(n)) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    // First line: "GET /callback?code=XXX&... HTTP/1.1"
    let first_line = request.lines().next().unwrap_or("");
    let target = first_line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    // Stray requests (favicon, liveness probes) are not the callback.
    if !path.starts_with("/callback") {
        log::debug!("[oauth] ignoring stray loopback request: {path}");
        let _ = stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }

    let (code, error, description) = parse_callback_query(query);
    let parsed = match (code, error) {
        (Some(code), _) => Some(OAuthCallback::Code(code)),
        (None, Some(error)) => Some(OAuthCallback::Error { error, description }),
        (None, None) => None,
    };

    let page = decide_callback_response(&server, parsed);
    let _ = stream.write_all(http_html(&page.render()).as_bytes()).await;
}

/// Decide what the browser is told and, when a sign-in is waiting, hand it the
/// callback. Split out from the socket work so the state lock is never held
/// across an `.await`.
fn decide_callback_response(
    server: &OAuthCallbackServer,
    parsed: Option<OAuthCallback>,
) -> CallbackPage {
    let mut st = server.state.lock();

    if !matches!(*st, CallbackState::Armed { .. }) {
        return if matches!(*st, CallbackState::Handled) {
            log::info!("[oauth] callback hit again after it was already handled");
            CallbackPage::AlreadyHandled
        } else {
            log::info!("[oauth] callback arrived with no sign-in waiting; serving the expired page");
            CallbackPage::Expired
        };
    }

    if let Some(cb) = parsed {
        match &cb {
            // Presence only — the code is a one-use credential and never
            // reaches the log file.
            OAuthCallback::Code(_) => log::info!("[oauth] callback delivered an auth code"),
            OAuthCallback::Error { error, .. } => {
                log::info!("[oauth] callback delivered error={error}")
            }
        }
        if let CallbackState::Armed { tx, .. } = std::mem::replace(&mut *st, CallbackState::Handled)
        {
            let _ = tx.send(cb);
        }
        return CallbackPage::Close;
    }

    // Nothing in the query. GoTrue returns OAuth *errors* in the URL
    // **fragment**, which a browser never sends to a server, so answer once
    // with a page whose JS copies `location.hash` into the query and reloads —
    // that is what lets the "identity already linked" conflict reach the replace
    // fallback instead of dying as "no code in callback". A PKCE success
    // arrives as `?code=` and never gets here. A second empty hit means the
    // fragment was empty too: drop the sender so the waiting sign-in fails
    // rather than hangs.
    if !matches!(*st, CallbackState::Armed { bounced: true, .. }) {
        if let CallbackState::Armed { bounced, .. } = &mut *st {
            *bounced = true;
        }
        log::info!("[oauth] callback had no query params, serving the fragment bounce");
        return CallbackPage::Bounce;
    }
    log::warn!("[oauth] callback carried neither code nor error after the bounce");
    *st = CallbackState::Handled;
    CallbackPage::Close
}

/// Pull `code` / `error` / `error_description` out of the callback query.
fn parse_callback_query(query: &str) -> (Option<String>, Option<String>, String) {
    let (mut code, mut error, mut description) = (None, None, String::new());
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        match k {
            "code" => code = Some(url_decode(v)),
            "error" => error = Some(url_decode(v)),
            // Fallback only — prefer the human-readable `error` over the code.
            "error_code" if error.is_none() => error = Some(url_decode(v)),
            "error_description" => description = url_decode(v),
            _ => {}
        }
    }
    (code, error, description)
}

/// The pages the callback server can serve. The browser is the surface the user
/// is watching during a sign-in, so every one of them names Navisual and says
/// what to do next — including, and especially, the ones that report a failure.
enum CallbackPage {
    /// The callback was handed to a waiting sign-in.
    Close,
    /// Looks identical to `Close`; carries the fragment-to-query reload.
    Bounce,
    /// A redirect arrived with nothing waiting for it — the attempt it belongs
    /// to gave up. This is the page that used to be `ERR_CONNECTION_REFUSED`.
    Expired,
    /// A repeat hit on a callback that was already consumed.
    AlreadyHandled,
}

impl CallbackPage {
    fn render(&self) -> String {
        match self {
            CallbackPage::Close => html_page(
                "You can close this tab",
                "Navisual has what it needs. Head back to the app to carry on.",
                "",
            ),
            CallbackPage::Bounce => html_page(
                "You can close this tab",
                "Navisual has what it needs. Head back to the app to carry on.",
                "<script>(function(){var h=location.hash?location.hash.slice(1):'';\
                 if(h){location.replace(location.pathname+'?'+h);}})();</script>",
            ),
            CallbackPage::Expired => html_page(
                "This sign-in expired",
                "Navisual stopped waiting for it. Go back to Navisual and press \
                 &ldquo;Continue with Google&rdquo; to try again.",
                "",
            ),
            CallbackPage::AlreadyHandled => html_page(
                "Already done",
                "Navisual has already received this sign-in. You can close this tab.",
                "",
            ),
        }
    }
}

/// One small dark page, so a sign-in that ends in the browser still looks like
/// it came from the app the user started it in.
fn html_page(heading: &str, body: &str, script: &str) -> String {
    format!(
        "<html><head><meta charset=\"utf-8\"><title>Navisual</title><style>\
         body{{font-family:'Segoe UI',system-ui,sans-serif;background:#18181b;color:#e4e4e7;\
         margin:0;height:100vh;display:flex;align-items:center;justify-content:center;\
         text-align:center}}\
         main{{max-width:30rem;padding:0 24px}}\
         h1{{font-size:1.35rem;font-weight:600;margin:0 0 10px}}\
         p{{color:#a1a1aa;line-height:1.55;margin:0}}\
         </style></head><body><main><h1>{heading}</h1><p>{body}</p></main>{script}</body></html>"
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_server() -> OAuthCallbackServer {
        OAuthCallbackServer {
            state: parking_lot::Mutex::new(CallbackState::Idle),
        }
    }

    /// The whole point of keeping the port bound: a redirect from an attempt
    /// that already gave up gets a page naming Navisual, not the browser's own
    /// connection error. It carries a real code and is still refused work,
    /// because nothing is waiting to redeem it.
    #[test]
    fn a_late_callback_explains_itself_instead_of_being_refused() {
        let s = idle_server();
        let page = decide_callback_response(&s, Some(OAuthCallback::Code("abc".into())));
        assert!(matches!(page, CallbackPage::Expired));
    }

    #[test]
    fn an_armed_sign_in_receives_the_code_and_the_tab_is_told_to_close() {
        let s = idle_server();
        let mut pending = s.arm().unwrap();

        let page = decide_callback_response(&s, Some(OAuthCallback::Code("abc".into())));
        assert!(matches!(page, CallbackPage::Close));
        match pending.try_recv() {
            Ok(OAuthCallback::Code(c)) => assert_eq!(c, "abc"),
            _ => panic!("the waiting sign-in should have received the code"),
        }

        // Refreshing that same URL must not read as an expiry.
        assert!(matches!(
            decide_callback_response(&s, None),
            CallbackPage::AlreadyHandled
        ));
    }

    /// GoTrue puts OAuth errors in the fragment, which never reaches a server —
    /// the bounce is what surfaces them. It fires once; a second empty hit means
    /// the fragment was empty too, and the waiting sign-in must fail then rather
    /// than hang for the full timeout.
    #[test]
    fn a_fragment_only_callback_bounces_once_then_gives_up() {
        let s = idle_server();
        let mut pending = s.arm().unwrap();

        assert!(matches!(
            decide_callback_response(&s, None),
            CallbackPage::Bounce
        ));
        assert!(matches!(
            decide_callback_response(&s, None),
            CallbackPage::Close
        ));
        assert!(
            pending.try_recv().is_err(),
            "the sender must be dropped so the sign-in fails instead of hanging"
        );
    }

    #[test]
    fn an_armed_error_reaches_the_waiting_sign_in() {
        let s = idle_server();
        let mut pending = s.arm().unwrap();
        let cb = OAuthCallback::Error {
            error: "server_error".into(),
            description: "Identity is already linked to another user".into(),
        };
        assert!(matches!(
            decide_callback_response(&s, Some(cb)),
            CallbackPage::Close
        ));
        // This is the path the replace fallback depends on.
        assert!(matches!(
            pending.try_recv(),
            Ok(OAuthCallback::Error { .. })
        ));
    }

    #[test]
    fn two_sign_ins_cannot_race_for_one_redirect() {
        let s = idle_server();
        let _first = s.arm().unwrap();
        assert!(s.arm().is_err());
    }

    /// A timed-out attempt must not block the retry — that retry is the recovery
    /// path users actually take. And a `disarm` racing a callback that already
    /// landed must not rewrite the outcome.
    #[test]
    fn disarming_frees_the_claim_but_never_clobbers_a_handled_callback() {
        let s = idle_server();
        let first = s.arm().unwrap();
        s.disarm();
        let second = s.arm().expect("a timed-out attempt must not block the retry");
        drop(first);

        let _ = decide_callback_response(&s, Some(OAuthCallback::Code("abc".into())));
        s.disarm();
        assert!(matches!(
            decide_callback_response(&s, None),
            CallbackPage::AlreadyHandled
        ));
        drop(second);
    }

    /// If a command is dropped mid-wait, its claim is dead but still recorded.
    /// Reading it as "a sign-in is in progress" would break Google sign-in for
    /// the rest of the session — the one failure this whole change exists to
    /// prevent.
    #[test]
    fn an_abandoned_claim_never_wedges_the_next_sign_in() {
        let s = idle_server();
        drop(s.arm().unwrap());
        assert!(s.arm().is_ok());
    }

    #[test]
    fn every_page_names_the_app_and_declares_its_charset() {
        for page in [
            CallbackPage::Close,
            CallbackPage::Bounce,
            CallbackPage::Expired,
            CallbackPage::AlreadyHandled,
        ] {
            let html = page.render();
            assert!(html.contains("Navisual"), "a page that does not name the app is the bug");
            assert!(html.contains("charset=\"utf-8\""));
        }
        assert!(CallbackPage::Bounce.render().contains("location.hash"));
        assert!(!CallbackPage::Close.render().contains("location.hash"));
    }

    /// Everything above tests the decision; this tests the socket underneath it
    /// - bind, accept, read, respond - which is the half that turns a bound port
    /// into a page the browser can actually show. Port 0 lets the OS pick, so it
    /// never fights the real 9876 or another test run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_server_answers_on_a_real_socket() {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = std_listener.local_addr().unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
        let server = std::sync::Arc::new(idle_server());
        tokio::spawn(accept_callbacks(listener, server.clone()));

        // Nothing armed: the reported failure, now answered instead of refused.
        let body = http_get(addr, "/callback?code=abc").await;
        assert!(body.starts_with("HTTP/1.1 200"));
        assert!(body.contains("This sign-in expired"));
        assert!(body.contains("Navisual"));

        // Armed: the waiting sign-in gets the code.
        let mut pending = server.arm().unwrap();
        let body = http_get(addr, "/callback?code=abc").await;
        assert!(body.contains("You can close this tab"));
        assert!(matches!(pending.try_recv(), Ok(OAuthCallback::Code(c)) if c == "abc"));

        // A stray hit is not the callback.
        let body = http_get(addr, "/favicon.ico").await;
        assert!(body.starts_with("HTTP/1.1 204"));
    }

    async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        out
    }

    #[test]
    fn callback_query_parsing_prefers_the_readable_error() {
        let (code, err, desc) =
            parse_callback_query("error_code=422&error=server_error&error_description=Already+linked");
        assert!(code.is_none());
        assert_eq!(err.as_deref(), Some("server_error"));
        assert_eq!(desc, "Already linked");

        let (code, err, _) = parse_callback_query("code=f280bfa0-3db1-4ed9");
        assert_eq!(code.as_deref(), Some("f280bfa0-3db1-4ed9"));
        assert!(err.is_none());
    }
}
