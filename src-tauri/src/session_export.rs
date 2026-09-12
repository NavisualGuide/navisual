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
    /// The locator returned a rect, but it sits outside this frame.
    ///
    /// Distinct from `Miss` because the record must not claim the locator failed
    /// when it returned something. Nothing is drawn either way — a clamped marker
    /// would be a confident pointer at the wrong control (§4.3).
    ///
    /// **This state is also a signal, not just bookkeeping.** Live 2026-09-04, a
    /// File Explorer session (window at x −1927..−475 on the secondary monitor)
    /// logged `HitA11y` for `target_text: "..."` at **x=1630, y=1048, 17×17** —
    /// on the *primary* monitor, bottom-right, and far too small to be the
    /// toolbar button the step meant. The locate did not find the target; it
    /// found some other "..." glyph entirely, and the pointer the user saw was
    /// either absent or wrong.
    ///
    /// The export did the right thing by refusing to draw it. What it also did
    /// was make a locator fault visible that leaves no other trace — the user
    /// reported the session as fine. Treat a run of `OffFrame` steps as worth
    /// investigating in the locator, not as noise in the export.
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
    /// Which mark the overlay ACTUALLY drew: `box`, `subtitle`, `hint`, `candidates`
    /// or `none` (`overlay::OverlayKind::as_str`).
    ///
    /// Recorded rather than inferred, because the frame alone cannot say why a mark
    /// looks the way it does: `box` is a located target, `hint` is the model's own
    /// bbox drawn dashed because both locator passes missed, `candidates` is a
    /// declared tie. A re-annotator needs that to redraw faithfully.
    ///
    /// It is NOT the model's old `overlay_type`, which was retired on 2026-09-11 --
    /// measured over 1,517 logged steps, that field tracked the vocabulary each
    /// provider was handed and nothing about the step. `None` here means a session
    /// exported before this field existed; it draws as a plain box, which is what
    /// every located step draws now anyway.
    pub overlay_kind: Option<String>,
    /// Frame pixels per logical pixel, for the fixed parts of the mark.
    ///
    /// The overlay draws in LOGICAL px (it divides by the canvas DPR and scales back),
    /// while this frame is in PHYSICAL pixels that may then have been downscaled on the
    /// way to disk. The located rect survives both, because `to_frame_coords` converts
    /// it with the frame; the CONSTANTS do not -- a 12 px pad is 24 physical px on a
    /// 200% display, and an exporter that drew 12 would put a half-size mark around a
    /// correctly-sized target.
    ///
    /// `monitor scale x (frame width / captured width)`. 1.0 on a 100% display with no
    /// downscale, which is why this stayed invisible until someone exported from a
    /// high-DPI laptop. Defaults to 1.0 for sessions written before it existed --
    /// exactly how they already render.
    #[serde(default = "unit_scale")]
    pub mark_scale: f32,
    /// Frame pixels at the bottom of this frame that the caption must stay clear
    /// of -- the taskbar, in practice.
    ///
    /// The overlay anchors the caption to the monitor's WORK AREA, so on screen it
    /// sits above the taskbar. A frame is a whole-monitor capture including the
    /// taskbar, so an exporter that anchors to the frame's own bottom edge draws
    /// the strip across the taskbar icons -- which is what the app was fixed for
    /// on 2026-09-07, and what the export went on doing. 0 for a session written
    /// before this field existed, or for a cropped frame that excludes it.
    #[serde(default)]
    pub caption_bottom_inset: u32,
    /// Set in the export preview. A redacted step keeps its structure and loses its
    /// image, so the step numbering a reader sees never develops holes (§4.4).
    pub redacted: bool,
}

/// How a step's mark was drawn, so a re-annotator can reproduce it.
///
/// Grouped because these two always travel together and always come from the same
/// decision -- the moment `execute_step` settles what to put on screen.
#[derive(Debug, Clone)]
pub struct DrawnAs {
    /// `overlay::OverlayKind::as_str` for the mark that was drawn.
    pub overlay_kind: Option<String>,
    /// Frame pixels per logical pixel. See [`ExportStep::mark_scale`].
    pub mark_scale: f32,
    /// Frame pixels of taskbar at the bottom. See [`ExportStep::caption_bottom_inset`].
    pub caption_bottom_inset: u32,
}

/// Default for [`ExportStep::mark_scale`] on a session written before it existed.
fn unit_scale() -> f32 {
    1.0
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
            overlay_kind: None,
            mark_scale: 1.0,
            caption_bottom_inset: 0,
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
        drawn: DrawnAs,
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
        if drawn.overlay_kind.is_some() {
            step.overlay_kind = drawn.overlay_kind;
        }
        if drawn.mark_scale.is_finite() && drawn.mark_scale > 0.0 {
            step.mark_scale = drawn.mark_scale;
        }
        step.caption_bottom_inset = drawn.caption_bottom_inset;
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
/// Read the export frame's pixels, nothing more. Must be called at the clean
/// moment (overlay down, no streamed caption); the encode that follows must not.
pub fn capture_frame_raw(target_rect: Option<Rect>) -> Option<crate::capture::RawExportFrame> {
    let region = match target_rect {
        Some(r) => monitor_containing(r).unwrap_or(r),
        None => crate::capture::enumerate_monitor_rects().into_iter().next()?,
    };
    crate::capture::capture_region_for_export_raw(region).ok()
}

/// Finish a frame started by `capture_frame_raw`. Deliberately called while the AI
/// request is already in flight, where ~364 ms of JPEG encoding costs nothing.
pub fn encode_frame(
    img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    rect: Rect,
) -> Option<(Vec<u8>, Rect)> {
    crate::capture::encode_export_frame(img)
        .ok()
        .map(|bytes| (bytes, rect))
}

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

/// Subfolder holding the untouched captures.
pub const CLEAN_DIR: &str = "steps";
/// Subfolder holding pointer/caption composites.
pub const ANNOTATED_DIR: &str = "steps-annotated";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOptions {
    /// Crop each frame to its `app_rect`, dropping the Navisual panel. Per-step
    /// override lives in `crop_steps`; this is the default for steps not listed.
    pub crop_to_app: bool,
    /// Step indices (flattened across turns) to crop, overriding `crop_to_app`.
    pub crop_steps: Vec<usize>,
    /// Write the untouched captures to `steps/`.
    ///
    /// **On by default and worth keeping on.** The clean frame is the only
    /// irreplaceable artifact here: a pointer or caption can be re-rendered from
    /// `session.json` at any time, but a screenshot that was annotated on the way
    /// out cannot be un-annotated. Keeping it is what makes annotation a
    /// re-doable post-process rather than a one-shot decision at save time.
    pub save_clean: bool,
    /// Draw the pointer into the `steps-annotated/` copies.
    pub draw_pointer: bool,
    /// Burn the step's instruction along the bottom of the annotated copies.
    pub draw_caption: bool,
    pub title: String,
    pub slug: String,
    /// The user's Pointer thickness, already mapped to a multiplier by
    /// `stroke_scale()`. The live overlay scales every stroke by this; the exporter
    /// used to hardcode 1.0, so any setting other than the default produced a mark
    /// of the wrong weight. Geometry (padding, arm length, ripple growth) is layout
    /// and deliberately does not scale -- see src/lib/overlay-weight.ts.
    pub stroke_scale: f32,
}

impl ExportOptions {
    /// Whether an annotated copy differs from the clean one. With both
    /// annotations off there is nothing to add, and writing a byte-identical
    /// second folder would only be confusing.
    pub fn wants_annotated(&self) -> bool {
        self.draw_pointer || self.draw_caption
    }
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            crop_to_app: false,
            crop_steps: Vec::new(),
            save_clean: true,
            draw_pointer: true,
            draw_caption: true,
            title: String::new(),
            slug: String::new(),
            stroke_scale: 1.0,
        }
    }
}

