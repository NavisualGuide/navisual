//! H7b (harness-plan.md): the learned per-app guide -- what earlier sessions on this computer found
//! out about an application, kept as one human-readable file per app and handed to the model
//! whenever that app's window is the target. This module is the file: its format, how it reads
//! and writes, and which notes go into a prompt. [`super::learn`] keeps it current.
//!
//! Organised the way skills are (founder, 2026-10-10): each note is a short title and a `when:`
//! line -- its description -- over a one- or two-line body. A short guide goes in whole; a long one
//! goes in as every title plus the bodies of the notes that match the task, so the description is
//! always there and the body comes when it is relevant. **Local code chooses, not the model**: a
//! tool the model has to call was used by one model of three in H2 and H3 (SDD §1.1).
//!
//! It layers on an authored nav-pack rather than replacing it (D6): the authored pack's
//! `[App Guide]` block is unchanged, and the guide is a block of its own that says it can be out
//! of date and that the screen wins.
//!
//! File: `<packs>/learned/<exe>/guide.md`. Everything about a note lives in the file, the
//! bookkeeping included (founder, 2026-10-10), so what the app knows is what a person can read:
//!
//! ```text
//! ---
//! app: Notepad
//! exe: notepad
//! ---
//! ## Word wrap
//! when: view, word, wrap
//! names: View > Word wrap
//! status: confirmed 2, last 2026-10-10, app 11.2508.38.0
//! View > Word wrap.
//! ```
//!
//! - `exe` (the process's file stem) is how a learned guide finds its app; a window title carries
//!   document names, so it is never written from one. `window_title_pattern` still works, for a
//!   guide written by hand.
//! - `names` is the path of controls the note describes, the link between a note and the steps
//!   built from it. A hand-written note without one gets it from a `A > B` path in its body.
//! - `status` is `confirmed N, last DATE, app VERSION`, or `withheld DATE, sessions K, app
//!   VERSION, reason TEXT`. A withheld note stays in the file and never reaches a prompt -- not
//!   even its title. No status: never confirmed, still sent.
//!
//! Content-free by rule: a note is about the application -- where a command lives, which control
//! does what -- never about the user's documents, names or numbers.

use std::path::{Path, PathBuf};

/// Above this many characters of note text the guide goes in as titles plus matching bodies.
const WHOLE_GUIDE_CHARS: usize = 1600;
/// Matching bodies included at most when the guide is long.
const MAX_MATCHED_NOTES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteStatus {
    /// Written by hand and never seen to work or fail.
    Unconfirmed,
    Confirmed { count: u32, last: String, app: String },
    /// One contradiction: kept, not sent. A second deletes it; a confirmation restores it.
    Withheld { since: String, sessions: u32, app: String, reason: String },
}

impl NoteStatus {
    fn render(&self) -> Option<String> {
        let app = |v: &str| if v.is_empty() { String::new() } else { format!(", app {v}") };
        match self {
            NoteStatus::Unconfirmed => None,
            NoteStatus::Confirmed { count, last, app: v } => Some(format!("confirmed {count}, last {last}{}", app(v))),
            NoteStatus::Withheld { since, sessions, app: v, reason } => {
                Some(format!("withheld {since}, sessions {sessions}{}, reason {reason}", app(v)))
            }
        }
    }

    fn parse(text: &str) -> NoteStatus {
        let text = text.trim();
        // `reason` is last and free text, so it is split off before the commas are.
        let (head, reason) = match text.split_once(", reason ") {
            Some((h, r)) => (h, r.trim().to_string()),
            None => (text, String::new()),
        };
        let mut parts = head.split(',').map(str::trim);
        let first = parts.next().unwrap_or("");
        let mut kv = std::collections::HashMap::new();
        for p in parts {
            if let Some((k, v)) = p.split_once(' ') {
                kv.insert(k.trim(), v.trim().to_string());
            }
        }
        let app = kv.get("app").cloned().unwrap_or_default();
        match first.split_once(' ') {
            Some(("confirmed", n)) => NoteStatus::Confirmed {
                count: n.trim().parse().unwrap_or(1),
                last: kv.get("last").cloned().unwrap_or_default(),
                app,
            },
            Some(("withheld", since)) => NoteStatus::Withheld {
                since: since.trim().to_string(),
                sessions: kv.get("sessions").and_then(|s| s.parse().ok()).unwrap_or(0),
                app,
                reason,
            },
            _ => NoteStatus::Unconfirmed,
        }
    }

