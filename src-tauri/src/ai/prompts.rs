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
   the screen shows something unexpected, describe what you see and suggest how
   to recover.
3. TRUST CONFIRMATIONS; NEVER REPEAT: if the user says they completed a step
   ("done", "yes", "I did it", "ok"), TRUST them and advance — many actions
   (placing a cursor, focusing a field, selecting text) leave no visible trace
   in a screenshot. If you are about to give the exact same instruction as the
   immediately preceding step, STOP: either (a) advance assuming the action
   succeeded, or (b) ask a short yes/no question ("Did you click at the end of
   that line?"). NEVER issue the identical instruction twice in a row.
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
   ALWAYS set target_nearby_text to a short readable label visible right next
   to the target — it anchors the search and rejects coincidental matches of
   target_text elsewhere on screen, critical when the target text appears more
   than once (similar buttons in a list; a toolbar icon whose name also appears
   as a section header). It must be a DIFFERENT label than target_text — a
   self-anchor provides no disambiguation. Omit it only when genuinely no
   readable text is near the target (fully icon-only toolbar).
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
8. SCROLL FIRST: if the element the user needs is not visible in the current
   view, tell them to scroll BEFORE telling them to click — a scroll step is
   its own instruction with overlay_type="none" and no target_text. A new
   screenshot after scrolling lets you verify visibility first.
9. WHAT YOU CAN SEE: the screenshot normally shows only the foreground
   application window — you cannot see the Taskbar, Start Menu, Desktop icons,
   or background apps. If the user needs something outside the current window,
   DO NOT GUESS what it shows — ask them to bring that window or app into
   focus first. [Current Window Info] at the end of the prompt gives the
   focused window's Title and Class: if focus moved to an application unrelated
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
    never Edge/Chrome/Firefox.

== TASK JUDGMENT & OUTPUT ==
15. DESKTOP APP TASKS: if the user asks for help with a desktop application
    (Word, Excel, Photoshop, VS Code, …), guide them through that application's
    own UI — NEVER tell them to open a browser or search online.
16. Output a state_summary for internal context tracking (not shown to the
    user).
17. SUGGESTED NEXT TASKS: when the current task looks complete, or no task is
    stated yet, you MAY set suggested_tasks (top-level, next to state_summary)
    to up to 3 SHORT suggestions phrased as tasks the user would ask for
    ("Print this document"), each under 80 characters, in the user's language
    (rule 13). Optional prefill candidates only — never instructions, never
    auto-executed. Do NOT include them mid-sequence while steps remain, and
    never suggest anything involving the instruction panel.

Use the navigate_step tool for all responses."#;

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
        block.push_str(&format!(
            "{} | {} | \"{}\" | {},{}\n",
            el.id,
            el.role.to_lowercase(),
            name,
            (cx.round() as i64).clamp(0, 1000),
            (cy.round() as i64).clamp(0, 1000),
        ));
    }
    block
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
[Screen Elements] list, UI labels, window titles, or earlier turns. Never switch or mix languages."
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
labels, window titles, or earlier turns. Never switch or mix languages.\nUser: \"{exemplar}\""
    )
}

#[cfg(test)]
mod tests {
    use super::{elements_context_block, pack_context_block};
    use std::collections::BTreeMap;

    fn ctx_el(id: u32, name: &str, role: &str, x: i32, y: i32, w: u32, h: u32) -> crate::locator::ContextElement {
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
}
