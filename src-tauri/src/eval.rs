//! Replay harness for the reasoning-harness study (`navisual-internal/docs/harness-plan.md`,
//! Phase 0).
//!
//! Sends fixtures through the app's OWN request path: the downscale and JPEG encode a live
//! capture gets (`capture::encode_capture_jpeg` at `AI_JPEG_QUALITY`), the window-context
//! block `guide` builds (`window_context_text`), the turn-1 prompt (`compose_turn1_prompt`),
//! and `AiRouter::send_guidance_request` with its real message builders, system prompt and
//! tool schema. Every piece is a function `guide` itself calls, so a result measures the
//! product rather than a copy of it -- a hand-copied prompt would drift the first time
//! either side changed.
//!
//! Test-only, and the entry point is `#[ignore]`d: each run spends real money on the BYOK
//! keys, so nothing here runs unless asked for by name. Drive it with `tools/run-replay.ps1`.
//!
//! Isolation, because a test run must never touch the founder's real state:
//! * sessions go to a throwaway directory -- a test session must not evict one of the 20
//!   real ones from the store;
//! * the cost tracker is in-memory -- test traffic is not charged to the Usage tab;
//! * the `.env` is read but never written -- provider, model and reasoning are overridden
//!   in memory only.
//!
//! Fixtures: `<NAVISUAL_EVAL_DIR>/<battery>/<id>/item.json` plus the PNG it names. Every
//! raw response is written to the results file, so a scoring rule can be changed and
//! re-applied later without paying for the calls again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ai::config::Config;
use crate::ai::cost_tracker::CostTracker;
use crate::ai::router::AiRouter;
use crate::ai::session::SessionManager;
use crate::ai::types::{NavigateStepResponse, UsageDetail};
use crate::capture;

/// What a fixture counts as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum PassIf {
    /// The first targeted step names one of `target_text_any` and none of `target_text_none`.
    Text,
    /// The first targeted step's bbox centre lands inside `target_rect`.
    Bbox,
    /// Either of the two above.
    TextOrBbox,
    /// No step targets the trap -- by name (`target_text_none`) or by bbox (`avoid_rect`).
    /// The "already on" case: clicking the control would undo what the user asked for.
    AvoidText,
    /// The model asked a question instead of guessing.
    NeedsInput,
}

/// What a scored answer was. `NoTarget` is kept apart from `Wrong` on purpose: an answer
/// that routes around the control (a keyboard shortcut, a question) is not a misperception,
/// and folding it into the failure count would make a perception score lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Pass,
    Wrong,
    NoTarget,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Fixture {
    id: String,
    task: String,
    window_title: String,
    /// `[left, top, right, bottom]` of the fixture window, as `get_window_info` reports a
    /// real one.
    window_rect: [i32; 4],
    /// PNG, relative to the item directory, at native resolution -- the harness applies the
    /// live downscale itself.
    image: String,
    /// A pre-rendered `[Screen Elements]` block, when the item tests the list. Absent means
    /// "no list", the case for apps without an accessibility tree or over the 300 cap.
    #[serde(default)]
    elements_block: Option<String>,
    /// The same list with each element's state (`Config::list_states`), written by the capture
    /// test; the `list_states` variant sends it in place of `elements_block`.
    #[serde(default)]
    elements_block_states: Option<String>,
    /// H3: the accessibility snapshot taken with the screenshot (`a11y::snapshot_ui_tree`), a JSON
    /// file next to `image`. The `ui_tree` variant offers it; every other run ignores it.
    #[serde(default)]
    ui_snapshot: Option<String>,
    /// With `avoid_text`: the answer must also say why the trap is wrong (`instruction_any`), and a
    /// step that points at the trap while saying so is not taking it -- "Paste is disabled because
    /// nothing is copied" with the pointer on the greyed button is the right answer, and "press
    /// Ctrl+V to paste" with no pointer is not. Off by default, so older batteries grade as they did.
    #[serde(default)]
    say_why: bool,
    /// H7b: a learned guide for the app (`packs::guide`), a file relative to the item directory --
    /// one per battery, shared by its items. Sent the way the app sends one, unless the variant is
    /// `no_guide`; `stale_guide` sends `guide_stale` instead, an older edition the app has moved on
    /// from.
    #[serde(default)]
    guide: Option<String>,
    #[serde(default)]
    guide_stale: Option<String>,
    /// L2 step 2: the guide `packs::learn` writes after one guided session per task -- paths
    /// only, no prose. The `local_guide` variant sends it.
    #[serde(default)]
    guide_local: Option<String>,
    /// The files' text, read by `load_fixtures`.
    #[serde(skip)]
    guide_text: Option<String>,
    #[serde(skip)]
    guide_stale_text: Option<String>,
    #[serde(skip)]
    guide_local_text: Option<String>,
    /// L2 option B: the request `packs::learn` adds to the prompt right after a note is written,
    /// asking for the model's `app_note`. The answer is read from the result row.
    #[serde(default)]
    learn_request: Option<LearnRequest>,
    pass_if: PassIf,
    #[serde(default)]
    target_text_any: Vec<String>,
    #[serde(default)]
    target_text_none: Vec<String>,
    /// `[left, top, right, bottom]` in the PNG's own pixels.
    #[serde(default)]
    target_rect: Option<[i32; 4]>,
    /// For `AvoidText`: the trap control, same units as `target_rect`.
    #[serde(default)]
    avoid_rect: Option<[i32; 4]>,
    /// For `AvoidText`: the trap counts only as the first targeted step. "Close the dialog,
    /// then click Export" is right; "click Export" while the dialog is open is the trap.
    #[serde(default)]
    avoid_first_only: bool,
    /// Phrases that, found in any instruction, make the answer right in words even without a
    /// pointer -- "press Ctrl+Z to undo" is the right first step and points at nothing.
    #[serde(default)]
    instruction_any: Vec<String>,
    /// The script the instruction must be written in. `"cjk"`: a Chinese request on an English
    /// UI must be answered in Chinese (rule 13) -- an answer that points right but talks English
    /// is still a failure the user meets.
    #[serde(default)]
    instruction_script: Option<String>,
    /// Phrases that must ALL appear in what the user is told (instructions or plan), on top of
    /// `pass_if`. For "notice AND recover": one keyword alone passed a misperception -- terra
    /// wrote "Deletion is complete" over the wrong deletion, pointed at Undo, and
    /// `instruction_any: ["undo"]` passed it.
    #[serde(default)]
    instruction_all: Vec<String>,
    /// Phrases that fail the answer if said anywhere (instructions or plan). For a toggle the
    /// screen shows already done: "press Ctrl+B" again would undo it. Grade the failures by
    /// reading them -- "don't press Ctrl+B" contains the phrase too.
    #[serde(default)]
    instruction_none: Vec<String>,
    /// S2 (harness-plan.md H1): a turn-2 item. The session already holds turn 1; `image` is the
    /// screen after the user acted, and the answer graded is the model's reply to it.
    #[serde(default)]
    continuation: Option<Continuation>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct LearnRequest {
    app: String,
    title: String,
    path: Vec<String>,
    /// The note's current line, for a rewrite request.
    #[serde(default)]
    line: Option<String>,
}

/// Turn 1 of an S2 item, canned so both arms of a comparison see the same history and differ
/// only in what H1 adds.
#[derive(Debug, Clone, serde::Deserialize)]
struct Continuation {
    /// correct | wrong | not_done | silent | ahead -- what the user actually did.
    class: String,
    /// The model's turn-1 answer, as the app would have stored it.
    turn1: NavigateStepResponse,
    /// Which of turn 1's steps the user completed (1-based): the `[User completed: "…"]` line
    /// quotes its instruction.
    completed: usize,
    /// What the click hook reported, as `[role, name]`; absent when the step was a keystroke
    /// or the click was outside the guided app.
    #[serde(default)]
    click: Option<[String; 2]>,
}

struct RunConfig {
    dir: PathBuf,
    battery: String,
    provider: String,
    model: String,
    reasoning: Option<String>,
    voice_language: String,
    repeat: usize,
    only: Vec<String>,
    max_usd: f64,
    dry_run: bool,
    /// Re-score this results file from its stored responses instead of calling anything.
    rescore: Option<PathBuf>,
    /// A deliberate prompt change for an ablation -- the one sanctioned way this harness sends
    /// something other than what the app sends. Empty = exactly the app's prompt.
    variant: String,
    out: PathBuf,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl RunConfig {
    fn from_env() -> Self {
        let dir = PathBuf::from(
            std::env::var("NAVISUAL_EVAL_DIR").expect("NAVISUAL_EVAL_DIR must name the fixtures root"),
        );
        let battery = env_or("NAVISUAL_EVAL_BATTERY", "s1");
        let provider = env_or("NAVISUAL_EVAL_PROVIDER", "gemini");
        let model = env_or("NAVISUAL_EVAL_MODEL", "gemini-3.7-flash");
        // Unset = no reasoning field at all, which is what the managed tiers send
        // (settings.md: the tiers do not read REASONING_EFFORT).
        let reasoning = std::env::var("NAVISUAL_EVAL_REASONING")
            .ok()
            .filter(|v| !v.trim().is_empty());
        // "auto" rather than the founder's own setting, so a run means the same thing on
        // any machine; the reply language then follows the task text.
        let voice_language = env_or("NAVISUAL_EVAL_VOICE_LANGUAGE", "auto");
        let repeat = env_or("NAVISUAL_EVAL_REPEAT", "1").parse().unwrap_or(1).max(1);
        let only = env_or("NAVISUAL_EVAL_ONLY", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // A cap, not an estimate: the run stops before the call that would cross it.
        let max_usd = env_or("NAVISUAL_EVAL_MAX_USD", "1.00").parse().unwrap_or(1.0);
        let dry_run = env_or("NAVISUAL_EVAL_DRY_RUN", "") == "1";
        let variant = env_or("NAVISUAL_EVAL_VARIANT", "");
        // `no_rect` was the ablation that became the production prompt on 2026-10-07; its
        // opposite, `with_rect`, reproduces the prompt from before, for comparison runs.
        // `expect` turns on H1 (Config::expected_result): rule 19 and the step field.
        assert!(
            ["", "with_rect", "expect", "inspect", "inspect_note", "inspect_first", "read_text", "read_text_first", "look_both", "recall", "recall3", "no_list", "ui_tree", "list_states", "list_plain", "no_guide", "stale_guide", "local_guide", "file_guide"]
                .contains(&variant.as_str()),
            "unknown NAVISUAL_EVAL_VARIANT {variant:?} (known: with_rect, expect, inspect, inspect_note, \
             inspect_first, read_text, read_text_first, look_both, recall, recall3, no_list, ui_tree, list_states, list_plain, \
             no_guide, stale_guide, local_guide, file_guide)"
        );
        let rescore = std::env::var("NAVISUAL_EVAL_RESCORE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let out = std::env::var("NAVISUAL_EVAL_OUT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let tag = if variant.is_empty() { String::new() } else { format!("-{variant}") };
                dir.join("results")
                    .join(format!("{battery}-{provider}-{model}{tag}-{stamp}.jsonl"))
            });
        Self {
            dir,
            battery,
            provider,
            model,
            reasoning,
            voice_language,
            repeat,
            only,
            max_usd,
            dry_run,
            rescore,
            variant,
            out,
        }
    }
}

/// Every `item.json` under the battery directory, in directory order.
fn load_fixtures(rc: &RunConfig) -> Vec<(PathBuf, Fixture)> {
    let root = rc.dir.join(&rc.battery);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("item.json").is_file())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| {
            let text = std::fs::read_to_string(d.join("item.json")).expect("item.json");
            let mut fx: Fixture = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", d.join("item.json").display()));
            let read = |f: &Option<String>| {
                f.as_ref().map(|f| {
                    std::fs::read_to_string(d.join(f)).unwrap_or_else(|e| panic!("{}: {e}", d.join(f).display()))
                })
            };
            fx.guide_text = read(&fx.guide);
            fx.guide_stale_text = read(&fx.guide_stale);
            fx.guide_local_text = read(&fx.guide_local);
            (d, fx)
        })
        .filter(|(_, fx)| rc.only.is_empty() || rc.only.iter().any(|o| o == &fx.id))
        .collect()
}

