//! H7b step 2 (harness-plan.md): keeping the learned guides current -- writing a note from a path
//! the user was guided through and clicked, confirming the notes such a path contains, and taking
//! a note out of the prompt when the screen contradicts it.
//!
//! **Why local** (SDD §1.1, decision 8). On the L2 battery, *"when the screen disagrees, follow the
//! screen"* held for one model of three: two kept repeating a stale shortcut over an open menu
//! that showed the new one. So staleness is caught here, from facts the app already has -- whether
//! the locator found a name, what the user clicked, ✗ Wrong -- and never left to the model.
//!
//! The rules (founder, 2026-10-10/11):
//! - **Written** from a run of two or more guided clicks: the step named a control and the user
//!   clicked a control of that name, in a role that is the app's own vocabulary (a menu item, a
//!   button, a tab). A single click is not written -- the screen already showed it.
//! - **Only what the model did not know.** A new note is written only when the task had a failed
//!   first try: a step whose target was not on screen, ✗ Wrong with no pointer, or a switch from
//!   one menu to its neighbour (File, then Publish). Found first time, the model knew it, and a
//!   note would only cost tokens. A path restarts at such a switch, so the detour never becomes
//!   part of the note.
//! - **The user's own way counts** (founder, 2026-10-11). When a step's target was not on screen
//!   and the user clicked a command of their own instead, that click is kept as a candidate; if
//!   the next step continues from it -- the user then clicks what that step named, and it is not a
//!   neighbour of the candidate -- the candidate joins the path. Live: a stale note sent the model
//!   to a Format menu that does not exist, the user opened View themselves and clicked Word wrap
//!   when the model then named it, and nothing was learned, because View was nobody's step.
//! - **Hand edits win.** The file is re-read whenever it changed since the app last read or
//!   wrote it, before anything is sent or written; a file edited into something that does not
//!   parse is neither sent nor written until it parses again; a deleted file empties the guide.
//! - **One line from the model** (option B). After a note is written, the next request asks the
//!   model for one line about it -- how a user might ask for it, what had to be true first --
//!   because a path alone has neither: on the L2 battery flash read *Send as PDF* next to the
//!   user's *save as a PDF* and still went to File. The line is checked here like a label is, and
//!   a note keeps its path whether a line comes or not. A note confirmed in a task that still had
//!   a failed try asks for its line again, to keep what holds and add what the session showed --
//!   the precondition one session met and another did not. A line in another language than the
//!   request is refused (luna wrote 2 of 12 in French on the battery).
//! - **Confirmed** whenever a run of guided clicks contains a note's path, whether or not the note
//!   was sent. Confirmation restores a withheld note.
//! - **One contradiction withholds a note**: a step built from it named a control that the locator
//!   did not find and the [Screen Elements] list does not have, or the user pressed ✗ Wrong on such
//!   a step while no pointer was drawn. A withheld note stays in the file and is never sent.
//! - **A second contradiction deletes it**, and so does going [`WITHHELD_SESSIONS`] sessions in
//!   that app without a confirmation.
//!
//! Content-free by construction: a path is UI labels only, from command roles, and a label that
//! looks like content -- a file name, a path, an address, a long number, part of the document's
//! name in the title bar -- breaks the path instead of entering the file.

use super::guide::{Guide, GuideNote, NoteStatus};
use std::collections::HashMap;
use std::path::PathBuf;

/// Sessions in an app a withheld note waits for a confirmation before it is deleted.
pub const WITHHELD_SESSIONS: u32 = 5;
/// Roles whose names are the application's vocabulary. List and tree items are left out on
/// purpose: in a file manager or a mail client their names are the user's files and subjects.
const PATH_ROLES: &[&str] = &["MenuItem", "Button", "SplitButton", "TabItem", "CheckBox", "RadioButton"];
const MAX_NAME_CHARS: usize = 48;
/// Confirm-and-close buttons at the end of a path: kept in the path, never its title.
const CLOSERS: &[&str] = &[
    "ok", "done", "apply", "close", "cancel", "yes", "no", "save", "确定", "完成", "应用", "关闭", "取消", "是", "否", "保存",
];

/// One guide as the Settings list shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GuideInfo {
    pub app: String,
    pub exe: String,
    pub notes: usize,
    /// Notes withheld after a contradiction ("paused" in the panel).
    pub withheld: usize,
}

/// The guided app, as the bookkeeping needs it.
#[derive(Debug, Clone, Default)]
pub struct AppKey {
    /// Process file stem, lowercase.
    pub exe: String,
    /// Display name, for a new guide's header.
    pub name: String,
    /// The exe's file version, recorded on each confirmation and contradiction.
    pub version: String,
    /// The window title -- used to match hand-written guides and to keep the document's name
    /// out of a path, never written to the file.
    pub title: String,
}

/// One click in the guided app, as `last_click` resolved it.
#[derive(Debug, Clone, Default)]
pub struct Click {
    pub role: String,
    pub name: String,
    /// The parent container's identity, compared and never written.
    pub parent: String,
}

/// Requests a written note's line is asked for before the note settles for its path alone.
const LINE_ASKS: u8 = 2;
const MAX_LINE_CHARS: usize = 220;

#[derive(Debug, Clone)]
struct Expect {
    exe: String,
    target: String,
    located: bool,
    /// The target was not on screen: not located, and not in this screen's list. The user's own
    /// next click may then be the way (`candidate`).
    missing: bool,
    /// The sent note the target came from, when it came from one.
    from_note: Option<(usize, usize)>,
}

#[derive(Debug, Default)]
pub struct Learned {
    dir: PathBuf,
    guides: Vec<Guide>,
    /// Notes whose bodies went into the last prompt: the ones a step can have come from.
    sent: Vec<(usize, usize)>,
    /// The step on screen now, waiting for the user's click.
    expect: Option<Expect>,
    /// Guided clicks so far, in order, and the app they were in.
    run: Vec<String>,
    run_exe: String,
    /// The note this run wrote, so a longer run updates it instead of adding another.
    run_note: Option<(usize, usize)>,
    /// Notes this run has already confirmed, so one run counts once.
    run_confirmed: Vec<(usize, usize)>,
    /// The session each guide last counted, for the withheld expiry.
    counted: HashMap<usize, String>,
    /// The parents of the run's clicks, beside `run`.
    run_parents: Vec<String>,
    /// This task had a failed first try, so what it finds is worth writing.
    task_struggled: bool,
    /// This task's guided clicks, `(parent, name)`, for spotting a switch between neighbours.
    task_clicks: Vec<(String, String)>,
    /// A note just written, waiting for the model's line: `(guide, note, times asked)`.
    pending_line: Option<(usize, usize, u8)>,
    /// The request and goal of the last prompt, to judge a line's language against.
    last_focus: String,
    /// The user's own command click after a step whose target was not on screen: joins the path
    /// if the next guided click continues from it.
    candidate: Option<Click>,
    /// Switched off (`LEARNED_NOTES=false`): nothing is read, sent or written.
    off: bool,
}

/// What re-reading a guide's file found.
#[derive(Debug, PartialEq, Eq)]
enum Freshness {
    /// As last read or written: references into it still hold.
    Same,
    /// Re-read (or emptied) after a change on disk: every reference into it was dropped.
    Reloaded,
    /// Does not parse: not to be sent or written.
    Unusable,
}

impl Learned {
    /// Every guide under `dir` (`<user packs>\learned`).
    pub fn load(dir: PathBuf) -> Learned {
        let guides = super::guide::load_dir(&dir);
        for g in &guides {
            let withheld = g.notes.iter().filter(|n| n.status.is_withheld()).count();
            log::info!("[guide] '{}': {} notes ({withheld} withheld) from {}", g.app, g.notes.len(), g.path.display());
        }
        Learned { dir, guides, ..Default::default() }
    }

    /// Learned notes switched off: no guide is read, sent or written this run.
    pub fn disabled(dir: PathBuf) -> Learned {
        log::info!("[guide] learned notes off (LEARNED_NOTES=false)");
        Learned { dir, off: true, ..Default::default() }
    }

