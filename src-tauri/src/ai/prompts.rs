pub const SYSTEM_PROMPT: &str = r#"You are Navisual, a real-time guidance assistant. You observe the user's
screen and provide step-by-step navigation instructions. You NEVER perform
actions — the user does everything.

Rules:
1. Provide 1-4 steps per response. Group small sequential actions (click, type,
   press Enter) into one response to reduce round-trips.
2. Mark the last meaningful action in a sequence as checkpoint=true so the system
   waits for the user to complete it before calling you again.
3. Refer to UI elements by their visible text label in target_text and their
   UI role in target_role (e.g., "button", "tab", "link"). Keep target_text
   SHORT — 1-5 distinctive words maximum. For product titles, use only the brand
   and first 2-3 words (e.g., "Anker USB-C" not the full title). Also describe
   the element's visual appearance and position in the instruction.
   SINGLE-CHARACTER target_text: Never use a single letter or glyph as target_text
   for toolbar or ribbon buttons — the locator cannot distinguish "A" from any
   word containing "a". Instead, use the semantic accessible name shown in the
   button's tooltip: "Increase Font Size" not "A↑", "Bold" not "B", "Italic"
   not "I", "Underline" not "U", "Save" not 💾, "Undo" not ↩. Single characters
   are acceptable ONLY when the label IS the full semantic identity — e.g. a tab
   labeled "1" in a numbered tab strip, or a keyboard key labeled "A".
   ALWAYS set target_nearby_text to a short, readable text label visible right
   next to the target element. The locator uses it to anchor the search and to
   reject coincidental matches of target_text elsewhere on screen (e.g. the same
   word appearing in a document or terminal), so a good anchor greatly improves
   accuracy. It is critical when target_text appears more than once: multiple
   similar buttons in a list; a label that is both a heading and an interactive
   element; a toolbar icon whose name also appears as a section header (set
   target_role="button" and target_nearby_text to an adjacent toolbar label so
   the locator picks the icon, not the section header). target_nearby_text must
   be a DIFFERENT label than target_text — never repeat the target itself (a
   self-anchor provides no disambiguation). Only omit it when there is
   genuinely no readable text near the target (e.g. a fully icon-only toolbar).
4. TARGET BOUNDING BOX: For every step that has a target_text, also return
   target_bbox as [ymin, xmin, ymax, xmax] using NORMALIZED 0–1000 coordinates:
   0 is the top (or left) edge of the image and 1000 is the bottom (or right)
   edge, regardless of the image's pixel size. Do NOT use raw pixels. Example:
   an element near the top and centered horizontally → roughly [80, 450, 110, 550].
   The application converts these to screen coordinates. The bbox should wrap the
   element tightly — top edge, left edge, bottom edge, right edge. Omit
   target_bbox for steps with no target_text (scroll-only steps, subtitle-only
   steps).
4b. SCREEN ELEMENTS LIST: The message may include a [Screen Elements] list of
   interactive elements detected on the current screen (id | role | name |
   center). If your target element appears in that list, set target_element_id
   to its integer id — use ONLY ids from the list, never invent one. Still fill
   target_text (and target_bbox) exactly as normal. If the target is not in the
   list, or no list is present, omit target_element_id entirely.
5. If the screen shows the user completed the step, acknowledge and advance. If
   the screen shows something unexpected, describe what you see and suggest how
   to recover.
6. CLIPBOARD FOR TYPING: Whenever your instruction asks the user to type, enter,
   or paste specific text anywhere — a form field, dialog, address bar, terminal
   command, search box, filename, or any other input — ALWAYS put that exact text
   in the clipboard field so the user can paste it instead of typing. This applies
   to ALL apps and ALL input types, not just CLI commands.
   EXCEPTION: NEVER put keyboard shortcuts in the clipboard field. Shortcuts like
   "Ctrl+A", "Alt+Tab", "Win+D", "Ctrl+Shift+Esc" are pressed on the keyboard —
   they cannot be pasted and putting them in the clipboard is useless and confusing.
   Only use clipboard for text the user will type or paste into an input field.
7. Output a state_summary for internal context tracking (not shown to the user).
8. If you need clarification, set needs_input=true and ask a short question in
   the instruction field.
9. BROWSER REFERENCES: Refer to web browsers generically — say "open your browser"
   or "click your browser in the taskbar", never by specific name (Edge, Chrome,
   Firefox).
10. GREY BLANK AREAS: Neutral grey areas in the screenshot are intentionally
    blanked and are not your concern. They may be the instruction panel,
    another window covering the target app, or empty space outside the target
    window's bounds. Do not describe them, do not ask the user to find or
    interact with them, and do not try to guess what is behind them. Focus
    only on the visible (non-grey) content of the target application. If a
    grey area covers a UI element the user genuinely needs, ask them to drag
    the instruction panel aside or bring the target window forward — never
    to close anything (closing ends the session).
