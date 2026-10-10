// Consolidated 2026-07-13 (design #8): the prompt had grown incident-by-incident to 23
// rules; weak free-tier models follow shorter, themed prompts better. Every behavioral
// constraint is preserved — redundant pairs are merged, nothing dropped. Old→new map
// (for tracing SDD/appendix references to historical rule numbers):
//   1,2→1 · 5→2 · 17,18→3 · 8→4 · 3→5 · 4→6 · 4b→7 · 11→8 · 15,16→9 · 10,20→10 ·
//   6→11 · 12,13→12 · 19→13 (the LANGUAGE rule) · 21,23,9→14 (the SPOKEN-TEXT rule) ·
//   14→15 · 7→16 · 22→17
pub const SYSTEM_PROMPT: &str = r#"You are Navisual, a real-time guidance assistant. You observe the user's
screen and provide step-by-step navigation instructions. You NEVER perform
actions — the user does everything.

== STEPS & FLOW ==
1. Provide 1-4 steps per response; group small sequential actions (click, type,
   press Enter) into one response. Mark the last meaningful action in a sequence
   as checkpoint=true so the system waits for the user to complete it before
   calling you again.
2. If the screen shows the user completed the step, acknowledge and advance. If
   the step did something other than intended — the wrong formatting, text typed
   into the wrong place, the wrong item opened, changed or deleted — say plainly
   what happened, and make your FIRST step undo it (Ctrl+Z or the Undo button;
   Esc for a wrong menu or dialog; the same button again for wrong formatting).
   Only then retry the original step. Never build on top of an unintended change.
3. TRUST CONFIRMATIONS FOR WHAT LEAVES NO TRACE; NEVER REPEAT: if the user says
   they completed a step ("done", "yes", "I did it", "ok"), TRUST them and
   advance when the action leaves no visible trace (placing a cursor, focusing a
   field, selecting text, copying). But when the step should have changed
   something you can see — a button turning on, a word appearing in the status
   bar, text turning bold, a name changing — look for that change first. If it
   is not there, the screen outranks the confirmation: say it did not take
   effect, and give a different way to do it than the one that just failed. If
   you are about to give the exact same instruction as the immediately
   preceding step, STOP: either (a) advance assuming the action succeeded, or
   (b) ask a short yes/no question ("Did you click at the end of that line?").
   NEVER issue the identical instruction twice in a row.
4. If you need clarification, set needs_input=true and ask a short question in
   the instruction field.

== TARGETING ==
5. Refer to UI elements by their visible text label in target_text and their UI
   role in target_role (e.g. "button", "tab", "link"). Keep target_text SHORT —
   1-5 distinctive words (product titles: brand + first 2-3 words). Also
   describe the element's appearance and position in the instruction.
   NEVER use a single letter or glyph as target_text for toolbar/ribbon buttons
   (the locator cannot distinguish "A" from any word containing "a") — use the
   semantic tooltip name: "Bold" not "B", "Save" not 💾, "Undo" not ↩. Single
   characters ONLY when the label IS the full identity (a tab labeled "1", a
   keyboard key labeled "A").
   BARE NUMBERS ARE THE WORST CASE, not an exception to the rule above — a page
   number, a numbered-list marker ("2."), a heading style name ("Heading 2"),
   and a slide/page counter can all show the same bare digit on one screen, so
   "the label IS the full identity" does NOT make a page number safe to target
   alone. NEVER target a bare number or counter without target_nearby_text
   pointing at something distinctive right next to it (a section/header tag,
   a caption, a neighboring field) — leaving nearby_text empty here is how a
   page-number step ends up pointing at an unrelated numbered list item or a
   ribbon style gallery instead.
   ALWAYS set target_nearby_text to a short readable label visible right next
   to the target — it anchors the search and rejects coincidental matches of
   target_text elsewhere on screen, critical when the target text appears more
   than once (similar buttons in a list; a toolbar icon whose name also appears
   as a section header; any bare number, per above). It must be a DIFFERENT
   label than target_text — a self-anchor provides no disambiguation. Omit it
   only when genuinely no readable text is near the target (fully icon-only
   toolbar) — never for a bare number.
6. TARGET BOUNDING BOX: for every step with a target_text, return target_bbox
   as [ymin, xmin, ymax, xmax] in NORMALIZED 0–1000 coordinates: 0 is the
   top/left edge and 1000 the bottom/right edge of the image, regardless of its
   pixel size — never raw pixels. Wrap the element tightly. Omit target_bbox
   for steps with no target_text (scroll-only, subtitle-only).
7. SCREEN ELEMENTS LIST: the message may include a [Screen Elements] list of
   detected interactive elements (id | role | name | center). If your target
   appears there, set target_element_id to its integer id — ONLY ids from the
   list, never invented. Still fill target_text (and target_bbox) as normal;
   omit target_element_id when the target is not listed or no list is present.
   DISABLED (greyed-out) elements: a row marked "| DISABLED" cannot be clicked
   — the app itself reports it as unavailable, which is more reliable than how
   it looks in the screenshot. NEVER write an instruction that tells the user to
   click, press, tick, or choose a DISABLED element; telling someone to click a
   dead control is worse than saying nothing, because they assume they did it
   wrong. Instead work out WHY it is unavailable and give the step that makes it
   available — a greyed control almost always has an unmet precondition (nothing
   is selected yet, a required field is empty, a prerequisite checkbox is off, a
   different mode or tab must be active first). Point at the control that
   satisfies the precondition, not at the dead one. You MAY still target a
   DISABLED element when the user's own question is about it ("why is X greyed
   out?") — then the instruction explains its state, and must not use a click
   verb. If you cannot tell from this screen what would enable it, say plainly
   that it is unavailable here and may have to be changed elsewhere (another
   dialog, another app, an administrator) — an honest dead end beats a confident
   guess that wastes the user's time.
8. SCROLL FIRST: if the element the user needs is not visible in the current
   view, tell them to scroll BEFORE telling them to click — a scroll step is
   its own instruction with no target_text, which is how you say "there is
   nothing to point at yet". A new screenshot after scrolling lets you verify
   visibility first.
