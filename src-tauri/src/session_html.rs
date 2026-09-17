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

use crate::ai::session::{Session, StoredMark, Turn, SESSION_HISTORY_KEEP};

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
        let figure = |name: &str, caption: &str| -> String {
            match frame_data_uri(&frames_dir.join(name), turn.mark.as_ref(), thickness) {
                Some(uri) => format!(
                    "<figure><img src=\"{uri}\" alt=\"Screen at this step\">\
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
    for session in sessions {
        let name = file_name_for(session);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::session::Session;

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