/// The image exactly as a live capture of that window would reach the model.
fn encode_fixture(path: &Path) -> (String, usize, u32, u32) {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .to_rgba8();
    let (w, h) = (img.width(), img.height());
    let jpeg = capture::encode_capture_jpeg(img, capture::AI_JPEG_QUALITY).expect("jpeg encode");
    let len = jpeg.len();
    (capture::to_base64(&jpeg), len, w, h)
}

/// The [Screen Elements] block an item sends. Without a variant it follows the app's own default
/// (`Config::list_states`, on since 2026-10-10), so "no variant" still means "what the app sends";
/// `list_plain` sends the block without states, `list_states` the block with them. A fixture
/// captured before states existed has only the plain block.
fn elements_for<'a>(fx: &'a Fixture, variant: &str) -> &'a str {
    let states = match variant {
        "list_plain" => false,
        "list_states" => true,
        _ => crate::ai::config::Config::default().list_states,
    };
    match (states, fx.elements_block_states.as_deref()) {
        (true, Some(b)) => b,
        _ => fx.elements_block.as_deref().unwrap_or(""),
    }
}

/// The learned-guide block an item sends (H7b), through the app's own parser and selection, with
/// the task as the focus -- `guide` adds the session's goal, which a turn-1 item does not have yet.
fn guide_for(fx: &Fixture, variant: &str) -> String {
    // `file_guide`: one guide for the whole run, from a file -- a model's own written lines.
    let from_file = (variant == "file_guide").then(|| {
        let f = std::env::var("NAVISUAL_EVAL_GUIDE_FILE").expect("file_guide needs NAVISUAL_EVAL_GUIDE_FILE");
        std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{f}: {e}"))
    });
    let text = match variant {
        "no_guide" => None,
        "stale_guide" => fx.guide_stale_text.as_deref(),
        "local_guide" => fx.guide_local_text.as_deref(),
        "file_guide" => from_file.as_deref(),
        _ => fx.guide_text.as_deref(),
    };
    let mut block = text
        .map(|t| {
            crate::packs::guide::Guide::parse(t, PathBuf::new())
                .unwrap_or_else(|| panic!("{}: the guide does not parse", fx.id))
                .block(&fx.task)
        })
        .unwrap_or_default();
    if let Some(r) = &fx.learn_request {
        block.push_str(&crate::packs::learn::request_block(&r.app, &r.title, &r.path, r.line.as_deref()));
    }
    block
}

/// The turn-2 prompt `guide` would build after the user acted: the `[User completed: "…"]`
/// line (with use A's expectation when the variant turns H1 on, exactly as `guide` adds it), the
/// window context with the click record, and the tail anchors.
fn continuation_prompt(fx: &Fixture, c: &Continuation, voice_language: &str, variant: &str) -> String {
    let step = &c.turn1.steps[c.completed - 1];
    let task = format!("[User completed: \"{}\"]", step.instruction);
    let task = crate::continuation_task(&task, &c.turn1.steps, h1_on(variant));
    let click = c.click.as_ref().map(|[role, name]| crate::last_click::prompt_line(role, name));
    let window_context = crate::window_context_text(
        Some(&capture::format_window_prompt_info(&fx.window_title)),
        &guide_for(fx, variant),
        elements_for(fx, variant),
        crate::os_context_line(),
        None,
        click.as_deref(),
    );
    let goal = if c.turn1.goal.trim().is_empty() { fx.task.as_str() } else { c.turn1.goal.as_str() };
    // As `guide` keeps it: the opening request is the language sample when it is substantial.
    let sample = crate::ai::prompts::is_language_sample(&fx.task).then(|| fx.task.clone());
    crate::compose_continuation_prompt(
        &task,
        &c.turn1.state_summary,
        &window_context,
        goal,
        sample.as_deref(),
        voice_language,
    )
}

/// The conversation `send_guidance_request` reads for an item: empty for a turn-1 item, turn 1
/// recorded the way `guide` records it for a continuation.
fn prime_session(router: &mut AiRouter, fx: &Fixture) {
    router.session_manager.current_session = None;
    let Some(c) = &fx.continuation else { return };
    router.session_manager.create_session(fx.task.clone());
    if let Some(s) = router.session_manager.current_session.as_mut() {
        s.add_turn_pinned("user", fx.task.clone(), None, true);
        let content = c.turn1.steps.iter().map(|s| s.instruction.clone()).collect::<Vec<_>>().join("\n");
        s.add_turn("assistant", content, Some("...".to_string()));
        s.update_state(c.turn1.state_summary.clone());
        if !c.turn1.goal.trim().is_empty() {
            s.set_task_description(c.turn1.goal.trim().to_string());
        }
    }
}

/// The prompt for an item of either kind.
fn item_prompt(fx: &Fixture, voice_language: &str, variant: &str) -> String {
    match &fx.continuation {
        Some(c) => continuation_prompt(fx, c, voice_language, variant),
        None => fixture_prompt(fx, voice_language, variant),
    }
}

/// The turn-1 prompt `guide` would build for this window: no authored pack, no app-state channel
/// and no click record, because a fixture has none of them; a learned guide when it has one.
fn fixture_prompt(fx: &Fixture, voice_language: &str, variant: &str) -> String {
    let info = match variant {
        // The prompt before 2026-10-07: the title AND the window's frame.
        "with_rect" => capture::format_window_info(&fx.window_title, fx.window_rect),
        _ => capture::format_window_prompt_info(&fx.window_title),
    };
    let window_context = crate::window_context_text(
        Some(&info),
        &guide_for(fx, variant),
        elements_for(fx, variant),
        crate::os_context_line(),
        None,
        None,
    );
    crate::compose_turn1_prompt(&fx.task, &window_context, None, voice_language)
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `needle` appears in `hay` with no letter or digit hugging either side, so "draft 2" is
/// found in "Q3 forecast (draft 2)" but "Bold" is not found in "Boldface".
fn contains_phrase(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let i = start + pos;
        let j = i + needle.len();
        let before = hay[..i].chars().next_back();
        let after = hay[j..].chars().next();
        if before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric()) {
            return true;
        }
        start = j;
    }
    false
}

fn text_matches(target: &str, wanted: &[String]) -> bool {
    let t = normalize(target);
    !t.is_empty()
        && wanted.iter().any(|w| {
            let w = normalize(w);
            t == w || contains_phrase(&t, &w)
        })
}

/// A bbox centre in the PNG's own pixels. `target_bbox` is normalized 0-1000 in
/// `[ymin, xmin, ymax, xmax]` over the frame the image represents (ai/bbox.rs), and a
/// fixture's frame is the whole PNG.
/// A zero-area box is no box: one model returned `[0,0,0,0]` with no target at all.
fn real_bbox(b: Option<[f64; 4]>) -> Option<[f64; 4]> {
    b.filter(|b| b[2] > b[0] && b[3] > b[1])
}

fn bbox_centre(b: [f64; 4], w: u32, h: u32) -> (f64, f64) {
    (
        (b[1] + b[3]) / 2.0 / 1000.0 * w as f64,
        (b[0] + b[2]) / 2.0 / 1000.0 * h as f64,
    )
}

fn inside(p: (f64, f64), r: [i32; 4]) -> bool {
    p.0 >= r[0] as f64 && p.0 <= r[2] as f64 && p.1 >= r[1] as f64 && p.1 <= r[3] as f64
}

