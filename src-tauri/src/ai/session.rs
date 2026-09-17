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

/// How many finished sessions are kept on disk. Twenty is a starting number, not a
/// derived one: it is about two weeks of the founder's own use, it fits a list the user
/// can read without searching (§8 of the plan: "20 sessions do not need search"), and it
/// is small enough that `list_sessions` parsing all of them stays free.
pub const SESSION_HISTORY_KEEP: usize = 20;

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
    /// What the user actually clicked to produce this turn, as a resolved control
    /// (`Button "Insert"`) from `last_click`. Stored so a reopened session shows the same
    /// thing the live one did -- the row records an action, and the action is the click,
    /// not the sentence we asked for. `#[serde(default)]` so older turns load without it.
    ///
    /// Not serialized when absent, so the key's presence in a file is itself the fact:
    /// `grep clicked` finds the turns where the user actually did something, instead of
    /// matching a `null` on every assistant turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicked: Option<String>,
    /// What moved the session on when the user did not click in the guided app:
    /// `next` (the button, hotkey or menu), `autopilot` (a screen change advanced it) or
    /// `already_done` (the user said the step was already satisfied). Stored for the same
    /// reason `clicked` is -- so a reopened row does not claim an action the user never
    /// took, and Autopilot does not get credited to them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_by: Option<String>,
    /// File name of this turn's frame inside the session's own frame directory, when the
    /// user asked for screenshots to be kept (plan §4). A name rather than a path: the
    /// directory is the manager's business, and moving a session to the archive must not
    /// have to rewrite every turn. `None` for every turn captured while the setting was off,
    /// which is every turn of every session stored before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
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
    /// How many leading `plan_outline` milestones are done — index of the CURRENT
    /// one (0-based). Always in `0..=plan_outline.len()`; see `set_plan_completed_count`.
    #[serde(default)]
    pub plan_completed_count: usize,
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
            plan_completed_count: 0,
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
            // Filled in right after by `set_last_user_turn_facts`, which has the hook's
            // answer and the frontend's; a turn is built by its caller, those facts are not.
            clicked: None,
            advanced_by: None,
            frame: None,
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
        // A revision can shrink the list out from under a previously-valid progress
        // count (or grow it — either way the old count is still a safe lower bound).
        self.plan_completed_count = self.plan_completed_count.min(self.plan_outline.len());
    }

    /// How far along `plan_outline` the model says the session has progressed.
    /// Clamped to the list's current length so a stale/overcounted value from the
    /// model can never index past the end — the frontend trusts this number as an
    /// in-bounds array index/highlight cutoff without re-checking it.
    pub fn set_plan_completed_count(&mut self, count: usize) {
        self.plan_completed_count = count.min(self.plan_outline.len());
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

    /// Record the observable facts about the user turn already in the conversation: what
    /// they clicked, if anything, and what advanced the step when they did not.
    ///
    /// Set after the fact rather than passed to `add_turn` for two reasons: only USER turns
    /// carry these (the assistant turn that follows must not inherit them), and the click
    /// comes from the click hook rather than from the caller building the turn. Searches
    /// backwards for the user turn, so the order of pushes here cannot silently attach them
    /// to the wrong side.
    pub fn set_last_user_turn_facts(
        &mut self,
        clicked: Option<String>,
        advanced_by: Option<String>,
        frame: Option<String>,
    ) {
        if let Some(turn) = self.conversation.iter_mut().rev().find(|t| t.role == "user") {
            turn.clicked = clicked;
            turn.advanced_by = advanced_by;
            turn.frame = frame;
        }
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

    /// Read a stored session and make it the live one.
    ///
    /// **The in-flight step state is dropped on the way in.** A session is not a
    /// document — it is a position in a task, on a machine whose screen has since
    /// changed. `current_step_sequence` and `current_step_index` describe a screen that
    /// no longer exists, and restoring them would have the app advance through steps
    /// against windows that may not be open. The next request re-captures and re-plans.
    /// What is kept is what gives the model context to carry on: the conversation, the
    /// task, the plan outline and the state summary.
    ///
    /// The caller is responsible for the parts that live outside the session — the
    /// stored target HWND and the export ring. See `resume_session` in `lib.rs`.
    pub fn load_session(&mut self, session_id: &str) -> Option<Session> {
        let file_path = self.session_dir.join(format!("{}.json", session_id));
        let content = fs::read_to_string(file_path).ok()?;
        let mut session = serde_json::from_str::<Session>(&content).ok()?;
        session.current_step_sequence.clear();
        session.current_step_index = 0;
        // Resuming is the user saying "I am working on this now", so it moves to the top
        // of the list and out of the prune's reach. Without this a session you reopened
        // and then left for an hour could be retired while it was on screen, because
        // only `add_turn` moves the stamp and reading one adds no turn.
        session.last_active_at = Local::now().to_rfc3339();
        self.current_session = Some(session.clone());
        self.save_session(Some(&session));
        Some(session)
    }

    /// Where one session's frames live: `sessions/frames/<id>/`.
    ///
    /// A directory beside the session files rather than inside a per-session folder, because
    /// the JSON layout stays flat (every reader and writer here assumes it) and because this
    /// gives `prune` exactly one extra thing to move or delete when a session is retired.
    pub fn frames_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir.join("frames").join(session_id)
    }

    /// Write one frame, named for the turn it belongs to. Returns the file name to record on
    /// that turn; `None` when it could not be written — a frame that fails to save must never
    /// cost the session it belongs to, and the caller has nothing better to do than carry on.
    pub fn save_frame(&self, session_id: &str, turn_index: usize, png: &[u8]) -> Option<String> {
        let dir = self.frames_dir(session_id);
        fs::create_dir_all(&dir).ok()?;
        let name = format!("{turn_index}.png");
        fs::write(dir.join(&name), png).ok()?;
        Some(name)
    }

    /// How many session files exist, without parsing any of them.
    fn count_sessions(&self) -> usize {
        fs::read_dir(&self.session_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Every stored session, newest first, as the summaries a list needs.
    ///
    /// Ordered by `last_active_at` read from INSIDE each file rather than by the file's
    /// mtime: a backup or sync client rewrites mtime, which would silently reorder
    /// "recent" and — once `prune` uses the same order — retire the wrong ones. mtime is
    /// only the fallback for a file whose stamp will not parse.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut out: Vec<SessionSummary> = Vec::new();
        let Ok(entries) = fs::read_dir(&self.session_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            // A half-written or hand-edited file is skipped, not fatal: one bad file
            // must not cost the user the whole list.
            let Ok(session) = serde_json::from_str::<Session>(&text) else {
                continue;
            };
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let sort_key = chrono::DateTime::parse_from_rfc3339(&session.last_active_at)
                .map(|t| t.timestamp())
                .unwrap_or(mtime);
            out.push(SessionSummary {
                sort_key,
                id: session.id.to_string(),
                task_description: session.task_description.clone(),
                summary_text: session
                    .current_state_summary
                    .as_ref()
                    .map(|s| s.summary_text.clone()),
                turns: session.conversation.len(),
                last_active_at: session.last_active_at.clone(),
            });
        }
        out.sort_by(|a, b| b.sort_key.cmp(&a.sort_key).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// Keep the `keep` most recent sessions; retire the rest.
    ///
    /// **The first run archives instead of deleting.** Going from an unbounded store to a
    /// bounded one destroys whatever was already there, and export does not exist yet, so
    /// there would be no way to get any of it back. Surplus moves to `archive-<date>/`
    /// once, guarded by a marker file; every later prune deletes normally. That way the
    /// irreversible step is taken by a person emptying that folder, not by an app update.
    ///
    /// Returns (deleted, archived).
    pub fn prune(&self, keep: usize) -> (usize, usize) {
        // Cheap gate first. This is called after every save, and the answer is almost
        // always "nothing to do" — counting directory entries costs one syscall walk,
        // where `list_sessions` reads and parses every file, on the guidance hot path
        // while the router lock is held.
        if self.count_sessions() <= keep {
            return (0, 0);
        }
        let all = self.list_sessions();
        if all.len() <= keep {
            return (0, 0);
        }
        // Never retire the live session. Prune runs on the same timeline as the thing
        // writing these files, and deleting the one in use is a self-inflicted bug.
        let active = self.current_session.as_ref().map(|s| s.id.to_string());

        let marker = self.session_dir.join(".pruned");
        let archive_dir = (!marker.exists()).then(|| {
            self.session_dir
                .join(format!("archive-{}", Local::now().format("%Y-%m-%d")))
        });
        if let Some(dir) = &archive_dir {
            if let Err(e) = fs::create_dir_all(dir) {
                // Cannot archive => do not delete. Losing the sessions is the worse
                // outcome; carrying too many for one more launch is the better one.
                log::warn!("[sessions] first prune skipped, cannot create {dir:?}: {e}");
                return (0, 0);
            }
        }

        let (mut deleted, mut archived) = (0usize, 0usize);
        for s in all.iter().skip(keep) {
            if active.as_deref() == Some(s.id.as_str()) {
                continue;
            }
            let path = self.session_dir.join(format!("{}.json", s.id));
            let frames = self.frames_dir(&s.id);
            match &archive_dir {
                Some(dir) => match fs::rename(&path, dir.join(format!("{}.json", s.id))) {
                    Ok(()) => {
                        archived += 1;
                        // The pictures belong to the session, so they move with it: an archive
                        // holding transcripts whose frames were deleted is not an archive.
                        if frames.is_dir() {
                            let dest = dir.join("frames");
                            let _ = fs::create_dir_all(&dest);
                            if let Err(e) = fs::rename(&frames, dest.join(&s.id)) {
                                log::warn!(
                                    "[sessions] archived {} but not its frames: {e}",
                                    s.id
                                );
                            }
                        }
                    }
                    Err(e) => log::warn!("[sessions] could not archive {path:?}: {e}"),
                },
                None => match fs::remove_file(&path) {
                    Ok(()) => {
                        deleted += 1;
                        // Orphaned frames would outlive the transcript with nothing pointing
                        // at them -- invisible disk creep, and the reason prune knows about
                        // this directory at all.
                        if frames.is_dir() {
                            let _ = fs::remove_dir_all(&frames);
                        }
                    }
                    Err(e) => log::warn!("[sessions] could not remove {path:?}: {e}"),
                },
            }
        }
        if let Some(dir) = &archive_dir {
            let _ = fs::write(&marker, "sessions pruned at least once\n");
            log::info!(
                "[sessions] first prune: {} of {} moved to {} rather than deleted",
                archived,
                all.len(),
                dir.display()
            );
        } else if deleted > 0 {
            log::info!(
                "[sessions] pruned {deleted} of {}, keeping the most recent {keep}",
                all.len()
            );
        }
        (deleted, archived)
    }
}

/// One row of the history list. Deliberately not the whole `Session`: the list renders
/// twenty of these and never needs the conversation bodies.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub task_description: String,
    /// The model's own running summary — the obvious second line for a row.
    pub summary_text: Option<String>,
    pub turns: usize,
    pub last_active_at: String,
    /// Epoch seconds parsed from `last_active_at`, or mtime if that failed. Internal:
    /// the UI sorts by nothing, it renders the order it is given.
    #[serde(skip)]
    sort_key: i64,
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
    fn plan_completed_count_is_clamped_to_outline_length() {
        let mut s = Session::new("add page numbers".into());
        s.set_plan_outline(vec!["Open Insert tab".into(), "Add page numbers".into()]);
        // A model that overcounts (or a stale count from before) must never index
        // past the end of the list.
        s.set_plan_completed_count(5);
        assert_eq!(s.plan_completed_count, 2);
    }

    #[test]
    fn plan_completed_count_is_reclamped_when_outline_shrinks() {
        let mut s = Session::new("add page numbers".into());
        s.set_plan_outline(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        s.set_plan_completed_count(3);
        // The model revises down to a shorter route — the old count must not
        // survive pointing past the new, shorter list.
        s.set_plan_outline(vec!["a".into(), "b".into()]);
        assert_eq!(s.plan_completed_count, 2);
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

    /// A `SessionManager` over a fresh temp dir, plus that dir's path.
    fn temp_manager(tag: &str) -> (SessionManager, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "navisual-session-test-{}-{}",
            tag,
            Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&dir);
        (SessionManager::new(dir.clone()), dir)
    }

    /// Write a session whose `last_active_at` is `minutes_ago`, and whose file mtime is
    /// therefore NOT in the same order (every file is written now, newest-mtime-last).
    fn write_session(mgr: &SessionManager, task: &str, minutes_ago: i64) -> Uuid {
        let mut session = Session::new(task.to_string());
        session.last_active_at = (Local::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339();
        mgr.save_session(Some(&session));
        session.id
    }

    #[test]
    fn click_attaches_to_the_user_turn_not_the_assistant_one() {
        let mut session = Session::new("task".to_string());
        session.add_turn_pinned("user", "[User completed: \"Click Save\"]".to_string(), None, false);
        session.add_turn("assistant", "next step".to_string(), None);
        session.set_last_user_turn_facts(
            Some("Button \"Save\"".to_string()),
            Some("next".to_string()),
            Some("4.png".to_string()),
        );

        assert_eq!(
            session.conversation[0].clicked.as_deref(),
            Some("Button \"Save\""),
            "the click belongs to what the user did"
        );
        assert_eq!(
            session.conversation[1].clicked, None,
            "the assistant turn the app wrote must not inherit it"
        );
        assert_eq!(
            session.conversation[1].advanced_by, None,
            "and must not inherit what advanced the step either"
        );
        assert_eq!(session.conversation[0].advanced_by.as_deref(), Some("next"));
        assert_eq!(session.conversation[0].frame.as_deref(), Some("4.png"));
        assert_eq!(
            session.conversation[1].frame, None,
            "and the assistant turn gets no frame either"
        );
    }

    #[test]
    fn a_turn_without_a_click_does_not_write_the_key() {
        // The key's presence has to mean "the user clicked something" -- otherwise every
        // assistant turn in every file carries `"clicked":null` and grepping for the real
        // ones is useless.
        let mut session = Session::new("task".to_string());
        session.add_turn("assistant", "next step".to_string(), None);
        let json = serde_json::to_string(&session).expect("serialises");
        assert!(
            !json.contains("clicked"),
            "an absent click must not be written at all"
        );
    }

    #[test]
    fn turns_written_before_the_click_field_still_load() {
        // A stored turn without `clicked` — the shape every session on disk has today.
        let json = r#"{"role":"user","content":"hi","screenshot_hash":null,"timestamp":"2026-09-16T00:00:00-07:00","pinned":false}"#;
        let turn: Turn = serde_json::from_str(json).expect("older turns must still load");
        assert_eq!(turn.clicked, None);
    }

    #[test]
    fn a_frame_is_written_under_the_session_that_owns_it() {
        let (mgr, dir) = temp_manager("frame-write");
        let id = write_session(&mgr, "task", 10).to_string();
        let name = mgr.save_frame(&id, 3, b"png-bytes").expect("frame is written");

        assert_eq!(name, "3.png", "named for the turn it belongs to");
        assert_eq!(fs::read(mgr.frames_dir(&id).join(&name)).unwrap(), b"png-bytes");
        // Beside the session files, never among them: every reader here walks `*.json`.
        assert_eq!(mgr.list_sessions().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_retired_session_takes_its_frames_with_it() {
        let (mgr, dir) = temp_manager("frame-prune");
        for i in 0..5 {
            let id = write_session(&mgr, &format!("task {i}"), 500 - i).to_string();
            mgr.save_frame(&id, 0, b"x").expect("frame");
        }
        // Marker present => the delete path, not the one-time archive.
        fs::write(dir.join(".pruned"), "already").unwrap();
        mgr.prune(2);

        let kept = fs::read_dir(dir.join("frames")).unwrap().count();
        assert_eq!(
            kept, 2,
            "an orphaned frame would outlive its transcript with nothing pointing at it"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_first_prune_archives_frames_with_their_session() {
        let (mgr, dir) = temp_manager("frame-archive");
        for i in 0..5 {
            let id = write_session(&mgr, &format!("task {i}"), 500 - i).to_string();
            mgr.save_frame(&id, 0, b"x").expect("frame");
        }
        let (deleted, archived) = mgr.prune(2);
        assert_eq!((deleted, archived), (0, 3));

        let archive = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("archive-"))
            .expect("an archive directory");
        assert_eq!(
            fs::read_dir(archive.join("frames")).unwrap().count(),
            3,
            "the pictures move with their sessions -- an archive without them is not one"
        );
        assert_eq!(fs::read_dir(dir.join("frames")).unwrap().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn list_sessions_orders_by_stamp_not_mtime() {
        let (mgr, dir) = temp_manager("order");
        // Written oldest-stamp-first, so mtime order is the REVERSE of the right answer.
        write_session(&mgr, "oldest", 300);
        write_session(&mgr, "middle", 200);
        write_session(&mgr, "newest", 100);

        let listed = mgr.list_sessions();
        let tasks: Vec<&str> = listed.iter().map(|s| s.task_description.as_str()).collect();
        assert_eq!(tasks, vec!["newest", "middle", "oldest"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn list_sessions_skips_unparseable_files_without_losing_the_rest() {
        let (mgr, dir) = temp_manager("garbage");
        write_session(&mgr, "good", 10);
        fs::write(dir.join("not-a-session.json"), "{ half-written").unwrap();

        let listed = mgr.list_sessions();
        assert_eq!(listed.len(), 1, "one bad file must not cost the whole list");
        assert_eq!(listed[0].task_description, "good");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn first_prune_archives_and_later_prunes_delete() {
        let (mgr, dir) = temp_manager("archive");
        for i in 0..5 {
            write_session(&mgr, &format!("task {i}"), 500 - i);
        }

        // First run: nothing is destroyed, the surplus is moved.
        let (deleted, archived) = mgr.prune(2);
        assert_eq!((deleted, archived), (0, 3));
        assert_eq!(mgr.list_sessions().len(), 2);
        let archive: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(archive.len(), 1, "exactly one archive dir");
        assert_eq!(fs::read_dir(archive[0].path()).unwrap().count(), 3);

        // Second run, now over the limit again: the marker means these really go.
        for i in 0..3 {
            write_session(&mgr, &format!("later {i}"), 100 - i);
        }
        let (deleted, archived) = mgr.prune(2);
        assert_eq!((deleted, archived), (3, 0));
        assert_eq!(mgr.list_sessions().len(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_never_retires_the_active_session() {
        let (mut mgr, dir) = temp_manager("active");
        // The active session is the OLDEST, so ordering alone would retire it first.
        let mut active = Session::new("the live one".to_string());
        active.last_active_at = (Local::now() - chrono::Duration::minutes(900)).to_rfc3339();
        mgr.save_session(Some(&active));
        let active_id = active.id;
        mgr.current_session = Some(active);
        for i in 0..4 {
            write_session(&mgr, &format!("task {i}"), 100 - i);
        }
        fs::write(dir.join(".pruned"), "already").unwrap();

        mgr.prune(2);
        let ids: Vec<String> = mgr.list_sessions().into_iter().map(|s| s.id).collect();
        assert!(
            ids.contains(&active_id.to_string()),
            "the session being written to must survive its own prune"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn loading_a_session_keeps_the_context_and_drops_the_screen_position() {
        let (mut mgr, dir) = temp_manager("load");
        let mut session = Session::new("rename a layer".to_string());
        session.add_turn("user", "rename a layer".to_string(), None);
        session.add_turn("assistant", "Click the Layers tab".to_string(), None);
        session.set_plan_outline(vec!["open Layers".to_string(), "rename".to_string()]);
        session.set_plan_completed_count(1);
        session.current_state_summary = Some(StateSummary {
            summary_text: "Layers panel open".to_string(),
            turn_index: 2,
        });
        session.current_step_sequence =
            vec![serde_json::from_str::<GuidanceStep>(r#"{"instruction":"Click Layers"}"#).unwrap()];
        session.current_step_index = 1;
        let id = session.id;
        mgr.save_session(Some(&session));
        mgr.current_session = None;

        let loaded = mgr.load_session(&id.to_string()).expect("stored session");

        // Kept: what lets the model carry on.
        assert_eq!(loaded.conversation.len(), 2);
        assert_eq!(loaded.task_description, "rename a layer");
        assert_eq!(loaded.plan_outline.len(), 2);
        assert_eq!(loaded.plan_completed_count, 1);
        assert_eq!(
            loaded.current_state_summary.as_ref().map(|s| s.summary_text.as_str()),
            Some("Layers panel open")
        );
        // Dropped: what describes a screen that no longer exists.
        assert!(loaded.current_step_sequence.is_empty());
        assert_eq!(loaded.current_step_index, 0);
        // And it is the live session now, with the drop persisted rather than in-memory
        // only — a reload must not resurrect the stale position.
        assert_eq!(mgr.current_session.as_ref().map(|s| s.id), Some(id));
        let again = mgr.load_session(&id.to_string()).expect("still there");
        assert!(again.current_step_sequence.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resuming_moves_a_session_out_of_the_prune_s_reach() {
        let (mut mgr, dir) = temp_manager("resume-order");
        let stale = write_session(&mgr, "the old one", 5000);
        for i in 0..3 {
            write_session(&mgr, &format!("task {i}"), 100 - i);
        }
        assert_eq!(mgr.list_sessions().last().unwrap().id, stale.to_string());

        mgr.load_session(&stale.to_string()).expect("stored session");

        assert_eq!(
            mgr.list_sessions().first().unwrap().id,
            stale.to_string(),
            "reopening a session is the user saying they are working on it now"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_under_the_limit_does_nothing() {
        let (mgr, dir) = temp_manager("noop");
        write_session(&mgr, "only", 10);
        assert_eq!(mgr.prune(20), (0, 0));
        assert!(
            !dir.join(".pruned").exists(),
            "a no-op prune must not burn the one-time archive grace"
        );
        assert_eq!(mgr.list_sessions().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
