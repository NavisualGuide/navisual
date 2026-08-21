use crate::ai::types::{GuidanceStep, Message, Role};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

/// How many turns are dropped at once when the window overflows. Larger = the retained slice
/// (and therefore the prompt prefix) stays byte-identical across more consecutive requests,
/// which is what makes provider prefix caching reachable; smaller = less history carried past
/// the nominal budget. 6 ≈ three exchanges of slack.
const EVICTION_BATCH: usize = 6;

/// Ceiling on pinned turns. Pins are a backstop against the model's summary drifting, not an
/// archive of everything the user ever said — uncapped they would grow the prompt without
/// bound in a chatty session. Oldest-first eviction: if the goal has been restated since, the
/// first phrasing is the stale one.
const MAX_PINNED_TURNS: usize = 5;

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
    /// Never evicted by the sliding window — a turn where the user stated intent in their own
    /// words (the opening task, a `needs_input` answer, a correction). Retention here is by
    /// **kind, not recency**: those are the only turns the model cannot reconstruct from a
    /// summary, and the first one is *not* reliably turn 1 — an opener is often
    /// "show me around this app" with the real goal arriving several turns later.
    /// `#[serde(default)]` so sessions saved before this field load as unpinned.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub task_description: String,
    /// Model-maintained route overview toward `task_description` — see
    /// `NavigateStepResponse::plan_outline`. `#[serde(default)]` so sessions saved
    /// before this field existed load as an empty (no-plan-shown) list.
    #[serde(default)]
    pub plan_outline: Vec<String>,
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
            plan_outline: Vec::new(),
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
        self.add_turn_pinned(role, content, screenshot_hash, false)
    }

    /// `pinned` turns survive the sliding window — see `Turn::pinned`.
    pub fn add_turn_pinned(
        &mut self,
        role: &str,
        content: String,
        screenshot_hash: Option<String>,
        pinned: bool,
    ) {
        self.conversation.push(Turn {
            role: role.to_string(),
            content,
            screenshot_hash,
            timestamp: Local::now().to_rfc3339(),
            pinned,
        });
        self.last_active_at = Local::now().to_rfc3339();
        if pinned {
            self.enforce_pin_cap();
        }
    }

    /// The task the user is actually trying to accomplish. Mutable because the opening message
    /// is frequently *not* the goal — "help me with this document" followed by the AI asking
    /// what for, and the real objective arriving as a reply, is the common shape. Before this
    /// existed, `task_description` was write-once and that opener stayed the goal forever.
    pub fn set_task_description(&mut self, task: String) {
        if !task.trim().is_empty() {
            self.task_description = task;
        }
    }

    /// Replaces the stored route overview wholesale — a REVISION, not an append.
    /// Deliberately not merged/accumulated: a plan that only grows would show every
    /// abandoned direction alongside the current one, which is worse than no plan.
    /// Called only when the model actually returned a non-empty list (see
    /// `NavigateStepResponse::plan_outline`); empty means "unchanged" and never
    /// reaches here.
    pub fn set_plan_outline(&mut self, outline: Vec<String>) {
        self.plan_outline = outline;
    }

    /// Keep only the most recent `MAX_PINNED_TURNS` pins, un-pinning older ones.
    ///
    /// Pins are a **backstop, not an archive**. Without a cap a long chatty session accumulates
    /// them until they dominate the prompt — the exact opposite of the point, in a workload
    /// where input is ~97% of cost. The oldest intent is also the most likely to be stale: if
    /// the user has restated their goal five times since, the first phrasing is history.
    fn enforce_pin_cap(&mut self) {
        let pinned: Vec<usize> = self
            .conversation
            .iter()
            .enumerate()
            .filter(|(_, t)| t.pinned)
            .map(|(i, _)| i)
            .collect();
        if pinned.len() <= MAX_PINNED_TURNS {
            return;
        }
        for &i in &pinned[..pinned.len() - MAX_PINNED_TURNS] {
            self.conversation[i].pinned = false;
        }
        log::info!(
            "[memory] pin cap: {} pinned -> {} (oldest un-pinned)",
            pinned.len(),
            MAX_PINNED_TURNS
        );
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

    /// Conversation to send, in **exchanges** (one user + one assistant turn), not raw turns.
    ///
    /// The unit matters: a request appends *two* turns, so the previous `max_turns: 10` delivered
    /// five exchanges while reading as "ten steps" — the constant was measured in a unit nobody
    /// reasons in, and that is what hid the eviction problem.
    ///
    /// Two behaviours beyond a plain tail:
    ///
    /// * **Pinned turns always survive** (`Turn::pinned`) — retention by kind, not recency.
    /// * **Eviction happens in BATCHES.** A window that slides by two turns per request changes
    ///   the prompt prefix on *every* request, which independently defeats provider prefix
    ///   caching (Gemini/OpenAI/Anthropic all key on an exact prefix). Holding the window still
    ///   and dropping a chunk only on overflow keeps the prefix byte-stable in between — the
    ///   same reasoning behind Anthropic's `clear_at_least`. Memory and caching are one fix.
    pub fn get_conversation_for_api_exchanges(&self, max_exchanges: usize) -> Vec<Message> {
        let budget = max_exchanges.saturating_mul(2).max(2);
        // Overflow is allowed to run to `budget + EVICTION_BATCH` before anything is dropped, so
        // the retained slice is identical across that whole span instead of shifting every turn.
        let keep = if self.conversation.len() > budget + EVICTION_BATCH {
            // Drop whole batches, then keep the most recent `budget`.
            budget
        } else {
            self.conversation.len()
        };
        let start = self.conversation.len().saturating_sub(keep);

        let mut messages = Vec::new();
        for (i, turn) in self.conversation.iter().enumerate() {
            // Pinned turns are emitted wherever they fall, in order, even from before the window.
            if i < start && !turn.pinned {
                continue;
            }
            match turn.role.as_str() {
                "correction" | "user" => messages.push(Message {
                    role: Role::User,
                    content: turn.content.clone(),
                }),
                "assistant" => messages.push(Message {
                    role: Role::Assistant,
                    content: turn.content.clone(),
                }),
                _ => {}
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

    fn fill(s: &mut Session, n: usize) {
        for i in 0..n {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            s.add_turn(role, format!("turn {i}"), None);
        }
    }

    #[test]
    fn conversation_window_is_measured_in_exchanges() {
        let mut s = Session::new("task".into());
        fill(&mut s, 30);
        // 5 exchanges = 10 turns of budget. Well past the eviction batch, so the tail is exact.
        let msgs = s.get_conversation_for_api_exchanges(5);
        assert_eq!(msgs.len(), 10, "5 exchanges must mean 10 turns, not 5");
        assert_eq!(msgs.last().unwrap().content, "turn 29");
    }

    /// The regression this whole design exists for: a window that slides every request changes
    /// the prompt prefix every request, which defeats provider prefix caching. Eviction must
    /// happen in batches so the retained slice is byte-identical across consecutive requests.
    #[test]
    fn eviction_is_batched_so_the_prefix_holds_still() {
        let budget = 10; // 5 exchanges
        let mut a = Session::new("task".into());
        fill(&mut a, budget + 1);
        let mut b = Session::new("task".into());
        fill(&mut b, budget + 2);

        // Both are over budget but within the eviction batch, so both still start at turn 0 —
        // adding a turn did NOT shift the front.
        assert_eq!(a.get_conversation_for_api_exchanges(5)[0].content, "turn 0");
        assert_eq!(b.get_conversation_for_api_exchanges(5)[0].content, "turn 0");

        // Past budget + EVICTION_BATCH, a whole batch is dropped at once.
        let mut c = Session::new("task".into());
        fill(&mut c, budget + EVICTION_BATCH + 1);
        let first = &c.get_conversation_for_api_exchanges(5)[0].content;
        assert_ne!(first, "turn 0", "overflow past the batch must finally evict");
    }

    #[test]
    fn pinned_turns_survive_eviction() {
        let mut s = Session::new("task".into());
        // The user's real goal, stated at turn 0 and pinned.
        s.add_turn_pinned("user", "add page numbers from page 3".into(), None, true);
        fill(&mut s, 40); // bury it far past any window
        let msgs = s.get_conversation_for_api_exchanges(5);
        assert!(
            msgs.iter().any(|m| m.content == "add page numbers from page 3"),
            "a pinned intent turn must never be evicted — it is the one thing the model \
             cannot reconstruct from a summary"
        );
        assert_eq!(msgs[0].content, "add page numbers from page 3", "and it stays first");
    }

    #[test]
    fn goal_is_mutable_but_not_erasable_by_blank_input() {
        let mut s = Session::new("help me with this document".into());
        // The real goal arrives later, as a needs_input reply.
        s.set_task_description("add page numbers from page 3".into());
        assert_eq!(s.task_description, "add page numbers from page 3");
        // A blank/whitespace answer must not wipe it.
        s.set_task_description("   ".into());
        assert_eq!(s.task_description, "add page numbers from page 3");
    }

    #[test]
    fn plan_outline_is_replaced_wholesale_not_merged() {
        let mut s = Session::new("add page numbers".into());
        s.set_plan_outline(vec!["Open Insert tab".into(), "Add page numbers".into()]);
        // The model revises its route (e.g. the user asked to also centre them) —
        // the new list must fully replace the old one, not accumulate alongside it.
        s.set_plan_outline(vec![
            "Open Insert tab".into(),
            "Add page numbers".into(),
            "Centre them at the bottom".into(),
        ]);
        assert_eq!(s.plan_outline.len(), 3, "revision replaces, it doesn't append");
    }

    #[test]
    fn correction_turns_map_to_user_role() {
        let mut s = Session::new("task".into());
        s.add_turn("user", "do the thing".into(), None);
        s.add_turn("assistant", "click X".into(), None);
        s.add_turn("correction", "that was wrong".into(), None);
        let msgs = s.get_conversation_for_api_exchanges(5);
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
        assert_eq!(s.get_conversation_for_api_exchanges(5).len(), 1);
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