/// Slider position (1-10) -> stroke weight multiplier.
///
/// Must stay in step with `strokeScale` in `src/lib/overlay-weight.ts`: the exported
/// pointer and the on-screen one are supposed to be the same picture. Grouped so the
/// default divides exactly, for the same reason the TypeScript is.
pub fn stroke_scale(thickness: u32) -> f32 {
    const DEFAULT_THICKNESS: f32 = 4.0;
    let t = (thickness.clamp(1, 10)) as f32;
    if t <= DEFAULT_THICKNESS {
        0.6 + ((t - 1.0) / 3.0) * 0.4
    } else {
        1.0 + (t - DEFAULT_THICKNESS) / 6.0
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

        // Both folders are written by default. `steps/` is the archive — the only
        // thing here that cannot be regenerated — and `steps-annotated/` is a
        // derived view that `tools/annotate-session.ps1` can rebuild differently
        // at any time from the clean frames plus session.json.
        let want_annotated = opts.wants_annotated();
        if opts.save_clean {
            std::fs::create_dir_all(dir.join(CLEAN_DIR))?;
        }
        if want_annotated {
            std::fs::create_dir_all(dir.join(ANNOTATED_DIR))?;
        }
        if !opts.save_clean && !want_annotated {
            return Err(anyhow!(
                "nothing selected to save — enable the clean screenshots, the pointer, or the caption"
            ));
        }

        let mut manifest = Vec::new();
        let mut flat = 0usize;
        for turn in &self.turns {
            for (si, step) in turn.assistant.steps.iter().enumerate() {
                let name = format!("{:02}-{}.png", flat + 1, slugify(&step.instruction));
                let written = if step.redacted {
                    None
                } else {
                    self.write_step_images(&dir, &name, step, flat, opts)?
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

    /// Write one step's clean and/or annotated image.
    ///
    /// Returns the filename (shared by both folders) if anything was written, so
    /// the manifest can reference it without caring which folders exist.
    fn write_step_images(
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
        let full_h = img.height();
        let mut caption_inset = step.caption_bottom_inset;
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
                    // A crop to the app window keeps only what the app occupies, so
                    // whatever taskbar the full frame carried is no longer in this
                    // image and the caption has the real bottom edge to itself.
                    // Reduce the inset by however much was cut off the bottom.
                    let cut_from_bottom = full_h.saturating_sub(y + h);
                    caption_inset = caption_inset.saturating_sub(cut_from_bottom);
                }
            }
        }

        // The clean copy goes out first, before a single pixel is annotated.
        // Ordering matters: if annotation panics or the font is missing, the
        // irreplaceable artifact is already safe on disk.
        if opts.save_clean {
            img.save(dir.join(CLEAN_DIR).join(name))?;
        }

        if opts.wants_annotated() {
            let mut annotated = img;
            if opts.draw_pointer {
                if let Some([px, py, pw, ph]) = step.pointer.draw_rect() {
                    draw_pointer(
                        &mut annotated,
                        [px - origin.0, py - origin.1, pw, ph],
                        opts.stroke_scale,
                        step.mark_scale,
                        matches!(step.pointer, PointerState::Hint { .. }),
                    );
                }
            }
            if opts.draw_caption {
                draw_caption(&mut annotated, &step.instruction, caption_inset);
            }
            annotated.save(dir.join(ANNOTATED_DIR).join(name))?;
        }

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
                .map(|f| {
                    let folder =
                        if opts.wants_annotated() { ANNOTATED_DIR } else { CLEAN_DIR };
                    format!("{folder}/{f}")
                })
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
                            "overlay_kind": s.overlay_kind,
                            "mark_scale": s.mark_scale,
                            "caption_bottom_inset": s.caption_bottom_inset,
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
                    Some(f) => {
                        let folder =
                            if opts.wants_annotated() { ANNOTATED_DIR } else { CLEAN_DIR };
                        out.push_str(&format!("![Step {n}]({folder}/{f})

"))
                    }
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

/// The caption font, loaded once from the OS.
///
/// **Microsoft YaHei, not a bundled file and not a Latin-only bitmap.**
/// Instructions are written in the user's own language (`reply_language_directive`
/// pins the reply to it), so a caption renderer that cannot draw Chinese would be
/// unusable for a large part of the intended audience — including this project's
/// own author. YaHei covers Latin and CJK from one face, ships with every Windows
/// install since Vista, and costs nothing to bundle because we do not bundle it.
///
/// `None` when the font cannot be read, in which case captions are silently
/// skipped: a missing caption is a far better outcome than a failed export.
#[cfg(windows)]
fn caption_font() -> Option<&'static ab_glyph::FontVec> {
    use std::sync::OnceLock;
    static FONT: OnceLock<Option<ab_glyph::FontVec>> = OnceLock::new();
    FONT.get_or_init(|| {
        // Ordered by coverage, not preference: the first two are full CJK faces,
        // the rest are Latin fallbacks for a stripped-down Windows image.
        for name in ["msyh.ttc", "simsun.ttc", "segoeui.ttf", "arial.ttf", "tahoma.ttf"] {
            let path = std::path::Path::new(r"C:\Windows\Fonts").join(name);
            let Ok(bytes) = std::fs::read(&path) else { continue };
            // .ttc is a collection; index 0 is the regular face in both of ours.
            let font = if name.ends_with(".ttc") {
                ab_glyph::FontVec::try_from_vec_and_index(bytes, 0)
            } else {
                ab_glyph::FontVec::try_from_vec(bytes)
            };
            if let Ok(f) = font {
                log::debug!("[export] caption font: {name}");
                return Some(f);
            }
        }
        log::warn!("[export] no usable caption font found — captions will be skipped");
        None
    })
    .as_ref()
}

#[cfg(not(windows))]
fn caption_font() -> Option<&'static ab_glyph::FontVec> {
    None
}

/// Burn the instruction into the frame, matching the app's on-screen caption.
///
/// **Deliberately the same shape as `drawSubtitle` in `Overlay.svelte`**: a
/// rounded strip fitted to the text, centred horizontally, `rgba(0,0,0,0.52)`,
/// white centred text, sitting just above the bottom edge. A full-width
/// left-aligned band shipped first and read as a different product — someone
/// comparing an exported figure with their own screen should see the same thing.
///
/// The one deliberate divergence is size. The app draws 18 logical px because it
/// is read at 1:1 on the monitor; an exported frame is a 1920-wide image shown a
/// few hundred pixels wide in an article, so the caption is scaled to the frame
/// instead and survives that reduction.
/// `bottom_inset` is how many pixels of the frame are taskbar (or whatever else
/// the work area excludes). The live overlay anchors the caption to the WORK AREA
/// rather than the monitor, so the strip sits above the taskbar instead of across
/// its icons -- fixed in the app on 2026-09-07 after a live report, and the export
/// kept drawing over it, because it only ever knew the frame's own bottom edge.
fn draw_caption(img: &mut image::RgbaImage, text: &str, bottom_inset: u32) {
    use ab_glyph::{Font, ScaleFont};

    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let Some(font) = caption_font() else { return };

    let (w, h) = (img.width(), img.height());
    let px = (h as f32 * 0.022).clamp(14.0, 40.0);
    let scaled = font.as_scaled(px);
    let line_h = scaled.height().ceil() as i32;

    // Wrap on whole words where the language has them, and on any character
    // where it does not — CJK has no spaces, and a word-only wrapper would emit
    // one unbreakable line straight off the edge of the frame.
    // 78% of the frame, the same proportion the overlay wraps at.
    let max_w = w as f32 * 0.78;
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0.0f32;
    for ch in text.chars() {
        let adv = scaled.h_advance(font.glyph_id(ch));
        let breakable = ch == ' ' || !ch.is_ascii();
        if line_w + adv > max_w && !line.is_empty() {
            if breakable || !line.contains(' ') {
                lines.push(std::mem::take(&mut line));
                line_w = 0.0;
            } else {
                // Back up to the last space so a Latin word is not split.
                let cut = line.rfind(' ').unwrap_or(0);
                let rest = line.split_off(cut).trim_start().to_string();
                lines.push(std::mem::take(&mut line));
                line_w = rest.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum();
                line = rest;
            }
        }
        if ch == ' ' && line.is_empty() {
            continue;
        }
        line.push(ch);
        line_w += adv;
        if lines.len() >= 3 {
            break;
        }
    }
    if !line.is_empty() && lines.len() < 3 {
        lines.push(line);
    }
    if lines.is_empty() {
        return;
    }

    // Strip fitted to the widest line and centred, exactly as the overlay does —
    // a full-width band is a different visual object and reads as a different
    // product when a reader compares the figure with their own screen.
    let line_w_of = |l: &str| -> f32 {
        l.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum()
    };
    let widest = lines.iter().map(|l| line_w_of(l)).fold(0.0f32, f32::max);
    let h_pad = (px * 1.2).round() as i32;
    let v_pad = (px * 0.65).round() as i32;
    let strip_w = (widest.ceil() as i32 + h_pad * 2).min(w as i32);
    let strip_h = line_h * lines.len() as i32 + v_pad * 2;
    let strip_x = (w as i32 - strip_w) / 2;
    // Floated just clear of the bottom edge, like the overlay's 10px gap -- where
    // "the bottom" is the work area's, not the frame's.
    let bottom_gap = (px * 0.6).round() as i32;
    let floor = (h as i32 - bottom_inset as i32).max(strip_h + bottom_gap);
    let strip_y = (floor - strip_h - bottom_gap).max(0);
    let radius = (px * 0.55).round() as i32;

    // rgba(0,0,0,0.52) — the overlay's own value. Translucent on purpose: the
    // caption must not hide the part of the screen it is describing.
    fill_round_rect(img, strip_x, strip_y, strip_w, strip_h, radius, 0.52);

    for (i, l) in lines.iter().enumerate() {
        let baseline = strip_y + v_pad + line_h * i as i32 + scaled.ascent() as i32;
        // Each line centred within the strip, not left-aligned to it.
        let mut cx = strip_x as f32 + (strip_w as f32 - line_w_of(l)) / 2.0;
        for ch in l.chars() {
            let gid = font.glyph_id(ch);
            let glyph = gid.with_scale_and_position(px, ab_glyph::point(cx, baseline as f32));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    if cov <= 0.01 {
                        return;
                    }
                    let x = bounds.min.x as i32 + gx as i32;
                    let y = bounds.min.y as i32 + gy as i32;
                    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                        return;
                    }
                    let p = img.get_pixel_mut(x as u32, y as u32);
                    let a = cov.min(1.0);
                    for k in 0..3 {
                        p.0[k] = (p.0[k] as f32 * (1.0 - a) + 255.0 * a) as u8;
                    }
                });
            }
            cx += scaled.h_advance(gid);
        }
    }
}

