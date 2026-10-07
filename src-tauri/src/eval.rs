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
        assert!(
            ["", "with_rect"].contains(&variant.as_str()),
            "unknown NAVISUAL_EVAL_VARIANT {variant:?} (known: with_rect)"
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
            let fx: Fixture = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", d.join("item.json").display()));
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

/// The turn-1 prompt `guide` would build for this window: no pack, no app-state channel and
/// no click record, because a fixture has none of them.
fn fixture_prompt(fx: &Fixture, voice_language: &str, variant: &str) -> String {
    let info = match variant {
        // The prompt before 2026-10-07: the title AND the window's frame.
        "with_rect" => capture::format_window_info(&fx.window_title, fx.window_rect),
        _ => capture::format_window_prompt_info(&fx.window_title),
    };
    let window_context = crate::window_context_text(
        Some(&info),
        "",
        fx.elements_block.as_deref().unwrap_or(""),
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
    let said_it = !fx.instruction_any.is_empty()
        && resp.steps.iter().any(|s| {
            let i = normalize(&s.instruction);
            fx.instruction_any.iter().any(|p| contains_phrase(&i, &normalize(p)))
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
    let judge = |ok: bool| if ok { Outcome::Pass } else { Outcome::Wrong };
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
            let trap = considered.into_iter().find(|s| {
                s.target_text
                    .as_deref()
                    .is_some_and(|t| text_matches(t, &fx.target_text_none))
                    || matches!((fx.avoid_rect, real_bbox(s.target_bbox)), (Some(r), Some(b)) if inside(bbox_centre(b, w, h), r))
            });
            match trap {
                Some(s) => (Outcome::Wrong, format!("instructed the trap: {:?}", s.instruction)),
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
fn language_suspect(task: &str, resp: &NavigateStepResponse) -> bool {
    if !task.is_ascii() {
        return false;
    }
    const FOREIGN: &[&str] = &["el", "la", "los", "las", "una", "para", "y", "con", "del", "que", "haz", "presiona", "clique"];
    resp.steps.iter().any(|s| {
        let i = s.instruction.as_str();
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
    config
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
            let prompt = fixture_prompt(fx, &rc.voice_language, &rc.variant);
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
            let prompt = fixture_prompt(fx, &rc.voice_language, &rc.variant);

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
                "ttft_ms": ttft_ms,
                "total_ms": total_ms,
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
                    println!("[eval] {:<28} ERROR | {e}", fx.id);
                    row["error"] = serde_json::json!(e.to_string());
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
            pass_if,
            target_text_any: vec!["Q3 forecast (draft 2)".into()],
            target_text_none: vec!["Q2 forecast (draft 2)".into()],
            target_rect: Some([100, 100, 200, 200]),
            avoid_rect: Some([500, 500, 600, 600]),
            avoid_first_only: false,
            instruction_any: vec![],
        }
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
