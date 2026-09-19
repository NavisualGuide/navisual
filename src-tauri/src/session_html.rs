//! One stored session as a self-contained HTML file — plan §6.
//!
//! Readable in any browser with no Navisual and no network: the task, the plan, the
//! conversation, what the user clicked, and — when they were kept — the frames with their
//! pointers composited in. It also carries the exact session JSON in a
//! `<script type="application/json">` block, so one artifact satisfies both "readable
//! without the app" and "loadable back into it". A Markdown file only satisfies the first,
//! a JSON file only the second.
//!
//! **Deliberately not `session_export`.** That one writes an annotated folder from the
//! in-memory frame ring, for the session still in progress. This writes one file per stored
//! session, read from disk, for sessions that ended days ago — and half of them have no
//! frames at all. Sharing the word "export" between the two would be one word doing two
//! jobs, which is the mistake the capture mask and the picker rule already made once.

use anyhow::{Context, Result};
use std::path::Path;

use crate::ai::session::{safe_frame_name, Session, StoredMark, Turn, SESSION_HISTORY_KEEP};

/// The id the JSON block carries. Read back by anything that wants to load the artifact
/// into the app (the id rather than the tag name, so the block can be found without
/// depending on the element's position in the document).
pub const JSON_BLOCK_ID: &str = "navisual-session";

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The session JSON, made safe to sit inside a `<script>` element.
///
/// A conversation can contain `</script>` — a user asking about HTML, or an instruction
/// quoting a page — and an unescaped one ends the block there, silently truncating the
/// session. `<` is the JSON escape for `<`, so this is invisible to any parser and the
/// file still round-trips exactly.
fn json_block(session: &Session) -> String {
    serde_json::to_string(session)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c")
}

/// `2026-09-17-optimize-for-3d-gaming.html`, or the id alone when the task has no ASCII in
/// it — a Chinese task slugifies to nothing, and the filename still has to be one.
pub fn file_name_for(session: &Session) -> String {
    let date: String = session.last_active_at.chars().take(10).collect();
    let short: String = session.id.to_string().chars().take(8).collect();
    let slug = crate::session_export::slugify(&session.task_description);
    if slug.is_empty() {
        format!("{date}-{short}.html")
    } else {
        format!("{date}-{slug}.html")
    }
}

/// What a stored turn meant, in the artifact's own words.
///
/// The panel phrases the same two fields for a live UI ("✓ You clicked Button X",
/// "✓ Autopilot advanced"). This is a document, so it states the facts plainly rather than
/// borrowing the app's voice — the phrasing is deliberately not shared, but the facts are
/// the same two fields, and neither is inferred.
fn completion_line(turn: &Turn) -> String {
    match (&turn.clicked, turn.advanced_by.as_deref()) {
        (Some(clicked), _) => format!("Completed — clicked {clicked}"),
        (None, Some("autopilot")) => "Completed — Autopilot advanced".to_string(),
        (None, Some("already_done")) => "Completed — already done".to_string(),
        (None, Some("next")) => "Completed — Next pressed".to_string(),
        // A kind this build does not know (a newer version wrote it) still says the one
        // thing that is certainly true.
        (None, Some(_)) | (None, None) => "Completed".to_string(),
    }
}

fn is_completion(turn: &Turn) -> bool {
    turn.role == "user"
        && turn.content.starts_with("[User completed: \"")
        && turn.content.ends_with("\"]")
}

/// One frame, pointer composited in, as a data URI — or `None` when it cannot be read.
///
/// The pointer is drawn here exactly as the panel draws it (`compose_stored_frame`, one
/// implementation for both), so the artifact and the app cannot disagree about where a step
/// was pointed. A missing file is skipped rather than failing the export: the transcript is
/// the artifact, the pictures are a bonus.
fn frame_data_uri(png_path: &Path, mark: Option<&StoredMark>, thickness: u32) -> Option<String> {
    let bytes = std::fs::read(png_path).ok()?;
    let img = crate::compose_stored_frame(&bytes, mark, thickness).ok()?;
    let mut out = Vec::new();
    {
        use image::codecs::jpeg::JpegEncoder;
        let mut enc = JpegEncoder::new_with_quality(&mut out, 80);
        enc.encode_image(&img).ok()?;
    }
    Some(format!(
        "data:image/jpeg;base64,{}",
        crate::capture::to_base64(&out)
    ))
}