/// The rect the MARK is built on: the located rect, padded, then floored so a tiny
/// target still gets something you can spot.
///
/// Extracted so it can be pinned by a test and quoted exactly. Must match `markRect`
/// in `src/Overlay.svelte` and the `$pad`/`$MIN_MARK` block in
/// `tools/annotate-session.ps1` -- the three are supposed to produce the same
/// picture, and `exporter_and_annotator_draw_the_same_pointer` checks two of them
/// against each other for real. Geometry, so none of it scales with stroke weight.
///
/// Returns `(px, py, pw, ph)`, centred on the located rect.
pub(crate) fn mark_rect(x: i32, y: i32, w: i32, h: i32, scale: f32) -> (f32, f32, f32, f32) {
    const MIN_MARK: f32 = 36.0;
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (bw, bh) = (w as f32, h as f32);
    // The RATIO term is scale-free -- it is a fraction of the element, which is already
    // in frame pixels. Only the fixed bounds and the floor are logical-pixel constants.
    let pad = (20.0 * s).min((12.0 * s).max(bw.min(bh) * 0.45));
    let pw = (bw + pad * 2.0).max(MIN_MARK * s);
    let ph = (bh + pad * 2.0).max(MIN_MARK * s);
    let cx = x as f32 + bw / 2.0;
    let cy = y as f32 + bh / 2.0;
    (cx - pw / 2.0, cy - ph / 2.0, pw, ph)
}

/// Bracket arm length for a mark of this size. Shared for the same reason.
pub(crate) fn mark_arm(pw: f32, ph: f32, scale: f32) -> f32 {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (26.0 * s).min((14.0 * s).max((pw * 0.38).min(ph * 0.5)))
}