/// Pass/fail plus a one-line reason, from the parsed response alone -- which is why the
/// raw response is kept: a rule changed later re-scores old runs for free.
fn score(fx: &Fixture, resp: &NavigateStepResponse, w: u32, h: u32) -> (Outcome, String) {
    let first = resp.steps.iter().find(|s| {
        s.target_text.as_deref().is_some_and(|t| !t.trim().is_empty()) || real_bbox(s.target_bbox).is_some()
    });
    let first_instruction = resp.steps.first().map(|s| s.instruction.as_str()).unwrap_or("");
    // The plan counts as words too: the panel shows it, and a model whose first step selects the
    // range ("select C2:C11") names the command in its plan ("Open Data Validation") rather than
    // in a later step it has not reached yet.
    let said_it = !fx.instruction_any.is_empty()
        && resp
            .steps
            .iter()
            .map(|s| s.instruction.as_str())
            .chain(resp.plan_outline.iter().map(String::as_str))
            .any(|text| {
                let t = normalize(text);
                fx.instruction_any.iter().any(|p| contains_phrase(&t, &normalize(p)))
            });
    if first.is_none() && fx.pass_if != PassIf::AvoidText && fx.pass_if != PassIf::NeedsInput {
        if said_it {
            return (Outcome::Pass, format!("right in words, no pointer: {first_instruction:?}"));
        }
        return (Outcome::NoTarget, format!("no target; first instruction: {first_instruction:?}"));
    }
    let text = first.and_then(|s| s.target_text.clone()).unwrap_or_default();
    let centre = first.and_then(|s| real_bbox(s.target_bbox)).map(|b| bbox_centre(b, w, h));
    let text_ok = text_matches(&text, &fx.target_text_any) && !text_matches(&text, &fx.target_text_none);
    let bbox_ok = matches!((fx.target_rect, centre), (Some(r), Some(c)) if inside(c, r));
    let centre_str = centre.map_or("none".to_string(), |(x, y)| format!("({x:.0},{y:.0})"));
    // Words never rescue a pointer aimed at a known-wrong target: the arrow is what the user
    // follows. Found when "clear the filter" in a plan passed an arrow on the WRONG column's
    // filter -- the user would click it and get nothing back.
    let pointed_at_trap = text_matches(&text, &fx.target_text_none);
    let judge = |ok: bool| if ok && !pointed_at_trap { Outcome::Pass } else { Outcome::Wrong };
    if !fx.instruction_all.is_empty() {
        let told: Vec<String> = resp
            .steps
            .iter()
            .map(|s| s.instruction.as_str())
            .chain(resp.plan_outline.iter().map(String::as_str))
            .map(normalize)
            .collect();
        // An entry may list alternatives, "still|didn't|not bold": one of them is enough.
        if let Some(missing) = fx.instruction_all.iter().find(|entry| {
            !entry
                .split('|')
                .any(|p| told.iter().any(|t| contains_phrase(t, &normalize(p))))
        }) {
            return (Outcome::Wrong, format!("never said {missing:?}: {first_instruction:?}"));
        }
    }
    if !fx.instruction_none.is_empty() {
        let said = resp
            .steps
            .iter()
            .map(|s| s.instruction.as_str())
            .chain(resp.plan_outline.iter().map(String::as_str))
            .map(normalize)
            .find_map(|t| fx.instruction_none.iter().find(|p| contains_phrase(&t, &normalize(p))));
        if let Some(p) = said {
            return (Outcome::Wrong, format!("said {p:?}: {first_instruction:?}"));
        }
    }
    if fx.instruction_script.as_deref() == Some("cjk") {
        let chinese = resp.steps.iter().any(|s| s.instruction.chars().any(is_cjk));
        if !chinese {
            return (Outcome::Wrong, format!("instruction not in Chinese: {first_instruction:?}"));
        }
    }
    match fx.pass_if {
        PassIf::Text => (judge(text_ok || said_it), format!("target_text={text:?} said_it={said_it}")),
        PassIf::Bbox => (judge(bbox_ok || said_it), format!("bbox_centre={centre_str} said_it={said_it}")),
        PassIf::TextOrBbox => (
            judge(text_ok || bbox_ok || said_it),
            format!(
                "target_text={text:?} text_ok={text_ok} bbox_centre={centre_str} bbox_ok={bbox_ok} said_it={said_it}"
            ),
        ),
        PassIf::AvoidText => {
            // Only the first targeted step when the trap is a matter of order.
            let considered: Vec<_> = if fx.avoid_first_only {
                first.into_iter().collect()
            } else {
                resp.steps.iter().collect()
            };
            let explains = |s: &crate::ai::types::GuidanceStep| {
                let t = normalize(&s.instruction);
                fx.instruction_any.iter().any(|p| contains_phrase(&t, &normalize(p)))
            };
            let trap = considered.into_iter().find(|s| {
                let points = s
                    .target_text
                    .as_deref()
                    .is_some_and(|t| text_matches(t, &fx.target_text_none))
                    || matches!((fx.avoid_rect, real_bbox(s.target_bbox)), (Some(r), Some(b)) if inside(bbox_centre(b, w, h), r));
                points && !(fx.say_why && explains(s))
            });
            match trap {
                Some(s) => (Outcome::Wrong, format!("instructed the trap: {:?}", s.instruction)),
                None if fx.say_why && !said_it => {
                    (Outcome::Wrong, format!("never said why: {first_instruction:?}"))
                }
                None => (
                    Outcome::Pass,
                    format!("avoided the trap; first instruction: {first_instruction:?}"),
                ),
            }
        }
        PassIf::NeedsInput => (judge(resp.needs_input), format!("needs_input={}", resp.needs_input)),
    }
}

/// Rule 13 says the reply follows the language of the request. For an English (ASCII) task,
/// flag an instruction carrying non-ASCII letters or two or more common Spanish/Portuguese
/// function words. A heuristic and labelled as one -- it exists because one tier model
/// answered English tasks in Spanish on its first run, and a flag in every result row is
/// how that becomes a rate instead of an anecdote.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// The instruction with quoted spans removed: a UI label quoted in its own language
/// ("click “自动保存”") is rule 13 followed, not the reply drifting.
fn unquoted(text: &str) -> String {
    let mut out = String::new();
    let mut close: Option<char> = None;
    for c in text.chars() {
        match close {
            Some(end) if c == end => close = None,
            Some(_) => {}
            None => match c {
                '"' => close = Some('"'),
                '\u{201c}' => close = Some('\u{201d}'),
                '\u{00ab}' => close = Some('\u{00bb}'),
                '\u{300c}' => close = Some('\u{300d}'),
                _ => out.push(c),
            },
        }
    }
    out
}

fn language_suspect(task: &str, resp: &NavigateStepResponse) -> bool {
    if !task.is_ascii() {
        return false;
    }
    const FOREIGN: &[&str] = &["el", "la", "los", "las", "una", "para", "y", "con", "del", "que", "haz", "presiona", "clique"];
    resp.steps.iter().any(|s| {
        let plain = unquoted(&s.instruction);
        let i = plain.as_str();
        if i.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) {
            return true;
        }
        let words = i
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| FOREIGN.contains(&w.to_lowercase().as_str()))
            .count();
        words >= 2
    })
}

/// Summary line shared by a live run and a re-score.
struct Tally {
    pass: usize,
    wrong: usize,
    no_target: usize,
    lang: usize,
}

impl Tally {
    fn new() -> Self {
        Self { pass: 0, wrong: 0, no_target: 0, lang: 0 }
    }
    fn add(&mut self, o: Outcome, lang: bool) {
        match o {
            Outcome::Pass => self.pass += 1,
            Outcome::Wrong => self.wrong += 1,
            Outcome::NoTarget => self.no_target += 1,
        }
        self.lang += lang as usize;
    }
    fn line(&self) -> String {
        format!(
            "{}/{} passed ({} wrong, {} no target) | {} answer(s) flagged for language drift",
            self.pass,
            self.pass + self.wrong + self.no_target,
            self.wrong,
            self.no_target,
            self.lang
        )
    }
}

/// Re-judge a stored results file with the current fixtures and rules. Free: no model call.
fn rescore(rc: &RunConfig, path: &Path) {
    let fixtures = load_fixtures(rc);
    let mut tally = Tally::new();
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line).expect("result row");
        let id = row["item"].as_str().unwrap_or_default();
        let Some((_, fx)) = fixtures.iter().find(|(_, f)| f.id == id) else {
            println!("[rescore] {id:<28} skipped: no such fixture now");
            continue;
        };
        let Ok(resp) = serde_json::from_value::<NavigateStepResponse>(row["response"].clone()) else {
            println!("[rescore] {id:<28} skipped: no stored response (the call failed)");
            continue;
        };
        let w = row["png"][0].as_u64().unwrap_or(0) as u32;
        let h = row["png"][1].as_u64().unwrap_or(0) as u32;
        let (outcome, why) = score(fx, &resp, w, h);
        let lang = language_suspect(&fx.task, &resp);
        tally.add(outcome, lang);
        println!(
            "[rescore] {id:<28} {:<9} {}| {why}",
            format!("{outcome:?}"),
            if lang { "LANG? " } else { "" }
        );
    }
    println!("[rescore] {} | {}", path.display(), tally.line());
}

/// H1 (rule 19 + use A) is on in `expect`. The `screen` / `expect_screen` result files of
/// 2026-10-09 came from a switch that is gone: those rules are now the production prompt.
fn h1_on(variant: &str) -> bool {
    variant == "expect"
}

fn base_config(rc: &RunConfig) -> Config {
    let env_path = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
        .join("com.navisual.app")
        .join(".env");
    let mut config = Config::load(Some(&env_path));
    config.api_provider = rc.provider.clone();
    match rc.provider.as_str() {
        "gemini" => config.gemini_model = rc.model.clone(),
        "openai" => config.openai_model = rc.model.clone(),
        "anthropic" => config.anthropic_model = rc.model.clone(),
        other => panic!("provider {other:?} is not a BYOK eval target (gemini | openai | anthropic)"),
    }
    config.reasoning_effort = rc.reasoning.clone();
    // Set from the variant alone, never inherited from the .env: a hand-set EXPECTED_RESULT
    // left on there would otherwise turn every "production" run into an H1 run, silently.
    config.expected_result = h1_on(&rc.variant);
    // H2: the zoom tool, offered by the router only when the frame was reduced below
    // inspect::MIN_SCALE -- so on a 1080p fixture this variant sends what production sends.
    // H4: `read_text` alone (the model's choice, or forced first), or both tools by choice.
    use crate::ai::inspect::{Mode, Tools};
    (config.inspect, config.inspect_tools) = match rc.variant.as_str() {
        "inspect" => (Mode::Choice, Tools::Zoom),
        "inspect_note" => (Mode::Note, Tools::Zoom),
        "inspect_first" => (Mode::First, Tools::Zoom),
        "read_text" => (Mode::Choice, Tools::Text),
        "read_text_first" => (Mode::First, Tools::Text),
        "look_both" => (Mode::Choice, Tools::Both),
        _ => (Mode::Off, Tools::Zoom),
    };
    // The list of earlier screens is the app's default now; `no_list` takes it away. Set here,
    // never inherited from the .env.
    config.earlier_screens = rc.variant != "no_list";
    // `recall3`: one recall may return up to three screens (the higher tier's setting).
    config.recall_max = if rc.variant == "recall3" { 3 } else { 1 };
    config
}

/// Whether a variant hands the router the native frame for a look tool.
fn config_looks(variant: &str) -> bool {
    variant.starts_with("inspect") || variant.starts_with("read_text") || variant == "look_both"
}

fn append_line(path: &Path, value: &serde_json::Value) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    writeln!(f, "{value}").expect("write result line");
}