    /// Every guide with a file, for Settings: re-read where it changed, new files picked up.
    pub fn list(&mut self) -> Vec<GuideInfo> {
        if self.off {
            return Vec::new();
        }
        for g in super::guide::load_dir(&self.dir) {
            if !self.guides.iter().any(|known| known.path == g.path) {
                self.guides.push(g);
            }
        }
        let mut out = Vec::new();
        for g in 0..self.guides.len() {
            if self.fresh(g) == Freshness::Unusable || !self.guides[g].path.exists() {
                continue;
            }
            let guide = &self.guides[g];
            out.push(GuideInfo {
                app: guide.app.clone(),
                exe: guide.exe.clone().unwrap_or_default(),
                notes: guide.notes.len(),
                withheld: guide.notes.iter().filter(|n| n.status.is_withheld()).count(),
            });
        }
        out.sort_by_key(|g| g.app.to_lowercase());
        out
    }

    /// The file of the guide for `exe`, for opening it in an editor.
    pub fn path_of(&self, exe: &str) -> Option<PathBuf> {
        self.guides.iter().find(|g| g.exe.as_deref() == Some(exe)).map(|g| g.path.clone())
    }

    /// Delete the guide for `exe`: its file (and its folder, once empty) go to the Recycle Bin,
    /// and nothing of it is kept in memory -- a path, a pending line, a run.
    pub fn delete(&mut self, exe: &str) -> std::io::Result<()> {
        let Some(g) = self.guides.iter().position(|g| g.exe.as_deref() == Some(exe)) else {
            return Ok(());
        };
        let path = self.guides[g].path.clone();
        if path.exists() {
            retire(&path)?;
        }
        if let Some(dir) = path.parent() {
            if dir.read_dir().is_ok_and(|mut e| e.next().is_none()) {
                let _ = retire(dir);
            }
        }
        log::info!("[guide] '{}': deleted from Settings -- {} to the Recycle Bin", self.guides[g].app, path.display());
        // Indices into the guides shift: forget every one rather than fix them up.
        self.end();
        self.sent.clear();
        self.pending_line = None;
        self.counted.clear();
        self.guides.remove(g);
        Ok(())
    }

    fn find(&self, app: &AppKey) -> Option<usize> {
        self.guides.iter().position(|g| g.matches(&app.title, Some(&app.exe)))
    }

    /// `find`, or a guide file for this app that appeared since startup -- written by hand, or
    /// copied from another machine.
    fn find_or_load(&mut self, app: &AppKey) -> Option<usize> {
        if let Some(g) = self.find(app) {
            return Some(g);
        }
        let path = self.dir.join(folder_for(&app.exe)).join("guide.md");
        let g = super::guide::load_file(&path)?;
        if !g.matches(&app.title, Some(&app.exe)) {
            return None;
        }
        log::info!("[guide] '{}': {} notes from {} (new since startup)", g.app, g.notes.len(), path.display());
        self.guides.push(g);
        Some(self.guides.len() - 1)
    }

    /// Re-read guide `g` if its file changed since the app last read or wrote it. Called before
    /// anything is sent from it or written to it, so a hand edit is picked up, never overwritten.
    fn fresh(&mut self, g: usize) -> Freshness {
        let now = super::guide::file_stamp(&self.guides[g].path);
        if now == self.guides[g].stamp {
            return if self.guides[g].unreadable { Freshness::Unusable } else { Freshness::Same };
        }
        self.drop_refs(g);
        let path = self.guides[g].path.clone();
        let Some(stamp) = now else {
            log::info!("[guide] '{}': {} was deleted -- the guide starts empty", self.guides[g].app, path.display());
            self.guides[g].notes.clear();
            self.guides[g].stamp = None;
            self.guides[g].unreadable = false;
            return Freshness::Reloaded;
        };
        let parsed = std::fs::read_to_string(&path).ok().and_then(|t| Guide::parse(&t, path.clone()));
        match parsed {
            Some(mut fresh) => {
                fresh.stamp = Some(stamp);
                log::info!("[guide] '{}': {} changed on disk -- re-read, {} notes", fresh.app, path.display(), fresh.notes.len());
                self.guides[g] = fresh;
                Freshness::Reloaded
            }
            None => {
                log::warn!("[guide] {} no longer parses -- not sent or written until it does", path.display());
                self.guides[g].stamp = Some(stamp);
                self.guides[g].unreadable = true;
                Freshness::Unusable
            }
        }
    }

    /// Forget every index into guide `g`: after a re-read they may point at other notes.
    fn drop_refs(&mut self, g: usize) {
        self.sent.retain(|&(sg, _)| sg != g);
        self.run_confirmed.retain(|&(sg, _)| sg != g);
        if self.run_note.is_some_and(|(sg, _)| sg == g) {
            self.run_note = None;
        }
        if self.pending_line.is_some_and(|(sg, _, _)| sg == g) {
            self.pending_line = None;
        }
        if let Some(e) = self.expect.as_mut() {
            if e.from_note.is_some_and(|(sg, _)| sg == g) {
                e.from_note = None;
            }
        }
    }

    /// The `[Learned notes]` block for this request, empty without a guide. Records which notes
    /// went in, and counts a new session against the guide's withheld notes.
    pub fn prompt_block(&mut self, app: &AppKey, focus: &str, session: &str) -> String {
        self.sent.clear();
        if self.off {
            return String::new();
        }
        self.last_focus = focus.to_string();
        let Some(g) = self.find_or_load(app) else { return String::new() };
        if self.fresh(g) == Freshness::Unusable {
            return String::new();
        }
        self.count_session(g, session);
        let (mut block, sent) = self.guides[g].block_with_sent(focus);
        self.sent = sent.into_iter().map(|n| (g, n)).collect();
        if let Some((pg, pn, asks)) = self.pending_line {
            if pg == g && asks < LINE_ASKS {
                let note = &self.guides[g].notes[pn];
                block.push_str(&request_block(&self.guides[g].app, &note.title, &note.names, note_line(note)));
                self.pending_line = Some((pg, pn, asks + 1));
            } else if pg == g {
                log::info!("[guide] '{}': no line for \"{}\" -- the path stands alone", self.guides[g].app, self.guides[g].notes[pn].title);
                self.pending_line = None;
            }
        }
        if !block.is_empty() {
            // What reached the prompt, since nothing else shows it.
            log::info!(
                "[guide] '{}' in the prompt: {} of {} notes, {} chars",
                self.guides[g].app,
                self.sent.len(),
                self.guides[g].notes.len(),
                block.len()
            );
        }
        block
    }

    fn count_session(&mut self, g: usize, session: &str) {
        if session.is_empty() || self.counted.get(&g).map(String::as_str) == Some(session) {
            return;
        }
        self.counted.insert(g, session.to_string());
        let mut changed = false;
        let mut n = 0;
        while n < self.guides[g].notes.len() {
            if let NoteStatus::Withheld { sessions, .. } = &mut self.guides[g].notes[n].status {
                *sessions += 1;
                changed = true;
                if *sessions >= WITHHELD_SESSIONS {
                    log::info!(
                        "[guide] '{}': deleted \"{}\" -- withheld for {WITHHELD_SESSIONS} sessions with no confirmation",
                        self.guides[g].app,
                        self.guides[g].notes[n].title
                    );
                    self.delete_note(g, n);
                    continue;
                }
            }
            n += 1;
        }
        if changed {
            self.save(g);
        }
    }

    /// The user's clicks in the guided app since the last call, oldest first. A click on the
    /// control the step named extends the run; anything else ends it.
    pub fn clicks(&mut self, app: &AppKey, clicks: &[Click], today: &str) {
        if self.off {
            return;
        }
        let Some(exp) = self.expect.take() else {
            self.candidate = None;
            return;
        };
        if exp.exe != app.exe {
            self.end_run();
            return;
        }
        match clicks.iter().find(|c| names_match(&c.name, &exp.target)) {
            Some(c) => {
                if let Some(cand) = self.candidate.take() {
                    // The step continued from the user's own click (it is not the candidate's
                    // neighbour, and nothing else was clicked between): the click was the way.
                    let continues = self.run.is_empty()
                        && (cand.parent.is_empty() || cand.parent != c.parent)
                        && !names_match(&cand.name, &c.name);
                    if continues {
                        log::info!("[guide] '{}': \"{}\" -- the user's own click -- joins the path", app.exe, cand.name);
                        self.task_clicks.push((cand.parent.clone(), cand.name.clone()));
                        self.extend_run(app, cand.name, cand.parent, today);
                    }
                }
                self.guided_click(app, c, today)
            }
            None => {
                self.end_run();
                // The step's target was not there and the user went another way: their last
                // command click may be the way, if the next step continues from it.
                if exp.missing {
                    self.candidate = clicks.iter().rev().find(|c| is_ui_label(&c.role, &c.name, app)).cloned();
                    if let Some(c) = &self.candidate {
                        log::info!("[guide] '{}': \"{}\" clicked instead of a missing \"{}\" -- held as a candidate", app.exe, c.name, exp.target);
                    }
                }
            }
        }
    }