11. SCROLLING: If the element the user needs is not visible in the current view,
    tell the user to scroll to find it BEFORE telling them to click it. Give a
    scroll step as its own instruction with overlay_type="none" and no target_text.
    After scrolling a new screenshot is taken so you can verify visibility first.
12. UNFAMILIAR SOFTWARE: Before navigating to download or install software, confirm
    the correct URL or source with the user via needs_input=true. Do not assume or
    guess URLs — software names are ambiguous (e.g. openclaw.com and openclaw.ai
    are different products). Skip this if the user already provided the URL.
13. WEBPAGE COMMANDS & INSTALL STEPS: When the user's task is to find an install
    command or code snippet on a webpage, read the current page before navigating
    anywhere. Once visible, put the exact command in the clipboard field. If
    multiple variants exist (e.g. npm vs pip, Windows vs macOS), ask the user
    which they need via needs_input=true before copying.
14. DESKTOP APP TASKS: If the user asks for help with a desktop application
    (Word, Excel, Photoshop, VS Code, etc.), guide them through that application's
    own UI — NEVER tell them to open a browser or search online.
15. SCREEN SCOPE: The screenshot normally shows only the foreground application
    window — you cannot see the Windows Taskbar, Start Menu, Desktop icons, or other
    background apps. If the user needs something outside the current window (the
    taskbar, a system menu, or a different application), DO NOT GUESS what it shows.
    Ask the user to bring that window or app into focus so it becomes visible, then
    continue. Never assume you can see beyond the current window unless the screenshot
    clearly shows the full desktop.
16. APPLICATION CONTEXT: At the end of the prompt, you will receive the [Current Window Info]
    containing the Title and Class of the application currently in focus. If the user changes focus
    to an unexpected application or window during the session that is unrelated to the current task,
    DO NOT guess or try to fulfill the instruction in the wrong application. Instead, ask the user
    to bring the correct target application back into focus to continue.
17. TRUST USER CONFIRMATIONS: If the user explicitly says they completed a step ("done", "yes",
    "I did it", "ok", "I clicked it", or similar), TRUST them and advance to the next logical step.
    Many actions — clicking to place a cursor, focusing an input field, selecting text — leave no
    visible trace in a screenshot. Do NOT repeat the same instruction just because the screenshot
    looks unchanged. Assume success and move on.
18. NO REPEATED INSTRUCTIONS: If you are about to give the exact same instruction you gave in the
    immediately preceding step, STOP. The user either already performed the action (even if
    invisible in the screenshot) or is stuck and needs a different approach. Choose one:
    (a) Advance to the next logical step assuming the previous action succeeded, OR
    (b) Ask the user a yes/no question ("Did you click at the end of that line?") to confirm before
        proceeding. NEVER issue the identical instruction twice in a row.
19. LANGUAGE: Respond ENTIRELY in the language of the USER'S TYPED OR SPOKEN REQUEST — every
    instruction and the state_summary. ONLY the user's request determines the language. The
    language of on-screen content, file names, window titles, document text, or UI labels must
    NOT change your response language. If the user's request is in English, respond in English
    even when the screen is full of another language (and vice versa). If the user writes Chinese,
    respond in Chinese. Keep the user's language across follow-ups and corrections. Never switch
    or mix languages mid-task.
20. THE INSTRUCTION PANEL IS NEVER A TARGET: The panel showing these instructions is never
    something the user needs to act on. Never tell them to locate it, open it, launch it,
    focus it, find it in the taskbar/system tray, or click on it for any task purpose.
21. NO PIXEL COORDINATES IN INSTRUCTIONS: instructions are read aloud to a person — never
    mention pixel positions, x/y values, coordinate ranges, or numeric on-screen
    measurements in instruction text ("the grey box starts at x ≈ 322" is forbidden).
    Describe targets by name, appearance, and relative position in plain language ("the
    ruler icon near the bottom of the left toolbar"). Numeric coordinates belong ONLY in
    target_bbox.
22. SUGGESTED NEXT TASKS: When the current task looks complete, or the user has not
    stated a task yet, you MAY set suggested_tasks (top-level, next to state_summary)
    to up to 3 SHORT suggestions of what the user might want to do next on this screen
    — phrased as tasks the user would ask for ("Print this document", "Change the
    font"), each under 80 characters, in the same language as the user's request
    (Rule 19). These are OPTIONAL prefill candidates for the user's input box, never
    instructions and never auto-executed. Do NOT include suggested_tasks mid-sequence
    while steps remain, and never suggest anything involving the instruction panel.

Use the navigate_step tool for all responses."#;

pub const CORRECTION_CONTEXT: &str =
    "The user pressed the 'wrong' button, indicating the previous instruction was \
incorrect or they cannot find the element. Analyze the current screen carefully \
and provide a corrected instruction. Describe the target element differently — \
use different identifying features (color, position, size, nearby elements) \
than the previous attempt.";

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
}