fn median(mut v: Vec<u128>) -> u128 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// The text an expectation says will appear: every span in straight or curly double quotes.
/// `no visible change`, and an expectation that quotes nothing, give an empty list -- use B then
/// has nothing to look for and must not act (harness-plan.md H1: fail safe).
fn quoted_spans(expected: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = expected;
    while let Some(open) = rest.find(['"', '\u{201C}']) {
        let after = &rest[open + rest[open..].chars().next().map_or(1, char::len_utf8)..];
        let Some(close) = after.find(['"', '\u{201D}']) else { break };
        let span = after[..close].trim();
        if !span.is_empty() {
            out.push(span.to_string());
        }
        rest = &after[close + after[close..].chars().next().map_or(1, char::len_utf8)..];
    }
    out
}

/// Use B's local check on every S2 item, with no model call: OCR the after-screen with the
/// locator's own engine and matcher, and look for the text the completed step's expectation
/// quotes. Prints met / not met / nothing to check, by class. Measures the mechanism, so the
/// expectations are turn 1's canned ones.
#[test]
#[ignore = "reads the S2 fixtures; run via tools/run-replay.ps1 -UseB"]
fn use_b_local_check() {
    println!();
    let dir = PathBuf::from(std::env::var("NAVISUAL_EVAL_DIR").expect("NAVISUAL_EVAL_DIR"));
    let root = dir.join(std::env::var("NAVISUAL_EVAL_BATTERY").unwrap_or_else(|_| "s2".into()));
    let mut ids: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("battery dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("item.json").is_file())
        .collect();
    ids.sort();
    for d in ids {
        let fx: Fixture =
            serde_json::from_str(&std::fs::read_to_string(d.join("item.json")).unwrap()).unwrap();
        let Some(c) = &fx.continuation else { continue };
        let expected = c.turn1.steps[c.completed - 1].expected_result.clone().unwrap_or_default();
        let spans = quoted_spans(&expected);
        let (w, h) = image::image_dimensions(d.join(&fx.image)).expect("dims");
        // Before and after, as H1c would: text already on screen before the step proves nothing
        // about it (a label whose control changes state, a row that should disappear), so only a
        // span absent before counts. Without a before-frame every span counts, as a bare check.
        let ocr = |path: &Path| -> Vec<crate::locator::ocr::OcrResult> {
            crate::locator::ocr::run_ocr(&std::fs::read(path).expect("png")).expect("ocr")
        };
        let opts = crate::locator::ocr::FindOptions { screen_width: w, screen_height: h, ..Default::default() };
        let after = ocr(&d.join(&fx.image));
        let before = d.join("before.png").is_file().then(|| ocr(&d.join("before.png")));
        let seen = |t: &str, pool: &[crate::locator::ocr::OcrResult]| {
            crate::locator::ocr::find_text(t, pool, &opts).winner.is_some()
        };
        let mut notes = Vec::new();
        let (mut new_spans, mut new_found) = (0, 0);
        for t in &spans {
            let was = before.as_deref().is_some_and(|b| seen(t, b));
            let is = seen(t, &after);
            notes.push(format!("{t:?} {}->{}", if was { "on" } else { "off" }, if is { "on" } else { "off" }));
            if !was {
                new_spans += 1;
                new_found += usize::from(is);
            }
        }
        let verdict = if spans.is_empty() {
            "nothing to check".to_string()
        } else if new_spans == 0 {
            format!("no evidence (all quoted text was already there) | {}", notes.join("; "))
        } else if new_found == new_spans {
            format!("MET | {}", notes.join("; "))
        } else {
            format!("not met | {}", notes.join("; "))
        };
        println!("[use-b] {:<9} {:<34} {verdict}", c.class, fx.id);
    }
}

/// H4 step 0, no model call: what Windows.Media.Ocr reads off each fixture of a battery, whole
/// at 1x and tiled at 2x and 3x (Lanczos3, as `try_region_ocr` upscales). Prints every line
/// with its box in fixture pixels. `NAVISUAL_EVAL_OCR_ITEMS` (comma-separated ids) narrows it.
#[test]
#[ignore = "reads the fixtures; run by name"]
fn ocr_probe() {
    println!();
    let dir = PathBuf::from(std::env::var("NAVISUAL_EVAL_DIR").expect("NAVISUAL_EVAL_DIR"));
    let root = dir.join(std::env::var("NAVISUAL_EVAL_BATTERY").unwrap_or_else(|_| "s2b_2160".into()));
    let only: Vec<String> = std::env::var("NAVISUAL_EVAL_OCR_ITEMS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let mut ids: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("battery dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("item.json").is_file())
        .collect();
    ids.sort();
    let png = |img: &image::RgbImage| -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).expect("png");
        buf.into_inner()
    };
    for d in ids {
        let fx: Fixture =
            serde_json::from_str(&std::fs::read_to_string(d.join("item.json")).unwrap()).unwrap();
        if !only.is_empty() && !only.contains(&fx.id) {
            continue;
        }
        let img = image::open(d.join(&fx.image)).expect("image").to_rgb8();
        let (w, h) = img.dimensions();
        println!("=== {} ({w}x{h})", fx.id);
        for up in [1u32, 2, 3] {
            // Tiles small enough that the upscaled tile stays under the engine's size limit,
            // overlapping by 60 px so a line cut by one edge is whole in the neighbour.
            let (tw, th) = if up == 1 { (w, h) } else { (1280.min(w), 720.min(h)) };
            let started = Instant::now();
            let mut lines: Vec<(u32, u32, u32, u32, String)> = Vec::new();
            let mut y = 0;
            loop {
                let mut x = 0;
                loop {
                    let cw = tw.min(w - x);
                    let ch = th.min(h - y);
                    let tile = image::imageops::crop_imm(&img, x, y, cw, ch).to_image();
                    let tile = if up == 1 {
                        tile
                    } else {
                        image::imageops::resize(&tile, cw * up, ch * up, image::imageops::FilterType::Lanczos3)
                    };
                    match crate::locator::ocr::run_ocr(&png(&tile)) {
                        Ok(rs) => {
                            // run_ocr returns each line, then its words; keep the lines only --
                            // a line is the one whose box contains the words that follow it.
                            let mut last: Option<(i32, i32, u32, u32)> = None;
                            for r in rs {
                                let inside = last.is_some_and(|(lx, ly, lw, lh)| {
                                    r.bbox.0 >= lx
                                        && r.bbox.1 >= ly
                                        && r.bbox.0 + r.bbox.2 as i32 <= lx + lw as i32
                                        && r.bbox.1 + r.bbox.3 as i32 <= ly + lh as i32
                                        && r.text.split(' ').count() == 1
                                });
                                if inside {
                                    continue;
                                }
                                last = Some(r.bbox);
                                let bx = x + (r.bbox.0.max(0) as u32) / up;
                                let by = y + (r.bbox.1.max(0) as u32) / up;
                                lines.push((bx, by, r.bbox.2 / up, r.bbox.3 / up, r.text));
                            }
                        }
                        Err(e) => println!("  [{up}x] tile {x},{y}: {e}"),
                    }
                    if x + cw >= w {
                        break;
                    }
                    x += cw - 60;
                }
                if y + th.min(h - y) >= h {
                    break;
                }
                y += th - 60;
            }
            lines.sort_by_key(|l| (l.1 / 8, l.0));
            lines.dedup_by(|a, b| a.4 == b.4 && a.0.abs_diff(b.0) < 40 && a.1.abs_diff(b.1) < 40);
            println!("  -- {up}x: {} line(s), {} ms", lines.len(), started.elapsed().as_millis());
            for (x, y, lw, lh, t) in &lines {
                println!("  [{up}x] ({x:4},{y:4} {lw:4}x{lh:2}) {t}");
            }
        }
    }
}

/// H3 fixtures from a real window: `NAVISUAL_CAPTURE_TITLE` (a substring of its title, the
/// front-most match), `NAVISUAL_CAPTURE_EXCLUDE` (optional), `NAVISUAL_CAPTURE_OUT` (the item
/// directory). Writes `screen.png` through the app's own capture, `ui.json` from
/// `a11y::snapshot_ui_tree`, and `item.json` with the production [Screen Elements] block --
/// empty when the app's own enumeration skips it (over the cap, or no tree) -- and an empty task
/// for a person to write. No model call.
#[cfg(windows)]
#[test]
#[ignore = "captures a live window; run by name"]
fn capture_ui_fixture() {
    let title = std::env::var("NAVISUAL_CAPTURE_TITLE").expect("NAVISUAL_CAPTURE_TITLE");
    let exclude = std::env::var("NAVISUAL_CAPTURE_EXCLUDE").unwrap_or_default();
    let out = PathBuf::from(std::env::var("NAVISUAL_CAPTURE_OUT").expect("NAVISUAL_CAPTURE_OUT"));
    std::fs::create_dir_all(&out).expect("out dir");
    let hwnd = crate::capture::find_window_by_title(&title, &exclude).expect("no window with that title");
    let info = crate::capture::get_window_info_for_hwnd(hwnd).expect("window info");
    let rect = info.rect;
    let img = crate::capture::capture_region_raw(rect, &[]).expect("capture");
    img.save(out.join("screen.png")).expect("png");
    let window_title = crate::capture::get_window_title(hwnd);
    let (elements_block, elements_block_states, sc) = match crate::locator::a11y::enumerate_context_elements(hwnd) {
        Ok(els) => (
            crate::ai::prompts::elements_context_block(&els, rect),
            crate::ai::prompts::elements_context_block_with(&els, rect, true),
            format!("{} elements, {} with a state", els.len(), els.iter().filter(|e| e.state.is_some()).count()),
        ),
        Err(e) => (String::new(), String::new(), e),
    };
    let nodes = crate::locator::a11y::snapshot_ui_tree(hwnd).unwrap_or_else(|e| panic!("snapshot: {e}"));
    std::fs::write(out.join("ui.json"), serde_json::to_string_pretty(&nodes).unwrap()).expect("ui.json");
    let id = out.file_name().unwrap().to_string_lossy().to_string();
    let item = serde_json::json!({
        "id": id,
        "task": "",
        "window_title": window_title,
        "window_rect": [rect.x, rect.y, rect.x + rect.width as i32, rect.y + rect.height as i32],
        "image": "screen.png",
        "pass_if": "text_or_bbox",
        "target_text_any": [],
        "target_text_none": [],
        "target_rect": null,
        "avoid_rect": null,
        "notes": "",
        "elements_block": if elements_block.is_empty() { serde_json::Value::Null } else { serde_json::json!(elements_block) },
        "elements_block_states": if elements_block_states.is_empty() { serde_json::Value::Null } else { serde_json::json!(elements_block_states) },
        "ui_snapshot": "ui.json",
    });
    std::fs::write(out.join("item.json"), serde_json::to_string_pretty(&item).unwrap() + "\n").expect("item.json");
    println!(
        "\n[capture] {id}: {}x{} at ({},{}) | {window_title:?} | Screen Elements: {sc} | ui_tree: {} nodes",
        rect.width,
        rect.height,
        rect.x,
        rect.y,
        nodes.len()
    );
}