    fn guided_click(&mut self, app: &AppKey, c: &Click, today: &str) {
        // A neighbour of a control clicked earlier in this task -- the next menu on the bar, the
        // next tab -- means the first one was the wrong place.
        let switched = !c.parent.is_empty()
            && self.task_clicks.iter().any(|(p, n)| *p == c.parent && !names_match(n, &c.name));
        if switched && !self.task_struggled {
            log::info!("[guide] '{}': switched to \"{}\" -- the first try was elsewhere", app.exe, c.name);
            self.task_struggled = true;
        }
        self.task_clicks.push((c.parent.clone(), c.name.clone()));
        if !is_ui_label(&c.role, &c.name, app) {
            log::debug!("[guide] path ends: a {} is not part of a learnable path", c.role);
            self.end_run();
            return;
        }
        // The path restarts at the neighbour: the detour is not part of it.
        if let Some(i) = self.run_parents.iter().position(|p| !p.is_empty() && *p == c.parent) {
            self.run.truncate(i);
            self.run_parents.truncate(i);
            self.run_note = None;
            self.run_confirmed.clear();
        }
        self.extend_run(app, c.name.clone(), c.parent.clone(), today);
    }

    /// A step went on screen. `located`: the locator found its target. `list`: the names in the
    /// [Screen Elements] list captured with this screen -- `None` when there is no fresh list (no
    /// tree, or a local advance whose list predates the menu it is in).
    pub fn step_shown(&mut self, app: &AppKey, target: Option<&str>, located: bool, list: Option<&[String]>, today: &str) {
        if self.off {
            return;
        }
        let Some(target) = target.map(first_segment).filter(|t| !t.is_empty()) else {
            // Nothing to click -- a keystroke, a question, the task done: the path ends here.
            self.end_run();
            self.expect = None;
            return;
        };
        let mut from_note = self.note_for_target(&target);
        let missing = !located && list.is_some_and(|l| !l.iter().any(|e| names_match(e, &target)));
        if missing && !self.task_struggled {
            log::info!("[guide] '{}': \"{target}\" is not on screen -- a failed try", app.exe);
            self.task_struggled = true;
        }
        if let (Some((g, n, i)), false, Some(list)) = (from_note, located, list) {
            // Only a name that should be visible now can be missing: the path's first, or the
            // next one after the control the user just clicked.
            let names = &self.guides[g].notes[n].names;
            let reachable = i == 0 || self.run.last().is_some_and(|last| names_match(last, &names[i - 1]));
            if reachable && !list.iter().any(|e| names_match(e, &target)) {
                self.miss(g, n, app, format!("\"{target}\" not on screen"), today);
                from_note = None;
            }
        }
        if let Some(list) = list {
            self.passive_check(app, &target, located, list, today);
        }
        self.expect = Some(Expect {
            exe: app.exe.clone(),
            target,
            located,
            missing,
            from_note: from_note.map(|(g, n, _)| (g, n)),
        });
    }

    /// A sent note whose path runs through the control the user just clicked says what comes next.
    /// When this screen -- whose list was captured after that click -- does not have it, the note
    /// is contradicted whether or not the model followed it: a model that reads the screen and
    /// ignores a stale note would otherwise leave it in the file for ever. Judged only when the
    /// step points at something visible other than the control just clicked, which shows the
    /// click opened what it opens (a menu that closed again gets "open View" and no verdict).
    fn passive_check(&mut self, app: &AppKey, target: &str, located: bool, list: &[String], today: &str) {
        let Some(last) = self.run.last().cloned() else { return };
        if !located || names_match(target, &last) || !list.iter().any(|e| names_match(e, target)) {
            return;
        }
        let stale: Vec<(usize, usize, String)> = self
            .sent
            .iter()
            .filter_map(|&(g, n)| {
                let names = &self.guides[g].notes[n].names;
                let i = names.iter().position(|nm| names_match(nm, &last))?;
                let next = names.get(i + 1)?;
                (!names_match(next, target) && !list.iter().any(|e| names_match(e, next))).then(|| (g, n, next.clone()))
            })
            .collect();
        for (g, n, next) in stale {
            if !self.guides[g].notes[n].status.is_withheld() {
                self.miss(g, n, app, format!("\"{next}\" not on screen after \"{last}\""), today);
            }
        }
    }

    /// ✗ Wrong on the step on screen. It contradicts the note behind the step only when no
    /// pointer was drawn -- with a pointer, "wrong" is as likely to mean the pointer as the note.
    pub fn wrong(&mut self, app: &AppKey, today: &str) {
        if self.off {
            return;
        }
        if let Some(exp) = self.expect.take() {
            if !exp.located && exp.exe == app.exe {
                self.task_struggled = true;
            }
            if let (Some((g, n)), false, true) = (exp.from_note, exp.located, exp.exe == app.exe) {
                self.miss(g, n, app, format!("\"{}\" marked wrong", exp.target), today);
            }
        }
        self.end_run();
    }

    /// The task ended (a new one, or a new session): the path ends with it, and so does what
    /// was learned about how hard it was.
    pub fn end(&mut self) {
        self.end_run();
        self.expect = None;
        self.task_struggled = false;
        self.task_clicks.clear();
    }

    /// The model's `app_note` for the request just answered. Kept as the pending note's line when
    /// one was asked for and it passes the content check; otherwise ignored.
    pub fn attach_line(&mut self, app: &AppKey, text: &str, today: &str) {
        if self.off {
            return;
        }
        let Some((g, n, _)) = self.pending_line else { return };
        if text.trim().is_empty() || !self.guides[g].matches(&app.title, Some(&app.exe)) {
            return;
        }
        if self.fresh(g) != Freshness::Same {
            return; // edited by hand meanwhile: the request no longer points at a known note
        }
        self.pending_line = None;
        let Some(line) = line_ok(text, app) else {
            log::info!("[guide] '{}': line for \"{}\" refused by the content check", self.guides[g].app, self.guides[g].notes[n].title);
            return;
        };
        if !same_language(&line, &self.last_focus) {
            log::info!("[guide] '{}': line for \"{}\" refused -- not in the language of the request: {line}", self.guides[g].app, self.guides[g].notes[n].title);
            return;
        }
        if !adds_something(&line, &self.guides[g].notes[n], &self.guides[g].app) {
            log::info!("[guide] '{}': line for \"{}\" refused -- it only repeats the path: {line}", self.guides[g].app, self.guides[g].notes[n].title);
            return;
        }
        let app_name = self.guides[g].app.clone();
        let note = &mut self.guides[g].notes[n];
        note.body = format!("{}. {line}", note.names.join(" > "));
        for w in when_words(&line) {
            if !note.when.contains(&w) {
                note.when.push(w);
            }
        }
        log::info!("[guide] '{app_name}': line for \"{}\": {line}", note.title);
        let _ = today;
        self.save(g);
    }

    fn end_run(&mut self) {
        self.run.clear();
        self.run_parents.clear();
        self.run_note = None;
        self.run_confirmed.clear();
        self.candidate = None;
    }

