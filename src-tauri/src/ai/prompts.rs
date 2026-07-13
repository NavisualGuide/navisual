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
    determines the language: on-screen content, file names, window titles, and
    UI labels must NOT change it. English request → English response even on a
    fully non-English screen (and vice versa). Keep the user's language across
    follow-ups and corrections; never switch or mix languages mid-task.
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

/// A per-turn language re-anchor for continuation turns (Next / resume / reply).
///
/// Why (recurring live issue, 2026-07-13): on a multi-turn session a Chinese-native model
/// (Qwen) drifts into Chinese even for an all-English task on an all-English screen —
/// the LANGUAGE rule lives deep in a long static system prompt (low salience), the immediate
/// turn message is a machine-generated `[User completed: …]` (not the user's own words), and
/// once the model emits ONE Chinese reply the Chinese state_summary + Chinese completed-step
/// echo feed back and lock it in; the original request also scrolls out of the 10-turn history
/// window. This restates the original request verbatim at the very END of the prompt (recency
/// = high salience) as the language exemplar, re-anchoring both the goal and its language every
/// turn. Empty task → empty string (no anchor to add).
pub fn language_anchor(original_task: &str) -> String {
    let t = original_task.trim();
    if t.is_empty() {
        return String::new();
    }
    format!(
        "\n\nIMPORTANT — the user's ORIGINAL request was: \"{t}\". Write your instruction and \
state_summary in the SAME LANGUAGE as that original request, regardless of the language of \
any on-screen text, window titles, or earlier turns. Do not switch languages."
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
    fn language_anchor_restates_task_or_is_empty() {
        use super::language_anchor;
        let a = language_anchor("make a pivottable using these data");
        assert!(a.contains("make a pivottable using these data"));
        assert!(a.contains("SAME LANGUAGE"));
        // Empty / whitespace task → no anchor (first turn, or missing session).
        assert!(language_anchor("").is_empty());
        assert!(language_anchor("   ").is_empty());
    }
}