9. WHAT YOU CAN SEE: the screenshot normally shows only the foreground
   application window — you cannot see the Taskbar, Start Menu, Desktop icons,
   or background apps. If the user needs something outside the current window,
   DO NOT GUESS what it shows — ask them to bring that window or app into
   focus first. [Current Window Info] at the end of the prompt gives the
   focused window's title: if focus moved to an application unrelated
   to the task, do not guess inside the wrong application — ask the user to
   bring the correct one back into focus.
10. GREY AREAS & THE INSTRUCTION PANEL: neutral grey areas in the screenshot
    are intentionally blanked (the instruction panel, another window covering
    the target, or space outside the target's bounds). Do not describe them,
    interact with them, or guess what is behind them. If grey covers a UI
    element the user genuinely needs, ask them to drag the instruction panel
    aside or bring the target window forward — NEVER to close anything. The
    panel showing these instructions is NEVER a target: never tell the user to
    locate it, open it, focus it, or find it in the taskbar/system tray.

== TYPING & CLIPBOARD ==
11. Whenever your instruction asks the user to type, enter, or paste specific
    text anywhere — form field, dialog, address bar, terminal command, search
    box, filename — ALWAYS put that exact text in the clipboard field so they
    can paste instead of typing. EXCEPTION: NEVER put keyboard shortcuts
    ("Ctrl+A", "Alt+Tab", "Win+D") in clipboard — shortcuts are pressed on the
    keyboard, they cannot be pasted.
12. COMMANDS, URLS & INSTALLS: before navigating to download or install
    software, confirm the correct URL or source via needs_input=true — never
    guess URLs (similar names are different products); skip if the user already
    provided it. When the task is to find an install command or code snippet on
    a webpage, read the current page before navigating anywhere, then put the
    exact command in clipboard; if multiple variants exist (npm vs pip, Windows
    vs macOS), ask which via needs_input=true first.

== WORDING — instructions are READ ALOUD to a person ==
13. LANGUAGE: respond ENTIRELY in the language of the USER'S TYPED OR SPOKEN
    REQUEST — every instruction and the state_summary. ONLY the user's request
    determines the language. The app's own interface is frequently in a
    DIFFERENT language from the user, and that must NOT change your reply: the
    screenshot, the [Screen Elements] list, on-screen content, file names,
    window titles, and UI labels are all evidence about WHERE to click, never
    about which language to write in. A Chinese request on an English app →
    reply in Chinese; an English request on a Chinese/Japanese/German app →
    reply in English. When you must name a UI label the user has to find, you
    may quote it verbatim, but the surrounding instruction stays in the user's
    language. Keep that language across every follow-up and correction; never
    switch or mix languages mid-task. A LANGUAGE LOCK line at the end of the
    prompt names the target language — obey it over any other signal.
14. PLAIN SPOKEN TEXT: never put machine references in instruction text — no
    pixel positions, x/y values, or coordinate ranges ("the grey box starts at
    x ≈ 322" is forbidden; coordinates belong ONLY in target_bbox), and no ids
    from the [Screen Elements] list ("click the Search box (id 30)" is
    forbidden; the id belongs ONLY in target_element_id). Describe targets by
    name, appearance, and relative position ("the ruler icon near the bottom of
    the left toolbar"). Refer to web browsers generically — "your browser",
    never Edge/Chrome/Firefox. Each instruction is READ ALOUD and shown as a
    one-glance caption on screen, so keep it to one or two short sentences —
    aim for under 160 characters — in plain text: no Markdown (**bold**,
    headings, bullet lists, code ticks). If the user asked a question whose
    answer needs more than that, give the short answer in the instruction and
    the detail in state_summary.

== TASK JUDGMENT & OUTPUT ==
15. DESKTOP APP TASKS: if the user asks for help with a desktop application
    (Word, Excel, Photoshop, VS Code, …), guide them through the UI yourself —
    NEVER tell them to open a browser or search online. The answer is usually
    inside that application; when it genuinely lives elsewhere (Windows
    Settings, or another application), say so and guide them there, asking them
    to bring that window into focus first (rule 9).
16. state_summary is your ONLY memory between turns — earlier turns get
    truncated away, so anything you leave out is gone for good. Rewrite it in
    full each turn, carrying: the user's GOAL in their own words (keep it
    verbatim until they change it), any CONSTRAINTS they stated ("not the
    title page"), what is DONE so far, and anything TRIED THAT FAILED so you
    do not propose it again. Never assume an earlier turn is still visible.
    Not shown to the user. When a screen shows something the user may still
    need once it is gone, put it in keep, copied exactly: kept notes are never
    rewritten and come back on every later turn, so write each one once. The
    same goes for anything you look up on an earlier screen.
17. SUGGESTED NEXT TASKS: when the current task looks complete, or no task is
    stated yet, you MAY set suggested_tasks (top-level, next to state_summary)
    to up to 3 SHORT suggestions phrased as tasks the user would ask for
    ("Print this document"), each under 80 characters, in the user's language
    (rule 13). Optional prefill candidates only — never instructions, never
    auto-executed. Do NOT include them mid-sequence while steps remain, and
    never suggest anything involving the instruction panel.
18. PLAN OUTLINE: set plan_outline (top-level) to 2-8 short plain-language
    milestones toward the goal — a route overview the user can open from a
    "Working on: ..." panel, like a map app showing the whole trip rather
    than only the next turn. Not turn-by-turn (steps/instruction already
    cover that) and not required for a simple one-step task. REVISE the
    whole list — replace it, don't append — whenever a step succeeds, fails,
    or your understanding of the route otherwise changes; a stale plan is
    worse than none. Omit the field to leave whatever you set last unchanged.
    Also set plan_completed_count (top-level, next to plan_outline) to how
    many of its milestones are already done, counting from the first (0 =
    still on the first one). Update it as milestones finish, and again
    whenever you revise plan_outline itself — count against the NEW list,
    not the old one. Omit to leave the previous count unchanged; it means
    nothing without plan_outline, so omit both together.

Use the navigate_step tool for all responses."#;

// ---- H1: predict, then verify (navisual-internal docs/harness-plan.md) ----------------------
//
// Behind `Config::expected_result`, off by default. Off, every function here hands back exactly
// what the request carried before H1 existed -- `Cow::Borrowed` of the same constant -- so the
// production prompt is byte-for-byte unchanged until a measurement decides otherwise.

/// The line every system prompt ends with; rule 19 goes in just before it.
const PROMPT_TAIL: &str = "\n\nUse the navigate_step tool for all responses.";

/// Rule 19, appended after rule 18 when H1 is on.
pub const EXPECTED_RESULT_RULE: &str = r#"19. EXPECTED RESULT: for every step, set expected_result to one short
    sentence saying what the screen will show right after the user does
    that step. Put any text that will newly appear in double quotes, exactly
    as it will be displayed - e.g. The "Page Number" menu opens. A step
    whose effect cannot be seen (select all, copy) gets exactly "no visible
    change". Describe the result, never the action. Navisual checks it
    against the next screen; the user does not see it."#;

/// The schema description of the step field (tool-calling providers).
const EXPECTED_RESULT_FIELD: &str = "What the screen will show right after the user does this step, in one short sentence. Put any text that will newly appear in double quotes, exactly as it will be displayed. Exactly \"no visible change\" when the step changes nothing visible.";

/// The same, as a line in a prompted-JSON field list (DeepSeek, Qwen, Ollama, custom).
const EXPECTED_RESULT_FIELD_LINE: &str = "- expected_result: one short sentence - what the screen will show right after this step, with any newly appearing text in double quotes exactly as displayed; exactly \"no visible change\" when the step changes nothing visible (required)";

// ---- Rule 16's schema twin ------------------------------------------------------------------
//
// The description of `state_summary` in the navigate_step schema says what rule 16 says. It was
// three identical literals (Anthropic, Gemini, managed -- which OpenAI also uses) until rule 16
// gained its fifth slot on 2026-10-10: S3 found the summary kept what the user DID and never what
// the screen SHOWED, because no slot asked for it (harness-plan.md). One constant now, so the
// copies cannot disagree; the two prompted-JSON field lines are pinned by a test instead.
pub const STATE_SUMMARY_FIELD: &str = "Your ONLY memory between turns — earlier turns are truncated away. Rewrite it in full each turn, carrying: the user's GOAL in their own words (verbatim until they change it), any CONSTRAINTS they stated, what is DONE, and anything TRIED THAT FAILED. Never assume an earlier turn is still visible. Facts to keep for the rest of the session go in keep, not here.";

/// The schema description of `keep` (rule 16). No example, by rule 21: an example names a kind of
/// fact, and that kind is then what the model keeps.
pub const KEEP_FIELD: &str = "Facts the user may still need after they leave the screen, each copied exactly as it was shown. Kept notes are never rewritten and come back on every later turn: add a fact once, and never repeat one already kept. Omit when there is nothing new.";

/// The system prompt, carrying rule 19 when H1 is on.
pub fn system_prompt(expected_result: bool) -> std::borrow::Cow<'static, str> {
    if !expected_result {
        return std::borrow::Cow::Borrowed(SYSTEM_PROMPT);
    }
    // Asserted by `h1_system_prompt_gets_rule_19_before_the_tail`.
    let at = SYSTEM_PROMPT.rfind(PROMPT_TAIL).unwrap_or(SYSTEM_PROMPT.len());
    std::borrow::Cow::Owned(format!(
        "{}\n{}{}",
        &SYSTEM_PROMPT[..at],
        EXPECTED_RESULT_RULE,
        &SYSTEM_PROMPT[at..]
    ))
}

/// A prompted-JSON format instruction, with the `expected_result` line added after the
/// `target_element_id` line when H1 is on.
pub fn with_expected_result_field(
    format_instruction: &'static str,
    expected_result: bool,
) -> std::borrow::Cow<'static, str> {
    const ANCHOR: &str = "\n- target_element_id:";
    let Some(start) = format_instruction.find(ANCHOR).filter(|_| expected_result) else {
        return std::borrow::Cow::Borrowed(format_instruction);
    };
    let line_end = format_instruction[start + 1..]
        .find('\n')
        .map(|i| start + 1 + i)
        .unwrap_or(format_instruction.len());
    std::borrow::Cow::Owned(format!(
        "{}\n{}{}",
        &format_instruction[..line_end],
        EXPECTED_RESULT_FIELD_LINE,
        &format_instruction[line_end..]
    ))
}

/// Use A: what follows `[User completed: "…"]` on the next request when the completed step
/// carried an expectation -- the expectation itself, and how to weigh it against the new screen.
/// Three outcomes, not two, because an unmet expectation may mean the user went ahead
/// (harness-plan.md H1); `no visible change` hands the step back to rule 3.
pub fn expectation_note(expected: &str) -> String {
    format!(
        " [Expected after that step: {expected}] Compare it with the current screen. If it \
         matches, continue. If the screen is further along (the user went ahead), continue \
         from where they are and never send them back. If neither, say what happened and \
         correct course. For \"no visible change\", trust the confirmation."
    )
}

/// Adds the required `expected_result` property to a navigate_step schema's step items,
/// wherever the provider nests them (`input_schema`, `parameters`, `function.parameters`, or
/// Ollama's bare format schema). Returns whether it found them.
pub fn add_expected_result(schema: &mut serde_json::Value) -> bool {
    let Some(obj) = schema.as_object_mut() else {
        return false;
    };
    if obj.contains_key("steps") {
        let items = &mut obj["steps"]["items"];
        if let Some(props) = items.get_mut("properties").and_then(|p| p.as_object_mut()) {
            props.insert(
                "expected_result".into(),
                serde_json::json!({"type": "string", "description": EXPECTED_RESULT_FIELD}),
            );
            if let Some(req) = items.get_mut("required").and_then(|r| r.as_array_mut()) {
                req.push(serde_json::json!("expected_result"));
            }
            return true;
        }
    }
    obj.values_mut().any(add_expected_result)
}

// Category-neutral (audit 2026-07-12 C8): a specific steering hint usually follows this
// (the frontend folds in one of the ✗ Wrong reason categories — "wrong spot", "already
// did that", etc.) but not always (a bare Wrong with no reason reaches this alone), so
// this base text must work standalone AND not prescribe a remedy that fights the category
// that follows. It used to say "describe the target element differently", which directly
// contradicts the "already did that → advance, don't re-point" category. Now it sets up
// the situation and asks for a fix in the general — compatible with re-pointing, advancing,
// or scrolling, whichever the following guidance (or the model's own read) calls for.
pub const CORRECTION_CONTEXT: &str =
    "The user pressed the 'wrong' button on the previous instruction. Re-examine the \
current screen carefully and provide a corrected instruction that resolves the problem.";

pub fn session_resume_template(state_summary: &str) -> String {
    format!(
        "Resuming session. Last known state: {}. \
        Here is the current screen. Assess whether the state is still valid \
        and provide the next instruction.",
        state_summary
    )
}

/// Format a Nav-Pack's context for injection into the prompt (Workstream C, hooks 1 & 2):
/// the pack's free-text guidance plus, when present, a shortcut table that steers the AI to
/// return a `clipboard` key press instead of pointing at an icon. Returns an empty string when
/// the pack carries neither, so callers can append unconditionally. Shortcuts iterate in the
/// map's stable (sorted) order.
pub fn pack_context_block(
    target_app: &str,
    injection: &str,
    shortcuts: &std::collections::BTreeMap<String, String>,
) -> String {
    let injection = injection.trim();
    if injection.is_empty() && shortcuts.is_empty() {
        return String::new();
    }
    let app = if target_app.trim().is_empty() {
        "this application".to_string()
    } else {
        target_app.trim().to_string()
    };
    let mut block = format!("\n[App Guide: {app}]\n");
    if !injection.is_empty() {
        block.push_str(injection);
        block.push('\n');
    }
    if !shortcuts.is_empty() {
        block.push_str(
            "Keyboard shortcuts for this app — when one matches the user's goal, instruct \
them to press it and put the key combo in the instruction (NOT the clipboard — shortcuts \
are pressed, not pasted), instead of pointing at a button:\n",
        );
        for (action, key) in shortcuts {
            block.push_str(&format!("  • {action}: {key}\n"));
        }
    }
    block
}

/// S.2 (v0.7 Workstream S) — format the Structured-Context element list for the prompt.
/// `capture_rect` is the virtual-desktop rect the screenshot covers; element centres are
/// emitted in the same normalized 0–1000 space as `target_bbox` (Decision 3) so the
/// model can cross-reference the list against what it sees in the screenshot. Names are
/// truncated for display only — the id resolves into the full snapshot. Empty input or
/// a degenerate capture rect → empty string (callers append unconditionally).
pub fn elements_context_block(
    elements: &[crate::locator::ContextElement],
    capture_rect: crate::capture::Rect,
) -> String {
    if elements.is_empty() || capture_rect.width == 0 || capture_rect.height == 0 {
        return String::new();
    }
    const NAME_DISPLAY_MAX: usize = 60;
    let mut block = String::from(
        "\n[Screen Elements] — interactive elements detected on the current screen.\n\
         If your target is one of these, set target_element_id to its id (and still fill target_text).\n\
         A row ending in | DISABLED is greyed out and CANNOT be clicked — see rule 7.\n\
         id | role | name | center x,y (0-1000)\n",
    );
    for el in elements {
        let cx = ((el.rect.x - capture_rect.x) as f64 + el.rect.width as f64 / 2.0)
            / capture_rect.width as f64
            * 1000.0;
        let cy = ((el.rect.y - capture_rect.y) as f64 + el.rect.height as f64 / 2.0)
            / capture_rect.height as f64
            * 1000.0;
        let name: String = if el.name.chars().count() > NAME_DISPLAY_MAX {
            el.name.chars().take(NAME_DISPLAY_MAX).collect::<String>() + "…"
        } else {
            el.name.clone()
        };
        // Marker only on disabled rows: enabled is the overwhelming majority, so this
        // costs ~0 tokens in the normal case and stays visually obvious when it fires.
        block.push_str(&format!(
            "{} | {} | \"{}\" | {},{}{}\n",
            el.id,
            el.role.to_lowercase(),
            name,
            (cx.round() as i64).clamp(0, 1000),
            (cy.round() as i64).clamp(0, 1000),
            if el.enabled { "" } else { " | DISABLED" },
        ));
    }
    block
}

/// Restate the session's goal on every continuation turn.
///
/// The goal used to be stated exactly once — `initial_context_template` on turn 1 — and then
/// evicted with the conversation window, which holds only ~5 exchanges. From roughly the sixth
/// exchange the model was steering from `state_summary`: its own rolling paraphrase of its own
/// previous paraphrase, with nothing left saying what "done" meant.
///
/// ~20 tokens/turn. Empty when there is no goal to restate, so turn 1 (where the request *is*
/// the prompt) is unaffected. Deliberately placed at the TAIL for the same reason the language
/// directive is: a short goal near the top gets out-shouted by a full screenshot and a
/// `[Screen Elements]` list in between.
/// One line naming the OS, for the prompt.
///
/// **Why this is worth tokens:** Windows 10 and 11 ship substantially different Settings and
/// Explorer UIs, and until now the model had to infer which one from pixels alone — the
/// window title carries no version and `[Current Window Info]` sends only the title. When
/// it infers wrong it does not fail visibly; it produces a confident, plausible path through a
/// UI that is not on screen, which is the v0.7.25 failure mode and the dangerous one.
///
/// **The framing is copied deliberately** from the `[App State]` blocks: the screenshot stays
/// authoritative for layout. Without that sentence a version line invites the model to trust a
/// remembered layout over the pixels in front of it, which would make this worse than sending
/// nothing.
pub fn os_context(os: &str) -> String {
    format!(
        "
[System] {os}. Authoritative for the OS version only; the screenshot remains authoritative for layout and for anything not stated here. Do not assume a UI from the version — use it to choose wording and to rule out steps that cannot exist on this release.
"
    )
}

pub fn goal_anchor(task_description: &str) -> String {
    let t = task_description.trim();
    if t.is_empty() {
        return String::new();
    }
    format!(
        "\n\nTHE USER'S GOAL, restated: \"{t}\". Everything you do serves this. \
         If the current screen shows it is already achieved, say so instead of inventing \
         another step."
    )
}

pub fn initial_context_template(task_description: &str) -> String {
    format!(
        "The user wants help with the following task: {}\n\n\
        Here is their current screen. Analyze it and provide the first navigation instruction.",
        task_description
    )
}

/// English name of a BCP-47 tag or bare language code, for the LANGUAGE LOCK. `None` for
/// anything we can't name confidently → the caller falls back to the exemplar form.
fn lang_name(code: &str) -> Option<&'static str> {
    let primary = code
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some(match primary.as_str() {
        "en" => "English",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        "ru" => "Russian",
        "ar" => "Arabic",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "pt" => "Portuguese",
        "it" => "Italian",
        _ => return None,
    })
}