    fn extend_run(&mut self, app: &AppKey, name: String, parent: String, today: &str) {
        if self.run_exe != app.exe {
            self.end_run();
            self.run_exe = app.exe.clone();
        }
        self.run.push(name);
        self.run_parents.push(parent);
        if self.run.len() < 2 {
            return;
        }
        let run = self.run.clone();
        let title = title_for(&run);
        let mut changed = false;
        let existing = self.find_or_load(app);
        if let Some(g) = existing {
            if self.fresh(g) == Freshness::Unusable {
                return; // a hand edit that does not parse: write nothing over it
            }
        }
        if let Some(g) = existing {
            let app_name = self.guides[g].app.clone();
            // Every note whose path this run now contains is confirmed -- once per run.
            for n in 0..self.guides[g].notes.len() {
                if self.run_note == Some((g, n)) || self.run_confirmed.contains(&(g, n)) {
                    continue;
                }
                if contains_path(&run, &self.guides[g].notes[n].names) {
                    let note = &mut self.guides[g].notes[n];
                    let was_withheld = note.status.is_withheld();
                    confirm(note, app, today);
                    log::info!(
                        "[guide] '{app_name}': confirmed \"{}\"{}",
                        self.guides[g].notes[n].title,
                        if was_withheld { " -- restored" } else { "" }
                    );
                    self.run_confirmed.push((g, n));
                    changed = true;
                    // Found through the note, yet the task still had a failed try: the note did
                    // not say enough. Ask for its line again, to keep what holds and add the rest.
                    if self.task_struggled && self.pending_line.is_none() {
                        self.pending_line = Some((g, n, 0));
                    }
                }
            }
            if let Some((_, rn)) = self.run_note {
                set_path(&mut self.guides[g].notes[rn], &run);
                changed = true;
            } else if self.guides[g].notes.iter().any(|n| same_path(&n.names, &run)) {
                // Already known, and confirmed above.
            } else if let Some(n) = self.guides[g].notes.iter().position(|n| names_match(&n.title, &title)) {
                // Same command, another path: it moved. The new path replaces the old.
                let note = &mut self.guides[g].notes[n];
                log::info!("[guide] '{app_name}': \"{title}\" moved: {} -> {}", note.names.join(" > "), run.join(" > "));
                set_path(note, &run);
                note.status = NoteStatus::Confirmed { count: 1, last: today.to_string(), app: app.version.clone() };
                self.run_note = Some((g, n));
                // It moved, so the old note misled: the new path is worth the model's line too.
                self.pending_line = Some((g, n, 0));
                changed = true;
            } else if self.task_struggled {
                self.add_note(g, &run, app, today);
                changed = true;
            }
            if changed {
                self.save(g);
            }
        } else if self.task_struggled {
            let g = self.guide_for_writing(app);
            self.add_note(g, &run, app, today);
            self.save(g);
            changed = true;
        }
        if !changed && !self.task_struggled {
            // Found first time: the model knew it. Nothing new to write.
            log::debug!("[guide] '{}': not written, found first time: {}", app.exe, run.join(" > "));
        }
    }

    fn add_note(&mut self, g: usize, run: &[String], app: &AppKey, today: &str) {
        let mut note = GuideNote {
            title: String::new(),
            when: Vec::new(),
            names: Vec::new(),
            status: NoteStatus::Confirmed { count: 1, last: today.to_string(), app: app.version.clone() },
            body: String::new(),
        };
        set_path(&mut note, run);
        log::info!("[guide] '{}': learned \"{}\": {}", self.guides[g].app, note.title, note.body);
        self.guides[g].notes.push(note);
        let n = self.guides[g].notes.len() - 1;
        self.run_note = Some((g, n));
        self.pending_line = Some((g, n, 0));
    }

    /// The guide notes for `app` are written to: the one that matches it, or a new one.
    fn guide_for_writing(&mut self, app: &AppKey) -> usize {
        if let Some(g) = self.find(app) {
            return g;
        }
        let name = if app.name.is_empty() { app.exe.as_str() } else { app.name.as_str() };
        self.guides.push(Guide::new(name, &app.exe, self.dir.join(folder_for(&app.exe)).join("guide.md")));
        self.guides.len() - 1
    }

    /// The first sent note whose path names `target`, with the name's position in it.
    fn note_for_target(&self, target: &str) -> Option<(usize, usize, usize)> {
        self.sent.iter().find_map(|&(g, n)| {
            self.guides[g].notes[n].names.iter().position(|nm| names_match(nm, target)).map(|i| (g, n, i))
        })
    }

    fn miss(&mut self, g: usize, n: usize, app: &AppKey, reason: String, today: &str) {
        if self.fresh(g) != Freshness::Same {
            return; // edited by hand meanwhile: (g, n) may name another note now
        }
        let title = self.guides[g].notes[n].title.clone();
        if self.guides[g].notes[n].status.is_withheld() {
            log::info!("[guide] '{}': deleted \"{title}\" -- contradicted twice ({reason})", self.guides[g].app);
            self.delete_note(g, n);
        } else {
            log::info!("[guide] '{}': withheld \"{title}\" -- {reason}", self.guides[g].app);
            self.guides[g].notes[n].status =
                NoteStatus::Withheld { since: today.to_string(), sessions: 0, app: app.version.clone(), reason };
        }
        self.save(g);
    }

    fn delete_note(&mut self, g: usize, n: usize) {
        self.guides[g].notes.remove(n);
        let fix = |slot: &mut Option<(usize, usize)>| {
            *slot = match *slot {
                Some((sg, sn)) if sg == g && sn == n => None,
                Some((sg, sn)) if sg == g && sn > n => Some((sg, sn - 1)),
                other => other,
            };
        };
        fix(&mut self.run_note);
        if let Some(e) = self.expect.as_mut() {
            fix(&mut e.from_note);
        }
        self.pending_line = match self.pending_line {
            Some((pg, pn, _)) if pg == g && pn == n => None,
            Some((pg, pn, a)) if pg == g && pn > n => Some((pg, pn - 1, a)),
            other => other,
        };
        for list in [&mut self.sent, &mut self.run_confirmed] {
            list.retain(|&(sg, sn)| !(sg == g && sn == n));
            for slot in list.iter_mut() {
                if slot.0 == g && slot.1 > n {
                    slot.1 -= 1;
                }
            }
        }
    }

    fn save(&mut self, g: usize) {
        if let Err(e) = self.guides[g].save() {
            log::warn!("[guide] could not write {}: {e}", self.guides[g].path.display());
        }
    }

    #[cfg(test)]
    fn guide(&self, exe: &str) -> Option<&Guide> {
        self.guides.iter().find(|g| g.exe.as_deref() == Some(exe))
    }
}

/// A guide deleted from Settings goes to the Recycle Bin. Tests delete outright, so `cargo test`
/// never fills the developer's Recycle Bin.
#[cfg(all(windows, not(test)))]
fn retire(path: &std::path::Path) -> std::io::Result<()> {
    crate::recycle::to_recycle_bin(path)
}