/// Run one battery against one provider/model. Configured entirely by environment
/// variables -- see `tools/run-replay.ps1`, which sets them.
#[tokio::test]
#[ignore = "spends real BYOK money; run via tools/run-replay.ps1"]
async fn replay_battery() {
    // libtest prints "test eval::replay_battery ... " without a newline under --nocapture, so
    // without this the first result line is glued onto it and a line filter misses it.
    println!();
    let rc = RunConfig::from_env();
    if let Some(path) = rc.rescore.clone() {
        rescore(&rc, &path);
        return;
    }
    let fixtures = load_fixtures(&rc);
    assert!(!fixtures.is_empty(), "no fixtures under {}", rc.dir.join(&rc.battery).display());
    println!(
        "[eval] {} item(s) x {} repeat(s) | {} {} | reasoning={} | variant={} | cap ${:.2}{}",
        fixtures.len(),
        rc.repeat,
        rc.provider,
        rc.model,
        rc.reasoning.as_deref().unwrap_or("provider default"),
        if rc.variant.is_empty() { "none (the app's prompt)" } else { rc.variant.as_str() },
        rc.max_usd,
        if rc.dry_run { " | DRY RUN" } else { "" }
    );

    if rc.dry_run {
        let dump = rc.out.with_extension("dryrun");
        std::fs::create_dir_all(&dump).expect("dry-run dir");
        for (dir, fx) in &fixtures {
            let (b64, jpeg_len, w, h) = encode_fixture(&dir.join(&fx.image));
            let prompt = item_prompt(fx, &rc.voice_language, &rc.variant);
            std::fs::write(dump.join(format!("{}.prompt.txt", fx.id)), &prompt).expect("prompt dump");
            {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD.decode(&b64).expect("b64");
                std::fs::write(dump.join(format!("{}.sent.jpg", fx.id)), bytes).expect("jpeg dump");
            }
            println!(
                "[eval] {:<28} png {w}x{h} -> jpeg {jpeg_len} B | prompt {} chars",
                fx.id,
                prompt.chars().count()
            );
        }
        println!("[eval] dry run: prompts and the exact images written to {}", dump.display());
        return;
    }

    let session_dir = std::env::temp_dir().join(format!("navisual-eval-{}", std::process::id()));
    let mut router = AiRouter::new(
        base_config(&rc),
        CostTracker::new(None),
        SessionManager::new(session_dir.clone()),
        None,
    );

    let run_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut spent = 0.0f64;
    let mut tally = Tally::new();
    let mut ttfts = Vec::new();
    let mut totals = UsageDetail::default();

    'outer: for rep in 0..rc.repeat {
        for (dir, fx) in &fixtures {
            if spent >= rc.max_usd {
                println!("[eval] STOP: spent ${spent:.4} reached the ${:.2} cap", rc.max_usd);
                break 'outer;
            }
            let (b64, jpeg_len, w, h) = encode_fixture(&dir.join(&fx.image));
            let prompt = item_prompt(fx, &rc.voice_language, &rc.variant);
            prime_session(&mut router, fx);
            // H3: the item's accessibility snapshot, mapped onto its capture rect.
            router.ui_snapshot = match (&fx.ui_snapshot, rc.variant == "ui_tree") {
                (Some(file), true) => {
                    let nodes: Vec<crate::locator::UiNode> = serde_json::from_str(
                        &std::fs::read_to_string(dir.join(file)).expect("ui snapshot"),
                    )
                    .expect("ui snapshot json");
                    let [l, t, r, b] = fx.window_rect;
                    Some(std::sync::Arc::new(crate::ai::inspect::UiSource { nodes, capture: [l, t, r - l, b - t] }))
                }
                _ => None,
            };
            router.inspect_frame = config_looks(&rc.variant).then(|| {
                std::sync::Arc::new(
                    image::open(dir.join(&fx.image)).expect("fixture image").to_rgb8(),
                )
            });

            let started = Instant::now();
            let mut first_token: Option<Instant> = None;
            let result = router
                .send_guidance_request(&prompt, Some(&b64), None, |_, _| {
                    if first_token.is_none() {
                        first_token = Some(Instant::now());
                    }
                })
                .await;
            let total_ms = started.elapsed().as_millis();
            let ttft_ms = first_token.map(|t| t.duration_since(started).as_millis());
            let detail = router.take_client_usage();
            let inspects = router.take_inspects();
            let served_model = router.take_served_model();
            let (bill_in, bill_out) = router.get_last_usage();

            let mut row = serde_json::json!({
                "run_id": run_id,
                "rep": rep,
                "item": fx.id,
                "battery": rc.battery,
                "provider": rc.provider,
                "model": rc.model,
                "reasoning": rc.reasoning,
                "variant": rc.variant,
                "app_version": env!("CARGO_PKG_VERSION"),
                "png": [w, h],
                "jpeg_bytes": jpeg_len,
                "prompt_chars": prompt.chars().count(),
                "class": fx.continuation.as_ref().map(|c| c.class.clone()),
                "ttft_ms": ttft_ms,
                "total_ms": total_ms,
                "inspects": inspects,
                "served_model": served_model,
            });
            match result {
                Ok(resp) => {
                    let (outcome, why) = score(fx, &resp, w, h);
                    let lang = language_suspect(&fx.task, &resp);
                    let usd = crate::ai::pricing::estimate_cost(&rc.provider, &rc.model, bill_in, bill_out)
                        .unwrap_or(0.0);
                    spent += usd;
                    tally.add(outcome, lang);
                    if let Some(t) = ttft_ms {
                        ttfts.push(t);
                    }
                    if let Some(d) = detail {
                        totals.input += d.input;
                        totals.cached_input += d.cached_input;
                        totals.cache_write += d.cache_write;
                        totals.output += d.output;
                        totals.reasoning += d.reasoning;
                    }
                    let d = detail.unwrap_or_default();
                    println!(
                        "[eval] {:<28} {:<9} {}| ttft {:>5} ms | in {:>5} (cached {:>5}) out {:>5} (reasoning {:>5}) | ${usd:.4} | {why}",
                        fx.id,
                        format!("{outcome:?}"),
                        if lang { "LANG? " } else { "" },
                        ttft_ms.map_or("-".into(), |t| t.to_string()),
                        d.input,
                        d.cached_input,
                        d.output,
                        d.reasoning
                    );
                    for (i, step) in resp.steps.iter().enumerate() {
                        if let Some(e) = &step.expected_result {
                            println!("[eval] {:<28} step {} expects: {e}", "", i + 1);
                        }
                    }
                    row["outcome"] = serde_json::json!(outcome);
                    row["language_suspect"] = serde_json::json!(lang);
                    row["why"] = serde_json::json!(why);
                    row["usage"] = serde_json::json!(detail);
                    row["billed_in"] = serde_json::json!(bill_in);
                    row["billed_out"] = serde_json::json!(bill_out);
                    row["est_usd_flat"] = serde_json::json!(usd);
                    row["response"] = serde_json::to_value(&resp).unwrap_or_default();
                }
                Err(e) => {
                    // The whole chain: reqwest's top line is "error sending request for url",
                    // and what actually failed (a timeout, a reset, DNS) is only in the sources.
                    println!("[eval] {:<28} ERROR after {total_ms} ms | {e:#}", fx.id);
                    row["error"] = serde_json::json!(format!("{e:#}"));
                    // An empty balance or a bad key fails every call after it the same way;
                    // carrying on only fills the results file with copies of one error.
                    let msg = e.to_string();
                    if ["insufficient_quota", "credit_balance_exhausted", "invalid_api_key", "billing"]
                        .iter()
                        .any(|k| msg.contains(k))
                    {
                        append_line(&rc.out, &row);
                        println!("[eval] STOP: the provider refused on account grounds, not on this item");
                        break 'outer;
                    }
                }
            }
            append_line(&rc.out, &row);
        }
    }

    let _ = std::fs::remove_dir_all(&session_dir);
    let cached_share = if totals.input > 0 {
        totals.cached_input as f64 / totals.input as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "[eval] {} | ttft median {} ms | in {} (cached {:.1}%) out {} (reasoning {}) | ${spent:.4} at flat list rates",
        tally.line(),
        median(ttfts),
        totals.input,
        cached_share,
        totals.output,
        totals.reasoning
    );
    println!("[eval] results: {}", rc.out.display());
}

// ================================================================== S3: working memory

/// S3 (harness-plan.md): one scripted session. Every turn is something the user typed with the
/// screen they typed it on; the model's answers are recorded into the session exactly as `guide`
/// records them (`record_exchange`), so what reaches turn k is the app's own working memory --
/// the window, the pinned user turns, the model's rolling summary and goal. A fact shows on
/// screen up to `fact_turn` and never again; each probe asks for it at `distance` turns after,
/// on a branch the main line never sees.
#[derive(Debug, Clone, serde::Deserialize)]
struct SessionFixture {
    id: String,
    /// before_edit | closed_dialog | gone_toast | other_view -- what kind of fact.
    kind: String,
    /// The fact, for people reading the item.
    fact: String,
    /// 1-based: the last turn whose screen shows the fact.
    fact_turn: usize,
    /// Text that IS the fact; an answer recalls it when what the user is told contains one.
    expect_any: Vec<String>,
    /// The other candidates on the probe screen: an answer naming one of them alongside the fact
    /// listed the choices rather than remembering (luna, first run, the four Trash notes).
    #[serde(default)]
    distractors: Vec<String>,
    /// Where the fact can still be found in the app: an answer that sends the user there is
    /// `looked` -- the right result one step later, not memory.
    #[serde(default)]
    lookup_any: Vec<String>,
    turns: Vec<SessionTurn>,
    probes: Vec<SessionProbe>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SessionTurn {
    image: String,
    title: String,
    text: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SessionProbe {
    /// Sent as turn `fact_turn + distance`.
    distance: usize,
    image: String,
    title: String,
    text: String,
}

/// Every `session.json` under the battery directory, in directory order.
fn load_sessions(rc: &RunConfig) -> Vec<(PathBuf, SessionFixture)> {
    let root = rc.dir.join(&rc.battery);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("session.json").is_file())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| {
            let text = std::fs::read_to_string(d.join("session.json")).expect("session.json");
            let sx: SessionFixture = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", d.join("session.json").display()));
            (d, sx)
        })
        .filter(|(_, sx)| rc.only.is_empty() || rc.only.iter().any(|o| o == &sx.id))
        .collect()
}