#[derive(Default)]
struct ScriptCounts {
    latin: u32,
    kana: u32,
    hangul: u32,
    han: u32,
    cyr: u32,
    arab: u32,
}

impl ScriptCounts {
    fn nonlatin(&self) -> u32 {
        self.kana + self.hangul + self.han + self.cyr + self.arab
    }
}

/// Per-script letter counts. Shared by [`dominant_script_lang`] (which non-Latin script, if any)
/// and [`is_language_sample`] (is the message a reliable language signal at all).
fn count_scripts(text: &str) -> ScriptCounts {
    let mut c = ScriptCounts::default();
    for ch in text.chars() {
        let u = ch as u32;
        if ch.is_ascii_alphabetic() || (0x00c0..=0x024f).contains(&u) {
            c.latin += 1;
        } else if (0x3040..=0x30ff).contains(&u) {
            c.kana += 1;
        } else if (0xac00..=0xd7af).contains(&u) || (0x1100..=0x11ff).contains(&u) {
            c.hangul += 1;
        } else if (0x4e00..=0x9fff).contains(&u) || (0x3400..=0x4dbf).contains(&u) {
            c.han += 1;
        } else if (0x0400..=0x04ff).contains(&u) {
            c.cyr += 1;
        } else if (0x0600..=0x06ff).contains(&u) {
            c.arab += 1;
        }
    }
    c
}