#[cfg(any(not(windows), test))]
fn retire(path: &std::path::Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn confirm(note: &mut GuideNote, app: &AppKey, today: &str) {
    let count = match &note.status {
        NoteStatus::Confirmed { count, .. } => count + 1,
        _ => 1,
    };
    note.status = NoteStatus::Confirmed { count, last: today.to_string(), app: app.version.clone() };
}

/// A note's title, keywords, path and body, all from the path -- keeping a line the model wrote.
fn set_path(note: &mut GuideNote, path: &[String]) {
    let old = format!("{}.", note.names.join(" > "));
    let line = if note.names.is_empty() { "" } else { note.body.strip_prefix(&old).unwrap_or("").trim() };
    let line_words = when_words(line);
    note.title = title_for(path);
    note.when = when_for(path);
    for w in line_words {
        if !note.when.contains(&w) {
            note.when.push(w);
        }
    }
    note.names = path.to_vec();
    note.body = if line.is_empty() { format!("{}.", path.join(" > ")) } else { format!("{}. {line}", path.join(" > ")) };
}

/// What the prompt asks for when a note has just been written, or needs its line again (option
/// B). No example route: an example in a prompt is read as a preference everywhere (rule 21). It
/// asks for a precondition only when there was one -- *"anything that had to be true"* drew
/// padding like *"the note must be open first"* from two models of three on the battery.
pub fn request_block(app: &str, title: &str, path: &[String], line: Option<&str>) -> String {
    let path = path.join(" > ");
    match line {
        None => format!(
            "\n[Learned notes: write one] In {app}, the user just reached \"{title}\" through {path} after a \
             different first try. In app_note, write one line for the next time someone asks for this \
             here: the words they might use for it, and -- only if the user had to do something first \
             for it to work -- what that was. About the application only: no document names, text, \
             numbers or people.\n"
        ),
        Some(line) => format!(
            "\n[Learned notes: rewrite one] In {app}, the note for \"{title}\" ({path}) reads: \"{line}\" \
             This task still had a failed first try. In app_note, write that line again for the next \
             time: keep what is still true, and add what this session showed -- the words the user \
             used for it, or something they had to do first for it to work. About the application \
             only: no document names, text, numbers or people.\n"
        ),
    }
}

/// A note's line: its body after the path, when the model wrote one.
fn note_line(note: &GuideNote) -> Option<&str> {
    let path = format!("{}.", note.names.join(" > "));
    note.body.strip_prefix(path.as_str()).map(str::trim).filter(|l| !l.is_empty())
}

/// Whether a line says anything the note does not already: a word beyond its title, its path and
/// the app's name, prose aside. *"Word wrap: View > Word wrap."* under the path View > Word wrap
/// says nothing; *"Word wrap may also be called line wrapping"* says what the path cannot.
fn adds_something(line: &str, note: &GuideNote, app: &str) -> bool {
    let mut known: Vec<String> = when_for(&note.names);
    known.extend(when_for(&[note.title.clone(), app.to_string()]));
    known.extend(words_plain(&note.names.join(" ")));
    known.extend(words_plain(&format!("{} {app}", note.title)));
    when_words(line).iter().any(|w| !known.contains(w))
}

/// Every word of `s`, lowercase, no filtering: what a path or a title is made of.
fn words_plain(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// The folder a new guide for `exe` is written to.
fn folder_for(exe: &str) -> String {
    exe.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Whether a line is in the language of the request. Judged only for a request written in
/// plain ASCII (an English one): there a line with accented or non-Latin letters outside quotes,
/// or two Romance function words, is the rule-13 drift -- luna's French. Any other request is not
/// judged here.
fn same_language(line: &str, focus: &str) -> bool {
    let foreign = |s: &str| s.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
    if focus.trim().is_empty() || foreign(focus) {
        return true;
    }
    let bare = unquoted(line);
    if foreign(&bare) {
        return false;
    }
    const ROMANCE: &[&str] = &["le", "les", "une", "des", "pour", "avec", "dans", "el", "los", "las", "para", "con", "una", "por", "del"];
    let lower = bare.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    ROMANCE.iter().filter(|w| words.contains(w)).count() < 2
}

/// A line without its quoted spans: a UI label quoted in its own language is not drift.
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

/// The model's line, flattened, when it passes the same content check a label does.
fn line_ok(text: &str, app: &AppKey) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim().trim_matches('"').trim().to_string();
    if line.is_empty() || line.chars().count() > MAX_LINE_CHARS || line.contains('\\') || line.contains("://") || line.contains('@') {
        return None;
    }
    let mut digits = 0;
    for c in line.chars() {
        digits = if c.is_ascii_digit() { digits + 1 } else { 0 };
        if digits >= 3 {
            return None;
        }
    }
    // A file name anywhere in it.
    for word in line.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '\'')) {
        let w = word.trim_end_matches(['.', '…', ':']);
        if let Some((stem, ext)) = w.rsplit_once('.') {
            if !stem.is_empty() && (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                return None;
            }
        }
    }
    let doc = document_part(&app.title, app);
    if doc.chars().count() >= 4 && contains_words(&norm(&line), &doc) {
        return None;
    }
    Some(line)
}

/// Keywords from a model's line: its words, as a path's are chosen, without the prose that
/// matches every request (*"under, open, first, can, toggled"* on the live run).
fn when_words(line: &str) -> Vec<String> {
    const PROSE: &[&str] = &[
        "under", "open", "opens", "first", "can", "must", "need", "needs", "then", "use", "using", "choose",
        "click", "select", "before", "after", "there", "here", "menu", "tab", "option", "setting", "settings",
        "toggle", "toggled", "turn", "enable", "enabled", "you", "your", "its", "are", "when", "where", "what",
        "which", "only", "also", "any", "has", "have", "had", "was", "were", "will", "would", "should", "could",
        "not", "via", "into", "onto", "just", "now", "may", "might", "called", "through", "often",
        "usually", "simply",
    ];
    when_for(&[line.to_string()]).into_iter().filter(|w| !PROSE.contains(&w.as_str())).collect()
}

/// The command a path reaches: its last name, past any confirm-and-close buttons that follow a
/// command in a dialog ("Edit > Preferences > Saving > Autosave > OK" is about Autosave).
fn title_for(path: &[String]) -> String {
    let mut end = path.len();
    while end > 2 && is_closer(&path[end - 1]) {
        end -= 1;
    }
    path[end - 1].clone()
}

fn is_closer(name: &str) -> bool {
    CLOSERS.contains(&norm(name).as_str())
}

fn when_for(path: &[String]) -> Vec<String> {
    const STOP: &[&str] = &["and", "the", "for", "with", "from", "into", "this", "that"];
    let mut out: Vec<String> = Vec::new();
    for name in path.iter().filter(|n| !is_closer(n)) {
        for w in norm(name).split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 2 || !w.is_ascii()) {
            if !w.is_empty() && !STOP.contains(&w) && !out.iter().any(|o| o == w) {
                out.push(w.to_string());
            }
        }
    }
    out
}

