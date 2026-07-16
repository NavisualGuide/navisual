//! Shared JSONL appender for the rolling diagnostic logs (`prompt_log.jsonl`,
//! `locate_log.jsonl`) — one rotation policy instead of two copies.
//!
//! Rotation (at ~5 MB) has two modes, chosen per call by `archive`:
//!  - `false` (pure-diagnostic use, the pre-2026-07-15 behavior): the previous
//!    `.jsonl.1` backup is deleted and the live file takes its place — a bounded
//!    ~10 MB window, which doubles as the privacy safety net for developers who
//!    turn a log on and forget it.
//!  - `true` (training capture on — llm-finetuning-eval.md §5b): the full file is
//!    MOVED to `training/logs/<stem>.<YYYYMMDD_HHMMSS>.jsonl` next to the app-data
//!    root instead of being destroyed. Training data is an accumulating asset;
//!    rotation must never be the thing that silently eats it. The `training/` dir
//!    is exempt from `cleanup_old_debug_artifacts` by construction (that cleanup
//!    only targets `debug/*` and the two live jsonl paths).

use std::io::Write;
use std::path::Path;

const ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// Append one serialized JSON line, rotating first if the file is over the cap.
pub fn append_line(path: &Path, line: &str, archive: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > ROTATE_BYTES {
            rotate(path, archive);
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn rotate(path: &Path, archive: bool) {
    if archive {
        // app-data root = the live log's own directory; archives accumulate under
        // training/logs/ beside it.
        let Some(root) = path.parent() else { return };
        let dir = root.join("training").join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("log")
            .to_string();
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let dest = dir.join(format!("{stem}.{ts}.jsonl"));
        let _ = std::fs::rename(path, &dest);
    } else {
        let backup = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::rename(path, &backup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_rotates_to_archive() {
        let dir = std::env::temp_dir().join(format!("jsonl_log_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_log.jsonl");

        append_line(&path, r#"{"a":1}"#, true).unwrap();
        assert!(path.exists());

        // Force the file over the cap, then append with archive=true — the old
        // content must land under training/logs/, not be deleted.
        let big = "x".repeat((ROTATE_BYTES + 1) as usize);
        std::fs::write(&path, &big).unwrap();
        append_line(&path, r#"{"b":2}"#, true).unwrap();

        let archived: Vec<_> = std::fs::read_dir(dir.join("training").join("logs"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(archived.len(), 1, "rotated file must be archived, not lost");
        assert!(
            std::fs::metadata(archived[0].path()).unwrap().len() > ROTATE_BYTES,
            "archive holds the full pre-rotation content"
        );
        // The live file restarted with just the new line.
        assert!(std::fs::metadata(&path).unwrap().len() < 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_archive_rotation_keeps_single_backup() {
        let dir = std::env::temp_dir().join(format!("jsonl_log_test2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diag.jsonl");

        let big = "x".repeat((ROTATE_BYTES + 1) as usize);
        std::fs::write(&path, &big).unwrap();
        append_line(&path, r#"{"a":1}"#, false).unwrap();

        assert!(path.with_extension("jsonl.1").exists());
        assert!(!dir.join("training").exists(), "diagnostic mode must not create training/");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