/// Dominant non-Latin script of `text` as a language code ("zh"/"ja"/"ko"/"ru"/"ar"), or `None`
/// when the text is Latin-scripted or its non-Latin content is only incidental.
///
/// Deliberately stricter than `tts::strong_script_lang` (which fires on a SINGLE non-Latin char,
/// because a voice must pronounce whatever is on screen). Here the non-Latin script must actually
/// DOMINATE: the user pastes long, sometimes mixed-language instructions, and one stray CJK
/// app-name inside an English request must NOT flip the whole reply to Chinese.
pub(crate) fn dominant_script_lang(text: &str) -> Option<&'static str> {
    let c = count_scripts(text);
    // Kana is unique to Japanese, so it disambiguates ja from zh even when kanji dominate.
    let dominates = |n: u32| n >= 2 && n * 2 >= c.latin;
    if dominates(c.kana) {
        return Some("ja");
    }
    let (code, count) = [("ko", c.hangul), ("zh", c.han), ("ru", c.cyr), ("ar", c.arab)]
        .into_iter()
        .max_by_key(|&(_, n)| n)?;
    dominates(count).then_some(code)
}

/// True when a single script (Latin OR a specific non-Latin) clearly dominates `text` — i.e. the
/// message is a reliable sample of the language the user is *currently* writing in, worth storing
/// as the session-sticky reply language. Short or mixed fragments (a lone "OK", a filename dropped
/// into a Chinese sentence) return false, so they don't flip the language.
///
/// The symmetric partner of [`dominant_script_lang`]: it ALSO fires on a dominant-Latin message,
/// which is exactly how a user switches BACK ("how do I…" / "please speak English." after a
/// Chinese stretch). The whole model is "reply in the language of your last substantial message."
pub(crate) fn is_language_sample(text: &str) -> bool {
    let c = count_scripts(text);
    let nl = c.nonlatin();
    // Non-Latin: responsive (a couple of chars — CJK is morpheme-dense), so switching INTO a
    // non-Latin language stays easy for a non-English speaker.
    if nl >= 2 && nl >= 2 * c.latin {
        return true;
    }
    // Latin: needs real mass (~two words), so a lone "ok"/"done"/a filename dropped into a
    // non-Latin session does NOT flip us to English — only a genuine phrase does.
    c.latin >= 6 && c.latin >= 2 * nl
}

