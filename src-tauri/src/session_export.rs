//! Session export — the ring buffer and the folder writer.
//!
//! Design: `navisual-internal/docs/session-export-design.md`. The parts of that
//! document this module implements, and the reasons they are shaped this way:
//!
//! - **§0.7 — a session is a conversation, not a step list.** The buffer stores
//!   [`Turn`]s, each pairing what the user did with what Navisual answered. One
//!   answer can carry several steps, so turn → step is 1:N. A flat step list
//!   would discard which steps came from which exchange, and every word the user
//!   typed. Both are unrecoverable once flattened, which is why the shape has to
//!   be right at capture time rather than at export time.
//!
//! - **§0.4 — clean frames, pointer composited late.** Frames are stored WITHOUT
//!   the pointer drawn on them, together with a [`PointerState`]. A pointer burned
//!   in at capture time cannot be moved or removed, which makes the wrong-pointer
//!   step — the one that most needs rescuing — the only unfixable one. Compositing
//!   at export time keeps every state editable and costs nothing extra on disk,
//!   because the clean frame never leaves memory.
//!
//! - **§3.1 — 30 steps, in memory, always on.** Retention is capped by STEP count
//!   rather than turn count, because steps are what own the screenshots and
//!   therefore the memory. Measured at AI-JPEG size this is a few MB, less than the
//!   one 1080p RGBA buffer the OCR path already allocates and discards on every
//!   locate.
//!
//! Nothing here writes to disk until [`ExportBuffer::write_folder`] is called.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::capture::Rect;

/// Retained steps. §3.1's founder call. Counted in steps, not turns, because
/// steps hold the frames and therefore all of the memory.
pub const MAX_STEPS: usize = 30;

// ---------------------------------------------------------------------------
// Conversation model (§0.7)
// ---------------------------------------------------------------------------

/// What the user did to produce a turn.
///
/// The distinction that matters most here is `typed`: a `Next` turn carries a
/// synthesized `[User completed: "..."]` string that the APP composed, not words
/// the person wrote. The session-memory work (`memory-management-plan.md`) hit
/// exactly this — turn-pinning matched 18 of 36 turns because it could not tell a
/// machine continuation string from real input. An exporter that gets this wrong
/// puts the app's own words into an article as if the reader had said them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputKind {
    /// The initial request that started the session.
    Task,
    /// Free text typed mid-session.
    Followup,
    /// An answer to a `needs_input` question.
    Reply,
    /// ✗ Wrong, with a reason chip.
    Correction,
    /// → Next or its hotkey. Nothing was typed.
    Next,
    /// The screen changed enough to auto-advance. **Not a human confirmation** —
    /// the v0.7.3 audit (finding C3) already ruled this for the per-model success
    /// metric, for the same reason: automation is not a person agreeing.
    Autopilot,
}

impl UserInputKind {
    /// Whether a human actually produced words for this turn. False for `Next`
    /// (synthesized continuation string) and `Autopilot` (no human at all).
    pub fn is_typed(self) -> bool {
        matches!(self, Self::Task | Self::Followup | Self::Reply | Self::Correction)
    }