/// Draw the pointer annotation, matching what the app actually puts on screen:
/// ripple rings, corner brackets, centre crosshair.
///
/// It used to be three nested rectangles, "kept simple on purpose" — and a comment
/// in `tools/annotate-session.ps1` claimed that was "the same shape the app draws",
/// which it never was. The exporter's own rule, stated beside the caption code, is
/// that someone comparing an exported figure with their own screen should see the
/// same thing; the caption honoured it and the pointer did not.
///
/// The geometry below is `Overlay.svelte`'s `drawBox`, frozen. Two deliberate
/// departures, because a still cannot lie about motion:
///   - the ripples are drawn at the three phases they occupy at t=0 (0, 1/3, 2/3),
///     which is one real frame of the animation rather than an invented one;
///   - the sweeping scan line is omitted entirely. It reads as a highlight only
///     because it moves; frozen it is just a bar across the element.
fn draw_pointer(img: &mut image::RgbaImage, rect: [i32; 4], k: f32, mark_scale: f32, hint: bool) {
    let [x, y, w, h] = rect;
    const ACCENT: [u8; 3] = [255, 107, 53];
    let (cx, cy) = (x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0);
    // The live overlay pulses on a timer; a still takes the value each pulse holds at
    // t=0, which for `(sin(0)+1)/2` is the midpoint.
    const PULSE: f32 = 0.5;
    let (px, py, pw, ph) = mark_rect(x, y, w, h, mark_scale);
    // Every fixed length below is a LOGICAL-pixel constant lifted from `drawBox`, so
    // each one needs converting into this frame's pixels. Stroke widths carry the
    // user's thickness on top; `g` is the geometry-only multiplier.
    let g = if mark_scale.is_finite() && mark_scale > 0.0 { mark_scale } else { 1.0 };
    let k = k * g;

    // ── Ripple rings ────────────────────────────────────────────────────────
    // Ellipse, not circle: a circle sized by max(bw,bh) balloons past a long thin
    // element's short axis. Growth is capped on the short axis of a wide row for
    // the same reason (drawBox carries the same two fixes).
    let base_rx = pw / 2.0 + 8.0 * g;
    let base_ry = ph / 2.0 + 8.0 * g;
    let growth = pw.min(ph) * 0.7;
    let ry_growth = if pw > ph * 2.0 { growth.min(ph * 0.4) } else { growth };
    for i in 0..3 {
        let phase = i as f32 / 3.0;
        let rx = base_rx + phase * growth;
        let ry = base_ry + phase * ry_growth;
        let alpha = ((1.0 - phase) * if hint { 0.40 } else { 0.55 } * 255.0) as u8;
        let thickness = (if hint { 2.0 - phase * 1.4 } else { 2.5 - phase * 1.8 } * k).max(1.0);
        stroke_ellipse(img, cx, cy, rx, ry, [ACCENT[0], ACCENT[1], ACCENT[2], alpha], thickness);
    }

    // ── Corner brackets ─────────────────────────────────────────────────────
    // A dark stroke under the accent one, so the mark survives on any background.
    let arm = mark_arm(pw, ph, g);
    for &(ox, oy, dx, dy) in &[
        (px, py, 1.0, 1.0),
        (px + pw, py, -1.0, 1.0),
        (px, py + ph, 1.0, -1.0),
        (px + pw, py + ph, -1.0, -1.0),
    ] {
        let layers = if hint {
            // Looser and more tentative: thinner, softer, and dashed.
            [
                ([0u8, 0, 0, 166], 4.5 * k),
                ([ACCENT[0], ACCENT[1], ACCENT[2], ((0.72 + PULSE * 0.15) * 255.0) as u8], 2.5 * k),
            ]
        } else {
            [
                ([0u8, 0, 0, 191], 5.5 * k),
                ([ACCENT[0], ACCENT[1], ACCENT[2], 255], 3.0 * k),
            ]
        };
        for (colour, t) in layers {
            if hint {
                dashed_line(img, ox + dx * arm, oy, ox, oy, colour, t, 5.0 * g, 4.0 * g);
                dashed_line(img, ox, oy, ox, oy + dy * arm, colour, t, 5.0 * g, 4.0 * g);
            } else {
                stroke_line(img, ox + dx * arm, oy, ox, oy, colour, t);
                stroke_line(img, ox, oy, ox, oy + dy * arm, colour, t);
            }
        }
        // No corner dot on a hint -- it is an exact-centre cue, and a hint has no
        // exact centre to claim.
        if !hint {
            fill_disc(img, ox, oy, 3.5 * k, [ACCENT[0], ACCENT[1], ACCENT[2], 255]);
        }
    }

    // ── Centre crosshair ────────────────────────────────────────────────────
    // The app pulses this between 0.35 and 0.60 alpha; a still takes the midpoint.
    // A hint draws no crosshair for the same reason it draws no corner dots: both are
    // exact-centre cues, and the whole point of the dashed mark is that the centre is
    // the model's estimate, not a located element.
    if !hint {
        let cr = 5.0 * g;
        let cross = [ACCENT[0], ACCENT[1], ACCENT[2], (0.475 * 255.0) as u8];
        stroke_line(img, cx - cr, cy, cx + cr, cy, cross, 1.5 * k);
        stroke_line(img, cx, cy - cr, cx, cy + cr, cross, 1.5 * k);
    }
}

/// Stroke a segment as a dash pattern, matching canvas `setLineDash([on, off])`.
#[allow(clippy::too_many_arguments)]
fn dashed_line(
    img: &mut image::RgbaImage,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    c: [u8; 4],
    t: f32,
    on: f32,
    off: f32,
) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.0 || on <= 0.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let mut at = 0.0;
    while at < len {
        let end = (at + on).min(len);
        stroke_line(img, x0 + ux * at, y0 + uy * at, x0 + ux * end, y0 + uy * end, c, t);
        at += on + off;
    }
}

/// Alpha-blend one pixel, clipped to the image.
fn blend_px(img: &mut image::RgbaImage, x: i32, y: i32, c: [u8; 4], coverage: f32) {
    if coverage <= 0.0 || x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    let a = (c[3] as f32 / 255.0) * coverage.min(1.0);
    let dst = img.get_pixel_mut(x as u32, y as u32);
    for (d, src) in dst.0.iter_mut().zip(c.iter()).take(3) {
        *d = (*d as f32 * (1.0 - a) + *src as f32 * a).round() as u8;
    }
}