/// The whole artifact for one session.
pub fn session_to_html(session: &Session, frames_dir: &Path, thickness: u32) -> String {
    let mut body = String::new();
    // A frame belongs to the user turn that was captured, but reads as the picture the NEXT
    // instruction was given from — so it is held here and flushed after that instruction.
    let mut pending: Option<String> = None;

    for turn in &session.conversation {
        // A frame belongs to the user turn just before this one. Normally its assistant
        // follows and flushes it into place below -- the app always writes the pair together,
        // and a failed request writes neither, so a session the app wrote can never put two
        // user turns back to back. The flush is for files that came from elsewhere: an
        // imported or hand-edited conversation can, and without it the second turn's
        // picture would silently overwrite the first one's.
        if pending.is_some() && turn.role != "assistant" {
            body.push_str(&pending.take().unwrap());
        }
        let figure = |name: &str, caption: &str| -> String {
            match frame_data_uri(&frames_dir.join(name), turn.mark.as_ref(), thickness) {
                // `data-frame` carries the stored name so an import can put each picture back
                // on the turn it came from. Position would do until one picture went missing
                // or a file was edited by hand, and then it would silently mislabel the rest.
                Some(uri) => format!(
                    "<figure data-frame=\"{name}\" data-ext=\"jpeg\">\
                     <img src=\"{uri}\" alt=\"Screen at this step\">\
                     <figcaption>{}</figcaption></figure>",
                    esc(caption)
                ),
                None => String::new(),
            }
        };

        if is_completion(turn) {
            body.push_str(&format!(
                "<p class=\"done\">✓ {}</p>\n",
                esc(&completion_line(turn))
            ));
        } else if turn.role == "user" {
            body.push_str(&format!("<p class=\"you\">{}</p>\n", esc(&turn.content)));
        } else if turn.role == "assistant" {
            body.push_str(&format!("<div class=\"ai\">{}</div>\n", esc(&turn.content)));
        }
        // Anything else (a correction, a system note) is part of the record too.
        else {
            body.push_str(&format!(
                "<p class=\"note\">{} · {}</p>\n",
                esc(&turn.role),
                esc(&turn.content)
            ));
        }

        if let Some(name) = turn.frame.as_deref() {
            let caption = session
                .conversation
                .iter()
                .skip_while(|t| !std::ptr::eq(*t, turn))
                .nth(1)
                .filter(|t| t.role == "assistant")
                .map(|t| t.content.clone())
                .unwrap_or_default();
            let html = figure(name, &caption);
            if turn.role == "user" {
                pending = Some(html);
            } else {
                body.push_str(&html);
            }
        }
        // The picture the next instruction was given from.
        if turn.role == "assistant" {
            if let Some(html) = pending.take() {
                body.push_str(&html);
            }
        }
    }
    if let Some(html) = pending {
        body.push_str(&html);
    }

    let plan = if session.plan_outline.is_empty() {
        String::new()
    } else {
        let items: String = session
            .plan_outline
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let mark = if i < session.plan_completed_count { "✓ " } else { "" };
                format!("<li>{}{}</li>", mark, esc(step))
            })
            .collect();
        format!("<section class=\"plan\"><h2>Plan</h2><ol>{items}</ol></section>")
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
 :root {{ color-scheme: light dark; }}
 body {{ margin: 0 auto; padding: 2rem 1rem 4rem; max-width: 46rem;
        font: 16px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif;
        background: #fff; color: #1a1a1a; }}
 h1 {{ font-size: 1.4rem; margin: 0 0 .25rem; }}
 h2 {{ font-size: 1rem; margin: 2rem 0 .5rem; color: #555; }}
 .meta {{ color: #666; font-size: .85rem; margin: 0 0 1.5rem; }}
 .you {{ background: #ff6b35; color: #18110d; padding: .5rem .75rem; border-radius: 16px 16px 4px 16px;
        margin: 1rem 0 1rem auto; max-width: 80%; width: fit-content; }}
 .ai {{ margin: 1rem 0; white-space: pre-wrap; }}
 .done {{ color: #3a7d44; font-weight: 600; margin: 1rem 0; }}
 .note {{ color: #777; font-size: .85rem; font-style: italic; margin: .75rem 0; }}
 figure {{ margin: 1rem 0; }}
 img {{ max-width: 100%; border-radius: 8px; border: 1px solid #ddd; }}
 figcaption {{ color: #666; font-size: .8rem; margin-top: .35rem; }}
 footer {{ margin-top: 3rem; color: #888; font-size: .8rem; border-top: 1px solid #eee; padding-top: 1rem; }}
 @media (prefers-color-scheme: dark) {{
   body {{ background: #17181a; color: #e8e8e8; }}
   h2, .meta, figcaption, .note {{ color: #9aa0a6; }}
   img {{ border-color: #333; }}
   footer {{ color: #9aa0a6; border-color: #2a2c2f; }}
 }}
</style>
<h1>{title}</h1>
<p class="meta">{meta}</p>
{plan}
<main>
{body}</main>
<footer>Exported from Navisual on {exported}. The conversation is complete; the pictures were kept only if screenshots were switched on while the session ran.</footer>
<script type="application/json" id="{block_id}">{json}</script>
</html>
"#,
        title = esc(&session.task_description),
        meta = esc(&format!(
            "Started {} · last active {} · {} turns",
            session.started_at, session.last_active_at, session.conversation.len()
        )),
        plan = plan,
        body = body,
        exported = esc(&chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()),
        block_id = JSON_BLOCK_ID,
        json = json_block(session),
    )
}

/// Write every kept session plus an index into `dest`. Returns how many sessions were written.
///
/// Takes the sessions directory rather than the manager so the caller can release the router
/// lock before any of this touches the disk: writing a megabyte of HTML per session while
/// holding the lock the guidance loop needs would stall it for no reason.
pub fn write_all(
    session_dir: &Path,
    sessions: &[Session],
    dest: &Path,
    thickness: u32,
) -> Result<usize> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;

    let mut written: Vec<(String, &Session)> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    for session in sessions {
        // Two sessions about the same thing on the same day slugify to the same name, and
        // the second would silently overwrite the first -- which is exactly what a round trip
        // over this machine's own sessions caught: twenty written, nineteen files.
        let mut name = file_name_for(session);
        if used.contains(&name) {
            let stem = name.trim_end_matches(".html").to_string();
            let mut n = 2;
            while used.contains(&format!("{stem}-{n}.html")) {
                n += 1;
            }
            name = format!("{stem}-{n}.html");
        }
        used.push(name.clone());
        let frames = crate::ai::session::SessionManager::frames_dir_in(session_dir, &session.id.to_string());
        let html = session_to_html(session, &frames, thickness);
        std::fs::write(dest.join(&name), html)
            .with_context(|| format!("writing {}", dest.join(&name).display()))?;
        written.push((name, session));
    }

    let rows: String = written
        .iter()
        .map(|(name, s)| {
            format!(
                "<li><a href=\"{}\">{}</a><span class=\"meta\"> · {} turns · {}</span></li>",
                esc(name),
                esc(&s.task_description),
                s.conversation.len(),
                esc(&s.last_active_at.chars().take(10).collect::<String>())
            )
        })
        .collect();
    let index = format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>Navisual sessions</title>
<style>
 body {{ margin: 2rem auto; max-width: 46rem; padding: 0 1rem;
        font: 16px/1.7 system-ui, -apple-system, "Segoe UI", sans-serif; }}
 li {{ margin: .5rem 0; }}
 .meta {{ color: #666; font-size: .85rem; }}
</style>
<h1>Navisual sessions</h1>
<p class="meta">The most recent {keep} are kept in the app; each file here is one of them, complete on its own.</p>
<ul>{rows}</ul>
</html>
"#,
        keep = SESSION_HISTORY_KEEP,
        rows = rows,
    );
    std::fs::write(dest.join("index.html"), index)?;

    Ok(written.len())
}

/// The session out of an exported artifact, or `None` when the file is not one.
///
/// Split from reading the file so the format can be checked against a string -- the importer
/// reads each file once and hands the text straight in.
pub fn parse_artifact(html: &str) -> Option<Session> {
    let marker = format!("id=\"{JSON_BLOCK_ID}\">");
    let start = html.find(&marker)? + marker.len();
    let end = html[start..].find("</script>")? + start;
    serde_json::from_str(&html[start..end]).ok()
}

/// Every picture inside an artifact, as `(name, extension, bytes)`.
///
/// A scan rather than a parser: the exporter writes one shape of figure, and the name it
/// records in `data-frame` is what makes the round trip exact instead of positional.
fn embedded_frames(html: &str) -> Vec<(String, String, Vec<u8>)> {
    let mut out = Vec::new();
    for chunk in html.split("<figure data-frame=\"").skip(1) {
        let Some(name_end) = chunk.find('"') else { continue };
        let name = chunk[..name_end].to_string();
        let ext = chunk
            .find("data-ext=\"")
            .and_then(|i| chunk.get(i + "data-ext=\"".len()..))
            .and_then(|s| s.find('"').map(|j| s[..j].to_string()))
            .unwrap_or_else(|| "jpeg".to_string());
        if !safe_frame_name(&name) {
            log::warn!("[sessions] import: refusing a picture named {name:?}");
            continue;
        }
        let Some(b64_start) = chunk.find(";base64,") else { continue };
        let after = &chunk[b64_start + ";base64,".len()..];
        let Some(b64_end) = after.find('"') else { continue };
        if let Some(bytes) = crate::capture::from_base64(&after[..b64_end]) {
            out.push((name, ext, bytes));
        }
    }
    out
}

/// A session read out of a file, ready for the panel — and deliberately NOT written
/// anywhere.
///
/// **The file is the archive; the store is the recent working set.** Importing used to mean
/// writing into a store that keeps twenty sessions, so restoring an old one raced the very
/// prune it was meant to escape — the 2026-09-17 log has three sessions imported and pruned
/// seven minutes later. Opening the file instead removes the conflict rather than arbitrating
/// it: the session appears in the panel, and it joins the store only if the user carries on
/// working in it, which is the same act that keeps any other session alive.
///
/// Nothing reaches disk here, which is why this carries the pictures inline.
pub struct OpenedArtifact {
    pub session: Session,
    /// Base64 JPEG per conversation index, in the encoding `session_frame` returns for a
    /// stored frame, so the panel renders both the same way.
    pub pictures: Vec<(usize, String)>,
}

/// Read one artifact for the panel. `None` means the file was not a session.
///
/// Two things are deliberately stripped from the session on the way through:
///
/// * **`frame`**, because the name refers to a file in the store this session is not in.
///   Carrying it would leave a dangling reference the moment the session is saved, and the
///   pictures travel beside it instead.
/// * **`mark`**, because an artifact's pictures already have the pointer drawn into them —
///   that is what makes them readable without the app — so re-drawing would put a second
///   pointer on the first.
pub fn open_artifact(html: &str) -> Option<OpenedArtifact> {
    let mut session = parse_artifact(html)?;

    // The id becomes the live session's id, and once the user works in it, it becomes a file
    // name and a frames directory. A hand-edited artifact could carry anything there, so an
    // id that is not uuid-shaped is not trusted and the session arrives as a new one.
    let id = session.id.to_string();
    if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        session.id = uuid::Uuid::new_v4();
    }

    let embedded = embedded_frames(html);
    let mut pictures = Vec::new();
    for (index, turn) in session.conversation.iter_mut().enumerate() {
        let named = turn.frame.take();
        turn.mark = None;
        let Some(name) = named else { continue };
        if let Some((_, _, bytes)) = embedded.iter().find(|(n, _, _)| *n == name) {
            pictures.push((index, crate::capture::to_base64(bytes)));
        }
    }

    log::info!(
        "[sessions] opened \"{}\" from a file ({} turns, {} picture(s)) -- not stored",
        session.task_description,
        session.conversation.len(),
        pictures.len()
    );
    Some(OpenedArtifact { session, pictures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::session::Session;

    /// The name travels inside the file and then into a path join, so the shapes that
    /// escape the store are refused rather than sanitised into something plausible.
    #[test]
    fn a_frame_name_that_leaves_the_store_is_refused() {
        assert!(safe_frame_name("0.png"));
        assert!(safe_frame_name("12.jpg"));
        assert!(!safe_frame_name("../../../../evil.bat"));
        assert!(!safe_frame_name("..\\..\\evil.bat"));
        assert!(!safe_frame_name("sub/dir.png"));
        assert!(!safe_frame_name("C:/windows/system32/x.dll"));
        assert!(!safe_frame_name(""));
        assert!(!safe_frame_name(&"a".repeat(65)));
    }

    /// Opening a file writes nothing, so a hostile picture name has nowhere to land here.
    /// The rule is still enforced at the write itself — see
    /// `ai::session::tests::a_frame_name_cannot_escape_its_directory`, which owns that case
    /// now that the only writer is the app's own capture.
    #[test]
    fn a_hostile_picture_name_opens_without_touching_the_disk() {
        let (manager, dir) = store("hostile-name");
        let mut original = Session::new("A task".to_string());
        original.add_turn_pinned("user", "help".to_string(), None, false);
        original.set_last_user_turn_facts(None, None, Some("0.png".to_string()));
        let blank = image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 255, 255, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(blank)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("test frame encodes");
        manager.save_frame(&original.id.to_string(), 0, &png).unwrap();
        let html = session_to_html(&original, &manager.frames_dir(&original.id.to_string()), 4);

        // Repoint the picture at something outside the store, as a hand-edited artifact would.
        let hostile = html.replace(
            "<figure data-frame=\"0.png\"",
            "<figure data-frame=\"../../escaped.png\"",
        );
        assert!(hostile.contains("escaped.png"), "the test edited the artifact");

        let opened = open_artifact(&hostile).expect("a session");
        assert!(opened.pictures.is_empty(), "a name like that carries no picture");
        assert!(!dir.join("escaped.jpg").exists() && !dir.join("escaped.png").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn session_with(conversation: Vec<Turn>) -> Session {
        let mut s = Session::new("Test task".to_string());
        s.conversation = conversation;
        s
    }

    fn turn(role: &str, content: &str) -> Turn {
        Turn {
            role: role.to_string(),
            content: content.to_string(),
            screenshot_hash: None,
            timestamp: "2026-09-17T00:00:00-07:00".to_string(),
            pinned: false,
            clicked: None,
            advanced_by: None,
            frame: None,
            mark: None,
        }
    }

    /// The block has to survive a conversation that talks about HTML — an unescaped
    /// `</script>` ends the element early and silently truncates the session.
    #[test]
    fn a_conversation_containing_a_script_tag_still_round_trips() {
        let mut closing = turn("user", "how do I close a </script> tag?");
        closing.clicked = Some("Button \"Help\"".to_string());
        let session = session_with(vec![closing, turn("assistant", "Put it in <code>.")]);

        let html = session_to_html(&session, Path::new("nowhere"), 4);
        assert!(
            !html.contains("</script> tag"),
            "the raw tag must not appear inside the JSON block"
        );

        // And the block still parses back to the same session.
        let start = html.find(&format!("id=\"{JSON_BLOCK_ID}\">")).expect("block present")
            + format!("id=\"{JSON_BLOCK_ID}\">").len();
        let end = html[start..].find("</script>").expect("block closed") + start;
        let parsed: Session =
            serde_json::from_str(&html[start..end]).expect("the block is valid JSON");
        assert_eq!(parsed.id, session.id);
        assert_eq!(parsed.conversation[0].content, "how do I close a </script> tag?");
        assert_eq!(parsed.conversation[0].clicked.as_deref(), Some("Button \"Help\""));
    }

    #[test]
    fn the_transcript_reads_without_the_app() {
        let mut completed = turn("user", "[User completed: \"Click Save\"]");
        completed.clicked = Some("Button \"Save\"".to_string());
        let session = session_with(vec![
            turn("user", "help me save"),
            turn("assistant", "Click Save"),
            completed,
        ]);
        let html = session_to_html(&session, Path::new("nowhere"), 4);

        assert!(html.contains("help me save"));
        assert!(html.contains("Click Save"));
        assert!(
            html.contains("Completed — clicked Button &quot;Save&quot;"),
            "the completion states the fact, escaped"
        );
        assert!(html.contains("Test task"), "the task is the title");
    }

    #[test]
    fn a_session_with_no_frames_exports_without_pictures() {
        let session = session_with(vec![turn("user", "hello")]);
        let html = session_to_html(&session, Path::new("nowhere"), 4);
        assert!(!html.contains("data:image/"), "nothing to embed");
        assert!(html.contains("id=\"navisual-session\""), "the record still travels");
    }

    /// The whole artifact, written the way the command writes it: one file per session, an
    /// index beside them, and the frame inside the file rather than beside it — which is
    /// what makes the export readable on a machine that has neither Navisual nor the
    /// session directory.
    #[test]
    fn an_export_writes_one_file_per_session_with_its_picture_inside() {
        use crate::ai::session::SessionManager;

        let dir = std::env::temp_dir().join(format!("navisual-html-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::new(dir.join("sessions"));
        let mut session = Session::new("Optimize for 3D gaming".to_string());
        session.add_turn_pinned("user", "help me".to_string(), None, false);
        session.add_turn("assistant", "Click Save".to_string(), None);
        session.set_last_user_turn_facts(None, None, Some("0.png".to_string()));
        session.set_last_user_turn_mark(Some(StoredMark {
            pointer: crate::session_export::PointerState::Hit { rect: [4, 4, 8, 8] },
            mark_scale: 1.0,
        }));
        manager.save_session(Some(&session));

        let blank = image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 255, 255, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(blank)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("test frame encodes");
        manager
            .save_frame(&session.id.to_string(), 0, &png)
            .expect("frame stored");

        let dest = dir.join("out");
        let count = write_all(&manager.session_dir, &[session.clone()], &dest, 4).expect("export");
        assert_eq!(count, 1);

        let name = file_name_for(&session);
        let html = std::fs::read_to_string(dest.join(&name)).expect("the session file");
        assert!(
            html.contains("data:image/jpeg;base64,"),
            "the picture travels inside the file, not beside it"
        );
        assert!(html.contains("Click Save"), "and so does the conversation");

        let index = std::fs::read_to_string(dest.join("index.html")).expect("the index");
        assert!(index.contains(&name), "the index links every written file");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Visual inspection against the real thing: writes the machine's own sessions out so
    /// the artifact can be opened in a browser. `#[ignore]`d because it depends on what
    /// happens to be on disk — the same shape as the exporter's own visual checks.
    ///
    ///     cargo test --lib session_html::tests::export_real_sessions_for_inspection -- --ignored
    #[test]
    #[ignore = "visual check against the real sessions directory"]
    fn export_real_sessions_for_inspection() {
        use crate::ai::session::SessionManager;
        let base = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA");
        let sessions = std::path::PathBuf::from(base).join("com.navisual.app").join("sessions");
        let manager = SessionManager::new(sessions.clone());
        let all = manager.all_sessions();
        let dest = std::path::PathBuf::from(std::env::var("TEMP").unwrap_or_default())
            .join("navisual-session-export-check");
        let _ = std::fs::remove_dir_all(&dest);

        let count = write_all(&sessions, &all, &dest, 4).expect("export");
        println!("wrote {count} session(s) to {}", dest.display());
        for entry in std::fs::read_dir(&dest).into_iter().flatten().flatten() {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            println!("  {:>8} bytes  {}", len, entry.file_name().to_string_lossy());
        }
    }

    // ── Opening a file ─────────────────────────────────────

    fn store(tag: &str) -> (crate::ai::session::SessionManager, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("navisual-open-{tag}-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        (crate::ai::session::SessionManager::new(dir.clone()), dir)
    }

    #[test]
    fn a_session_survives_the_round_trip_through_a_file() {
        let mut original = Session::new("Optimize for 3D gaming".to_string());
        original.add_turn_pinned("user", "help me".to_string(), None, false);
        original.add_turn("assistant", "Click Save".to_string(), None);
        original.plan_outline = vec!["Open settings".to_string(), "Save".to_string()];
        original.plan_completed_count = 1;

        let html = session_to_html(&original, Path::new("nowhere"), 4);
        let opened = open_artifact(&html).expect("it is a session file");

        let back = opened.session;
        assert_eq!(back.id, original.id, "the same session, not a lookalike");
        assert_eq!(back.conversation.len(), original.conversation.len());
        assert_eq!(back.task_description, original.task_description);
        assert_eq!(back.plan_outline, original.plan_outline);
        assert_eq!(back.plan_completed_count, 1);
        assert!(opened.pictures.is_empty(), "this one had none");
    }

    /// Opening the same file twice is simply opening it twice. The whole no-op / replace /
    /// duplicate question this used to test disappeared with the store write: there is no
    /// second copy to make, because there is no copy.
    #[test]
    fn opening_the_same_file_twice_writes_nothing_either_time() {
        let (manager, dir) = store("twice");
        let original = Session::new("A task".to_string());
        let html = session_to_html(&original, Path::new("nowhere"), 4);

        let first = open_artifact(&html).expect("a session");
        let second = open_artifact(&html).expect("a session");
        assert_eq!(first.session.id, second.session.id, "same file, same session");
        assert_eq!(
            manager.all_sessions().len(),
            0,
            "opening a file must not put anything in the store"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_picture_comes_back_inline_with_no_second_pointer_and_no_dangling_name() {
        use crate::ai::session::StoredMark;

        let (manager, dir) = store("picture");
        let mut original = Session::new("With a picture".to_string());
        original.add_turn_pinned("user", "help".to_string(), None, false);
        original.add_turn("assistant", "Click Save".to_string(), None);
        original.set_last_user_turn_facts(None, None, Some("0.png".to_string()));
        original.set_last_user_turn_mark(Some(StoredMark {
            pointer: crate::session_export::PointerState::Hit { rect: [4, 4, 8, 8] },
            mark_scale: 1.0,
        }));
        manager.save_session(Some(&original));
        let blank = image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 255, 255, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(blank)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        manager.save_frame(&original.id.to_string(), 0, &png).unwrap();

        let html = session_to_html(&original, &manager.frames_dir(&original.id.to_string()), 4);
        let opened = open_artifact(&html).expect("a session");

        assert_eq!(opened.pictures.len(), 1, "the picture came out of the file");
        let (index, b64) = &opened.pictures[0];
        assert_eq!(opened.session.conversation[*index].role, "user");
        assert!(!b64.is_empty() && !b64.starts_with("data:"), "bare base64, as the panel renders it");

        for turn in &opened.session.conversation {
            assert!(
                turn.frame.is_none(),
                "a frame name would point into a store this session is not in -- and would \
                 dangle the moment the user carries on and it gets saved"
            );
            assert!(
                turn.mark.is_none(),
                "the pointer is already in those pixels; a mark would draw a second one"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_is_not_a_session_opens_as_nothing() {
        assert!(open_artifact("<html><body>just a page</body></html>").is_none());
    }

    /// Export this machine's own sessions and open every one straight back, comparing what
    /// survived. `#[ignore]`d because it depends on what is on disk; run with
    ///
    ///     cargo test --lib session_html::tests::round_trip_the_real_sessions -- --ignored --nocapture
    #[test]
    #[ignore = "round trip against the real sessions directory"]
    fn round_trip_the_real_sessions() {
        use crate::ai::session::SessionManager;

        let base = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
            .join("com.navisual.app")
            .join("sessions");
        let real = SessionManager::new(base.clone());
        let originals = real.all_sessions();
        assert!(!originals.is_empty(), "nothing to round trip");

        let temp = std::path::PathBuf::from(std::env::var("TEMP").unwrap_or_default());
        let out = temp.join("navisual-round-trip-out");
        let _ = std::fs::remove_dir_all(&out);

        let written = write_all(&base, &originals, &out, 4).expect("export");
        assert_eq!(written, originals.len());

        let mut opened = Vec::new();
        let mut pictures = 0usize;
        for entry in std::fs::read_dir(&out).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("html") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read");
            // The index page is not a session, and opens as nothing.
            if let Some(o) = open_artifact(&text) {
                pictures += o.pictures.len();
                opened.push(o);
            }
        }
        println!("exported {written}, opened {}, pictures inline: {pictures}", opened.len());
        assert_eq!(opened.len(), originals.len(), "every session came back");

        let mut had_frames = 0usize;
        for original in &originals {
            let o = opened
                .iter()
                .find(|o| o.session.id == original.id)
                .unwrap_or_else(|| panic!("{} did not come back", original.id));
            let copy = &o.session;
            assert_eq!(copy.task_description, original.task_description);
            assert_eq!(copy.conversation.len(), original.conversation.len());
            assert_eq!(copy.plan_outline, original.plan_outline);
            assert_eq!(copy.plan_completed_count, original.plan_completed_count);
            for (i, (a, b)) in original
                .conversation
                .iter()
                .zip(copy.conversation.iter())
                .enumerate()
            {
                assert_eq!(a.role, b.role);
                assert_eq!(a.content, b.content);
                assert_eq!(a.clicked, b.clicked, "what the user clicked survives");
                assert_eq!(a.advanced_by, b.advanced_by);
                assert!(b.frame.is_none(), "names are stripped; pictures travel inline");
                // A turn that had a picture on disk has one in the artifact, by index.
                if a.frame.is_some()
                    && real
                        .frames_dir(&original.id.to_string())
                        .join(a.frame.as_deref().unwrap())
                        .exists()
                {
                    had_frames += 1;
                    assert!(
                        o.pictures.iter().any(|(idx, _)| *idx == i),
                        "turn {i} had a picture on disk and none came back"
                    );
                }
            }
        }
        println!("turns checked: {}", originals.iter().map(|s| s.conversation.len()).sum::<usize>());
        println!("turns whose picture came back: {had_frames}");
    }

    /// Two sessions about the same thing on the same day slugify to the same name. The
    /// round trip over this machine's own sessions caught exactly that: twenty written,
    /// nineteen files.
    #[test]
    fn two_sessions_that_want_the_same_name_both_get_a_file() {
        use crate::ai::session::SessionManager;

        let dir = std::env::temp_dir().join(format!("navisual-html-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::new(dir.join("sessions"));
        let mut a = Session::new("optimize my computer".to_string());
        a.last_active_at = "2026-09-16T10:00:00-07:00".to_string();
        let mut b = Session::new("optimize my computer".to_string());
        b.last_active_at = "2026-09-16T18:00:00-07:00".to_string();
        assert_eq!(
            file_name_for(&a),
            file_name_for(&b),
            "the premise: same day, same task, same name"
        );

        let dest = dir.join("out");
        let count = write_all(&manager.session_dir, &[a.clone(), b.clone()], &dest, 4).unwrap();
        assert_eq!(count, 2);
        let files: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "index.html")
            .collect();
        assert_eq!(files.len(), 2, "neither may overwrite the other: {files:?}");
        for session in [&a, &b] {
            let html = std::fs::read_to_string(dest.join(&files[0])).unwrap_or_default()
                + &std::fs::read_to_string(dest.join(&files[1])).unwrap_or_default();
            assert!(
                html.contains(&session.id.to_string()),
                "both sessions are present in the folder"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_names_are_unique_and_never_empty() {
        let mut a = Session::new("Optimize for 3D gaming".to_string());
        a.last_active_at = "2026-09-17T10:00:00-07:00".to_string();
        // A task with no ASCII in it slugifies to nothing; the name still has to work.
        let mut b = Session::new("优化电脑".to_string());
        b.last_active_at = "2026-09-17T10:00:00-07:00".to_string();

        let name_a = file_name_for(&a);
        let name_b = file_name_for(&b);
        assert_eq!(name_a, "2026-09-17-optimize-for-3d-gaming.html");
        assert!(name_b.starts_with("2026-09-17-"), "{name_b}");
        assert_ne!(name_a, name_b);
        assert!(name_b.ends_with(".html"));
    }
}