/// Best-effort "is this English?" — true when the text contains ≥2 distinctive English function
/// words. These are deliberately words the OTHER major Latin languages (French/Spanish/German/…)
/// do NOT share ("the", "you", "how", "with", "using", …), so a non-English Latin request won't
/// trip it and stays on the same-language exemplar. Lets the LANGUAGE LOCK NAME English (the
/// strong form) for the common case, instead of the weaker "same language as this message" that
/// gpt-5.4-mini was observed to ignore (drifting to Japanese/Georgian on all-English tasks).
fn looks_like_english(text: &str) -> bool {
    const EN: &[&str] = &[
        "the", "you", "your", "how", "what", "make", "using", "use", "with", "this", "that",
        "please", "help", "want", "need", "show", "create", "add", "select", "click", "and",
        "for", "are", "from", "open", "into", "when", "where", "which", "step", "next", "then",
    ];
    let mut hits = 0u8;
    for w in text.split(|c: char| !c.is_ascii_alphabetic()) {
        if !w.is_empty() && EN.contains(&w.to_ascii_lowercase().as_str()) {
            hits += 1;
            if hits >= 2 {
                return true;
            }
        }
    }
    false
}

/// The end-of-prompt LANGUAGE LOCK — applied to EVERY turn's prompt, including turn 1.
///
/// Why last, and why every turn (recurring live issue, first seen 2026-07-13; user re-reported
/// 2026-07-20 for the harder case where the user's language differs from the target app's UI):
/// Rule 13 lives deep in a long static system prompt (low salience), while a screenshot and a
/// `[Screen Elements]` list full of the APP's UI language sit right beside it, actively pulling a
/// weak model the other way. On multi-turn sessions the machine-generated `[User completed: …]`
/// turn carries no user language signal, one drifted reply feeds its own summary back, and the
/// original request scrolls out of history. This restates the target language at the point of
/// highest recency, every turn.
///
/// `sample` is the session-sticky language sample — the user's LAST substantial message (the
/// caller updates it whenever [`is_language_sample`] is true, in EITHER script direction), so a
/// mid-session switch takes effect and survives the machine `[User completed:]` turns. When absent
/// it falls back to `request_text`.
///
/// Resolution order on that source text: (1) a dominant non-Latin script → name the language
/// outright (the strongest form a weak model follows); (2) else an explicitly-chosen
/// `VOICE_LANGUAGE` (rescues pinyin mis-transcription of Chinese speech, and honors a user who
/// picked a language); (3) else the text itself as a same-language exemplar — which also flips a
/// session BACK to a Latin language once the user writes Latin again. Empty only when nothing
/// anchors it. The model is: "reply in the language of your last substantial message."
pub fn reply_language_directive(
    sample: Option<&str>,
    request_text: &str,
    voice_language: &str,
) -> String {
    let src = sample
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(request_text.trim());
    let named_lock = |name: &str| {
        format!(
            "\n\nIMPORTANT — LANGUAGE LOCK: the user's language is {name}. Write EVERY instruction \
and the state_summary ENTIRELY in {name}, no matter what language appears in the screenshot, the \
[Screen Elements] list, UI labels, window titles, or earlier turns. Never switch or mix languages.\n\
EXCEPT target_text and target_nearby_text: these must stay EXACTLY as the UI itself displays them, \
copied verbatim in the UI's own language/script — never translated into {name}. The locator matches \
target_text against the literal on-screen label; a translated target_text matches nothing and the \
pointer fails to appear. This applies even when the whole conversation is in {name} and the app's UI \
is not."
        )
    };
    // (1) dominant non-Latin script → name it outright.
    if let Some(name) = dominant_script_lang(src).and_then(lang_name) {
        return named_lock(name);
    }
    // (1b) English is the common case AND cleanly detectable (its function words aren't shared by
    // the other major Latin languages), so name it explicitly — the strong form — instead of the
    // weak exemplar. Live: gpt-5.4-mini IGNORED the exemplar and drifted to Japanese (turn 1) and
    // Georgian on all-English tasks; "Write ENTIRELY in English" is far harder for a model to
    // override. French/Spanish/… don't hit `looks_like_english`, so they stay on the exemplar
    // (which is correct for them — no risk of mis-naming a Latin language).
    if looks_like_english(src) {
        return named_lock("English");
    }
    // (2) Latin / undetermined: an explicit VOICE_LANGUAGE setting.
    let v = voice_language.trim();
    if !v.is_empty() && !v.eq_ignore_ascii_case("auto") {
        if let Some(name) = lang_name(v) {
            return named_lock(name);
        }
    }
    // (3) else the source text as a same-language exemplar. Cap the echo so a long pasted
    // instruction doesn't bloat the tail — a prefix is enough of a language sample.
    if src.is_empty() {
        return String::new();
    }
    let exemplar: String = src.chars().take(160).collect();
    format!(
        "\n\nIMPORTANT — LANGUAGE LOCK: write EVERY instruction and the state_summary in the SAME \
LANGUAGE as this message from the user, no matter what language appears in the screenshot, UI \
labels, window titles, or earlier turns. Never switch or mix languages.\nUser: \"{exemplar}\"\n\
EXCEPT target_text and target_nearby_text: these must stay EXACTLY as the UI itself displays them, \
copied verbatim in the UI's own language/script — never translated to match the user's message. The \
locator matches target_text against the literal on-screen label; a translated target_text matches \
nothing and the pointer fails to appear. This applies even when the app's UI is in a different \
language than the conversation."
    )
}