/// Lowercase; shortcut text after a tab, `&` accelerators, trailing dots and colons gone; a
/// leading "the" and a trailing role word the model sometimes adds ("the File menu") gone; spaces
/// collapsed.
fn norm(s: &str) -> String {
    let s = s.split('\t').next().unwrap_or(s).replace('&', "").to_lowercase();
    let mut s = s.trim().trim_end_matches(['.', '…', ':']).trim().to_string();
    if let Some(rest) = s.strip_prefix("the ") {
        s = rest.to_string();
    }
    for role in [" menu", " button", " tab", " option", " item", " toggle", " switch", " checkbox"] {
        if let Some(rest) = s.strip_suffix(role) {
            if !rest.trim().is_empty() {
                s = rest.to_string();
            }
            break;
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The same control name, once normalised. Exact on purpose: "Edit" is not "Edit Points".
fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

fn contains_words(hay: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let i = start + pos;
        let j = i + needle.len();
        let before = hay[..i].chars().next_back();
        let after = hay[j..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        start = i + needle.len().max(1);
    }
    false
}

/// What to click now, from a target the model wrote as a path ("Publish > Send as PDF").
fn first_segment(target: &str) -> String {
    let t = target.split(" > ").next().unwrap_or(target);
    t.trim().trim_matches(|c: char| matches!(c, '"' | '\'' | '\u{201c}' | '\u{201d}')).trim().to_string()
}

fn same_path(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| names_match(x, y))
}

/// `run` contains `path` as consecutive clicks.
fn contains_path(run: &[String], path: &[String]) -> bool {
    !path.is_empty() && path.len() <= run.len() && run.windows(path.len()).any(|w| same_path(w, path))
}

/// Whether a clicked control's name may enter a path: a command role, and nothing that looks like
/// the user's content.
fn is_ui_label(role: &str, name: &str, app: &AppKey) -> bool {
    if !PATH_ROLES.contains(&role) {
        return false;
    }
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME_CHARS {
        return false;
    }
    if n.contains(['\\', '/', '@']) || n.contains("://") {
        return false;
    }
    // A file name: "Q3 report.docx", but not "Save as...".
    let bare = n.trim_end_matches(['.', '…']);
    if let Some((_, ext)) = bare.rsplit_once('.') {
        if (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return false;
        }
    }
    // A long number: a date, an amount, an id.
    let mut digits = 0;
    for c in n.chars() {
        digits = if c.is_ascii_digit() { digits + 1 } else { 0 };
        if digits >= 3 {
            return false;
        }
    }
    // Part of the document's name in the title bar ("Q3 planning" in "Contoso Notes - Q3 planning").
    let doc = document_part(&app.title, app);
    let nn = norm(n);
    if nn.chars().count() >= 4 && !doc.is_empty() && (contains_words(&doc, &nn) || contains_words(&nn, &doc)) {
        return false;
    }
    true
}

/// "shopping list.txt" -> "shopping list": the words of a document's name, not its type.
fn strip_extension(seg: &str) -> &str {
    match seg.trim().rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty() && (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => seg,
    }
}

/// The window title without the segments that name the app.
fn document_part(title: &str, app: &AppKey) -> String {
    let app_name = norm(&app.name);
    let names_the_app = |s: &str| {
        (!app.exe.is_empty() && s.contains(app.exe.as_str())) || (!app_name.is_empty() && s.contains(app_name.as_str()))
    };
    let mut t = title.to_string();
    for sep in [" – ", " — ", " | "] {
        t = t.replace(sep, " - ");
    }
    t.split(" - ")
        .map(strip_extension)
        .map(norm)
        .filter(|s| !s.is_empty() && !names_the_app(s))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "navisual-learn-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn notepad() -> AppKey {
        AppKey { exe: "notepad".into(), name: "Notepad".into(), version: "11.1".into(), title: "shopping list.txt - Notepad".into() }
    }

    fn click(role: &str, name: &str, parent: &str) -> Vec<Click> {
        vec![Click { role: role.into(), name: name.into(), parent: parent.into() }]
    }

    /// Show `target`, then the user clicks it -- in a container of its own, so no click is
    /// anyone's neighbour unless a test says so.
    fn guided(l: &mut Learned, app: &AppKey, target: &str) {
        guided_in(l, app, target, &format!("in:{target}"));
    }

    fn guided_in(l: &mut Learned, app: &AppKey, target: &str, parent: &str) {
        l.step_shown(app, Some(target), true, None, "2026-10-10");
        l.clicks(app, &click("MenuItem", target, parent), "2026-10-10");
    }

    /// A failed first try in this task: a step whose target is not on screen.
    fn struggle(l: &mut Learned, app: &AppKey) {
        l.step_shown(app, Some("Nowhere"), false, Some(&["File".to_string()]), "2026-10-10");
    }

    fn with_guide(dir: &PathBuf, text: &str) -> Learned {
        let d = dir.join("notepad");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("guide.md"), text).unwrap();
        Learned::load(dir.clone())
    }

    const STALE: &str = "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: Format > Word wrap\nFormat > Word wrap.\n";

    #[test]
    fn two_guided_clicks_write_a_note_and_one_does_not() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        assert!(l.guide("notepad").is_none(), "one click is not knowledge");
        guided(&mut l, &app, "Word wrap");
        let text = std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap();
        assert!(text.contains("exe: notepad"), "{text}");
        assert!(text.contains("## Word wrap\nwhen: view, word, wrap\nnames: View > Word wrap\nstatus: confirmed 1, last 2026-10-10, app 11.1\nView > Word wrap.\n"), "{text}");
        assert!(!text.contains("shopping"), "the window title never reaches the file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_path_again_confirms_it_and_a_longer_run_updates_its_own_note() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        for _ in 0..2 {
            struggle(&mut l, &app);
            guided(&mut l, &app, "View");
            guided(&mut l, &app, "Word wrap");
            l.end();
        }
        let g = l.guide("notepad").unwrap();
        assert_eq!(g.notes.len(), 1);
        assert!(matches!(g.notes[0].status, NoteStatus::Confirmed { count: 2, .. }));
        struggle(&mut l, &app);
        for t in ["Edit", "Settings", "Font"] {
            guided(&mut l, &app, t);
        }
        let g = l.guide("notepad").unwrap();
        assert_eq!(g.notes.len(), 2, "the run's note grew instead of a second one appearing");
        assert_eq!(g.notes[1].names, vec!["Edit", "Settings", "Font"]);
        assert_eq!(g.notes[1].title, "Font");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stale_name_missing_from_the_list_withholds_the_note_and_it_leaves_the_prompt() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, STALE);
        assert!(l.prompt_block(&app, "turn on word wrap", "s1").contains("Format > Word wrap"));
        let list: Vec<String> = ["File", "Edit", "View"].map(String::from).to_vec();
        l.step_shown(&app, Some("Format"), false, Some(&list), "2026-10-11");
        let text = std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap();
        assert!(text.contains("status: withheld 2026-10-11, sessions 0, app 11.1, reason \"Format\" not on screen"), "{text}");
        assert_eq!(l.prompt_block(&app, "turn on word wrap", "s1"), "", "withheld: not sent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_contradiction_without_a_fresh_list_a_miss_or_a_reachable_name() {
        let dir = tmp();
        let app = notepad();
        let list: Vec<String> = ["File", "Edit", "View"].map(String::from).to_vec();
        let mut l = with_guide(&dir, STALE);
        l.prompt_block(&app, "wrap", "s1");
        l.step_shown(&app, Some("Format"), false, None, "d"); // no list: a local advance, or no tree
        l.step_shown(&app, Some("Format"), true, Some(&list), "d"); // the locator found it
        l.step_shown(&app, Some("Word wrap"), false, Some(&list), "d"); // second name, first never clicked
        assert!(!l.guide("notepad").unwrap().notes[0].status.is_withheld());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_note_that_was_not_sent_is_never_contradicted() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, STALE);
        let list: Vec<String> = vec!["File".into()];
        l.step_shown(&app, Some("Format"), false, Some(&list), "d");
        assert!(!l.guide("notepad").unwrap().notes[0].status.is_withheld(), "the model got Format elsewhere");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_without_a_pointer_withholds_with_a_pointer_does_not() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, STALE);
        l.prompt_block(&app, "wrap", "s1");
        l.step_shown(&app, Some("Format"), true, None, "d");
        l.wrong(&app, "d");
        assert!(!l.guide("notepad").unwrap().notes[0].status.is_withheld(), "a pointer: maybe the pointer was wrong");
        l.prompt_block(&app, "wrap", "s1");
        l.step_shown(&app, Some("Format"), false, None, "d");
        l.wrong(&app, "d");
        assert!(l.guide("notepad").unwrap().notes[0].status.is_withheld());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_confirmation_restores_and_a_second_contradiction_deletes() {
        let dir = tmp();
        let app = notepad();
        let withheld = STALE.replace("names:", "status: withheld 2026-10-11, sessions 2, reason x\nnames:");
        let mut l = with_guide(&dir, &withheld);
        guided(&mut l, &app, "Format");
        guided(&mut l, &app, "Word wrap");
        let n = &l.guide("notepad").unwrap().notes[0];
        assert!(matches!(n.status, NoteStatus::Confirmed { count: 1, .. }), "{:?}", n.status);
        l.end();

        let mut l = with_guide(&dir, &withheld);
        l.miss(0, 0, &app, "again".into(), "d");
        assert!(l.guide("notepad").unwrap().notes.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_command_found_on_a_new_path_replaces_the_old_one() {
        let dir = tmp();
        let app = notepad();
        let withheld = STALE.replace("names:", "status: withheld 2026-10-11, sessions 0, reason x\nnames:");
        let mut l = with_guide(&dir, &withheld);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        let g = l.guide("notepad").unwrap();
        assert_eq!(g.notes.len(), 1);
        assert_eq!(g.notes[0].names, vec!["View", "Word wrap"]);
        assert!(matches!(g.notes[0].status, NoteStatus::Confirmed { count: 1, .. }));
        assert!(l.prompt_block(&app, "wrap", "s1").contains("write one"), "a moved note asks for its line");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_withheld_note_is_deleted_after_five_sessions_without_confirmation() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, &STALE.replace("names:", "status: withheld 2026-10-11, sessions 0, reason x\nnames:"));
        for s in 1..WITHHELD_SESSIONS {
            l.prompt_block(&app, "x", &format!("s{s}"));
            l.prompt_block(&app, "x", &format!("s{s}")); // the same session counts once
        }
        assert_eq!(l.guide("notepad").unwrap().notes.len(), 1, "four sessions: still kept");
        l.prompt_block(&app, "x", "s5");
        assert!(l.guide("notepad").unwrap().notes.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn content_breaks_a_path_instead_of_entering_the_file() {
        let app = AppKey { title: "Contoso Notes - Q3 planning".into(), name: "Contoso Notes".into(), ..notepad() };
        for (role, name, ok) in [
            ("MenuItem", "Send as PDF", true),
            ("MenuItem", "Save as...", true),
            ("TabItem", "Heading 1", true),
            ("Button", "Notes", true),
            ("ListItem", "Team offsite", false),
            ("TreeItem", "Documents", false),
            ("MenuItem", "Q3 report.docx", false),
            ("MenuItem", "C:\\Users\\me\\notes", false),
            ("MenuItem", "Signed in as someone@example.com", false),
            ("Button", "Invoice 20261010", false),
            ("MenuItem", "Q3 planning", false),
            ("MenuItem", "Move to Q3 planning folder", false),
        ] {
            assert_eq!(is_ui_label(role, name, &app), ok, "{role} {name:?}");
        }
    }

    /// The L2 battery's `guide-local.md`: what this writer produces for Contoso Notes after one
    /// guided session per task (eval/gen-l2.mjs embeds the output). Prints the file.
    /// `cargo test --lib write_contoso_local_guide -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn write_contoso_local_guide() {
        let dir = tmp();
        let app = AppKey {
            exe: "contosonotes".into(),
            name: "Contoso Notes".into(),
            version: "4.0.0.0".into(),
            title: "Contoso Notes - Q3 planning".into(),
        };
        let mut l = Learned::load(dir.clone());
        for path in [
            vec!["File", "Publish", "Send as PDF"],
            vec!["Tools", "Edit", "Preferences", "Saving", "Autosave"],
            vec!["Tools", "View", "Statistics"],
            // Insert > Date and time is where every model looks first: nothing to write.
            vec!["Insert", "Date and time"],
        ] {
            for name in path {
                l.step_shown(&app, Some(name), true, None, "2026-10-10");
                let role = if name == "Saving" { "TabItem" } else if name == "Autosave" { "Button" } else { "MenuItem" };
                // The menu bar's items share a parent; everything else sits in a container of its own.
                let parent = if ["File", "Tools", "Publish", "Edit", "View", "Insert"].contains(&name) { "MenuBar".to_string() } else { format!("in:{name}") };
                l.clicks(&app, &click(role, name, &parent), "2026-10-10");
            }
            l.end();
        }
        println!("{}", std::fs::read_to_string(dir.join("contosonotes").join("guide.md")).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn other_apps_and_keystrokes_end_the_path() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        guided(&mut l, &app, "View");
        l.step_shown(&app, None, false, None, "d"); // "press Ctrl+S"
        guided(&mut l, &app, "Word wrap");
        assert!(l.guide("notepad").is_none(), "View, a keystroke, Word wrap is not a path");
        l.end();
        guided(&mut l, &app, "View");
        let paint = AppKey { exe: "mspaint".into(), ..notepad() };
        l.step_shown(&app, Some("Word wrap"), true, None, "d");
        l.clicks(&paint, &click("MenuItem", "Word wrap", "x"), "d");
        assert!(l.guide("notepad").is_none() && l.guide("mspaint").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_menu_switch_restarts_the_path_and_counts_as_the_failed_try() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        guided_in(&mut l, &app, "File", "MenuBar");
        guided_in(&mut l, &app, "Publish", "MenuBar");
        guided_in(&mut l, &app, "Send as PDF", "menu:Publish");
        let g = l.guide("notepad").unwrap();
        assert_eq!(g.notes.len(), 1);
        assert_eq!(g.notes[0].names, vec!["Publish", "Send as PDF"], "the detour through File is not part of it");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn found_first_time_is_not_written_but_still_confirms() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        assert!(l.guide("notepad").is_none(), "the model knew it");
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nView > Word wrap.\n");
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        assert!(matches!(l.guide("notepad").unwrap().notes[0].status, NoteStatus::Confirmed { count: 1, .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_model_is_asked_for_one_line_and_a_clean_one_is_kept() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        let block = l.prompt_block(&app, "wrap", "s1");
        assert!(block.contains("[Learned notes: write one]") && block.contains("View > Word wrap"), "{block}");
        l.attach_line(&app, "  To wrap long lines; it is in View, not Format.\n", "d");
        let n = &l.guide("notepad").unwrap().notes[0];
        assert_eq!(n.body, "View > Word wrap. To wrap long lines; it is in View, not Format.");
        assert!(n.when.contains(&"long".to_string()) && n.when.contains(&"format".to_string()), "{:?}", n.when);
        assert!(!l.prompt_block(&app, "wrap", "s1").contains("write one"), "asked once it is answered");
        // A longer run keeps the line.
        struggle(&mut l, &app);
        let file = std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap();
        assert!(file.contains("View > Word wrap. To wrap long lines"), "{file}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_that_carries_content_is_refused_and_the_path_stands() {
        let app = notepad(); // "shopping list.txt - Notepad"
        assert!(line_ok("Word wrap is under View.", &app).is_some());
        for bad in [
            "Used for shopping list.txt in View",
            "Wrap the shopping list via View",
            "Opened the 2026 budget",
            "Ask someone@example.com",
            "See https://example.com",
            &"x".repeat(MAX_LINE_CHARS + 1),
        ] {
            assert!(line_ok(bad, &app).is_none(), "{bad}");
        }
        let dir = tmp();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        l.attach_line(&app, "Wrap the shopping list via View", "d");
        assert_eq!(l.guide("notepad").unwrap().notes[0].body, "View > Word wrap.");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_line_is_asked_for_twice_then_the_path_stands_alone() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        assert!(l.prompt_block(&app, "x", "s1").contains("write one"));
        assert!(l.prompt_block(&app, "x", "s1").contains("write one"));
        assert!(!l.prompt_block(&app, "x", "s1").contains("write one"));
        l.attach_line(&app, "too late", "d");
        assert_eq!(l.guide("notepad").unwrap().notes[0].body, "View > Word wrap.");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_longer_run_keeps_the_models_line() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "Edit");
        guided(&mut l, &app, "Settings");
        l.prompt_block(&app, "x", "s1");
        l.attach_line(&app, "Settings live under Edit here.", "d");
        guided(&mut l, &app, "Font");
        let n = &l.guide("notepad").unwrap().notes[0];
        assert_eq!(n.body, "Edit > Settings > Font. Settings live under Edit here.");
        assert_eq!(n.title, "Font");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stale_next_step_is_caught_even_when_the_model_reads_the_screen() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Wrap long lines\nnames: View > Wrap long lines\nView > Wrap long lines.\n");
        l.prompt_block(&app, "turn on word wrap", "s1");
        guided(&mut l, &app, "View");
        let menu: Vec<String> = ["File", "Edit", "View", "Zoom", "Status bar", "Word wrap"].map(String::from).to_vec();
        // The menu did not open: the model asks for View again -- no verdict.
        l.step_shown(&app, Some("View"), true, Some(&menu), "d");
        assert!(!l.guide("notepad").unwrap().notes[0].status.is_withheld());
        // The menu is open and the model points at the real item: the note is contradicted.
        l.prompt_block(&app, "turn on word wrap", "s1");
        guided(&mut l, &app, "View");
        l.step_shown(&app, Some("Word wrap"), true, Some(&menu), "d");
        let n = &l.guide("notepad").unwrap().notes[0];
        assert!(n.status.is_withheld(), "{:?}", n.status);
        let file = std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap();
        assert!(file.contains("reason \"Wrap long lines\" not on screen after \"View\""), "{file}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Rewrite a guide file by hand, as a person would while the app runs. The stamp is the
    /// modified time and the size, so the edit also changes the length.
    fn hand_edit(dir: &PathBuf, text: &str) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("notepad").join("guide.md"), text).unwrap();
    }

    #[test]
    fn a_hand_edit_is_read_back_and_never_written_over() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap.\n");
        l.prompt_block(&app, "x", "s1");
        hand_edit(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap.\n\n## Font\nnames: Edit > Font\nEdit > Font, written by hand.\n");
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        let file = std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap();
        assert!(file.contains("Edit > Font, written by hand."), "the hand edit survived the app's write:\n{file}");
        assert!(file.contains("status: confirmed 1"), "and the confirmation landed on the re-read file:\n{file}");
        assert!(l.prompt_block(&app, "x", "s1").contains("written by hand"), "and the edit is what is sent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_no_longer_parses_is_neither_sent_nor_written() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap.\n");
        let broken = "---\napp: Notepad\n(half an edit, no exe line and no closing marker\n";
        hand_edit(&dir, broken);
        assert_eq!(l.prompt_block(&app, "x", "s1"), "", "not sent");
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        assert_eq!(std::fs::read_to_string(dir.join("notepad").join("guide.md")).unwrap(), broken, "not written over");
        // Fixed by a later edit: in use again.
        hand_edit(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap, fixed.\n");
        assert!(l.prompt_block(&app, "x", "s1").contains("fixed"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_deleted_file_empties_the_guide_and_a_new_one_is_picked_up() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap.\n");
        std::fs::remove_file(dir.join("notepad").join("guide.md")).unwrap();
        assert_eq!(l.prompt_block(&app, "x", "s1"), "", "deleted by hand: nothing sent");
        let paint = AppKey { exe: "mspaint".into(), name: "Paint".into(), title: "Untitled - Paint".into(), ..notepad() };
        std::fs::create_dir_all(dir.join("mspaint")).unwrap();
        std::fs::write(dir.join("mspaint").join("guide.md"), "---\napp: Paint\nexe: mspaint\n---\n## Resize\nnames: Image > Resize\nImage > Resize.\n").unwrap();
        assert!(l.prompt_block(&paint, "resize", "s1").contains("Image > Resize."), "a file that appeared after startup");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_note_found_in_a_task_that_still_struggled_asks_for_its_line_again() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap. To wrap long lines.\n");
        l.prompt_block(&app, "x", "s1");
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        let block = l.prompt_block(&app, "turn on word wrap", "s1");
        assert!(block.contains("[Learned notes: rewrite one]") && block.contains("reads: \"To wrap long lines.\""), "{block}");
        l.attach_line(&app, "To wrap long lines; only after the file is saved.", "d");
        assert_eq!(l.guide("notepad").unwrap().notes[0].body, "View > Word wrap. To wrap long lines; only after the file is saved.");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_in_another_language_than_the_request_is_refused() {
        assert!(same_language("To save a note as a PDF, use Publish > Send as PDF.", "Save this note as a PDF."));
        assert!(!same_language("Pour enregistrer une note en PDF, utilisez Publier > Send as PDF.", "Save this note as a PDF."));
        assert!(!same_language("Dans Contoso Notes, utilisez Publish pour une note avec PDF", "Save this note as a PDF."), "unaccented French");
        assert!(same_language("Click \u{201c}Enregistrer\u{201d} in the toolbar.", "Save it."), "a quoted label is not drift");
        assert!(same_language("要导出 PDF：Publish > Send as PDF。", "把笔记存成 PDF"), "a Chinese request is not judged here");
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        l.prompt_block(&app, "turn on word wrap", "s1");
        l.attach_line(&app, "Pour couper les lignes : Affichage > Retour à la ligne.", "d");
        assert_eq!(l.guide("notepad").unwrap().notes[0].body, "View > Word wrap.");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_lists_opens_and_deletes_a_guide() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, "---\napp: Notepad\nexe: notepad\n---\n## Word wrap\nnames: View > Word wrap\nView > Word wrap.\n\n## Font\nstatus: withheld 2026-10-11, sessions 0, reason x\nnames: Format > Font\nFormat > Font.\n");
        let list = l.list();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].app.as_str(), list[0].exe.as_str(), list[0].notes, list[0].withheld), ("Notepad", "notepad", 2, 1));
        assert_eq!(l.path_of("notepad").unwrap(), dir.join("notepad").join("guide.md"));
        l.prompt_block(&app, "x", "s1");
        l.delete("notepad").unwrap();
        assert!(!dir.join("notepad").exists(), "the file and its emptied folder are gone");
        assert!(l.list().is_empty());
        assert_eq!(l.prompt_block(&app, "x", "s1"), "", "and nothing is sent from memory");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_users_own_way_after_a_missing_target_joins_the_path() {
        let dir = tmp();
        let app = notepad();
        let mut l = with_guide(&dir, STALE); // Word wrap: Format > Word wrap
        l.prompt_block(&app, "turn on word wrap", "s1");
        let bar: Vec<String> = ["File", "Edit", "View"].map(String::from).to_vec();
        // The model follows the stale note; Format is nowhere: the note is withheld.
        l.step_shown(&app, Some("Format"), false, Some(&bar), "d");
        assert!(l.guide("notepad").unwrap().notes[0].status.is_withheld());
        // The user opens View themselves -- nobody's step -- and is held as a candidate.
        l.clicks(&app, &click("MenuItem", "View", "MenuBar"), "d");
        // The model continues from it; the user clicks what it named.
        let menu: Vec<String> = ["File", "Edit", "View", "Zoom", "Status bar", "Word wrap"].map(String::from).to_vec();
        l.step_shown(&app, Some("Word wrap"), true, Some(&menu), "d");
        l.clicks(&app, &click("MenuItem", "Word wrap", "menu:View"), "d");
        let n = &l.guide("notepad").unwrap().notes[0];
        assert_eq!(n.names, vec!["View", "Word wrap"], "the note moved to the user's way");
        assert!(matches!(n.status, NoteStatus::Confirmed { count: 1, .. }), "and is in use again: {:?}", n.status);
        assert!(l.prompt_block(&app, "x", "s1").contains("[Learned notes: write one]"), "and asks for its line");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_detour_from_a_step_that_was_on_screen_is_not_learned() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        let bar: Vec<String> = ["File", "Edit", "View"].map(String::from).to_vec();
        // The step named a control that was right there; the user clicked something else.
        l.step_shown(&app, Some("Edit"), true, Some(&bar), "d");
        l.clicks(&app, &click("MenuItem", "View", "MenuBar"), "d");
        guided_in(&mut l, &app, "Word wrap", "menu:View");
        assert!(l.guide("notepad").is_none(), "the user's detour from a visible step is not the way");
        // Nor does a candidate survive a step that names nothing.
        l.step_shown(&app, Some("Format"), false, Some(&bar), "d");
        l.clicks(&app, &click("MenuItem", "View", "MenuBar"), "d");
        l.step_shown(&app, None, false, None, "d");
        guided_in(&mut l, &app, "Word wrap", "menu:View");
        assert!(l.guide("notepad").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn switched_off_nothing_is_written_or_sent() {
        let dir = tmp();
        let app = notepad();
        let mut l = Learned::disabled(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        assert_eq!(l.prompt_block(&app, "turn on word wrap", "s1"), "");
        assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(), "no guide file");
        assert!(l.list().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_that_only_repeats_the_path_is_refused() {
        let note = GuideNote {
            title: "Word wrap".into(),
            when: Vec::new(),
            names: vec!["View".into(), "Word wrap".into()],
            status: NoteStatus::Unconfirmed,
            body: "View > Word wrap.".into(),
        };
        assert!(!adds_something("Word wrap: View > Word wrap.", &note, "Notepad"), "flash, live");
        assert!(!adds_something("In Notepad, use View > Word wrap.", &note, "Notepad"), "the app's name is not news");
        assert!(adds_something("Word wrap may also be called line wrapping; turn it on through View > Word wrap.", &note, "Notepad"), "terra, live");
        assert!(adds_something("Save the file first, then View > Word wrap.", &note, "Notepad"), "a precondition is news");

        let dir = tmp();
        let app = notepad();
        let mut l = Learned::load(dir.clone());
        struggle(&mut l, &app);
        guided(&mut l, &app, "View");
        guided(&mut l, &app, "Word wrap");
        l.prompt_block(&app, "turn on word wrap", "s1");
        l.attach_line(&app, "Word wrap: View > Word wrap.", "d");
        assert_eq!(l.guide("notepad").unwrap().notes[0].body, "View > Word wrap.", "the path stands alone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn names_match_through_accelerators_ellipses_and_shortcut_text() {
        assert!(names_match("Send as PDF...", "send as pdf"));
        assert!(names_match("&File", "File"));
        assert!(names_match("Word wrap", "Word wrap\tAlt+Z"));
        assert!(names_match("the File menu", "File"));
        assert!(names_match("Menu", "menu"), "a name that is only a role word stays itself");
        assert!(!names_match("View", "Review"));
        assert!(!names_match("Edit", "Edit Points"), "different commands");
        assert_eq!(first_segment("“Publish > Send as PDF”"), "Publish");
        assert_eq!(title_for(&["Edit".into(), "Preferences".into(), "Autosave".into(), "OK".into()]), "Autosave");
        assert_eq!(title_for(&["File".into(), "Save".into()]), "Save", "a two-name path keeps its command");
    }
}