/// Stroke an axis-aligned ellipse of the given thickness, anti-aliased by distance
/// to the ideal curve. Scanning the bounding box keeps it simple and the boxes here
/// are small; the normalised-radius trick avoids a per-pixel ellipse solve.
fn stroke_ellipse(
    img: &mut image::RgbaImage,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    c: [u8; 4],
    thickness: f32,
) {
    if rx <= 0.5 || ry <= 0.5 {
        return;
    }
    let half = thickness / 2.0;
    let (x0, x1) = ((cx - rx - half - 1.0) as i32, (cx + rx + half + 1.0) as i32);
    let (y0, y1) = ((cy - ry - half - 1.0) as i32, (cy + ry + half + 1.0) as i32);
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = (px as f32 + 0.5 - cx) / rx;
            let dy = (py as f32 + 0.5 - cy) / ry;
            // First-order distance to the implicit ellipse: |f| / |grad f|, where
            // f = (dx^2 + dy^2 - 1) and grad f = (2dx/rx, 2dy/ry).
            //
            // This used to scale the normalised distance by rx.min(ry), which is only
            // right where the curve runs along the LONG axis. On a wide row -- the
            // commonest target shape here -- the ends came out ~rx/ry times too thick,
            // so the exporter laid down half again as much ink as the re-annotator for
            // the same ripple. Caught by `exporter_and_annotator_draw_the_same_pointer`
            // on a 260x22 rect; invisible on anything near-square, which is why the
            // earlier by-eye comparison never showed it.
            let f = dx * dx + dy * dy - 1.0;
            let (gx, gy) = (dx / rx, dy / ry);
            let grad = 2.0 * (gx * gx + gy * gy).sqrt();
            let dist = if grad > 1e-6 { f.abs() / grad } else { f.abs() * rx.min(ry) };
            let coverage = (half + 0.5 - dist).clamp(0.0, 1.0);
            blend_px(img, px, py, c, coverage);
        }
    }
}

