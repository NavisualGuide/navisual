//! Element locator — Phase C.2.
//!
//! Finds UI elements on the user's screen. Primary strategy is Windows UI
//! Automation (A11y tree, < 5ms for most apps). OCR fallback lands in C.3.
//!
//! Returns bounding boxes in **physical pixels, virtual-desktop coordinates**
//! — same coordinate system as `capture::Rect` so the overlay renderer can
//! consume either without translation.

#[cfg(windows)]
pub mod a11y;

pub mod adapters;

#[cfg(windows)]
pub mod ocr;

#[cfg(windows)]
pub mod orchestrator;

#[cfg(windows)]
pub mod hit_test;

pub mod keepwarm;

pub mod template;

pub mod trace;

/// If `target` ends with an ellipsis ("…" or "..."), return the text before it
/// (trimmed) plus whether callers should treat it as a *prefix*. Vision models
/// often copy a visually-truncated UI label verbatim (e.g. "Sum of Output USD
/// per…"), but the underlying accessible name / full on-screen text is not
/// truncated — so prefix-matching the core lets the full name match. The prefix
/// flag is only set when the core is ≥5 chars (so a short clip like "Re…" can't
/// match half the screen); the ellipsis is stripped regardless.
/// Whether `c` is a CJK character (Han incl. Ext A, kana, hangul syllables).
#[cfg(windows)]
pub(crate) fn is_cjk_char(c: char) -> bool {
    let u = c as u32;
    (0x4e00..=0x9fff).contains(&u)      // CJK Unified Ideographs
        || (0x3400..=0x4dbf).contains(&u) // Ext A
        || (0x3040..=0x30ff).contains(&u) // Hiragana + Katakana
        || (0xac00..=0xd7af).contains(&u) // Hangul syllables
}

/// Whether the string contains any CJK character. CJK text has no space-separated
/// words, so word-boundary (`\b`) matching and whitespace tokenization silently
/// never fire on it — matchers gate their CJK fallback paths on this
/// (orchestrator's Selection cross-check, OCR's substring tier). Shared here so
/// every matcher agrees on what "CJK" means.
#[cfg(windows)]
pub(crate) fn contains_cjk(s: &str) -> bool {
    s.chars().any(is_cjk_char)
}

#[cfg(windows)]
pub(crate) fn strip_trailing_ellipsis(target: &str) -> (String, bool) {
    let trimmed = target.trim_end();
    let core = trimmed
        .strip_suffix('…')
        .or_else(|| trimmed.strip_suffix("..."))
        .map(|c| c.trim_end());
    match core {
        Some(c) if c.chars().count() >= 5 => (c.to_string(), true),
        Some(c) => (c.to_string(), false),
        None => (target.to_string(), false),
    }
}

/// S.1 (v0.7 Workstream S) — one entry of the Structured-Context element snapshot: an
/// interactive, named, on-screen UIA element enumerated at AI-capture time. The list is
/// sent to the AI (id | role | name | center) so it can *select* instead of grounding;
/// the id is a per-request index into this snapshot, never a UIA RuntimeId (Decision 2).
/// A returned id is verified against the live tree before use (`a11y::verify_context_element`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextElement {
    /// Per-request index (1-based, assigned in tree order after filtering).
    pub id: u32,
    /// Accessible name after paren-suffix + accelerator strip — what the AI sees and
    /// what the S.3 text cross-check / live verification compare against.
    pub name: String,
    /// UIA control type ("Button", "TabItem", …).
    pub role: String,
    /// Bounding rect in virtual-desktop physical pixels at capture time. The pointer
    /// never uses this directly — verification re-reads the live rect.
    pub rect: crate::capture::Rect,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LocateResult {
    /// Bounding box in physical pixels, virtual-desktop coords.
    pub bbox: crate::capture::Rect,
    /// Accessible name of the located element (for debugging/logging).
    pub name: String,
    /// UIA control type (e.g. "Button", "Hyperlink").
    pub role: String,
    /// 1.0 for A11y hits, < 1.0 for OCR (later).
    pub confidence: f32,
}

#[cfg(all(test, windows))]
mod tests {
    use super::strip_trailing_ellipsis;

    #[test]
    fn long_core_becomes_prefix() {
        assert_eq!(
            strip_trailing_ellipsis("Sum of Output USD per…"),
            ("Sum of Output USD per".to_string(), true)
        );
        assert_eq!(
            strip_trailing_ellipsis("Looooong..."),
            ("Looooong".to_string(), true)
        );
        // Trailing whitespace before the ellipsis is tolerated.
        assert_eq!(
            strip_trailing_ellipsis("Foo bartext… "),
            ("Foo bartext".to_string(), true)
        );
    }

    #[test]
    fn short_core_strips_but_never_prefixes() {
        // A short clip like "Re…" must not become a prefix that matches half
        // the screen — ellipsis is stripped, prefix flag stays false.
        assert_eq!(strip_trailing_ellipsis("Re…"), ("Re".to_string(), false));
        assert_eq!(
            strip_trailing_ellipsis("Save..."),
            ("Save".to_string(), false)
        );
    }

    #[test]
    fn plain_labels_pass_through() {
        assert_eq!(
            strip_trailing_ellipsis("Playback"),
            ("Playback".to_string(), false)
        );
    }
}
