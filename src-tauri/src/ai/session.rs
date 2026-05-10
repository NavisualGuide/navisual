use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Local;
use std::fs;
use std::path::PathBuf;
use crate::ai::types::{GuidanceStep, Role, Message};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSummary {
    pub summary_text: String,
    pub turn_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
    pub screenshot_hash: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub task_description: String,
    #[serde(default)]
    pub conversation: Vec<Turn>,
    pub current_state_summary: Option<StateSummary>,
    #[serde(default)]
    pub current_step_sequence: Vec<GuidanceStep>,
    #[serde(default)]
    pub current_step_index: usize,
    #[serde(default)]
    pub token_usage: TokenUsage,
    pub started_at: String,
    pub last_active_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

impl Session {
    pub fn new(task_description: String) -> Self {
        let now = Local::now().to_rfc3339();
        Self {
            id: Uuid::new_v4(),
            task_description,
            conversation: Vec::new(),
            current_state_summary: None,
            current_step_sequence: Vec::new(),
            current_step_index: 0,
            token_usage: TokenUsage::default(),
            started_at: now.clone(),
            last_active_at: now,
        }
    }

    pub fn add_turn(&mut self, role: &str, content: String, screenshot_hash: Option<String>) {
        self.conversation.push(Turn {
            role: role.to_string(),
            content,
            screenshot_hash,
            timestamp: Local::now().to_rfc3339(),
        });
        self.last_active_at = Local::now().to_rfc3339();
    }

    pub fn update_state(&mut self, summary_text: String) {
        self.current_state_summary = Some(StateSummary {
            summary_text,
            turn_index: self.conversation.len(),
        });
    }

    pub fn record_tokens(&mut self, input_tokens: u64, output_tokens: u64) {
        self.token_usage.input += input_tokens;
        self.token_usage.output += output_tokens;
    }

    pub fn get_conversation_for_api(&self, max_turns: usize) -> Vec<Message> {
        let start = if self.conversation.len() > max_turns {
            self.conversation.len() - max_turns
        } else {
            0
        };

        let mut messages = Vec::new();
        for turn in &self.conversation[start..] {
            if turn.role == "correction" || turn.role == "user" {
                messages.push(Message {
                    role: Role::User,
                    content: turn.content.clone(),
                });
            } else if turn.role == "assistant" {
                messages.push(Message {
                    role: Role::Assistant,
                    content: turn.content.clone(),
                });
            }
        }
        messages
    }
}

pub struct SessionManager {
    pub session_dir: PathBuf,
    pub current_session: Option<Session>,
}

impl SessionManager {
    pub fn new(session_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&session_dir);
        Self {
            session_dir,
            current_session: None,
        }
    }

    pub fn create_session(&mut self, task_description: String) -> Session {
        let session = Session::new(task_description);
        self.current_session = Some(session.clone());
        session
    }

    pub fn save_session(&self, session: Option<&Session>) {
        if let Some(s) = session.or(self.current_session.as_ref()) {
            let file_path = self.session_dir.join(format!("{}.json", s.id));
            if let Ok(json) = serde_json::to_string_pretty(s) {
                let _ = fs::write(file_path, json);
            }
        }
    }

    #[allow(dead_code)]
    pub fn load_session(&mut self, session_id: &str) -> Option<Session> {
        let file_path = self.session_dir.join(format!("{}.json", session_id));
        if let Ok(content) = fs::read_to_string(file_path) {
            if let Ok(session) = serde_json::from_str::<Session>(&content) {
                self.current_session = Some(session.clone());
                return Some(session);
            }
        }
        None
    }
}