/// Stroke a straight segment with square caps, anti-aliased across its width.
fn stroke_line(img: &mut image::RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 4], t: f32) {
    let half = t / 2.0;
    let (minx, maxx) = (x0.min(x1) - half - 1.0, x0.max(x1) + half + 1.0);
    let (miny, maxy) = (y0.min(y1) - half - 1.0, y0.max(y1) + half + 1.0);
    let (vx, vy) = (x1 - x0, y1 - y0);
    let len2 = vx * vx + vy * vy;
    for py in miny as i32..=maxy as i32 {
        for px in minx as i32..=maxx as i32 {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            // Distance to the segment, clamped to its ends.
            let tpar = if len2 > 0.0 {
                (((fx - x0) * vx + (fy - y0) * vy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (nx, ny) = (x0 + tpar * vx, y0 + tpar * vy);
            let dist = ((fx - nx).powi(2) + (fy - ny).powi(2)).sqrt();
            let coverage = (half + 0.5 - dist).clamp(0.0, 1.0);
            blend_px(img, px, py, c, coverage);
        }
    }
}

/// Filled anti-aliased disc — the bracket's corner dot.
fn fill_disc(img: &mut image::RgbaImage, cx: f32, cy: f32, r: f32, c: [u8; 4]) {
    for py in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
        for px in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
            let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
            blend_px(img, px, py, c, (r + 0.5 - d).clamp(0.0, 1.0));
        }
    }
}

/// Darken a rounded rectangle to `alpha` black, matching the overlay's caption
/// strip. Corners are anti-aliased by sampling coverage, so the rounding reads
/// cleanly at the sizes an exported frame is viewed at.
fn fill_round_rect(
    img: &mut image::RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: i32,
    alpha: f32,
) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let r = radius.min(w / 2).min(h / 2).max(0) as f32;
    for py in y.max(0)..(y + h).min(ih) {
        for px_ in x.max(0)..(x + w).min(iw) {
            // Distance into the nearest corner's circle; 1.0 inside the straight
            // edges, tapering across the corner arc.
            let (from_l, from_r) = ((px_ - x) as f32, ((x + w - 1) - px_) as f32);
            let (from_t, from_b) = ((py - y) as f32, ((y + h - 1) - py) as f32);
            let dx = if from_l < r {
                r - from_l
            } else if from_r < r {
                r - from_r
            } else {
                0.0
            };
            let dy = if from_t < r {
                r - from_t
            } else if from_b < r {
                r - from_b
            } else {
                0.0
            };
            let cov = if dx > 0.0 && dy > 0.0 {
                (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if cov <= 0.0 {
                continue;
            }
            let a = alpha * cov;
            let p = img.get_pixel_mut(px_ as u32, py as u32);
            for k in 0..3 {
                p.0[k] = (p.0[k] as f32 * (1.0 - a)) as u8;
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
           session.md         a readable walkthrough you can paste anywhere\n\
           session.json       the full record, including the conversation\n\
           steps/             the screenshots exactly as captured\n\
           steps-annotated/   the same shots with the pointer and/or caption drawn\n\n\
         REDOING THE MARKUP\n\
         steps/ is never written on. The pointer position and the instruction text\n\
         both live in session.json, so the annotated copies can be rebuilt at any\n\
         time, with different choices, without re-running anything:\n\n\
           tools\\annotate-session.ps1 -Path \"<this folder>\"\n\n\
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
        b.set_step_outcome(0, None, PointerState::OffFrame, Some("HitA11y".into()), None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
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
            b.set_step_outcome(0, None, PointerState::Hit { rect: [1, 2, 3, 4] }, None, None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
            b.set_step_outcome(0, None, later.clone(), None, None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
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
        b.set_step_outcome(0, None, PointerState::Hit { rect: [1, 2, 3, 4] }, None, None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
        b.set_step_outcome(0, None, PointerState::Miss, None, None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
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
        b.set_step_outcome(0, None, PointerState::Hit { rect: [5, 5, 10, 10] }, None, None, DrawnAs { overlay_kind: Some("box".into()), mark_scale: 1.0, caption_bottom_inset: 0 });
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
        s.overlay_kind = Some(crate::overlay::OverlayKind::Box.as_str().to_string());
        b.push(
            UserInput::new(UserInputKind::Task, Some("turn copilot off".into())),
            AssistantTurn { steps: vec![s], ..Default::default() },
        );
        b
    }

    /// The kind vocabulary is a wire format: it is written into every exported
    /// `session.json` and read back by tools/annotate-session.ps1. Renaming a variant
    /// would silently change how an existing export re-renders, so pin all six.
    #[test]
    fn overlay_kind_wire_names_are_stable() {
        use crate::overlay::OverlayKind as K;
        assert_eq!(K::Box.as_str(), "box");
        assert_eq!(K::Subtitle.as_str(), "subtitle");
        assert_eq!(K::AppBoundary.as_str(), "app_boundary");
        assert_eq!(K::Hint.as_str(), "hint");
        assert_eq!(K::Candidates.as_str(), "candidates");
        assert_eq!(K::None.as_str(), "none");
    }

    /// The stroke multiplier is duplicated in three places that must draw the same
    /// picture: `strokeScale` in src/lib/overlay-weight.ts (the live overlay),
    /// `stroke_scale` here (the exporter), and the `$k` expression in
    /// tools/annotate-session.ps1 (the re-annotator). Nothing type-checks one
    /// against the others, so pin the four anchors overlay-weight.ts documents.
    #[test]
    fn stroke_scale_matches_the_overlay_mapping() {
        assert!((stroke_scale(1) - 0.60).abs() < 1e-6);
        // The default must be an EXACT no-op, not 0.9999999 -- that is why the
        // TypeScript groups its arithmetic, and why this asserts equality.
        assert_eq!(stroke_scale(4), 1.0);
        assert!((stroke_scale(7) - 1.50).abs() < 1e-6);
        assert!((stroke_scale(10) - 2.00).abs() < 1e-6);
        // Out of range clamps rather than producing a degenerate or huge mark.
        assert_eq!(stroke_scale(0), stroke_scale(1));
        assert_eq!(stroke_scale(99), stroke_scale(10));
    }

    /// A step exported before `overlay_kind` existed reads back as unknown. It still
    /// renders, as a plain box -- which is what every located step draws now, so an
    /// old export and a new one are indistinguishable at the pointer.
    #[test]
    fn a_step_without_an_overlay_kind_still_renders() {
        let step: ExportStep = serde_json::from_str(
            r#"{"instruction":"x","target_text":null,"target_role":null,"clipboard":null,
                 "checkpoint":true,"pointer":{"state":"miss"},"locator_decision":null,
                 "locator_ms":null,"redacted":false}"#,
        )
        .expect("legacy step should still deserialize");
        assert_eq!(step.overlay_kind, None);
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
        // The resolved kind rides in session.json; annotate-session.ps1 branches on it.
        assert_eq!(v["turns"][0]["assistant"]["steps"][0]["overlay_kind"], "box");
        // §4.3: the path recorded must be the one actually written.
        let rel = v["turns"][0]["assistant"]["steps"][0]["screenshot"].as_str().unwrap();
        assert!(out.join(rel).is_file(), "recorded screenshot path must exist");

        let md = std::fs::read_to_string(out.join("session.md")).unwrap();
        assert!(md.contains("turn copilot off"), "the question belongs in the artifact");
        assert!(md.contains("Click Copilot"), "the step belongs in the artifact");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_clean_folder_is_never_annotated() {
        // The whole point of the split: `steps/` must be byte-identical whatever
        // the annotation options say, because it is the only artifact here that
        // cannot be regenerated from session.json.
        let dir = temp_dir("clean");
        let b = buffer_with_one_framed_step(PointerState::Hit { rect: [10, 10, 8, 6] });

        let all = b
            .write_folder(&dir, &ExportOptions { title: "a".into(), ..Default::default() })
            .unwrap();
        let none = b
            .write_folder(
                &dir,
                &ExportOptions {
                    title: "b".into(),
                    draw_pointer: false,
                    draw_caption: false,
                    ..Default::default()
                },
            )
            .unwrap();

        let clean = |d: &Path| {
            let f = std::fs::read_dir(d.join(CLEAN_DIR)).unwrap().flatten().next().unwrap();
            std::fs::read(f.path()).unwrap()
        };
        assert_eq!(clean(&all), clean(&none), "the archive copy must never be touched");
        // And with nothing to add, no annotated folder is created at all.
        assert!(!none.join(ANNOTATED_DIR).exists());
        assert!(all.join(ANNOTATED_DIR).is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pointer_is_composited_at_export_not_burned_in_at_capture() {
        // §0.4's core decision. The stored frame is clean; drawing happens here,
        // so the annotated copy must differ from the archive copy.
        let dir = temp_dir("pointer");
        let b = buffer_with_one_framed_step(PointerState::Hit { rect: [10, 10, 8, 6] });
        let out = b
            .write_folder(
                &dir,
                &ExportOptions {
                    title: "a".into(),
                    draw_pointer: true,
                    draw_caption: false,
                    ..Default::default()
                },
            )
            .unwrap();

        let read = |sub: &str| {
            let f = std::fs::read_dir(out.join(sub)).unwrap().flatten().next().unwrap();
            image::open(f.path()).unwrap().to_rgba8().into_raw()
        };
        assert_ne!(
            read(ANNOTATED_DIR),
            read(CLEAN_DIR),
            "drawing the pointer must change the annotated pixels"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_caption_changes_the_annotated_copy_only() {
        let dir = temp_dir("caption");
        let b = buffer_with_one_framed_step(PointerState::Miss);
        let out = b
            .write_folder(
                &dir,
                &ExportOptions {
                    title: "c".into(),
                    draw_pointer: false,
                    draw_caption: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let read = |sub: &str| {
            let f = std::fs::read_dir(out.join(sub)).unwrap().flatten().next().unwrap();
            image::open(f.path()).unwrap().to_rgba8().into_raw()
        };
        // Skipped rather than failed when no system font is available, since
        // draw_caption degrades to a no-op by design — a missing caption is a far
        // better outcome than a failed export.
        if caption_font().is_some() {
            assert_ne!(read(ANNOTATED_DIR), read(CLEAN_DIR), "the caption must be drawn");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn captions_render_non_latin_text() {
        // The reason the font is loaded from the OS rather than being a bundled
        // Latin bitmap. Instructions follow the user's own language, so a caption
        // renderer that cannot draw Chinese is useless to a large share of the
        // audience — including this project's author.
        let Some(_) = caption_font() else { return };
        let base = image::RgbaImage::from_pixel(600, 200, image::Rgba([30, 30, 30, 255]));

        let mut cjk = base.clone();
        draw_caption(&mut cjk, "点击顶部工具栏中的三点按钮", 0);
        assert_ne!(cjk.as_raw(), base.as_raw(), "Chinese must actually rasterise");

        let mut latin = base.clone();
        draw_caption(&mut latin, "Click the three-dots button", 0);
        assert_ne!(latin.as_raw(), base.as_raw());
        assert_ne!(cjk.as_raw(), latin.as_raw(), "different text, different pixels");
    }

    /// Writes a sample image for visual review. Ignored by default — it asserts
    /// nothing, and exists so the caption's look can be compared against the
    /// app's own without running a whole session.
    #[test]
    #[ignore = "visual check; run with --ignored"]
    fn caption_style_sample() {
        if caption_font().is_none() {
            return;
        }
        let out = std::env::temp_dir().join("navisual-caption-sample.png");
        let mut img = image::RgbaImage::from_pixel(1920, 1080, image::Rgba([24, 26, 32, 255]));
        for (x, y, p) in img.enumerate_pixels_mut() {
            if (x / 60 + y / 60) % 2 == 0 {
                p.0 = [38, 41, 50, 255];
            }
        }
        draw_pointer(&mut img, [700, 300, 120, 40], 1.0, 1.0, false);
        draw_caption(
            &mut img,
            "You are now on the customization page! You can adjust the font size using \
             the slider, select an accent color under 颜色, or switch your background theme.",
            0,
        );
        img.save(&out).unwrap();
    }

    #[test]
    fn an_empty_caption_leaves_the_frame_alone() {
        let base = image::RgbaImage::from_pixel(400, 120, image::Rgba([10, 10, 10, 255]));
        let mut img = base.clone();
        draw_caption(&mut img, "   ", 0);
        assert_eq!(img.as_raw(), base.as_raw(), "no text, no band");
    }

    #[test]
    fn saving_nothing_at_all_is_refused() {
        let dir = temp_dir("nothing");
        let b = buffer_with_one_framed_step(PointerState::Miss);
        let err = b
            .write_folder(
                &dir,
                &ExportOptions {
                    title: "d".into(),
                    save_clean: false,
                    draw_pointer: false,
                    draw_caption: false,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("nothing selected"), "got: {err}");
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

#[cfg(all(test, windows))]
mod pointer_parity {
    //! The exported pointer and the re-annotated one must be the same picture.
    //!
    //! `tools/annotate-session.ps1` exists to REDO what the export already did, so a
    //! divergence between them silently rewrites history: re-running the script on an
    //! old session would produce a figure that never matched the user's screen. Two
    //! such divergences shipped undetected because the only check here was a manual,
    //! `#[ignore]`d harness compared by eye -- the script drew three nested rectangles
    //! for months under a comment claiming it matched the app, and later drew the mark
    //! with no padding while the overlay padded every rect by 12 px.
    use std::path::{Path, PathBuf};

    /// Bounding box of pixels that differ from the flat background, plus their count.
    ///
    /// Comparing ink EXTENT rather than exact pixels is deliberate: the two renderers
    /// anti-alias differently (hand-rolled coverage vs GDI+), so identical geometry
    /// still differs pixel-for-pixel. Extent is what actually regressed both times.
    fn ink(img: &image::RgbaImage, bg: [u8; 3]) -> (i64, i64, i64, i64, u64) {
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (i64::MAX, i64::MAX, -1i64, -1i64, 0u64);
        for (x, y, px) in img.enumerate_pixels() {
            let d = (0..3)
                .map(|i| (px.0[i] as i32 - bg[i] as i32).abs())
                .max()
                .unwrap_or(0);
            if d > 24 {
                x0 = x0.min(x as i64);
                y0 = y0.min(y as i64);
                x1 = x1.max(x as i64);
                y1 = y1.max(y as i64);
                n += 1;
            }
        }
        (x0, y0, x1, y1, n)
    }

    /// Write the minimum an export needs for the script to re-annotate it.
    fn fixture(dir: &Path, rect: [i32; 4], frame: (u32, u32), state: &str) -> PathBuf {
        let clean = dir.join(super::CLEAN_DIR);
        std::fs::create_dir_all(&clean).expect("steps dir");
        let bg = image::RgbaImage::from_pixel(frame.0, frame.1, image::Rgba([20, 20, 20, 255]));
        bg.save(clean.join("01-parity.png")).expect("frame");
        let session = serde_json::json!({
            "schema": 2, "title": "parity", "slug": "parity",
            "app": {"exe": "test", "name": "Test", "window_title": "Test"},
            "navisual_version": "test", "provider": "test", "model": null,
            "created_local": "2026-09-11T00:00:00-07:00",
            "turns": [{
                "n": 1,
                "user": {"kind": "task", "text": "parity", "typed": true, "reason": null},
                "assistant": {"instruction": "parity", "steps": [{
                    "instruction": "parity", "target_text": null, "target_role": null,
                    "clipboard": null, "checkpoint": true,
                    "screenshot": "steps-annotated/01-parity.png",
                    "pointer": {"state": state, "rect": rect},
                    "overlay_kind": "box",
                    "locator": {"decision": null, "total_ms": null}, "redacted": false
                }]}
            }]
        });
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_vec_pretty(&session).unwrap(),
        )
        .expect("session.json");
        clean.join("01-parity.png")
    }

    /// Render the same rect through both implementations and compare what they drew.
    ///
    /// Skips (rather than fails) when PowerShell cannot run, so a box without a shell
    /// does not turn a missing dependency into a red build.
    #[test]
    fn exporter_and_annotator_draw_the_same_pointer() {
        const BG: [u8; 3] = [20, 20, 20];
        // A tight OCR word, a square glyph under the size floor, a wide row, a button.
        // A tight OCR word, a square glyph under the size floor, a wide row, a button,
        // and the dashed hint -- the styles have to match too, not just the geometry.
        for (tag, rect, state) in [
            ("tiny_word", [180, 200, 23, 8], "hit"),
            ("small_icon", [180, 200, 17, 17], "hit"),
            ("wide_row", [120, 200, 260, 22], "hit"),
            ("button", [140, 180, 125, 60], "hit"),
            ("hint_button", [140, 180, 125, 60], "hint"),
        ] {
            let dir =
                std::env::temp_dir().join(format!("navisual-parity-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("dir");
            let clean = fixture(&dir, rect, (520, 440), state);

            // A: the exporter.
            let mut a = image::open(&clean).expect("frame").to_rgba8();
            let hint = state == "hint";
            super::draw_pointer(&mut a, rect, 1.0, 1.0, hint);

            // B: the re-annotator, over the same untouched frame.
            let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("repo root")
                .join("tools")
                .join("annotate-session.ps1");
            let run = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script)
                .arg("-Path")
                .arg(&dir)
                .args(["-NoCaption", "-OutDir", "steps-ps"])
                .output();
            let Ok(run) = run else {
                eprintln!("skipping pointer parity: powershell unavailable");
                return;
            };
            assert!(
                run.status.success(),
                "annotate-session.ps1 failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );
            let b_path = dir.join("steps-ps").join("01-parity.png");
            assert!(b_path.is_file(), "{tag}: script wrote no image");
            let b = image::open(&b_path).expect("ps output").to_rgba8();

            let (ax0, ay0, ax1, ay1, an) = ink(&a, BG);
            let (bx0, by0, bx1, by1, bn) = ink(&b, BG);
            assert!(an > 0 && bn > 0, "{tag}: one of them drew nothing");

            // Extent must agree within a few px of anti-aliasing and stroke rounding.
            // A missing beacon moved this by ~86 px; missing padding, by 24.
            for (what, av, bv) in [
                ("left", ax0, bx0),
                ("top", ay0, by0),
                ("right", ax1, bx1),
                ("bottom", ay1, by1),
            ] {
                assert!(
                    (av - bv).abs() <= 3,
                    "{tag}: mark {what} edge differs -- exporter {av}, script {bv} (rect {rect:?}). The two must draw the same picture."
                );
            }
            // And they must lay down a comparable amount of ink, so neither can quietly
            // drop a whole element (the scan line, the crosshair) inside the same extent.
            let (lo, hi) = (an.min(bn) as f64, an.max(bn) as f64);
            assert!(
                lo / hi > 0.70,
                "{tag}: ink differs too much -- exporter {an}px, script {bn}px"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A hint must not look like a located hit. Both renderers agreeing is only worth
    /// something if they agree on a mark that still says "approximately here" --
    /// dashed brackets, no corner dots, no crosshair. Reported live on 2026-09-11:
    /// "step 12 is an AI estimate pointer. I see a solid one."
    #[test]
    fn a_hint_is_visibly_softer_than_a_hit() {
        let blank = || image::RgbaImage::from_pixel(400, 360, image::Rgba([20, 20, 20, 255]));
        let rect = [140, 150, 125, 60];
        let (mut hit, mut hint) = (blank(), blank());
        super::draw_pointer(&mut hit, rect, 1.0, 1.0, false);
        super::draw_pointer(&mut hint, rect, 1.0, 1.0, true);

        let (hx0, hy0, hx1, hy1, hit_n) = ink(&hit, [20, 20, 20]);
        let (nx0, ny0, nx1, ny1, hint_n) = ink(&hint, [20, 20, 20]);
        // Same footprint -- it points at the same place.
        for (av, bv) in [(hx0, nx0), (hy0, ny0), (hx1, nx1), (hy1, ny1)] {
            assert!((av - bv).abs() <= 3, "hint should occupy the same area as a hit");
        }
        // But visibly less ink: dashes, no dots, no crosshair, thinner strokes.
        assert!(
            (hint_n as f64) < (hit_n as f64) * 0.85,
            "hint drew {hint_n}px against a hit's {hit_n}px -- not distinguishable"
        );
        // The centre must be empty on a hint: no crosshair claiming an exact spot.
        let (cx, cy) = (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2);
        let centre_ink = |img: &image::RgbaImage| {
            let mut n = 0;
            for dy in -6i32..=6 {
                for dx in -6i32..=6 {
                    let p = img.get_pixel((cx + dx) as u32, (cy + dy) as u32);
                    if (p.0[0] as i32 - 20).abs() > 24 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(centre_ink(&hit) > 0, "a hit draws a crosshair");
        assert_eq!(centre_ink(&hint), 0, "a hint must not draw a crosshair");
    }

    /// The geometry every renderer has to agree on, pinned against hand-checked
    /// values. `markRect` in `src/Overlay.svelte` and the `$pad`/`$MIN_MARK` block in
    /// `tools/annotate-session.ps1` must produce these same numbers.
    #[test]
    fn mark_geometry_is_pinned() {
        // rect -> (px, py, pw, ph); pad = clamp(12, 20, min(w,h) * 0.45)
        for (rect, want) in [
            ([100, 100, 87, 22], (88.0f32, 88.0f32, 111.0f32, 46.0f32)), // pad 12, the floor
            ([100, 100, 125, 60], (80.0, 80.0, 165.0, 100.0)),           // pad 20, the ceiling
            ([100, 100, 60, 60], (80.0, 80.0, 100.0, 100.0)),            // pad 27 -> 20
            // MIN_MARK lifts anything under 36 px on either axis.
            ([100, 100, 23, 8], (88.0, 86.0, 47.0, 36.0)),
            ([100, 100, 4, 4], (84.0, 84.0, 36.0, 36.0)),
        ] {
            let got = super::mark_rect(rect[0], rect[1], rect[2], rect[3], 1.0);
            let fields = [
                (got.0, want.0),
                (got.1, want.1),
                (got.2, want.2),
                (got.3, want.3),
            ];
            for (i, (g, w)) in fields.iter().enumerate() {
                assert!(
                    (g - w).abs() < 0.01,
                    "rect {rect:?} field {i}: got {g}, want {w}"
                );
            }
            let (px, py, pw, ph) = got;
            // The mark is always centred on the located rect, never offset.
            assert!(((px + pw / 2.0) - (rect[0] as f32 + rect[2] as f32 / 2.0)).abs() < 0.01);
            assert!(((py + ph / 2.0) - (rect[1] as f32 + rect[3] as f32 / 2.0)).abs() < 0.01);
            // And never smaller than the element it marks.
            assert!(
                pw >= rect[2] as f32 && ph >= rect[3] as f32,
                "mark must not clip the target"
            );
        }
    }

    /// On a high-DPI display the frame is in physical pixels while the overlay drew in
    /// logical ones, so the fixed parts of the mark have to be converted. The located
    /// rect does not -- it was already carried into frame space.
    #[test]
    fn mark_scale_converts_the_constants_but_not_the_element() {
        // A 200% display: the same element arrives twice as large in the frame, and
        // the pad around it must double too.
        let at1 = super::mark_rect(100, 100, 87, 22, 1.0);
        let at2 = super::mark_rect(200, 200, 174, 44, 2.0);
        let pad1 = (at1.2 - 87.0) / 2.0;
        let pad2 = (at2.2 - 174.0) / 2.0;
        assert!((pad2 - pad1 * 2.0).abs() < 0.01, "pad {pad1} -> {pad2}, want doubled");

        // The floor scales with it, so a tiny glyph is lifted by the same amount of
        // PERCEIVED space on either display.
        let tiny1 = super::mark_rect(0, 0, 4, 4, 1.0);
        let tiny2 = super::mark_rect(0, 0, 8, 8, 2.0);
        assert!((tiny1.2 - 36.0).abs() < 0.01);
        assert!((tiny2.2 - 72.0).abs() < 0.01);

        // Bracket arms: both BOUNDS scale, while the ratio between them does not --
        // it is a fraction of a mark already measured in frame pixels.
        assert!((super::mark_arm(400.0, 300.0, 1.0) - 26.0).abs() < 0.01); // ceiling
        assert!((super::mark_arm(400.0, 300.0, 2.0) - 52.0).abs() < 0.01); // ceiling, doubled
        assert!((super::mark_arm(20.0, 20.0, 1.0) - 14.0).abs() < 0.01); // floor
        assert!((super::mark_arm(20.0, 20.0, 2.0) - 28.0).abs() < 0.01); // floor, doubled
        assert!((super::mark_arm(200.0, 100.0, 2.0) - 50.0).abs() < 0.01); // ratio binds

        // A nonsense scale is ignored rather than collapsing or exploding the mark.
        assert_eq!(super::mark_rect(0, 0, 40, 40, f32::NAN), super::mark_rect(0, 0, 40, 40, 1.0));
        assert_eq!(super::mark_rect(0, 0, 40, 40, 0.0), super::mark_rect(0, 0, 40, 40, 1.0));
    }
}
