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

/// Dominant non-Latin script of `text` as a language code ("zh"/"ja"/"ko"/"ru"/"ar"), or `None`
/// when the text is Latin-scripted or its non-Latin content is only incidental.
///
/// Deliberately stricter than `tts::strong_script_lang` (which fires on a SINGLE non-Latin char,
/// because a voice must pronounce whatever is on screen). Here the non-Latin script must actually
/// DOMINATE: the user pastes long, sometimes mixed-language instructions, and one stray CJK
/// app-name inside an English request must NOT flip the whole reply to Chinese.
pub(crate) fn dominant_script_lang(text: &str) -> Option<&'static str> {
    let (mut latin, mut kana, mut hangul, mut han, mut cyr, mut arab) = (0u32, 0, 0, 0, 0, 0);
    for c in text.chars() {
        let u = c as u32;
        if c.is_ascii_alphabetic() || (0x00c0..=0x024f).contains(&u) {
            latin += 1;
        } else if (0x3040..=0x30ff).contains(&u) {
            kana += 1;
        } else if (0xac00..=0xd7af).contains(&u) || (0x1100..=0x11ff).contains(&u) {
            hangul += 1;
        } else if (0x4e00..=0x9fff).contains(&u) || (0x3400..=0x4dbf).contains(&u) {
            han += 1;
        } else if (0x0400..=0x04ff).contains(&u) {
            cyr += 1;
        } else if (0x0600..=0x06ff).contains(&u) {
            arab += 1;
        }
    }
    // Kana is unique to Japanese, so it disambiguates ja from zh even when kanji dominate.
    let dominates = |n: u32| n >= 2 && n * 2 >= latin;
    if dominates(kana) {
        return Some("ja");
    }
    let (code, count) = [("ko", hangul), ("zh", han), ("ru", cyr), ("ar", arab)]
        .into_iter()
        .max_by_key(|&(_, n)| n)?;
    dominates(count).then_some(code)
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
/// Resolution order: (1) the user's request script when it DOMINATES → name the language outright
/// (the strongest form a weak model follows); (2) else an explicitly-chosen `VOICE_LANGUAGE`
/// (rescues pinyin mis-transcription of Chinese speech, and Latin languages on a non-Latin screen);
/// (3) else the request itself as a same-language exemplar. Empty only when nothing anchors it.
pub fn reply_language_directive(request_text: &str, voice_language: &str) -> String {
    let req = request_text.trim();
    let named = dominant_script_lang(req).and_then(lang_name).or_else(|| {
        let v = voice_language.trim();
        if v.is_empty() || v.eq_ignore_ascii_case("auto") {
            None
        } else {
            lang_name(v)
        }
    });
    if let Some(name) = named {
        return format!(
            "\n\nIMPORTANT — LANGUAGE LOCK: the user's language is {name}. Write EVERY instruction \
and the state_summary ENTIRELY in {name}, no matter what language appears in the screenshot, the \
[Screen Elements] list, UI labels, window titles, or earlier turns. Never switch or mix languages."
        );
    }
    if req.is_empty() {
        return String::new();
    }
    // Latin / undetermined: same-language exemplar. Cap the echo so a long pasted instruction
    // doesn't bloat the tail — a prefix is enough of a language sample and recency still holds.
    let exemplar: String = req.chars().take(160).collect();
    format!(
        "\n\nIMPORTANT — LANGUAGE LOCK: write EVERY instruction and the state_summary in the SAME \
LANGUAGE as the user's request, no matter what language appears in the screenshot, UI labels, \
window titles, or earlier turns. Never switch or mix languages.\nUser's request: \"{exemplar}\""
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
    fn directive_exemplar_for_latin_request() {
        use super::reply_language_directive;
        // Latin request, auto voice → exemplar form echoing the request.
        let a = reply_language_directive("make a pivottable using these data", "auto");
        assert!(a.contains("make a pivottable using these data"));
        assert!(a.contains("SAME LANGUAGE"));
        assert!(a.contains("LANGUAGE LOCK"));
        // Empty / whitespace + auto → nothing to anchor on.
        assert!(reply_language_directive("", "auto").is_empty());
        assert!(reply_language_directive("   ", "auto").is_empty());
    }

    #[test]
    fn directive_names_language_from_dominant_script() {
        use super::reply_language_directive;
        // A Chinese request names Chinese outright, regardless of voice setting.
        let a = reply_language_directive("帮我做一个数据透视表", "auto");
        assert!(a.contains("Chinese"), "{a}");
        assert!(!a.contains("data")); // no exemplar branch
    }

    #[test]
    fn directive_uses_explicit_voice_language_when_script_is_latin() {
        use super::reply_language_directive;
        // Latin request (e.g. pinyin mis-transcription) + an explicit zh-CN setting → name Chinese.
        let a = reply_language_directive("bang wo zuo", "zh-CN");
        assert!(a.contains("Chinese"), "{a}");
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