#[cfg(test)]
mod tests {
    use super::{elements_context_block, pack_context_block};

    // The line once carried three runs of ten spaces mid-sentence -- the mark of a string
    // continuation flattened into literal spaces by an edit -- and went out like that on every
    // request. Found by the replay harness's prompt dump, which shows the exact text sent.
    // The model is told the window's title and nothing else: a stated window size next to a
    // downscaled image made the OpenAI models normalise boxes against the wrong size
    // (capture::format_window_prompt_info has the measurement). Rule 9 named "Title and Class"
    // for years after Class was dropped -- a sentence describing behaviour owned by other code.
    #[test]
    fn the_model_gets_the_window_title_and_no_frame() {
        assert_eq!(crate::capture::format_window_prompt_info("Book1 - Excel"), "Title: 'Book1 - Excel'");
        assert!(!super::SYSTEM_PROMPT.contains("Title and Class"));
        // Rule 16 sends facts to keep, and every field list that restates the schema has it.
        assert!(super::SYSTEM_PROMPT.contains("put it in keep, copied exactly"), "rule 16");
        assert!(!super::SYSTEM_PROMPT.contains("anything you have SEEN"), "the summary slot is gone");
        assert!(super::STATE_SUMMARY_FIELD.contains("go in keep, not here"));
        for (who, format) in [
            ("deepseek", crate::ai::deepseek::JSON_FORMAT_INSTRUCTION),
            ("ollama", crate::ai::ollama::JSON_FORMAT_INSTRUCTION),
        ] {
            assert!(format.contains("- keep: facts the user may still need"), "{who}'s field line");
        }
    }

    #[test]
    fn os_context_has_no_runs_of_spaces() {
        let line = super::os_context("Windows 11 (build 26200)");
        assert!(!line.contains("  "), "{line:?}");
        assert!(line.contains("remains authoritative for layout"));
    }
    use std::collections::BTreeMap;

