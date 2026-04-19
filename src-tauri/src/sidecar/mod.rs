//! Python sidecar process manager.
//!
//! Spawns the Python sidecar on app startup, keeps it alive for the session,
//! and routes JSON-lines messages between Rust and Python over stdin/stdout.

pub mod protocol;

use anyhow::{anyhow, Result};
use protocol::{Request, Response};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

/// A running Python sidecar process with pending-request tracking.
pub struct Sidecar {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Response>>>>,
    _child: Child,
}

impl Sidecar {
    /// Spawn the Python sidecar. In dev, uses `python` from PATH pointing at
    /// `../../sidecar/main.py` relative to the backend crate root. In a bundled
    /// build this will be replaced by the Tauri-managed external binary path.
    pub async fn spawn(python_script: PathBuf) -> Result<Self> {
        let mut child = Command::new("python")
            .arg(&python_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("failed to spawn sidecar: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("sidecar stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("sidecar stdout missing"))?;

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Reader task: parse each line of stdout into a Response, route to the
        // waiting request by id.
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                match serde_json::from_str::<Response>(&line) {
                    Ok(resp) => {
                        let mut map = pending_reader.lock().await;
                        if let Some(sender) = map.remove(&resp.id) {
                            let _ = sender.send(resp);
                        } else {
                            log::warn!("sidecar response with no pending request: {}", resp.id);
                        }
                    }
                    Err(e) => {
                        log::warn!("unparseable sidecar line '{}': {}", line, e);
                    }
                }
            }
            log::info!("sidecar stdout closed");
        });

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            _child: child,
        })
    }

    /// Send a request and await the matching response.
    pub async fn request(&self, cmd: &str, payload: serde_json::Value) -> Result<Response> {
        let id = Uuid::new_v4().to_string();
        let req = Request {
            id: id.clone(),
            cmd: cmd.to_string(),
            payload,
        };
        let line = serde_json::to_string(&req)? + "\n";

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        let resp = rx
            .await
            .map_err(|_| anyhow!("sidecar response channel closed"))?;
        Ok(resp)
    }
}
