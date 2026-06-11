use crate::ai::types::{GuidanceStep, Message, Role};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_for_api_keeps_last_n_turns() {
        let mut s = Session::new("task".into());
        for i in 0..12 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            s.add_turn(role, format!("turn {i}"), None);
        }
        let msgs = s.get_conversation_for_api(10);
        assert_eq!(msgs.len(), 10);
        // Oldest two turns trimmed — window starts at turn 2.
        assert_eq!(msgs[0].content, "turn 2");
        assert_eq!(msgs.last().unwrap().content, "turn 11");
    }

    #[test]
    fn correction_turns_map_to_user_role() {
        let mut s = Session::new("task".into());
        s.add_turn("user", "do the thing".into(), None);
        s.add_turn("assistant", "click X".into(), None);
        s.add_turn("correction", "that was wrong".into(), None);
        let msgs = s.get_conversation_for_api(10);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1].role, Role::Assistant);
        // Corrections are presented to the provider as user messages.
        assert_eq!(msgs[2].role, Role::User);
    }

    #[test]
    fn unknown_roles_are_excluded() {
        let mut s = Session::new("task".into());
        s.add_turn("user", "hi".into(), None);
        s.add_turn("system", "internal note".into(), None);
        assert_eq!(s.get_conversation_for_api(10).len(), 1);
    }

    #[test]
    fn history_is_plain_text_only() {
        // Provider-agnostic invariant: turns store text + an optional hash,
        // never image data — switching providers mid-session must be safe.
        let mut s = Session::new("task".into());
        s.add_turn("assistant", "step 1\nstep 2".into(), Some("...".into()));
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.conversation[0].content, "step 1\nstep 2");
        assert_eq!(back.conversation[0].screenshot_hash.as_deref(), Some("..."));
    }
}
