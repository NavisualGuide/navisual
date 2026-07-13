/// Extract the decoded text of the **first** `"instruction"` value from a partial
/// (still-streaming) JSON string, up to (but not including) its closing quote.
///
/// Why the FIRST, not the last (audit 2026-07-12 C1): `execute_step` renders and speaks
/// `steps[0]`, so the first instruction is the one the user actually acts on — and a
/// multi-step response (Rule 1 allows 1–4 steps) contains several `"instruction":`
/// fields. Using `rfind` (the last one) meant that as step 2+ streamed in, the extracted
/// text jumped to a *different, later* string while the callers computed their emit-delta
/// against the previous length — splicing a mid-word tail of step 2 onto step 1's caption
/// ("Click the File menu" + "firm the dialog"), and byte-slicing across two different
/// strings, which **panics** when the cut lands mid-character (routine for the CJK replies
/// Rule 19 produces). Anchoring on the first instruction makes the returned prefix grow
/// **monotonically** (bytes, once decoded, never change), so every caller's recorded
/// length stays a valid char boundary and the delta is always a clean suffix — fixing both
/// the garble and the panic at the source.
///
/// Returns the decoded prefix (escape sequences resolved) — an empty string if no
/// `"instruction"` field has appeared yet, or its value hasn't started.
pub fn extract_visible_instruction(partial_json: &str) -> String {
    let prefix = "\"instruction\":";
    // FIRST occurrence — see the doc comment. Must be `find`, never `rfind`.
    if let Some(idx) = partial_json.find(prefix) {
        let remainder = &partial_json[idx + prefix.len()..];
        let trimmed = remainder.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('"') {
            let mut result = String::new();
            let mut chars = stripped.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    if let Some(next_ch) = chars.next() {
                        if next_ch == '"' {
                            result.push('"');
                        } else if next_ch == 'n' {
                            result.push('\n');
                        } else if next_ch == 't' {
                            result.push('\t');
                        } else {
                            result.push(next_ch);
                        }
                    }
                } else if ch == '"' {
                    break;
                } else {
                    result.push(ch);
                }
            }
            return result;
        }
    }
    String::new()
}

/// The clean streaming delta for a caller holding `already_emitted` bytes: the new text to
/// emit, and the new emitted-length to store. Char-boundary-safe by construction (see
/// [`extract_visible_instruction`] — the prefix grows monotonically, so `already_emitted`
/// is always a valid boundary), with a `get`-based guard so even an unexpected mid-char
/// index yields `""` instead of a panic. Callers replace the
/// `if visible.len() > emitted { on_chunk(&visible[emitted..]); emitted = visible.len() }`
/// idiom with this.
pub fn instruction_delta(partial_json: &str, already_emitted: usize) -> (String, usize) {
    let visible = extract_visible_instruction(partial_json);
    if visible.len() > already_emitted {
        let new_text = visible.get(already_emitted..).unwrap_or("").to_string();
        (new_text, visible.len())
    } else {
        (String::new(), already_emitted)
    }
}

/// How many steps have STARTED streaming in a partial `navigate_step` JSON: the count
/// of `"instruction"` keys seen so far. Monotonic (the buffer only appends), so the
/// panel can show "Step 1 of ~N" live as later steps arrive instead of discarding that
/// signal until the response completes (design suggestion #2, 2026-07-13).
///
/// Exact against values-that-mention-instructions: inside a JSON string value the
/// quotes would be escaped (`\"instruction\"`), so the literal `"instruction"`-then-`:`
/// pattern can only match a real key. The count is still an estimate of the FINAL step
/// count (hence "~N" in the UI) — more steps may follow.
pub fn count_streamed_steps(partial_json: &str) -> usize {
    let key = "\"instruction\"";
    let mut count = 0;
    let mut rest = partial_json;
    while let Some(idx) = rest.find(key) {
        let after = &rest[idx + key.len()..];
        if after.trim_start().starts_with(':') {
            count += 1;
        }
        rest = after;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::{count_streamed_steps, extract_visible_instruction, instruction_delta};

    #[test]
    fn extracts_first_instruction_not_last() {
        // A multi-step response mid-stream: step 1 complete, step 2 partial. The
        // extractor must stay on step 1 (what execute_step renders), never jump to step 2.
        let partial = r#"{"steps":[{"instruction":"Click the File menu","checkpoint":true},{"instruction":"Confi"#;
        assert_eq!(extract_visible_instruction(partial), "Click the File menu");
    }

    #[test]
    fn grows_monotonically_as_first_instruction_streams() {
        let a = r#"{"steps":[{"instruction":"Click the"#;
        let b = r#"{"steps":[{"instruction":"Click the File menu"#;
        assert_eq!(extract_visible_instruction(a), "Click the");
        assert_eq!(extract_visible_instruction(b), "Click the File menu");
        // b's decoded prefix starts with a's — the invariant the delta logic relies on.
        assert!(extract_visible_instruction(b).starts_with(&extract_visible_instruction(a)));
    }

    #[test]
    fn delta_is_a_clean_suffix() {
        let a = r#"{"steps":[{"instruction":"Click the"#;
        let (t1, n1) = instruction_delta(a, 0);
        assert_eq!(t1, "Click the");
        assert_eq!(n1, 9);
        let b = r#"{"steps":[{"instruction":"Click the File menu"#;
        let (t2, n2) = instruction_delta(b, n1);
        assert_eq!(t2, " File menu");
        assert_eq!(n2, 19);
    }

    #[test]
    fn delta_never_panics_on_cjk() {
        // A Chinese first instruction streaming in char by char — byte lengths land on
        // multi-byte boundaries. With the old rfind + raw-slice this could panic; here
        // every delta must be valid UTF-8 and reassemble to the full string.
        let full = "点击文件菜单"; // 6 chars, 18 bytes
        let mut emitted = 0usize;
        let mut assembled = String::new();
        for i in 1..=full.chars().count() {
            let sofar: String = full.chars().take(i).collect();
            let partial = format!(r#"{{"steps":[{{"instruction":"{sofar}"#);
            let (delta, n) = instruction_delta(&partial, emitted);
            assembled.push_str(&delta);
            emitted = n;
        }
        assert_eq!(assembled, full);
    }

    #[test]
    fn empty_before_instruction_appears() {
        assert_eq!(extract_visible_instruction(r#"{"steps":[{"#), "");
        assert_eq!(instruction_delta(r#"{"steps":[{"#, 0), (String::new(), 0));
    }

    #[test]
    fn counts_steps_as_they_stream() {
        assert_eq!(count_streamed_steps(r#"{"steps":[{"#), 0);
        assert_eq!(count_streamed_steps(r#"{"steps":[{"instruction":"Click"#), 1);
        assert_eq!(
            count_streamed_steps(
                r#"{"steps":[{"instruction":"Click File","checkpoint":true},{"instruction":"Con"#
            ),
            2
        );
        // Key mentioned INSIDE a value is escaped in raw JSON — must not count.
        assert_eq!(
            count_streamed_steps(r#"{"steps":[{"instruction":"type \"instruction\": here"#),
            1
        );
    }
}