    fn ctx_el(id: u32, name: &str, role: &str, x: i32, y: i32, w: u32, h: u32) -> crate::locator::ContextElement {
        ctx_el_state(id, name, role, x, y, w, h, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn ctx_el_state(
        id: u32,
        name: &str,
        role: &str,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        enabled: bool,
    ) -> crate::locator::ContextElement {
        crate::locator::ContextElement {
            id,
            name: name.to_string(),
            role: role.to_string(),
            rect: crate::capture::Rect {
                x,
                y,
                width: w,
                height: h,
            },
            enabled,
        }
    }

    #[test]
    fn elements_block_normalizes_centers_to_capture_rect() {
        // Capture rect origin (100, 50), 2000×1000. An element at VD (1090, 530) 20×40
        // → centre VD (1100, 550) → relative (1000, 500) → normalized (500, 500).
        let els = vec![ctx_el(3, "Save As", "Button", 1090, 530, 20, 40)];
        let rect = crate::capture::Rect {
            x: 100,
            y: 50,
            width: 2000,
            height: 1000,
        };
        let block = elements_context_block(&els, rect);
        assert!(block.contains("[Screen Elements]"));
        assert!(block.contains("3 | button | \"Save As\" | 500,500\n"), "{block}");
        // Centres are clamped, never out of the 0–1000 space.
        let off = vec![ctx_el(1, "X", "Button", -500, -500, 10, 10)];
        let block = elements_context_block(&off, rect);
        assert!(block.contains("| 0,0\n"), "{block}");
    }

    #[test]
    fn elements_block_marks_disabled_only() {
        // Found live 2026-07-26 (LibreOffice → Canon driver): a DISABLED
        // "Custom Paper Size..." was selected and the user told to click it, because
        // nothing in the pipeline consulted UIA IsEnabled. Enabled rows must stay
        // byte-identical (no token cost, no prompt churn); only disabled rows are marked.
        let rect = crate::capture::Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        };
        let els = vec![
            ctx_el_state(1, "Custom Paper Size...", "Button", 100, 100, 200, 20, false),
            ctx_el_state(2, "Page Options...", "Button", 400, 100, 200, 20, true),
        ];
        let block = elements_context_block(&els, rect);
        assert!(
            block.contains("1 | button | \"Custom Paper Size...\" | 200,110 | DISABLED\n"),
            "{block}"
        );
        assert!(
            block.contains("2 | button | \"Page Options...\" | 500,110\n"),
            "{block}"
        );
        // The enabled row carries no marker at all.
        assert_eq!(block.matches("DISABLED").count(), 2); // 1 header mention + 1 row
        // The header tells the model what the marker means.
        assert!(block.contains("CANNOT be clicked"), "{block}");
    }

    #[test]
    fn elements_block_empty_or_degenerate_yields_nothing() {
        let rect = crate::capture::Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 500,
        };
        assert!(elements_context_block(&[], rect).is_empty());
        let els = vec![ctx_el(1, "Save", "Button", 10, 10, 20, 20)];
        let degenerate = crate::capture::Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        assert!(elements_context_block(&els, degenerate).is_empty());
    }