    pub fn is_withheld(&self) -> bool {
        matches!(self, NoteStatus::Withheld { .. })
    }
}

#[derive(Debug, Clone)]
pub struct GuideNote {
    pub title: String,
    pub when: Vec<String>,
    /// The controls the note describes, in the order they are clicked.
    pub names: Vec<String>,
    pub status: NoteStatus,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct Guide {
    pub app: String,
    /// The process's file stem, lowercase.
    pub exe: Option<String>,
    title_re: Option<regex::Regex>,
    pub notes: Vec<GuideNote>,
    pub path: PathBuf,
    /// The file's modified time and size when it was last read or written -- how a hand edit
    /// made while the app runs is noticed before the app writes over it.
    pub stamp: Option<(std::time::SystemTime, u64)>,
    /// The file was edited into something that does not parse: not sent, not written, until a
    /// later edit fixes it.
    pub unreadable: bool,
}

/// A file's modified time and size, or `None` when it is not there.
pub fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl Guide {
    /// An empty guide for an app seen for the first time.
    pub fn new(app: &str, exe: &str, path: PathBuf) -> Guide {
        Guide { app: app.to_string(), exe: Some(exe.to_lowercase()), title_re: None, notes: Vec::new(), path, stamp: None, unreadable: false }
    }

    /// Parse a guide file's text. `None` without a way to find its app (`exe` or a valid
    /// `window_title_pattern`). A guide with no notes left is valid: the file is what remains
    /// after its last note was deleted.
    pub fn parse(text: &str, path: PathBuf) -> Option<Guide> {
        let text = text.replace("\r\n", "\n");
        let rest = text.strip_prefix("---\n")?;
        let (front, body) = match rest.split_once("\n---\n") {
            Some(split) => split,
            None => (rest.strip_suffix("\n---")?, ""),
        };
        let mut app = String::new();
        let mut exe = None;
        let mut pattern = String::new();
        for line in front.lines() {
            if let Some((k, v)) = line.split_once(':') {
                match k.trim() {
                    "app" => app = v.trim().to_string(),
                    "exe" => exe = Some(v.trim().to_lowercase()).filter(|e| !e.is_empty()),
                    "window_title_pattern" => pattern = v.trim().to_string(),
                    _ => {}
                }
            }
        }
        let title_re = if pattern.is_empty() { None } else { Some(regex::Regex::new(&pattern).ok()?) };
        if exe.is_none() && title_re.is_none() {
            return None;
        }
        let mut notes: Vec<GuideNote> = Vec::new();
        for section in body.split("\n## ").map(|s| s.trim_start_matches("## ")) {
            let mut lines = section.lines();
            let Some(title) = lines.next().map(str::trim).filter(|t| !t.is_empty()) else { continue };
            let (mut when, mut names, mut status, mut body_lines) = (Vec::new(), Vec::new(), NoteStatus::Unconfirmed, Vec::new());
            for line in lines {
                let l = line.trim();
                if let Some(w) = l.strip_prefix("when:") {
                    when.extend(w.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()));
                } else if let Some(n) = l.strip_prefix("names:") {
                    names = split_path(n);
                } else if let Some(s) = l.strip_prefix("status:") {
                    status = NoteStatus::parse(s);
                } else if !l.is_empty() {
                    body_lines.push(l);
                }
            }
            let body = body_lines.join(" ");
            if names.is_empty() {
                names = derive_names(&body);
            }
            if body.is_empty() {
                continue;
            }
            notes.push(GuideNote { title: title.to_string(), when, names, status, body });
        }
        Some(Guide { app, exe, title_re, notes, path, stamp: None, unreadable: false })
    }