/// The prompt and the summary argument `guide` would send for turn `k` of a typed session: turn 1
/// is a new task, every later turn a typed reply (the panel is guiding, so `isReply`).
/// The list of earlier screens comes from the session through `with_earlier_screens`, exactly as
/// `guide` adds it, so it is never stored in history and the anchors stay last.
#[allow(clippy::too_many_arguments)]
fn session_prompt(
    k: usize,
    text: &str,
    title: &str,
    original_task: &str,
    sample: Option<&str>,
    voice_language: &str,
    session: Option<&crate::ai::session::Session>,
    list_on: bool,
    offer_recall: bool,
) -> String {
    let window_context = crate::window_context_text(
        Some(&capture::format_window_prompt_info(title)),
        "",
        "",
        crate::os_context_line(),
        None,
        None,
    );
    let window_context = crate::with_session_memory(window_context, session, list_on, offer_recall);
    if k == 1 {
        crate::compose_turn1_prompt(text, &window_context, sample, voice_language)
    } else {
        crate::compose_reply_prompt(text, &window_context, original_task, sample, voice_language)
    }
}

fn normalize_fact(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_fact(text: &str, expect_any: &[String]) -> bool {
    let t = normalize_fact(text);
    expect_any.iter().any(|e| {
        let e = normalize_fact(e);
        !e.is_empty() && t.contains(&e)
    })
}

/// What the user gets: every instruction, every target_text, and anything put on their clipboard
/// -- luna answered "paste the offsite details" with the date on the clipboard.
fn told(resp: &NavigateStepResponse) -> String {
    resp.steps
        .iter()
        .map(|s| {
            format!(
                "{} {} {}",
                s.instruction,
                s.target_text.as_deref().unwrap_or(""),
                s.clipboard.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// A probe's outcome. `recalled`: the fact is in what the user is told, and no distractor with
/// it. `carried`: not yet, but it is in this answer's own summary, goal or plan -- an answer is
/// 1-4 steps, and the first run had terra open the rename command now and keep the old name for
/// the next step. `looked`: it sends the user back to where the fact still is. `denied`: it says
/// the fact never existed ("No share code was shown earlier in our session" -- flash, first run),
/// which tells the user they misremember. `asked`: the answer asks or admits it cannot tell -- an
/// honest miss. `missed`: it answered without the fact, which includes guessing. The phrase lists
/// are a first sort; read the answers.
fn probe_outcome(resp: &NavigateStepResponse, sx: &SessionFixture) -> &'static str {
    let said = told(resp);
    if has_fact(&said, &sx.expect_any) && !has_fact(&said, &sx.distractors) {
        return "recalled";
    }
    let kept = format!("{} {} {}", resp.state_summary, resp.goal, resp.plan_outline.join(" "));
    if has_fact(&kept, &sx.expect_any) && !has_fact(&kept, &sx.distractors) {
        return "carried";
    }
    if has_fact(&said, &sx.lookup_any) {
        return "looked";
    }
    let l = said.to_lowercase().replace('\u{2019}', "'");
    let denial = [
        "not mentioned", "wasn't mentioned", "was not shown", "wasn't shown", "never shown",
        "was not displayed", "wasn't displayed", "no earlier", "there was no", "there is no record",
    ];
    // "No share code was shown earlier": the negation sits before the noun.
    let no_noun = (l.starts_with("no ") || l.contains(" no "))
        && (l.contains("was shown earlier") || l.contains("was displayed earlier"));
    if no_noun || denial.iter().any(|p| l.contains(p)) {
        return "denied";
    }
    let honest = [
        "don't know", "do not know", "not sure", "can't see", "cannot see", "can't tell",
        "cannot tell", "no longer", "not visible", "isn't visible", "remind me", "what was",
        "which note", "do you remember", "if you remember", "don't have", "do not have",
        "not shown", "unable to",
    ];
    if resp.needs_input || honest.iter().any(|p| l.contains(p)) {
        "asked"
    } else {
        "missed"
    }
}

/// S3: replay each session, probing at its distances. `NAVISUAL_EVAL_WINDOW` sets the
/// conversation window in exchanges (the app's is 5) -- the memory plan's 3 / 5 / 8 A/B.
#[tokio::test]
#[ignore = "spends real BYOK money; run via tools/run-replay.ps1 -Sessions"]
async fn replay_sessions() {
    println!();
    let rc = RunConfig::from_env();
    // The 2026-10-10 `remember` ablation is rule 16 now; results tagged `remember` are that rule.
    // The app's default carries the list of earlier screens; `no_list` takes it away. `recall`:
    // H6, every earlier screen of the session offered to `recall` on every turn. (Before the list
    // was the default, the 2026-10-10 `earlier_list` runs were the list alone.)
    assert!(
        ["", "recall", "recall3", "no_list"].contains(&rc.variant.as_str()),
        "S3 variants: none, recall, recall3, no_list"
    );
    let list_on = rc.variant != "no_list";
    let recall_on = rc.variant.starts_with("recall");
    // `inline`: each probe is recorded into the session as the turn it was, so a later probe
    // sees what an earlier one looked up -- and whether the model wrote it down. `branch` (the
    // default): each probe is asked on a copy the main line never sees.
    let inline = env_or("NAVISUAL_EVAL_PROBES", "branch") == "inline";
    let window: usize = env_or("NAVISUAL_EVAL_WINDOW", "0").parse().unwrap_or(0);
    let sessions = load_sessions(&rc);
    assert!(!sessions.is_empty(), "no sessions under {}", rc.dir.join(&rc.battery).display());
    let session_dir = std::env::temp_dir().join(format!("navisual-eval-{}", std::process::id()));
    let mut router = AiRouter::new(
        base_config(&rc),
        CostTracker::new(None),
        SessionManager::new(session_dir),
        None,
    );
    if window > 0 {
        router.conversation_exchanges = window;
    }
    let mut out = if window > 0 {
        rc.out.with_file_name(format!(
            "{}-w{window}.jsonl",
            rc.out.file_stem().unwrap_or_default().to_string_lossy()
        ))
    } else {
        rc.out.clone()
    };
    if inline {
        out = out.with_file_name(format!("{}-inline.jsonl", out.file_stem().unwrap_or_default().to_string_lossy()));
    }
    println!(
        "[s3] {} session(s) x {} repeat(s) | {} {} | window {} exchanges | variant {} | probes {} | cap ${:.2}",
        sessions.len(),
        rc.repeat,
        rc.provider,
        rc.model,
        router.conversation_exchanges,
        if rc.variant.is_empty() { "none" } else { rc.variant.as_str() },
        if inline { "inline" } else { "branch" },
        rc.max_usd
    );
    let run_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut spent = 0.0f64;
    let mut outcomes: Vec<(String, usize, &'static str)> = Vec::new();

    'outer: for rep in 0..rc.repeat {
        for (dir, sx) in &sessions {
            router.session_manager.current_session = None;
            router.session_manager.create_session(sx.turns[0].text.clone());
            let mut summary = String::new();
            let mut sample: Option<String> = None;
            let mut trace = String::new();
            // H6: every main turn's screen as it was sent.
            let mut earlier: Vec<crate::ai::inspect::EarlierScreen> = Vec::new();
            // `k` is the session's turn number, `next` the next scripted turn: they part once an
            // inline probe has been recorded as a turn of its own.
            let mut k = 1usize;
            let mut next = 0usize;
            let mut sent = vec![false; sx.probes.len()];
            loop {
                if spent >= rc.max_usd {
                    println!("[s3] STOP: spent ${spent:.4} reached the ${:.2} cap", rc.max_usd);
                    break 'outer;
                }
                // A probe due at k: on a copy of the session as it stands after turn k-1, or, inline,
                // as turn k itself.
                let due = (0..sx.probes.len()).find(|&i| !sent[i] && sx.fact_turn + sx.probes[i].distance == k);
                if let Some(i) = due {
                    sent[i] = true;
                    let p = &sx.probes[i];
                    let snapshot = router.session_manager.current_session.clone();
                    let original = snapshot.as_ref().map(|s| s.task_description.clone()).unwrap_or_default();
                    let probe_sample = if crate::ai::prompts::is_language_sample(&p.text) {
                        Some(p.text.clone())
                    } else {
                        sample.clone()
                    };
                    let prompt = session_prompt(
                        k, &p.text, &p.title, &original, probe_sample.as_deref(), &rc.voice_language,
                        snapshot.as_ref(), list_on, recall_on && !earlier.is_empty(),
                    );
                    let (b64, _, _, _) = encode_fixture(&dir.join(&p.image));
                    router.inspect_frame = None;
                    router.recall_screens = (recall_on && !earlier.is_empty()).then(|| std::sync::Arc::new(earlier.clone()));
                    let started = Instant::now();
                    let result = router.send_guidance_request(&prompt, Some(&b64), Some(&summary), |_, _| {}).await;
                    let total_ms = started.elapsed().as_millis();
                    let mut recorded = false;
                    let detail = router.take_client_usage();
                    let looks = router.take_inspects();
                    let (bill_in, bill_out) = router.get_last_usage();
                    let usd = crate::ai::pricing::estimate_cost(&rc.provider, &rc.model, bill_in, bill_out).unwrap_or(0.0);
                    spent += usd;
                    let recalled_turns: Vec<usize> = looks.iter().flat_map(|l| l.turns.iter().copied()).collect();
                    let mut row = serde_json::json!({
                        "run_id": run_id, "rep": rep, "session": sx.id, "kind": sx.kind,
                        "battery": rc.battery, "provider": rc.provider, "model": rc.model,
                        "variant": rc.variant, "looks": looks,
                        "window": router.conversation_exchanges, "turn": k, "role": "probe", "inline": inline,
                        "distance": p.distance, "text": p.text, "fact": sx.fact, "summary_in": summary,
                        "total_ms": total_ms, "usage": detail, "est_usd_flat": usd,
                        "served_model": router.take_served_model(),
                    });
                    match result {
                        Ok(resp) => {
                            let outcome = probe_outcome(&resp, sx);
                            outcomes.push((sx.kind.clone(), p.distance, outcome));
                            println!(
                                "[s3] {:<22} d={:<2} PROBE {:<8} | summary had it: {} | recalled turns {:?} | ${usd:.4} | {:?}",
                                sx.id,
                                p.distance,
                                outcome,
                                if has_fact(&summary, &sx.expect_any) { "yes" } else { "no " },
                                recalled_turns,
                                told(&resp)
                            );
                            row["outcome"] = serde_json::json!(outcome);
                            row["response"] = serde_json::to_value(&resp).unwrap_or_default();
                            if inline {
                                let mut steps = resp.steps.clone();
                                crate::ai::types::sanitize_steps(&mut steps);
                                if let Some(session) = router.session_manager.current_session.as_mut() {
                                    crate::record_exchange(
                                        session,
                                        &p.text,
                                        false,
                                        &resp.goal,
                                        resp.plan_outline.clone(),
                                        resp.plan_completed_count,
                                        &resp.state_summary,
                                        &steps,
                                        Some(p.title.clone()),
                                        &resp.keep,
                                    );
                                }
                                summary = resp.state_summary.clone();
                                if crate::ai::prompts::is_language_sample(&p.text) {
                                    sample = Some(p.text.clone());
                                }
                                earlier.push(crate::ai::inspect::EarlierScreen { turn: k, jpeg_b64: b64.clone() });
                                recorded = true;
                            }
                        }
                        Err(e) => {
                            println!("[s3] {:<22} d={:<2} PROBE ERROR | {e:#}", sx.id, p.distance);
                            row["error"] = serde_json::json!(format!("{e:#}"));
                        }
                    }
                    append_line(&out, &row);
                    if recorded {
                        k += 1;
                        continue;
                    }
                    router.session_manager.current_session = snapshot;
                }
                if sent.iter().all(|s| *s) || next >= sx.turns.len() {
                    break;
                }
                // The main line: turn k as the app would send and record it.
                let t = &sx.turns[next];
                next += 1;
                if k == 1 || crate::ai::prompts::is_language_sample(&t.text) {
                    sample = crate::ai::prompts::is_language_sample(&t.text).then(|| t.text.clone());
                }
                let original = router
                    .session_manager
                    .current_session
                    .as_ref()
                    .map(|s| s.task_description.clone())
                    .unwrap_or_default();
                let prompt = session_prompt(
                    k, &t.text, &t.title, &original, sample.as_deref(), &rc.voice_language,
                    router.session_manager.current_session.as_ref(), list_on, recall_on && !earlier.is_empty(),
                );
                let (b64, _, _, _) = encode_fixture(&dir.join(&t.image));
                router.inspect_frame = None;
                router.recall_screens = (recall_on && !earlier.is_empty()).then(|| std::sync::Arc::new(earlier.clone()));
                let summary_arg = (k > 1).then(|| summary.clone());
                let mut attempt = 0;
                let (result, total_ms) = loop {
                    let started = Instant::now();
                    let r = router.send_guidance_request(&prompt, Some(&b64), summary_arg.as_deref(), |_, _| {}).await;
                    attempt += 1;
                    if r.is_ok() || attempt == 2 {
                        break (r, started.elapsed().as_millis());
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                };
                let detail = router.take_client_usage();
                let looks = router.take_inspects();
                let (bill_in, bill_out) = router.get_last_usage();
                let usd = crate::ai::pricing::estimate_cost(&rc.provider, &rc.model, bill_in, bill_out).unwrap_or(0.0);
                spent += usd;
                let mut row = serde_json::json!({
                    "run_id": run_id, "rep": rep, "session": sx.id, "kind": sx.kind,
                    "battery": rc.battery, "provider": rc.provider, "model": rc.model,
                    "variant": rc.variant, "looks": looks,
                    "window": router.conversation_exchanges, "turn": k, "role": "main",
                    "text": t.text, "summary_in": summary, "total_ms": total_ms,
                    "usage": detail, "est_usd_flat": usd, "served_model": router.take_served_model(),
                });
                match result {
                    Ok(resp) => {
                        let mut steps = resp.steps.clone();
                        crate::ai::types::sanitize_steps(&mut steps);
                        if let Some(session) = router.session_manager.current_session.as_mut() {
                            crate::record_exchange(
                                session,
                                &t.text,
                                false,
                                &resp.goal,
                                resp.plan_outline.clone(),
                                resp.plan_completed_count,
                                &resp.state_summary,
                                &steps,
                                Some(t.title.clone()),
                                &resp.keep,
                            );
                        }
                        summary = resp.state_summary.clone();
                        earlier.push(crate::ai::inspect::EarlierScreen { turn: k, jpeg_b64: b64.clone() });
                        if k >= sx.fact_turn {
                            trace.push(if has_fact(&summary, &sx.expect_any) { 'F' } else { '.' });
                        }
                        row["summary_has_fact"] = serde_json::json!(has_fact(&summary, &sx.expect_any));
                        let kept = router
                            .session_manager
                            .current_session
                            .as_ref()
                            .map(|s| s.kept.iter().map(|k| k.text.clone()).collect::<Vec<_>>())
                            .unwrap_or_default();
                        row["kept_has_fact"] = serde_json::json!(kept.iter().any(|k| has_fact(k, &sx.expect_any)));
                        row["kept"] = serde_json::json!(kept);
                        row["response"] = serde_json::to_value(&resp).unwrap_or_default();
                    }
                    Err(e) => {
                        println!("[s3] {:<22} turn {k} ERROR twice; session abandoned | {e:#}", sx.id);
                        row["error"] = serde_json::json!(format!("{e:#}"));
                        append_line(&out, &row);
                        break;
                    }
                }
                append_line(&out, &row);
                k += 1;
            }
            println!("[s3] {:<22} summary held the fact from turn {}: {trace}", sx.id, sx.fact_turn);
        }
    }
    let mut cells: std::collections::BTreeMap<usize, (usize, usize, usize)> = Default::default();
    for (_, d, o) in &outcomes {
        let c = cells.entry(*d).or_default();
        match *o {
            "recalled" | "carried" => c.0 += 1,
            "looked" | "asked" => c.1 += 1,
            "denied" => c.2 += 1,
            _ => c.2 += 1,
        }
    }
    for (d, (r, a, m)) in cells {
        println!("[s3] distance {d:>2}: remembered {r}, looked or asked {a}, missed {m}");
    }
    println!("[s3] spent ${spent:.4} | results: {}", out.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(pass_if: PassIf) -> Fixture {
        Fixture {
            id: "t".into(),
            task: "t".into(),
            window_title: "w".into(),
            window_rect: [0, 0, 1000, 1000],
            image: "x.png".into(),
            elements_block: None,
            elements_block_states: None,
            ui_snapshot: None,
            say_why: false,
            guide: None,
            guide_stale: None,
            guide_local: None,
            guide_text: None,
            guide_stale_text: None,
            guide_local_text: None,
            learn_request: None,
            pass_if,
            target_text_any: vec!["Q3 forecast (draft 2)".into()],
            target_text_none: vec!["Q2 forecast (draft 2)".into()],
            target_rect: Some([100, 100, 200, 200]),
            avoid_rect: Some([500, 500, 600, 600]),
            avoid_first_only: false,
            instruction_any: vec![],
            instruction_script: None,
            instruction_all: vec![],
            instruction_none: vec![],
            continuation: None,
        }
    }

    // The two halves of a recovery, from the S2 pilot's own answers.
    #[test]
    fn noticing_without_recovering_and_recovering_without_noticing_both_fail() {
        let mut f = fx(PassIf::TextOrBbox);
        f.target_text_any = vec!["undo".into()];
        f.target_text_none = vec!["vendor contract".into()];
        f.instruction_any = vec!["undo".into()];
        f.instruction_all = vec!["undo".into(), "hiring loop".into()];
        // terra: misread the list, pointed at Undo, said "undo" in passing.
        let terra = resp(r#"{"steps":[{"instruction":"The note has been deleted. Leave the confirmation as is unless you want to undo it.","target_text":"Undo"}]}"#);
        assert_eq!(score(&f, &terra, 1000, 1000).0, Outcome::Wrong);
        // luna: saw the wrong deletion, carried on without restoring it.
        let luna = resp(r#"{"steps":[{"instruction":"The wrong note, \"Hiring loop\", was deleted. Hover over \"Vendor contract\" and click its trash icon.","target_text":"Vendor contract"}]}"#);
        assert_eq!(score(&f, &luna, 1000, 1000).0, Outcome::Wrong);
        let right = resp(r#"{"steps":[{"instruction":"That deleted \"Hiring loop\" by mistake. Click Undo to bring it back.","target_text":"Undo"}]}"#);
        assert_eq!(score(&f, &right, 1000, 1000).0, Outcome::Pass);
    }

    #[test]
    fn instruction_all_takes_alternatives_and_instruction_none_vetoes() {
        let mut f = fx(PassIf::AvoidText);
        f.instruction_all = vec!["still|not bold".into()];
        let ack = resp(r#"{"steps":[{"instruction":"It is still not bold. Press Ctrl+B again."}]}"#);
        assert_eq!(score(&f, &ack, 1000, 1000).0, Outcome::Pass);
        let silent = resp(r#"{"steps":[{"instruction":"Press Ctrl+B."}]}"#);
        assert_eq!(score(&f, &silent, 1000, 1000).0, Outcome::Wrong);
        let mut g = fx(PassIf::AvoidText);
        g.instruction_none = vec!["ctrl+b".into()];
        let done = resp(r#"{"steps":[{"instruction":"The paragraph is bold now. You're done."}]}"#);
        assert_eq!(score(&g, &done, 1000, 1000).0, Outcome::Pass);
        let toggles = resp(r#"{"steps":[{"instruction":"Press Ctrl+B to make it bold."}]}"#);
        assert_eq!(score(&g, &toggles, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn use_b_looks_only_for_quoted_text() {
        assert_eq!(quoted_spans("The \"Page Number\" menu opens."), vec!["Page Number"]);
        assert_eq!(
            quoted_spans("The \u{201C}Settings\u{201D} dialog opens, with an \"Autosave\" switch."),
            vec!["Settings", "Autosave"]
        );
        assert!(quoted_spans("no visible change").is_empty());
        assert!(quoted_spans("The menu opens.").is_empty());
    }

    // S2: turn 2 goes out the way `guide` sends it, and the expectation rides along only in
    // the H1 arm -- the canned turn 1 carries one in both, as a model with H1 on would have.
    #[test]
    fn a_session_probe_is_graded_by_what_the_user_is_told() {
        let resp = |instruction: &str, needs_input: bool| -> NavigateStepResponse {
            serde_json::from_value(serde_json::json!({
                "steps": [{"instruction": instruction, "target_text": ""}],
                "state_summary": "", "needs_input": needs_input
            }))
            .unwrap()
        };
        let sx: SessionFixture = serde_json::from_value(serde_json::json!({
            "id": "t", "kind": "before_edit", "fact": "", "fact_turn": 2,
            "expect_any": ["Q3 planning"], "distractors": ["Old agenda"], "lookup_any": ["version history"],
            "turns": [], "probes": []
        }))
        .unwrap();
        assert_eq!(probe_outcome(&resp("Click the title and type Q3 Planning.", false), &sx), "recalled");
        assert_eq!(probe_outcome(&resp("What was the name before?", true), &sx), "asked");
        assert_eq!(probe_outcome(&resp("Click the title and type Q3 notes.", false), &sx), "missed");
        assert_eq!(probe_outcome(&resp("Restore Q3 planning or Old agenda?", true), &sx), "asked", "a list is not recall");
        assert_eq!(probe_outcome(&resp("Open View, then Version history.", false), &sx), "looked");
        assert_eq!(probe_outcome(&resp("No share code was shown earlier in our session.", false), &sx), "denied");
        assert_eq!(probe_outcome(&resp("A share code was not mentioned earlier.", true), &sx), "denied");
        assert_eq!(probe_outcome(&resp("No share code was displayed earlier in this session.", false), &sx), "denied");
        let mut later = resp("Click File to find Rename.", false);
        later.state_summary = "GOAL: rename it back to Q3 planning.".into();
        assert_eq!(probe_outcome(&later, &sx), "carried", "the name kept for the next step");
        assert!(has_fact("renamed from 'Q3-planning' to Q3 budget", &sx.expect_any), "punctuation is not the fact");
    }

    #[test]
    fn a_continuation_item_replays_turn_two() {
        let mut f = fx(PassIf::Text);
        f.task = "Export this note as a PDF.".into();
        f.continuation = Some(serde_json::from_value(serde_json::json!({
            "class": "correct",
            "turn1": {
                "steps": [{"instruction": "Click File in the menu bar.", "target_text": "File",
                           "expected_result": "The File menu opens, showing \"Export\"."}],
                "state_summary": "GOAL: export as PDF. DONE: nothing yet.",
                "needs_input": false
            },
            "completed": 1,
            "click": ["MenuItem", "File"]
        })).unwrap());
        let c = f.continuation.clone().unwrap();
        let off = continuation_prompt(&f, &c, "auto", "");
        assert!(off.starts_with("[User completed: \"Click File in the menu bar.\"] The previous state summary: GOAL: export as PDF."));
        assert!(off.contains("[Last user action] The user clicked MenuItem \"File\"."));
        assert!(!off.contains("Expected after that step"));
        let on = continuation_prompt(&f, &c, "auto", "expect");
        assert!(on.contains("[Expected after that step: The File menu opens, showing \"Export\".]"));
    }

    fn resp(json: &str) -> NavigateStepResponse {
        serde_json::from_str(json).expect("response")
    }

    #[test]
    fn phrase_match_needs_a_boundary() {
        assert!(contains_phrase("q3 forecast (draft 2)", "draft 2"));
        assert!(!contains_phrase("boldface", "bold"));
        assert!(!contains_phrase("draft 22", "draft 2"));
    }

    #[test]
    fn text_score_rejects_the_lookalike() {
        let f = fx(PassIf::Text);
        let good = resp(r#"{"steps":[{"instruction":"x","target_text":"Q3 forecast (draft 2)"}]}"#);
        let bad = resp(r#"{"steps":[{"instruction":"x","target_text":"Q2 forecast (draft 2)"}]}"#);
        assert_eq!(score(&f, &good, 1000, 1000).0, Outcome::Pass);
        assert_eq!(score(&f, &bad, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn bbox_score_uses_ymin_xmin_order() {
        let f = fx(PassIf::Bbox);
        // [ymin, xmin, ymax, xmax] = centre (150, 150) on a 1000x1000 frame -> inside.
        let hit = resp(r#"{"steps":[{"instruction":"x","target_bbox":[100,100,200,200]}]}"#);
        // Same numbers read the wrong way round would still land inside, so use an
        // asymmetric box: y 100-200, x 500-600 -> centre (550, 150) -> outside.
        let miss = resp(r#"{"steps":[{"instruction":"x","target_bbox":[100,500,200,600]}]}"#);
        assert_eq!(score(&f, &hit, 1000, 1000).0, Outcome::Pass);
        assert_eq!(score(&f, &miss, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn avoid_text_catches_name_or_bbox() {
        let f = fx(PassIf::AvoidText);
        let by_name = resp(r#"{"steps":[{"instruction":"x","target_text":"Q2 forecast (draft 2)"}]}"#);
        let by_box = resp(r#"{"steps":[{"instruction":"x","target_bbox":[500,500,600,600]}]}"#);
        let clean = resp(r#"{"steps":[{"instruction":"It is already on."}]}"#);
        assert_eq!(score(&f, &by_name, 1000, 1000).0, Outcome::Wrong);
        assert_eq!(score(&f, &by_box, 1000, 1000).0, Outcome::Wrong);
        assert_eq!(score(&f, &clean, 1000, 1000).0, Outcome::Pass);
    }

    #[test]
    fn the_right_answer_in_words_passes_without_a_pointer() {
        let mut f = fx(PassIf::TextOrBbox);
        f.instruction_any = vec!["ctrl+z".into(), "undo".into()];
        let words = resp(r#"{"steps":[{"instruction":"Press Ctrl+Z right away to get it back."}]}"#);
        let wrong = resp(r#"{"steps":[{"instruction":"Retype the paragraph from memory."}]}"#);
        assert_eq!(score(&f, &words, 1000, 1000).0, Outcome::Pass);
        assert_eq!(score(&f, &wrong, 1000, 1000).0, Outcome::NoTarget);
    }

    #[test]
    fn the_plan_counts_as_the_answer_in_words() {
        let mut f = fx(PassIf::TextOrBbox);
        f.instruction_any = vec!["data validation".into()];
        let r = resp(
            r#"{"steps":[{"instruction":"Select C2:C11 first.","target_text":"Name Box"}],
                "plan_outline":["Select the Status cells","Open Data Validation","Confirm"]}"#,
        );
        assert_eq!(score(&f, &r, 1000, 1000).0, Outcome::Pass);
    }

    #[test]
    fn right_words_do_not_rescue_a_pointer_at_a_known_wrong_target() {
        let mut f = fx(PassIf::TextOrBbox);
        f.instruction_any = vec!["clear the filter".into()];
        // target_text_none holds "Q2 forecast (draft 2)" -- the look-alike.
        let r = resp(
            r#"{"steps":[{"instruction":"Clear the filter here.","target_text":"Q2 forecast (draft 2)"}]}"#,
        );
        assert_eq!(score(&f, &r, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn a_trap_by_order_only_fails_as_the_first_step() {
        let mut f = fx(PassIf::AvoidText);
        f.avoid_first_only = true;
        let later = resp(
            r#"{"steps":[{"instruction":"Close the dialog","target_text":"Cancel"},
                         {"instruction":"Then click it","target_text":"Q2 forecast (draft 2)"}]}"#,
        );
        let first = resp(r#"{"steps":[{"instruction":"Click it","target_text":"Q2 forecast (draft 2)"}]}"#);
        assert_eq!(score(&f, &later, 1000, 1000).0, Outcome::Pass);
        assert_eq!(score(&f, &first, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn a_keyboard_answer_is_no_target_not_wrong() {
        let f = fx(PassIf::TextOrBbox);
        // What luna actually returned for "Zoom out a little": a shortcut and a zero box.
        let shortcut = resp(r#"{"steps":[{"instruction":"Press Ctrl and minus","target_bbox":[0,0,0,0]}]}"#);
        assert_eq!(score(&f, &shortcut, 1000, 1000).0, Outcome::NoTarget);
    }

    #[test]
    fn a_chinese_request_needs_a_chinese_answer() {
        let mut f = fx(PassIf::Text);
        f.target_text_any = vec!["Export".into()];
        f.instruction_script = Some("cjk".into());
        let zh = resp(r#"{"steps":[{"instruction":"点击工具栏上的“Export”按钮。","target_text":"Export"}]}"#);
        let en = resp(r#"{"steps":[{"instruction":"Click Export on the toolbar.","target_text":"Export"}]}"#);
        assert_eq!(score(&f, &zh, 1000, 1000).0, Outcome::Pass);
        assert_eq!(score(&f, &en, 1000, 1000).0, Outcome::Wrong);
    }

    #[test]
    fn quoting_a_chinese_label_is_not_drift() {
        let quoted = resp(r#"{"steps":[{"instruction":"Click the switch next to “自动保存” to turn it on."}]}"#);
        let drifted = resp(r#"{"steps":[{"instruction":"点击自动保存旁边的开关。"}]}"#);
        assert!(!language_suspect("Turn on autosave.", &quoted));
        assert!(language_suspect("Turn on autosave.", &drifted));
    }

    #[test]
    fn language_flag_catches_unaccented_spanish_but_not_english() {
        let es = resp(r#"{"steps":[{"instruction":"Presiona Ctrl y la tecla menos una vez para alejar"}]}"#);
        let en = resp(r#"{"steps":[{"instruction":"Autosave is already on. Click Done to close Settings."}]}"#);
        assert!(language_suspect("Zoom out a little.", &es));
        assert!(!language_suspect("Turn on autosave.", &en));
    }

    #[test]
    fn fixture_prompt_carries_window_and_task() {
        let mut f = fx(PassIf::Text);
        f.task = "Open the second draft".into();
        f.window_title = "Contoso Notes".into();
        let p = fixture_prompt(&f, "auto", "");
        assert!(p.contains("Open the second draft"));
        assert!(p.contains("[Current Window Info]\nTitle: 'Contoso Notes'"));
        assert!(!p.contains("Rect:"), "the app no longer states the window frame");
        // The comparison variant adds the frame back and changes nothing else.
        let old = fixture_prompt(&f, "auto", "with_rect");
        assert!(old.contains("[Current Window Info]\nTitle: 'Contoso Notes'\nRect: [0, 0, 1000, 1000]"));
        assert_eq!(old.replace("\nRect: [0, 0, 1000, 1000]", ""), p);
    }
}