    #[test]
    fn elements_block_truncates_long_names_for_display() {
        let long = "Extensions (Ctrl+Shift+X) - 4 require restart and a very long tail here";
        let els = vec![ctx_el(1, long, "TabItem", 10, 10, 20, 20)];
        let rect = crate::capture::Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 500,
        };
        let block = elements_context_block(&els, rect);
        assert!(block.contains('…'), "long names are ellipsised: {block}");
        assert!(!block.contains("very long tail"), "{block}");
        assert!(block.contains("tabitem"), "roles are lowercased: {block}");
    }

    #[test]
    fn empty_pack_yields_empty_block() {
        assert!(pack_context_block("Blender", "   ", &BTreeMap::new()).is_empty());
    }

    #[test]
    fn injection_only_has_no_shortcut_section() {
        let block = pack_context_block("TurboTax", "TurboTax web.", &BTreeMap::new());
        assert!(block.contains("[App Guide: TurboTax]"));
        assert!(block.contains("TurboTax web."));
        assert!(!block.contains("shortcuts"));
    }

    #[test]
    fn shortcuts_render_sorted_and_labeled() {
        let mut sc = BTreeMap::new();
        sc.insert("Rotate".to_string(), "R".to_string());
        sc.insert("Move (grab)".to_string(), "G".to_string());
        let block = pack_context_block("Blender", "Blender 3D.", &sc);
        assert!(block.contains("• Move (grab): G"));
        assert!(block.contains("• Rotate: R"));
        // BTreeMap order is alphabetical: "Move…" before "Rotate".
        assert!(block.find("Move (grab)").unwrap() < block.find("Rotate").unwrap());
    }

    #[test]
    fn blank_target_app_falls_back() {
        let block = pack_context_block("", "Some guidance.", &BTreeMap::new());
        assert!(block.contains("[App Guide: this application]"));
    }

    #[test]
    fn directive_exemplar_for_non_english_latin() {
        use super::reply_language_directive;
        // A non-English Latin request (French) doesn't hit looks_like_english, so it stays on the
        // same-language exemplar (which is correct — we must not mis-name it "English").
        let a = reply_language_directive(None, "faire un tableau avec ces données", "auto");
        assert!(a.contains("faire un tableau avec ces données"));
        assert!(a.contains("SAME LANGUAGE"));
        assert!(!a.contains("English"));
        // Empty / whitespace + auto → nothing to anchor on.
        assert!(reply_language_directive(None, "", "auto").is_empty());
        assert!(reply_language_directive(None, "   ", "auto").is_empty());
    }

    #[test]
    fn directive_names_english_for_english_request() {
        use super::reply_language_directive;
        // The common case: an all-English request gets the STRONG named form ("in English"),
        // not the weak exemplar that gpt-5.4-mini drifted past (→ Japanese/Georgian).
        let a = reply_language_directive(None, "make pivot table using the data", "auto");
        assert!(a.contains("English"), "{a}");
        assert!(!a.contains("SAME LANGUAGE")); // named form, not exemplar
        // A single stray English-ish word must NOT trigger it (needs ≥2 distinctive words).
        let b = reply_language_directive(None, "the", "auto");
        assert!(!b.contains("the user's language is English"), "{b}");
    }

    #[test]
    fn directive_names_language_from_dominant_script() {
        use super::reply_language_directive;
        // A Chinese request names Chinese outright, regardless of voice setting.
        let a = reply_language_directive(None, "帮我做一个数据透视表", "auto");
        assert!(a.contains("Chinese"), "{a}");
        assert!(!a.contains("data")); // no exemplar branch
    }

    #[test]
    fn directive_uses_explicit_voice_language_when_script_is_latin() {
        use super::reply_language_directive;
        // Latin request (e.g. pinyin mis-transcription) + an explicit zh-CN setting → name Chinese.
        let a = reply_language_directive(None, "bang wo zuo", "zh-CN");
        assert!(a.contains("Chinese"), "{a}");
    }

    #[test]
    fn directive_sticky_sample_wins_over_request_and_setting() {
        use super::reply_language_directive;
        // The sticky sample (user wrote "请说中文" mid-session) is the authority — it beats an
        // English request text and even an en-US setting; the switch is not suppressed.
        let a = reply_language_directive(Some("请说中文"), "open the File menu", "en-US");
        assert!(a.contains("Chinese"), "{a}");
        assert!(!a.contains("File")); // request text is not consulted when a sample is present
    }

    #[test]
    fn directive_sample_flips_back_to_latin() {
        use super::reply_language_directive;
        // The screenshot bug: a Chinese session, then the user writes a substantial Latin message.
        // The Latin sample must flip us back — NOT stay stuck on the original Chinese request.
        let a = reply_language_directive(Some("please speak English."), "帮我做数据透视表", "auto");
        assert!(!a.contains("Chinese"), "{a}");
        assert!(a.contains("please speak English."), "{a}");
    }

    #[test]
    fn is_language_sample_gates_on_dominance() {
        use super::is_language_sample;
        // Substantial messages in either script are samples.
        assert!(is_language_sample("请说中文"));
        assert!(is_language_sample("please speak English"));
        // A lone Latin token / one-word ack must NOT flip a non-Latin session to English.
        assert!(!is_language_sample("ok"));
        assert!(!is_language_sample("done"));
        // A Latin phrase with an incidental CJK label still reads as a Latin sample.
        assert!(is_language_sample("open the 文件 menu and click save"));
    }

    #[test]
    fn dominant_script_ignores_incidental_cjk() {
        use super::dominant_script_lang;
        // A couple of Han chars inside an English sentence must NOT read as Chinese.
        assert_eq!(dominant_script_lang("Open the 文件 menu and click Save"), None);
        // A predominantly-Chinese sentence with a stray English word IS Chinese.
        assert_eq!(dominant_script_lang("点击 File 菜单里的保存按钮"), Some("zh"));
        // Kana disambiguates Japanese from Chinese even when kanji are present.
        assert_eq!(dominant_script_lang("ファイルを保存してください"), Some("ja"));
        assert_eq!(dominant_script_lang("just plain english"), None);
    }

    // H1 (harness-plan.md). Off is the production default and must send exactly what was sent
    // before H1 existed: the switch is a measurement, and a measurement that changes the
    // control group measures nothing.
    #[test]
    fn h1_off_sends_the_prompt_unchanged() {
        use std::borrow::Cow;
        assert!(matches!(super::system_prompt(false), Cow::Borrowed(p) if p == super::SYSTEM_PROMPT));
        let fmt = "Step fields:\n- target_element_id: an id (optional)\n\nTop-level fields:";
        assert!(matches!(super::with_expected_result_field(fmt, false), Cow::Borrowed(f) if f == fmt));
        assert!(!super::SYSTEM_PROMPT.contains("expected_result"));
    }

    // Rules 2-3, rewritten 2026-10-09 (harness-plan.md §10): the screen outranks a reported
    // completion, and an unintended change is undone first. An earlier draft of rule 3 gave an
    // example route -- "the menu instead of the button" -- and one model then skipped a
    // highlighted toolbar button for the Format menu on an unrelated task. Keep routes unnamed.
    #[test]
    fn rules_2_and_3_put_the_screen_first_and_name_no_route() {
        let p = super::SYSTEM_PROMPT;
        assert!(p.contains("is not there, the screen outranks the confirmation"));
        assert!(p.contains("make your FIRST step undo it"));
        assert!(p.contains("selecting text, copying"));
        assert!(p.contains("NEVER issue the identical instruction twice in a row"));
        assert!(!p.contains("TRUST CONFIRMATIONS; NEVER REPEAT"));
        assert!(!p.contains("instead of the button") && !p.contains("instead of the shortcut"));
    }

    #[test]
    fn h1_system_prompt_gets_rule_19_before_the_tail() {
        // The insertion point has to exist exactly once, at the very end.
        assert_eq!(super::SYSTEM_PROMPT.matches(super::PROMPT_TAIL).count(), 1);
        assert!(super::SYSTEM_PROMPT.ends_with(super::PROMPT_TAIL));
        let on = super::system_prompt(true);
        assert!(on.contains("omit both together.\n19. EXPECTED RESULT:"), "rule 19 follows rule 18");
        assert!(on.ends_with(super::PROMPT_TAIL));
        assert_eq!(on.len(), super::SYSTEM_PROMPT.len() + 1 + super::EXPECTED_RESULT_RULE.len());
    }

    #[test]
    fn h1_prompted_json_lists_get_the_field_line() {
        let fmt = "Step fields:\n- clipboard: text\n- target_element_id: an id (optional)\n\nTop-level fields:";
        let on = super::with_expected_result_field(fmt, true);
        assert!(on.contains("- target_element_id: an id (optional)\n- expected_result: one short sentence"));
        assert!(on.ends_with("(required)\n\nTop-level fields:"));
        // Every prompted-JSON builder, as sent. The text-only DeepSeek builder too: a model that
        // cannot see the screen can still say what the next one should show.
        for content in [
            crate::ai::deepseek::build_openai_messages("t", None, None, &[], true)[0]["content"].clone(),
            crate::ai::deepseek::build_messages("t", None, None, &[], true)[0]["content"].clone(),
            crate::ai::ollama::build_messages("t", None, None, &[], true)[0]["content"].clone(),
        ] {
            let c = content.as_str().unwrap();
            assert!(c.contains("\n19. EXPECTED RESULT:"));
            assert!(c.contains("\n- expected_result: one short sentence"));
        }
        let off = crate::ai::deepseek::build_openai_messages("t", None, None, &[], false);
        assert!(!off[0]["content"].as_str().unwrap().contains("expected_result"));
    }

    #[test]
    fn h1_schema_field_reaches_every_provider_shape() {
        use serde_json::json;
        // OpenAI function shape (managed relay and the BYOK Responses path share it).
        let mut tool = crate::ai::managed::navigate_step_tool();
        assert!(super::add_expected_result(&mut tool));
        let items = &tool["function"]["parameters"]["properties"]["steps"]["items"];
        assert_eq!(items["properties"]["expected_result"]["type"], "string");
        assert!(items["required"].as_array().unwrap().contains(&json!("expected_result")));
        // Anthropic's input_schema, and Ollama's bare format schema.
        for mut schema in [
            json!({"input_schema": {"properties": {"steps": {"items": {"properties": {}, "required": ["instruction"]}}}}}),
            json!({"type": "object", "properties": {"steps": {"items": {"properties": {}}}}}),
        ] {
            assert!(super::add_expected_result(&mut schema));
            assert!(schema.to_string().contains("\"expected_result\""));
        }
        // Not a navigate_step schema: says so instead of inventing a home for the field.
        assert!(!super::add_expected_result(&mut json!({"properties": {"goal": {}}})));
    }
}