    /// The file's text. A note's body comes back on one line.
    pub fn to_text(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("app: {}\n", self.app));
        if let Some(e) = &self.exe {
            out.push_str(&format!("exe: {e}\n"));
        }
        if let Some(re) = &self.title_re {
            out.push_str(&format!("window_title_pattern: {}\n", re.as_str()));
        }
        out.push_str("---\n");
        for (i, n) in self.notes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n", n.title));
            if !n.when.is_empty() {
                out.push_str(&format!("when: {}\n", n.when.join(", ")));
            }
            if !n.names.is_empty() {
                out.push_str(&format!("names: {}\n", n.names.join(" > ")));
            }
            if let Some(s) = n.status.render() {
                out.push_str(&format!("status: {s}\n"));
            }
            out.push_str(&n.body);
            out.push('\n');
        }
        out
    }

    /// Write the file, through a temporary file so a crash never leaves half a guide.
    pub fn save(&mut self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("md.tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, &self.path)?;
        self.stamp = file_stamp(&self.path);
        Ok(())
    }

    /// Whether this guide describes the window: by its process when the guide names one, else by
    /// its title.
    pub fn matches(&self, window_title: &str, exe: Option<&str>) -> bool {
        match (&self.exe, exe) {
            (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
            (Some(_), None) => false,
            (None, _) => self.title_re.as_ref().is_some_and(|re| re.is_match(window_title)),
        }
    }

    /// The prompt block for a request whose task and goal are `focus` (the harness's view).
    #[cfg(test)]
    pub fn block(&self, focus: &str) -> String {
        self.block_with_sent(focus).0
    }

    /// The block, and the indexes of the notes whose bodies it carries -- the notes a step in the
    /// answer can have come from. Every note when the guide is short, otherwise every title and
    /// the bodies of the notes the focus matches. Withheld notes are left out entirely.
    pub fn block_with_sent(&self, focus: &str) -> (String, Vec<usize>) {
        let live: Vec<usize> = (0..self.notes.len()).filter(|&i| !self.notes[i].status.is_withheld()).collect();
        if live.is_empty() {
            return (String::new(), Vec::new());
        }
        let app = if self.app.is_empty() { "this application" } else { self.app.as_str() };
        let mut out = format!(
            "\n[Learned notes: {app}] From earlier sessions on this computer. They can be out of date: \
             when the screen disagrees, follow the screen.\n"
        );
        let total: usize = live.iter().map(|&i| self.notes[i].title.len() + self.notes[i].body.len()).sum();
        if total <= WHOLE_GUIDE_CHARS {
            for &i in &live {
                out.push_str(&format!("- {}: {}\n", self.notes[i].title, self.notes[i].body));
            }
            return (out, live);
        }
        let words = words_of(focus);
        let mut scored: Vec<(usize, usize)> =
            live.iter().map(|&i| (relevance(&self.notes[i], &words), i)).filter(|(s, _)| *s > 0).collect();
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        let shown: Vec<usize> = scored.iter().take(MAX_MATCHED_NOTES).map(|(_, i)| *i).collect();
        for &i in &shown {
            out.push_str(&format!("- {}: {}\n", self.notes[i].title, self.notes[i].body));
        }
        let others: Vec<&str> =
            live.iter().filter(|i| !shown.contains(i)).map(|&i| self.notes[i].title.as_str()).collect();
        if !others.is_empty() {
            out.push_str(&format!("Other notes kept for this app (not relevant here): {}\n", others.join("; ")));
        }
        (out, shown)
    }
}

/// `A > B > C` into its names.
fn split_path(s: &str) -> Vec<String> {
    s.split('>').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// The first `A > B` path in a hand-written body, up to the first comma or full stop: "View >
/// Statistics. The status bar shows no word count." gives `[View, Statistics]`. Empty when the
/// body names no path.
fn derive_names(body: &str) -> Vec<String> {
    let Some(first_gt) = body.find(" > ") else { return Vec::new() };
    let start = body[..first_gt].rfind(['.', ',', ':', ';']).map(|i| i + 1).unwrap_or(0);
    let tail = &body[start..];
    let end = tail.find([',', ';']).unwrap_or(tail.len());
    let mut seg = tail[..end].trim();
    // A full stop ends the path unless it belongs to an ellipsis ("Save as...").
    if let Some(i) = seg.find(". ") {
        seg = &seg[..i];
    }
    let names = split_path(seg.trim_end_matches('.'));
    if names.len() >= 2 { names } else { Vec::new() }
}

fn words_of(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect()
}

/// How many of the focus words a note's `when:` keywords and title contain.
fn relevance(note: &GuideNote, words: &[String]) -> usize {
    let mut keys: Vec<String> = note.when.iter().flat_map(|w| words_of(w)).collect();
    keys.extend(words_of(&note.title));
    words.iter().filter(|w| keys.contains(w)).count()
}

/// One guide file, stamped; `None` (logged) when it is missing or does not parse.
pub fn load_file(path: &Path) -> Option<Guide> {
    let stamp = file_stamp(path)?;
    let text = std::fs::read_to_string(path).ok()?;
    match Guide::parse(&text, path.to_path_buf()) {
        Some(mut g) => {
            g.stamp = Some(stamp);
            Some(g)
        }
        None => {
            log::warn!("skipping learned guide at {}: no exe or window_title_pattern", path.display());
            None
        }
    }
}

/// Every `<dir>/<app>/guide.md` that parses. A bad file is logged and skipped.
pub fn load_dir(dir: &Path) -> Vec<Guide> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("guide.md");
        if let Some(g) = load_file(&path) {
            out.push(g);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "---\napp: Contoso Notes\nwindow_title_pattern: (?i)contoso notes\n---\n\
## Export a note as PDF\nwhen: export, pdf, share a file\nIt is in the Share menu, not File.\n\
It stays greyed out until the note is saved.\n\n## Turn on autosave\nwhen: autosave\nTools > Preferences > Saving.\n";

    #[test]
    fn a_guide_parses_into_notes_and_matches_its_app_only() {
        let g = Guide::parse(TEXT, PathBuf::from("guide.md")).unwrap();
        assert_eq!(g.app, "Contoso Notes");
        assert_eq!(g.notes.len(), 2);
        assert_eq!(g.notes[0].when, vec!["export", "pdf", "share a file"]);
        assert_eq!(g.notes[0].body, "It is in the Share menu, not File. It stays greyed out until the note is saved.");
        assert!(g.notes[0].names.is_empty(), "no path in that body");
        assert_eq!(g.notes[1].names, vec!["Tools", "Preferences", "Saving"]);
        assert!(g.matches("Contoso Notes - Q3 planning", None));
        assert!(!g.matches("Untitled - Paint", None), "another app's window gets nothing");
        assert!(Guide::parse("## no header\nbody", PathBuf::new()).is_none());
    }

    #[test]
    fn a_learned_guide_finds_its_app_by_process_never_by_title() {
        let g = Guide::parse("---\napp: Notepad\nexe: Notepad\n---\n## Word wrap\nView > Word wrap.\n", PathBuf::new()).unwrap();
        assert!(g.matches("notes.txt - Notepad", Some("notepad")));
        assert!(!g.matches("Notepad tips - Word", Some("winword")), "a title naming the app is not the app");
        assert!(!g.matches("x", None));
    }

    #[test]
    fn names_come_from_a_hand_written_path() {
        for (body, want) in [
            ("View > Statistics. The status bar shows no word count.", vec!["View", "Statistics"]),
            ("Edit > Preferences, Saving tab.", vec!["Edit", "Preferences"]),
            ("Insert > Date and time, or Ctrl+Shift+D.", vec!["Insert", "Date and time"]),
            // Prose before the path becomes part of the first name. It then matches no step, so the
            // note simply never gets bookkeeping -- harmless, and a written `names:` line fixes it.
            ("It is under File > Save as... in the old layout", vec!["It is under File", "Save as"]),
            ("Ctrl+; inserts today's date.", vec![]),
        ] {
            let want: Vec<String> = want.into_iter().map(String::from).collect();
            assert_eq!(derive_names(body), want, "{body}");
        }
    }

    #[test]
    fn the_file_round_trips_with_its_bookkeeping() {
        let text = "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nwhen: view, word, wrap\nnames: View > Word wrap\n\
status: confirmed 2, last 2026-10-10, app 11.2508.38.0\nView > Word wrap.\n\n## Font\nnames: Format > Font\n\
status: withheld 2026-10-11, sessions 3, app 11.2508.38.0, reason \"Format\" not on screen, twice checked\nFormat > Font.\n";
        let g = Guide::parse(text, PathBuf::new()).unwrap();
        assert_eq!(
            g.notes[0].status,
            NoteStatus::Confirmed { count: 2, last: "2026-10-10".into(), app: "11.2508.38.0".into() }
        );
        assert_eq!(
            g.notes[1].status,
            NoteStatus::Withheld {
                since: "2026-10-11".into(),
                sessions: 3,
                app: "11.2508.38.0".into(),
                reason: "\"Format\" not on screen, twice checked".into()
            }
        );
        assert_eq!(g.to_text(), text, "written back exactly as read");
    }

    #[test]
    fn a_withheld_note_never_reaches_the_prompt_not_even_its_title() {
        let text = "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nView > Word wrap.\n\n## Font\n\
status: withheld 2026-10-11, sessions 0, reason x\nFormat > Font.\n";
        let g = Guide::parse(text, PathBuf::new()).unwrap();
        let (b, sent) = g.block_with_sent("change the font");
        assert!(b.contains("- Word wrap: View > Word wrap."));
        assert!(!b.contains("Font"), "{b}");
        assert_eq!(sent, vec![0]);
        let all_withheld = Guide::parse(&text.replace("## Word wrap\n", "## Word wrap\nstatus: withheld 2026-10-11, sessions 0, reason y\n"), PathBuf::new()).unwrap();
        assert_eq!(all_withheld.block("x"), "", "no live note, no block");
    }

    #[test]
    fn a_short_guide_goes_in_whole_and_says_the_screen_wins() {
        let g = Guide::parse(TEXT, PathBuf::new()).unwrap();
        let b = g.block("insert a table");
        assert!(b.contains("follow the screen"));
        assert!(b.contains("- Export a note as PDF: It is in the Share menu"));
        assert!(b.contains("- Turn on autosave: Tools > Preferences > Saving."));
    }

    #[test]
    fn a_long_guide_keeps_every_title_and_only_the_matching_bodies() {
        let mut text = String::from("---\napp: X\nwindow_title_pattern: x\n---\n");
        for i in 0..40 {
            text.push_str(&format!("## Topic {i} filler\nwhen: filler{i}\n{}\n", "words ".repeat(12)));
        }
        text.push_str("## Export a note as PDF\nwhen: export, pdf\nIn the Share menu.\n");
        let g = Guide::parse(&text, PathBuf::new()).unwrap();
        let (b, sent) = g.block_with_sent("Save this note as a PDF");
        assert!(b.contains("- Export a note as PDF: In the Share menu."), "{b}");
        assert!(!b.contains("- Topic 3 filler:"), "a body that does not match stays out");
        assert!(b.contains("Topic 3 filler"), "but its title is listed");
        assert_eq!(sent, vec![40], "only the note whose body went in counts as sent");
    }
}