    /// Whether this turn represents a person confirming anything.
    pub fn is_human(self) -> bool {
        !matches!(self, Self::Autopilot)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInput {
    pub kind: UserInputKind,
    /// What the user wrote, when they wrote anything. `None` for `Next`/`Autopilot`
    /// rather than the synthesized string, so no consumer can mistake one for the
    /// other by reading this field alone.
    pub text: Option<String>,
    /// `false` whenever `text` did not come from a keyboard. Redundant with `kind`
    /// by construction, and stored anyway: it is the field a consumer will reach
    /// for, and deriving it wrongly is the exact bug described on `UserInputKind`.
    pub typed: bool,
    /// The ✗ Wrong reason chip (`wrong_spot`, `cant_find`, …) for corrections.
    pub reason: Option<String>,
}

impl UserInput {
    pub fn new(kind: UserInputKind, text: Option<String>) -> Self {
        Self { typed: kind.is_typed() && text.is_some(), kind, text, reason: None }
    }

    pub fn correction(text: Option<String>, reason: Option<String>) -> Self {
        Self {
            kind: UserInputKind::Correction,
            typed: text.is_some(),
            text,
            reason,
        }
    }
}

/// Where the pointer ended up for one step. §0.4's four states.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PointerState {
    /// A pass located the control. Rect is frame-relative pixels.
    Hit { rect: [i32; 4] },
    /// No pass located it, but the model's bbox was trusted, so a looser ring drew.
    Hint { rect: [i32; 4] },
    /// Nothing was drawn. Recoverable: the export preview can place one (§0.4).
    Miss,
    /// The locator DID find the control, but it sits outside this frame — on
    /// another monitor, typically a dialog that opened over there.
    ///
    /// Distinct from `Miss` because the record must not claim the locator failed
    /// when it succeeded. Live 2026-09-04: a File Explorer session on a negative-x
    /// secondary monitor produced a step logged `pointer: miss` alongside
    /// `locator: HitA11y`, which is a straight contradiction for anyone reading
    /// the export. Nothing is drawn either way — a clamped marker would be a
    /// confident pointer at the wrong control (§4.3).
    OffFrame,
    /// The user rejected a pointer and a retry produced a different one.
    ///
    /// **Both rects are kept deliberately.** The rejected one is not noise: a
    /// control the locator got wrong once is usually one that looks like something
    /// else on screen, which is exactly the ambiguity an article should warn about.
    Corrected { rejected: [i32; 4], accepted: Option<[i32; 4]> },
}

impl PointerState {
    /// The rect to draw, if any. `Corrected` draws the accepted one.
    pub fn draw_rect(&self) -> Option<[i32; 4]> {
        match self {
            Self::Hit { rect } | Self::Hint { rect } => Some(*rect),
            Self::Corrected { accepted, .. } => *accepted,
            Self::Miss | Self::OffFrame => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Hit { .. } => "hit",
            Self::Hint { .. } => "hint",
            Self::Miss => "miss",
            Self::OffFrame => "off-screen",
            Self::Corrected { .. } => "corrected",
        }
    }
}

/// A stored screenshot: clean (no pointer), panel included, already downscaled.
///
/// `app_rect` is what makes §0.4's "panel optional per screenshot" work from a
/// single stored image. The frame is captured wide enough to include the Navisual
/// panel; cropping to `app_rect` at export time removes it. One frame serves both
/// the panel shot and the tight shot, so nothing is captured twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// JPEG bytes. Never written to disk by this struct's own accord.
    #[serde(skip)]
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The target application's window, in frame-relative pixels. `None` when the
    /// window could not be resolved into the frame.
    pub app_rect: Option<[i32; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportStep {
    pub instruction: String,
    pub target_text: Option<String>,
    pub target_role: Option<String>,
    pub clipboard: Option<String>,
    pub checkpoint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    pub pointer: PointerState,
    /// Locator decision string (`hit_selection`, `hit_a11y`, `miss`, …) and timing.
    pub locator_decision: Option<String>,
    pub locator_ms: Option<u64>,
    /// Set in the export preview. A redacted step keeps its structure and loses its
    /// image, so the step numbering a reader sees never develops holes (§4.4).
    pub redacted: bool,
}

impl ExportStep {
    pub fn new(instruction: String) -> Self {
        Self {
            instruction,
            target_text: None,
            target_role: None,
            clipboard: None,
            checkpoint: true,
            frame: None,
            pointer: PointerState::Miss,
            locator_decision: None,
            locator_ms: None,
            redacted: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistantTurn {
    pub instruction: String,
    pub state_summary: String,
    /// Model-owned session memory (v0.7.14/0.7.15). Carried because it is the
    /// model's own account of what it was trying to do, which is the closest thing
    /// a session has to an outline.
    pub goal: String,
    pub plan_outline: Vec<String>,
    pub plan_completed_count: usize,
    pub needs_input: bool,
    pub steps: Vec<ExportStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub n: usize,
    pub user: UserInput,
    pub assistant: AssistantTurn,
}

// ---------------------------------------------------------------------------
// The buffer
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ExportBuffer {
    turns: VecDeque<Turn>,
    next_n: usize,
    /// App identity, captured once from the first turn that knows it.
    pub app_name: Option<String>,
    pub app_exe: Option<String>,
    pub window_title: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl ExportBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total steps currently retained — the quantity [`MAX_STEPS`] caps.
    pub fn step_count(&self) -> usize {
        self.turns.iter().map(|t| t.assistant.steps.len()).sum()
    }

    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    pub fn turns(&self) -> &VecDeque<Turn> {
        &self.turns
    }

    /// Mutable access, for the export preview applying redaction (§4.4).
    pub fn turns_mut(&mut self) -> &mut VecDeque<Turn> {
        &mut self.turns
    }

    /// Drop everything. §3.1 requires this be an explicit rule rather than an
    /// accident: new session, or the user asking. Without a stated rule, thirty
    /// images of somebody's work sit in RAM until the process exits.
    pub fn clear(&mut self) {
        self.turns.clear();
        self.next_n = 0;
        self.app_name = None;
        self.app_exe = None;
        self.window_title = None;
    }

    /// Append a turn and evict from the front until the step budget is met.
    ///
    /// Eviction is whole-turn, never partial. Half a turn is a conversation with a
    /// question and no answer, which is worse than not having it — and the oldest
    /// turn is also the least likely to be the one worth publishing.
    pub fn push(&mut self, user: UserInput, assistant: AssistantTurn) -> usize {
        self.next_n += 1;
        let n = self.next_n;
        self.turns.push_back(Turn { n, user, assistant });
        while self.step_count() > MAX_STEPS && self.turns.len() > 1 {
            self.turns.pop_front();
        }
        n
    }

    /// Attach the locate outcome to a specific step of the most recent turn.
    /// `step_idx` is needed because one AI answer can carry several steps and the
    /// user walks through them one at a time.
    pub fn set_step_outcome(
        &mut self,
        step_idx: usize,
        frame: Option<Frame>,
        pointer: PointerState,
        decision: Option<String>,
        ms: Option<u64>,
    ) {
        let Some(turn) = self.turns.back_mut() else { return };
        let Some(step) = turn.assistant.steps.get_mut(step_idx) else { return };
        if frame.is_some() {
            step.frame = frame;
        }
        match (&step.pointer, &pointer) {
            // A retry after ✗ Wrong: the step is already marked Corrected with the
            // rejected rect held, and this is the replacement. Fill in the accepted
            // side rather than overwriting, so the rejected rect survives — it is
            // evidence of an ambiguous control, not noise (§0.4).
            (PointerState::Corrected { rejected, accepted: None }, new)
                if new.draw_rect().is_some() =>
            {
                step.pointer = PointerState::Corrected {
                    rejected: *rejected,
                    accepted: new.draw_rect(),
                };
            }
            // Never downgrade a recorded outcome to a miss: a later local re-render
            // of the same step must not erase what the first locate established.
            (existing, PointerState::Miss | PointerState::OffFrame)
                if !matches!(existing, PointerState::Miss | PointerState::OffFrame) => {}
            _ => step.pointer = pointer,
        }
        if decision.is_some() {
            step.locator_decision = decision;
        }
        if ms.is_some() {
            step.locator_ms = ms;
        }
    }

    /// Promote a step whose pointer the user rejected into `Corrected`, preserving
    /// the rejected rect (see [`PointerState::Corrected`]).
    pub fn mark_rejected(&mut self, step_idx: usize) {
        let Some(turn) = self.turns.back_mut() else { return };
        let Some(step) = turn.assistant.steps.get_mut(step_idx) else { return };
        if let Some(rect) = step.pointer.draw_rect() {
            step.pointer = PointerState::Corrected { rejected: rect, accepted: None };
        }
    }

    /// §0.7's publishability signal. A session that is nothing but a task and a run
    /// of Next turns produces a thin article, and saying so at export time is more
    /// use than discovering it after writing one.
    ///
    /// Warn, never refuse (§10): the same session is still a valid replay artifact
    /// or personal record, and neither has a publishing quality bar.
    pub fn thinness_warning(&self) -> Option<String> {
        if self.turns.is_empty() {
            return Some("Nothing recorded yet.".into());
        }
        let rich = self
            .turns
            .iter()
            .filter(|t| {
                matches!(
                    t.user.kind,
                    UserInputKind::Correction | UserInputKind::Reply | UserInputKind::Followup
                )
            })
            .count();
        if rich == 0 {
            Some(
                "This session ran straight through with no corrections, questions or follow-ups. \
                 It will export fine, but it has little to say beyond the steps themselves — \
                 the parts of an article readers value come from the places someone got stuck."
                    .into(),
            )
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Frame capture (§0.4)
// ---------------------------------------------------------------------------

/// Capture the export frame: wide enough to include the Navisual panel, clean of
/// any pointer, downscaled to the same budget as the AI image.
///
/// **Deliberately not the AI capture.** That one is cropped to the target window
/// and blanks our own panel, so it can never satisfy §0.4's "at least one shot with
/// the panel". This captures the monitor the target sits on, passing an empty
/// exclude list so nothing is blanked.
///
/// The caller is responsible for the overlay being clear — see the call site in
/// `execute_step`, which captures before drawing.
pub fn capture_frame(target_rect: Option<Rect>) -> Option<(Vec<u8>, Rect)> {
    let region = match target_rect {
        Some(r) => monitor_containing(r).unwrap_or(r),
        None => crate::capture::enumerate_monitor_rects().into_iter().next()?,
    };
    match crate::capture::capture_region_for_export(region) {
        Ok((bytes, rect)) => Some((bytes, rect)),
        Err(e) => {
            log::warn!("[export] frame capture failed: {e}");
            None
        }
    }
}

/// The monitor whose rect contains the centre of `r`, else the one it overlaps most.
fn monitor_containing(r: Rect) -> Option<Rect> {
    let monitors = crate::capture::enumerate_monitor_rects();
    if monitors.is_empty() {
        return None;
    }
    let cx = r.x + r.width as i32 / 2;
    let cy = r.y + r.height as i32 / 2;
    if let Some(m) = monitors.iter().find(|m| {
        cx >= m.x && cx < m.x + m.width as i32 && cy >= m.y && cy < m.y + m.height as i32
    }) {
        return Some(*m);
    }
    monitors.into_iter().max_by_key(|m| overlap_area(&r, m))
}

fn overlap_area(a: &Rect, b: &Rect) -> i64 {
    let x = (a.x.max(b.x), (a.x + a.width as i32).min(b.x + b.width as i32));
    let y = (a.y.max(b.y), (a.y + a.height as i32).min(b.y + b.height as i32));
    let w = (x.1 - x.0).max(0) as i64;
    let h = (y.1 - y.0).max(0) as i64;
    w * h
}

/// Convert a virtual-desktop rect into frame-relative pixels.
///
/// §4.3's portability rule: everything stored is relative to the exported
/// screenshot, never to the original monitor layout. Getting this wrong makes
/// every export machine-specific.
pub fn to_frame_coords(
    rect: Rect,
    frame_rect: Rect,
    frame_w: u32,
    frame_h: u32,
) -> Option<[i32; 4]> {
    if frame_rect.width == 0 || frame_rect.height == 0 {
        return None;
    }
    let sx = frame_w as f64 / frame_rect.width as f64;
    let sy = frame_h as f64 / frame_rect.height as f64;
    let x = ((rect.x - frame_rect.x) as f64 * sx).round() as i32;
    let y = ((rect.y - frame_rect.y) as f64 * sy).round() as i32;
    let w = (rect.width as f64 * sx).round() as i32;
    let h = (rect.height as f64 * sy).round() as i32;
    // Reject a rect that lands entirely outside the frame — a pointer on another
    // monitor is not a pointer on this screenshot, and drawing it clamped would put
    // a confident marker on the wrong control.
    if x + w <= 0 || y + h <= 0 || x >= frame_w as i32 || y >= frame_h as i32 {
        return None;
    }
    Some([x, y, w.max(1), h.max(1)])
}

// ---------------------------------------------------------------------------
// Export (§4, §9)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOptions {
    /// Crop each frame to its `app_rect`, dropping the Navisual panel. Per-step
    /// override lives in `crop_steps`; this is the default for steps not listed.
    pub crop_to_app: bool,
    /// Step indices (flattened across turns) to crop, overriding `crop_to_app`.
    pub crop_steps: Vec<usize>,
    /// Draw the pointer onto exported frames. §0.4 wants one on every screenshot;
    /// it is an option because a frame can legitimately be context-only.
    pub draw_pointer: bool,
    pub title: String,
    pub slug: String,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            crop_to_app: false,
            crop_steps: Vec::new(),
            draw_pointer: true,
            title: String::new(),
            slug: String::new(),
        }
    }
}

/// Derive a URL-safe slug. This becomes the article filename and therefore the
/// permanent URL (§0.1), which is why the export preview must show it rather than
/// deriving it silently.
pub fn slugify(s: &str) -> String {
    /// Long enough to stay readable, short enough to keep the eventual URL and
    /// the on-disk folder name sane. Truncation happens at a separator so a slug
    /// never ends mid-word.
    const MAX: usize = 60;

    let mut out = String::new();
    let mut prev_dash = true;
    for ch in s.chars().flat_map(|c| c.to_lowercase()) {
        if out.len() >= MAX {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    // Cutting at MAX can land mid-word. Back off to the last separator when doing
    // so leaves something substantial, otherwise keep the partial word.
    if out.len() >= MAX {
        if let Some(cut) = out.rfind('-') {
            if cut >= MAX / 2 {
                out.truncate(cut);
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

impl ExportBuffer {
    /// Write the export folder. Nothing in this module touches disk before this.
    ///
    /// Returns the folder actually created.
    pub fn write_folder(&self, parent: &Path, opts: &ExportOptions) -> Result<PathBuf> {
        if self.turns.is_empty() {
            return Err(anyhow!("nothing to export — no turns recorded"));
        }
        let slug = if opts.slug.is_empty() {
            slugify(&opts.title)
        } else {
            opts.slug.clone()
        };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = parent.join(format!("navisual-{stamp}-{slug}"));
        std::fs::create_dir_all(dir.join("steps"))?;

        let mut manifest = Vec::new();
        let mut flat = 0usize;
        for turn in &self.turns {
            for (si, step) in turn.assistant.steps.iter().enumerate() {
                let name = format!("{:02}-{}.png", flat + 1, slugify(&step.instruction));
                let written = if step.redacted {
                    None
                } else {
                    self.write_step_image(&dir, &name, step, flat, opts)?
                };
                manifest.push((turn.n, si, flat, written));
                flat += 1;
            }
        }

        std::fs::write(dir.join("session.json"), self.session_json(opts, &manifest)?)?;
        std::fs::write(dir.join("session.md"), self.session_md(opts, &manifest))?;
        std::fs::write(dir.join("ABOUT.txt"), about_text(&dir))?;
        Ok(dir)
    }

    /// Composite and write one step image. Returns the filename if one was written.
    fn write_step_image(
        &self,
        dir: &Path,
        name: &str,
        step: &ExportStep,
        flat_idx: usize,
        opts: &ExportOptions,
    ) -> Result<Option<String>> {
        let Some(frame) = &step.frame else { return Ok(None) };
        let mut img = image::load_from_memory(&frame.jpeg)?.to_rgba8();

        // Crop before drawing, so the pointer rect is translated once and the
        // annotation is never clipped by a crop applied after it.
        let crop = opts.crop_to_app != opts.crop_steps.contains(&flat_idx);
        let mut origin = (0i32, 0i32);
        if crop {
            if let Some([ax, ay, aw, ah]) = frame.app_rect {
                let x = ax.max(0) as u32;
                let y = ay.max(0) as u32;
                let w = (aw.max(1) as u32).min(img.width().saturating_sub(x));
                let h = (ah.max(1) as u32).min(img.height().saturating_sub(y));
                // Only reject a genuinely degenerate crop. An earlier `> 16` here
                // silently refused to crop a 16px-tall app rect and returned the
                // full frame instead, which looks like the option was ignored.
                if w >= 8 && h >= 8 {
                    img = image::imageops::crop_imm(&img, x, y, w, h).to_image();
                    origin = (x as i32, y as i32);
                }
            }
        }

        if opts.draw_pointer {
            if let Some([px, py, pw, ph]) = step.pointer.draw_rect() {
                draw_pointer(&mut img, px - origin.0, py - origin.1, pw, ph);
            }
        }

        img.save(dir.join("steps").join(name))?;
        Ok(Some(name.to_string()))
    }

    fn session_json(
        &self,
        opts: &ExportOptions,
        manifest: &[(usize, usize, usize, Option<String>)],
    ) -> Result<String> {
        let file_for = |t: usize, s: usize| {
            manifest
                .iter()
                .find(|(tn, si, _, _)| *tn == t && *si == s)
                .and_then(|(_, _, _, f)| f.clone())
                .map(|f| format!("steps/{f}"))
        };
        let turns: Vec<serde_json::Value> = self
            .turns
            .iter()
            .map(|t| {
                let steps: Vec<serde_json::Value> = t
                    .assistant
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(si, s)| {
                        serde_json::json!({
                            "instruction": s.instruction,
                            "target_text": s.target_text,
                            "target_role": s.target_role,
                            "clipboard": s.clipboard,
                            "checkpoint": s.checkpoint,
                            "screenshot": file_for(t.n, si),
                            "pointer": s.pointer,
                            "locator": { "decision": s.locator_decision, "total_ms": s.locator_ms },
                            "redacted": s.redacted,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "n": t.n,
                    "user": t.user,
                    "assistant": {
                        "instruction": t.assistant.instruction,
                        "state_summary": t.assistant.state_summary,
                        "goal": t.assistant.goal,
                        "plan_outline": t.assistant.plan_outline,
                        "plan_completed_count": t.assistant.plan_completed_count,
                        "needs_input": t.assistant.needs_input,
                        "steps": steps,
                    }
                })
            })
            .collect();

        let doc = serde_json::json!({
            "schema": 2,
            "navisual_version": env!("CARGO_PKG_VERSION"),
            "created_local": chrono::Local::now().to_rfc3339(),
            "title": opts.title,
            "slug": if opts.slug.is_empty() { slugify(&opts.title) } else { opts.slug.clone() },
            "app": {
                "name": self.app_name,
                "exe": self.app_exe,
                "window_title": self.window_title,
            },
            "provider": self.provider,
            "model": self.model,
            "turns": turns,
        });
        Ok(serde_json::to_string_pretty(&doc)?)
    }

    /// The portable Markdown artifact (§4.1). Deliberately generic — it owes
    /// nothing to navisualguide.com. The site-shaped article is a separate writing
    /// pass (§11.3), not a second renderer here.
    fn session_md(
        &self,
        opts: &ExportOptions,
        manifest: &[(usize, usize, usize, Option<String>)],
    ) -> String {
        let file_for = |t: usize, s: usize| {
            manifest
                .iter()
                .find(|(tn, si, _, _)| *tn == t && *si == s)
                .and_then(|(_, _, _, f)| f.clone())
        };
        let title = if opts.title.is_empty() { "Navisual session" } else { &opts.title };
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("title: \"{}\"
", yaml_scalar(title)));
        if let Some(a) = &self.app_name {
            out.push_str(&format!("app: \"{}\"
", yaml_scalar(a)));
        }
        out.push_str(&format!("created: {}\n", chrono::Local::now().to_rfc3339()));
        out.push_str(&format!("steps: {}\n", self.step_count()));
        out.push_str(&format!("navisual: {}\n", env!("CARGO_PKG_VERSION")));
        out.push_str("---\n\n");
        out.push_str(&format!("# {}

", yaml_scalar(title)));

        if let Some(first) = self.turns.iter().find(|t| t.user.kind == UserInputKind::Task) {
            if let Some(q) = &first.user.text {
                out.push_str(&format!("**The question:** {q}\n\n"));
            }
        }

        let mut n = 0usize;
        for turn in &self.turns {
            // A user turn that carried real words is part of the story — it is where
            // someone got stuck, which is the thing worth writing about (§0.7).
            if turn.user.typed && turn.user.kind != UserInputKind::Task {
                if let Some(text) = &turn.user.text {
                    let label = match turn.user.kind {
                        UserInputKind::Correction => turn
                            .user
                            .reason
                            .as_deref()
                            .map(|r| format!("You said this was wrong ({r})"))
                            .unwrap_or_else(|| "You said this was wrong".into()),
                        UserInputKind::Reply => "You answered".into(),
                        _ => "You asked".into(),
                    };
                    out.push_str(&format!("> **{label}:** {text}\n\n"));
                }
            }
            if !turn.user.kind.is_human() {
                // Audit finding C3: automation is not a person agreeing. Whoever
                // writes this up needs to know that nobody confirmed the previous
                // step worked -- the screen merely changed enough to advance.
                out.push_str("> *(advanced automatically by Autopilot, not confirmed by you)*

");
            }
            for (si, step) in turn.assistant.steps.iter().enumerate() {
                n += 1;
                out.push_str(&format!("## {n}. {}\n\n", step.instruction));
                match file_for(turn.n, si) {
                    Some(f) => out.push_str(&format!("![Step {n}](steps/{f})\n\n")),
                    None if step.redacted => out.push_str("*(screenshot removed)*\n\n"),
                    None => {}
                }
                if let Some(c) = &step.clipboard {
                    out.push_str(&format!("Copied to your clipboard:\n\n```\n{c}\n```\n\n"));
                }
            }
        }
        out
    }
}

/// Make a string safe inside a one-line, double-quoted YAML scalar.
///
/// Front matter is the first thing every downstream tool parses, so a stray
/// newline there does not degrade gracefully — it breaks the whole document.
/// This shipped: an app name carrying an embedded newline split `app:` across two
/// lines and produced invalid front matter (live 2026-09-04). That particular
/// string is fixed at its source too, but the writer should not depend on every
/// caller being careful with what it hands over.
fn yaml_scalar(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            // Any control character ends the scalar early or corrupts it; a space
            // is always safe and preserves word boundaries.
            '\n' | '\r' | '\t' => ' ',
            // The value is emitted inside double quotes, so only those need to go.
            '"' => '\'',
            other => other,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Draw the pointer annotation: a bright ring plus a soft halo, sized to the
/// element. Kept simple on purpose — this is a still image, not the live overlay,
/// and it has to read at both full width and click-to-enlarge (§0.4).
fn draw_pointer(img: &mut image::RgbaImage, x: i32, y: i32, w: i32, h: i32) {
    const ACCENT: [u8; 4] = [255, 107, 53, 255]; // --accent, matching the site
    let pad = 6;
    for (i, ring) in [3i32, 2, 1].into_iter().enumerate() {
        let alpha = [90u8, 170, 255][i];
        let r = [x - pad - ring * 2, y - pad - ring * 2, w + (pad + ring * 2) * 2, h + (pad + ring * 2) * 2];
        stroke_rect(img, r[0], r[1], r[2], r[3], [ACCENT[0], ACCENT[1], ACCENT[2], alpha], 2);
    }
}

fn stroke_rect(img: &mut image::RgbaImage, x: i32, y: i32, w: i32, h: i32, c: [u8; 4], t: i32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let blend = |dst: &mut image::Rgba<u8>, c: [u8; 4]| {
        let a = c[3] as f32 / 255.0;
        for (d, src) in dst.0.iter_mut().zip(c.iter()).take(3) {
            *d = (*d as f32 * (1.0 - a) + *src as f32 * a).round() as u8;
        }
    };
    for dy in 0..h {
        for dx in 0..w {
            let on_edge = dx < t || dy < t || dx >= w - t || dy >= h - t;
            if !on_edge {
                continue;
            }
            let (px, py) = (x + dx, y + dy);
            if px >= 0 && py >= 0 && px < iw && py < ih {
                blend(img.get_pixel_mut(px as u32, py as u32), c);
            }
        }
    }
}

/// The first-run default: `%USERPROFILE%\Documents\Navisual\exports`.
///
/// Documents, not AppData (§9.0). Every other file this app writes is
/// machine-local state that must not roam. An export is the opposite kind of
/// object — a document the user made. They should find it without being told a
/// path, and syncing it to their own cloud drive is a feature, not a leak.
pub fn default_destination() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Documents").join("Navisual").join("exports")
}

/// Native "choose a folder" dialog.
///
/// A dialog rather than a settings value, on purpose (§9.0). This is the one
/// feature that deliberately writes screenshots of the user's real screen to
/// disk, and a per-export choice is a stronger consent signal than a path
/// configured once and then forgotten.
#[cfg(windows)]
pub fn pick_folder(start_in: Option<PathBuf>) -> Option<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, SHCreateItemFromParsingName, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    // Its own STA thread. A modal shell dialog wants an STA, and the app's pooled
    // threads are MTA for UIA — initialising the wrong model on one of those would
    // poison it for every later locate (the class of bug fixed in 58b9f8f).
    let handle = std::thread::spawn(move || -> Option<PathBuf> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_err() {
                log::warn!("[export] CoInitializeEx for the folder picker failed: {hr:?}");
                return None;
            }
            let result = (|| {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
                dialog.SetOptions(dialog.GetOptions().ok()? | FOS_PICKFOLDERS).ok()?;
                if let Some(dir) = start_in.as_ref().filter(|d| d.exists()) {
                    let wide: Vec<u16> =
                        dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                    if let Ok(item) =
                        SHCreateItemFromParsingName::<_, _, windows::Win32::UI::Shell::IShellItem>(
                            PCWSTR(wide.as_ptr()),
                            None,
                        )
                    {
                        let _ = dialog.SetFolder(&item);
                    }
                }
                // A cancelled dialog is an Err here, which is the normal path, not a
                // failure worth logging.
                dialog.Show(None).ok()?;
                let item = dialog.GetResult().ok()?;
                let pw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let s = pw.to_string().ok()?;
                windows::Win32::System::Com::CoTaskMemFree(Some(pw.0 as *const _));
                Some(PathBuf::from(s))
            })();
            CoUninitialize();
            result
        }
    });
    handle.join().ok().flatten()
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(not(windows))]
pub fn pick_folder(_start_in: Option<PathBuf>) -> Option<PathBuf> {
    None
}

fn about_text(dir: &Path) -> String {
    format!(
        "Navisual session export\n\
         =======================\n\n\
         Folder: {}\n\n\
         WHAT THIS IS\n\
         A record of one Navisual session: what you asked, what Navisual answered,\n\
         and a screenshot for each step with the pointer drawn where it pointed.\n\n\
         WHAT IS IN IT\n\
           session.md    a readable walkthrough you can paste anywhere\n\
           session.json  the full record, including the conversation\n\
           steps/        one image per step\n\n\
         PRIVACY — READ THIS BEFORE SHARING\n\
         These screenshots are pictures of YOUR SCREEN at the moment each step ran.\n\
         They can contain file names, message contents, account names and anything\n\
         else that happened to be visible, including windows other than the app you\n\
         were working in.\n\n\
         Navisual wrote this folder because you asked it to, and wrote nothing\n\
         anywhere else. Look through steps/ before you send this to anyone.\n",
        dir.display()
    )
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> ExportStep {
        ExportStep::new(name.into())
    }

    fn turn_with(kind: UserInputKind, steps: usize) -> (UserInput, AssistantTurn) {
        (
            UserInput::new(kind, Some("text".into())),
            AssistantTurn {
                steps: (0..steps).map(|i| step(&format!("do {i}"))).collect(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn next_and_autopilot_are_not_typed() {
        // The trap from memory-management-plan.md: a Next re-query carries an
        // app-composed string, and treating it as user speech puts our words in
        // someone's mouth.
        assert!(!UserInputKind::Next.is_typed());
        assert!(!UserInputKind::Autopilot.is_typed());
        assert!(UserInputKind::Task.is_typed());
        assert!(UserInputKind::Correction.is_typed());
        // Autopilot is additionally not a human confirmation at all (audit C3).
        assert!(!UserInputKind::Autopilot.is_human());
        assert!(UserInputKind::Next.is_human());
    }

    #[test]
    fn user_input_text_none_forces_typed_false() {
        let u = UserInput::new(UserInputKind::Task, None);
        assert!(!u.typed, "no text cannot be typed text");
    }

    #[test]
    fn buffer_evicts_whole_turns_at_the_step_cap() {
        let mut b = ExportBuffer::new();
        for _ in 0..12 {
            let (u, a) = turn_with(UserInputKind::Next, 3);
            b.push(u, a);
        }
        assert!(b.step_count() <= MAX_STEPS, "step budget must hold");
        // Whole turns only — every retained turn keeps all three of its steps.
        assert!(b.turns().iter().all(|t| t.assistant.steps.len() == 3));
    }

    #[test]
    fn buffer_keeps_a_single_oversized_turn_rather_than_emptying() {
        // One answer carrying more than the cap must not evict itself into nothing;
        // an empty buffer is a worse outcome than a slightly over-budget one.
        let mut b = ExportBuffer::new();
        let (u, a) = turn_with(UserInputKind::Task, MAX_STEPS + 5);
        b.push(u, a);
        assert_eq!(b.turn_count(), 1);
        assert_eq!(b.step_count(), MAX_STEPS + 5);
    }

    #[test]
    fn an_off_frame_hit_is_not_recorded_as_a_miss() {
        // Live 2026-09-04: a step logged `pointer: miss` next to `locator: HitA11y`
        // because a dialog opened on the other monitor. The locator succeeded; the
        // screenshot simply cannot show it, and the record has to say which.
        let mut b = ExportBuffer::new();
        let (u, a) = turn_with(UserInputKind::Task, 1);
        b.push(u, a);
        b.set_step_outcome(0, None, PointerState::OffFrame, Some("HitA11y".into()), None);
        let step = &b.turns()[0].assistant.steps[0];
        assert!(matches!(step.pointer, PointerState::OffFrame));
        assert_eq!(step.pointer.label(), "off-screen");
        assert!(step.pointer.draw_rect().is_none(), "nothing may be drawn either way");
    }

    #[test]
    fn neither_miss_nor_off_frame_may_overwrite_a_recorded_hit() {
        for later in [PointerState::Miss, PointerState::OffFrame] {
            let mut b = ExportBuffer::new();
            let (u, a) = turn_with(UserInputKind::Task, 1);
            b.push(u, a);
            b.set_step_outcome(0, None, PointerState::Hit { rect: [1, 2, 3, 4] }, None, None);
            b.set_step_outcome(0, None, later.clone(), None, None);
            assert!(
                matches!(b.turns()[0].assistant.steps[0].pointer, PointerState::Hit { .. }),
                "a re-render must not erase the outcome the first locate established"
            );
        }
    }

    #[test]
    fn a_recorded_hit_is_never_downgraded_to_a_miss() {
        let mut b = ExportBuffer::new();
        let (u, a) = turn_with(UserInputKind::Task, 1);
        b.push(u, a);
        b.set_step_outcome(0, None, PointerState::Hit { rect: [1, 2, 3, 4] }, None, None);
        b.set_step_outcome(0, None, PointerState::Miss, None, None);
        assert!(matches!(
            b.turns()[0].assistant.steps[0].pointer,
            PointerState::Hit { .. }
        ));
    }

    #[test]
    fn rejecting_a_pointer_preserves_the_rejected_rect() {
        // §0.4: the rejected rect is evidence of an ambiguous control, not noise.
        let mut b = ExportBuffer::new();
        let (u, a) = turn_with(UserInputKind::Task, 1);
        b.push(u, a);
        b.set_step_outcome(0, None, PointerState::Hit { rect: [5, 5, 10, 10] }, None, None);
        b.mark_rejected(0);
        match &b.turns()[0].assistant.steps[0].pointer {
            PointerState::Corrected { rejected, accepted } => {
                assert_eq!(*rejected, [5, 5, 10, 10]);
                assert!(accepted.is_none());
            }
            other => panic!("expected Corrected, got {other:?}"),
        }
    }

    #[test]
    fn thinness_warns_on_a_straight_run_and_stays_quiet_otherwise() {
        let mut b = ExportBuffer::new();
        let (u, a) = turn_with(UserInputKind::Task, 1);
        b.push(u, a);
        for _ in 0..4 {
            let (u, a) = turn_with(UserInputKind::Next, 1);
            b.push(u, a);
        }
        assert!(b.thinness_warning().is_some(), "task + Next run is thin");

        let (u, a) = turn_with(UserInputKind::Correction, 1);
        b.push(u, a);
        assert!(b.thinness_warning().is_none(), "a correction makes it worth writing");
    }

    #[test]
    fn frame_coords_are_relative_to_the_screenshot() {
        // §4.3: portability depends entirely on this. A rect at the monitor's
        // origin must land at the frame's origin, scaled by the downscale factor.
        let frame_rect = Rect { x: 1920, y: 0, width: 1920, height: 1080 };
        let got = to_frame_coords(
            Rect { x: 1920 + 100, y: 200, width: 50, height: 20 },
            frame_rect,
            960,
            540,
        )
        .expect("inside the frame");
        assert_eq!(got, [50, 100, 25, 10]);
    }

    #[test]
    fn a_rect_on_another_monitor_yields_no_pointer() {
        // Clamping would draw a confident marker on the wrong control.
        let frame_rect = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        assert!(to_frame_coords(
            Rect { x: 3000, y: 100, width: 40, height: 20 },
            frame_rect,
            1920,
            1080
        )
        .is_none());
    }

    #[test]
    fn slugs_are_url_safe_bounded_and_never_empty() {
        assert_eq!(slugify("No Copilot tab in Word Options?"), "no-copilot-tab-in-word-options");
        assert_eq!(slugify("  ---  "), "session");
        assert!(!slugify(&"x ".repeat(200)).ends_with('-'));
        assert!(slugify(&"word ".repeat(200)).len() <= 61);
    }

    /// A real 40x30 JPEG, so `write_folder` exercises actual decode/encode rather
    /// than a stub that would hide a format bug.
    fn tiny_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([20, 20, 20, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .to_rgb8()
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .expect("encode");
        out.into_inner()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("navisual-export-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn buffer_with_one_framed_step(pointer: PointerState) -> ExportBuffer {
        let mut b = ExportBuffer::new();
        b.app_name = Some("Microsoft Word".into());
        let mut s = ExportStep::new("Click Copilot in the left sidebar".into());
        s.frame = Some(Frame {
            jpeg: tiny_jpeg(40, 30),
            width: 40,
            height: 30,
            app_rect: Some([4, 4, 20, 16]),
        });
        s.pointer = pointer;
        b.push(
            UserInput::new(UserInputKind::Task, Some("turn copilot off".into())),
            AssistantTurn { steps: vec![s], ..Default::default() },
        );
        b
    }

    #[test]
    fn write_folder_produces_the_whole_artifact() {
        let dir = temp_dir("full");
        let b = buffer_with_one_framed_step(PointerState::Hit { rect: [10, 10, 8, 6] });
        let opts = ExportOptions { title: "Turn Copilot off".into(), ..Default::default() };
        let out = b.write_folder(&dir, &opts).expect("write");

        for f in ["session.json", "session.md", "ABOUT.txt"] {
            assert!(out.join(f).is_file(), "{f} missing");
        }
        let shots: Vec<_> = std::fs::read_dir(out.join("steps")).unwrap().flatten().collect();
        assert_eq!(shots.len(), 1, "one step, one image");

        // The JSON must be valid and must carry the conversation, not just steps.
        let raw = std::fs::read_to_string(out.join("session.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(v["turns"][0]["user"]["kind"], "task");
        assert_eq!(v["turns"][0]["user"]["typed"], true);
        assert_eq!(v["turns"][0]["assistant"]["steps"][0]["pointer"]["state"], "hit");
        // §4.3: the path recorded must be the one actually written.
        let rel = v["turns"][0]["assistant"]["steps"][0]["screenshot"].as_str().unwrap();
        assert!(out.join(rel).is_file(), "recorded screenshot path must exist");

        let md = std::fs::read_to_string(out.join("session.md")).unwrap();
        assert!(md.contains("turn copilot off"), "the question belongs in the artifact");
        assert!(md.contains("Click Copilot"), "the step belongs in the artifact");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pointer_is_composited_at_export_not_burned_in_at_capture() {
        // §0.4's core decision. The stored frame is clean; drawing must happen here,
        // and turning it off must leave the pixels untouched.
        let dir = temp_dir("pointer");
        let b = buffer_with_one_framed_step(PointerState::Hit { rect: [10, 10, 8, 6] });

        let drawn = b
            .write_folder(&dir, &ExportOptions { title: "a".into(), draw_pointer: true, ..Default::default() })
            .unwrap();
        let plain = b
            .write_folder(&dir, &ExportOptions { title: "b".into(), draw_pointer: false, ..Default::default() })
            .unwrap();

        let pick = |d: &Path| {
            let f = std::fs::read_dir(d.join("steps")).unwrap().flatten().next().unwrap();
            image::open(f.path()).unwrap().to_rgba8()
        };
        assert_ne!(
            pick(&drawn).into_raw(),
            pick(&plain).into_raw(),
            "drawing the pointer must change the exported pixels"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cropping_to_the_app_rect_drops_the_panel() {
        // §0.4: one stored frame serves both the panel shot and the tight shot.
        let dir = temp_dir("crop");
        let b = buffer_with_one_framed_step(PointerState::Miss);
        let out = b
            .write_folder(&dir, &ExportOptions { title: "c".into(), crop_to_app: true, ..Default::default() })
            .unwrap();
        let f = std::fs::read_dir(out.join("steps")).unwrap().flatten().next().unwrap();
        let img = image::open(f.path()).unwrap();
        assert_eq!((img.width(), img.height()), (20, 16), "cropped to app_rect");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_redacted_step_keeps_its_place_and_loses_its_image() {
        // §4.4: structure survives redaction, so the numbering a reader sees never
        // develops holes.
        let dir = temp_dir("redact");
        let mut b = buffer_with_one_framed_step(PointerState::Hit { rect: [1, 1, 4, 4] });
        b.turns_mut()[0].assistant.steps[0].redacted = true;
        let out = b.write_folder(&dir, &ExportOptions { title: "d".into(), ..Default::default() }).unwrap();

        assert_eq!(std::fs::read_dir(out.join("steps")).unwrap().count(), 0, "no image written");
        let md = std::fs::read_to_string(out.join("session.md")).unwrap();
        assert!(md.contains("Click Copilot"), "the step itself remains");
        assert!(md.contains("screenshot removed"), "and says why the image is absent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn front_matter_survives_a_value_containing_a_newline() {
        // This shipped: an app name holding an embedded newline split `app:` over
        // two lines, producing invalid YAML at the very top of the artifact.
        let dir = temp_dir("yaml");
        let mut b = buffer_with_one_framed_step(PointerState::Miss);
        // Verbatim shape of the string that actually broke it in the field.
        b.app_name = Some("Title: 'x - File Explorer'\nRect: [-1927, 0, -475, 1087]".into());
        let out = b
            .write_folder(
                &dir,
                &ExportOptions { title: "Line one\nline two".into(), ..Default::default() },
            )
            .unwrap();

        let md = std::fs::read_to_string(out.join("session.md")).unwrap();
        let fm: Vec<&str> = md.split("---").nth(1).unwrap().trim().lines().collect();
        for line in &fm {
            assert!(
                line.contains(':'),
                "every front-matter line must be a key: value pair, got {line:?}"
            );
        }
        assert!(fm.iter().any(|l| l.starts_with("title:")));
        assert!(fm.iter().any(|l| l.starts_with("app:")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exporting_nothing_is_an_error_not_an_empty_folder() {
        let dir = temp_dir("empty");
        let b = ExportBuffer::new();
        assert!(b.write_folder(&dir, &ExportOptions::default()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_drops_everything_including_identity() {
        let mut b = ExportBuffer::new();
        b.app_name = Some("Word".into());
        let (u, a) = turn_with(UserInputKind::Task, 2);
        b.push(u, a);
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.step_count(), 0);
        assert!(b.app_name.is_none());
    }
}
