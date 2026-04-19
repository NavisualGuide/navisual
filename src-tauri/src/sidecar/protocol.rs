//! JSON-lines message schema for Rust ↔ Python sidecar IPC.

use serde::{Deserialize, Serialize};

/// Rust → Python command envelope.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    pub cmd: String,
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Python → Rust response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub id: String,
    pub event: String,
    #[serde(flatten)]
    pub payload: serde_json::Value,
}
