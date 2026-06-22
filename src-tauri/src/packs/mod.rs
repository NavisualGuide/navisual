//! Nav-Packs — v0.6 Workstream C (core).
//!
//! A Nav-Pack teaches Navisual about a specific application: its UI vocabulary, keyboard
//! shortcuts, and (later) icon templates + workflows. This is the **core loader** — it reads
//! `pack.json` and exposes the two highest-leverage, lowest-risk hooks:
//!
//!   1. **System-prompt injection** — append the pack's `system_prompt_injection` to the
//!      prompt when the focused window matches the pack. Works with zero template matching
//!      and immediately helps weak free-tier models on known apps.
//!   2. **Shortcut-first routing** — the pack's `shortcuts` map is formatted into the same
//!      injected block, so the AI can return `clipboard:"G"` instead of trying to point at
//!      an icon. Covers ~90 % of Blender actions with no template at all.
//!
//! Element hints, icon templates (Workstream B), and workflows are parsed-and-ignored here —
//! deferred to v0.6.x. The format matches `navisual-internal/docs/nav-packs.md` §4.
//!
//! Loading (see [`PackRegistry::load`]): user packs (`%LOCALAPPDATA%\com.navisual.app\packs\`)
//! are scanned first, then bundled packs (the Tauri resource `packs/`), so a user pack with
//! the same `id` shadows a bundled one. `get_active_pack(window_title)` returns the first
//! pack whose `window_title_pattern` matches — user packs win ties.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `pack.json` — only the fields the core loader uses are typed. Unknown fields
/// (`element_hints`, `icon_templates`, `workflows`, …) are ignored, so a full pack and a
/// prompt-injection-only pack both load. Missing optional fields default to empty.
#[derive(Debug, Clone, Deserialize)]
pub struct PackManifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub target_app: String,
    /// Regex (Rust `regex` syntax, e.g. `(?i)blender`) matched against the focused window title.
    pub window_title_pattern: String,
    /// Match priority — **lower is checked first**, so a specific pack (default 0) wins over a
    /// broad fallback like the generic-browser pack (e.g. 100) when both patterns match the
    /// same title (an app-specific web pack vs. the catch-all browser pack for the same window).
    #[serde(default)]
    pub priority: i32,
    /// Free-text guidance appended to the prompt when this pack is active.
    #[serde(default)]
    pub system_prompt_injection: String,
    /// Action label → keyboard shortcut ("Move" → "G"). Formatted into the injected block so
    /// the AI prefers a key press over visual targeting (BTreeMap = stable, sorted output).
    #[serde(default)]
    pub shortcuts: BTreeMap<String, String>,
}

/// Where a pack came from. User packs shadow bundled packs with the same `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    Bundled,
    User,
}

/// A loaded pack: its manifest plus the compiled title regex and provenance.
#[derive(Debug, Clone)]
pub struct Pack {
    pub manifest: PackManifest,
    pub title_re: regex::Regex,
    pub source: PackSource,
    pub dir: PathBuf,
}

/// All loaded packs, in match-priority order (user packs first).
#[derive(Debug, Clone, Default)]
pub struct PackRegistry {
    packs: Vec<Pack>,
}

impl PackRegistry {
    /// Scan `user_dir` then `bundled_dir`, parsing each immediate subdirectory's `pack.json`.
    /// User packs are added first so they win title-pattern ties; a bundled pack whose `id`
    /// already loaded from the user dir is skipped. Malformed packs are logged and skipped —
    /// a bad pack never blocks the others or the app.
    pub fn load(user_dir: Option<&Path>, bundled_dir: Option<&Path>) -> Self {
        let mut packs = Vec::new();
        let mut seen_ids: Vec<String> = Vec::new();
        for (dir, source) in [
            (user_dir, PackSource::User),
            (bundled_dir, PackSource::Bundled),
        ] {
            let Some(dir) = dir else { continue };
            for pack in scan_dir(dir, source) {
                if seen_ids.iter().any(|id| id == &pack.manifest.id) {
                    log::debug!(
                        "pack '{}' from {:?} shadowed by an earlier one",
                        pack.manifest.id,
                        source
                    );
                    continue;
                }
                seen_ids.push(pack.manifest.id.clone());
                packs.push(pack);
            }
        }
        // Stable sort by priority (lower first) so a specific pack beats a broad fallback
        // regardless of filesystem read order; ties keep load order (user packs before bundled).
        packs.sort_by_key(|p| p.manifest.priority);
        let reg = Self { packs };
        for p in &reg.packs {
            log::debug!(
                "nav-pack '{}' v{} ({}) [{:?}] from {}",
                p.manifest.id,
                p.manifest.version,
                p.manifest.name,
                p.source,
                p.dir.display(),
            );
        }
        log::info!("loaded {} nav-pack(s)", reg.len());
        reg
    }

    /// The first pack whose `window_title_pattern` matches `window_title`, or `None`.
    pub fn get_active_pack(&self, window_title: &str) -> Option<&Pack> {
        self.packs.iter().find(|p| p.title_re.is_match(window_title))
    }

    pub fn len(&self) -> usize {
        self.packs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }
}

/// Parse every immediate subdirectory of `dir` that contains a readable `pack.json`.
fn scan_dir(dir: &Path, source: PackSource) -> Vec<Pack> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Missing dir is normal (no user packs installed, or running uninstalled).
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("pack.json");
        if !manifest_path.is_file() {
            continue;
        }
        match load_manifest(&manifest_path) {
            Ok(pack) => out.push(Pack {
                source,
                dir: path,
                ..pack
            }),
            Err(e) => log::warn!("skipping pack at {}: {e}", manifest_path.display()),
        }
    }
    out
}

/// Read + validate one `pack.json`. Returns a `Pack` with `source`/`dir` set by the caller.
fn load_manifest(path: &Path) -> anyhow::Result<Pack> {
    let text = std::fs::read_to_string(path)?;
    let manifest: PackManifest = serde_json::from_str(&text)?;
    if manifest.id.trim().is_empty() {
        anyhow::bail!("pack.json has an empty id");
    }
    let title_re = regex::Regex::new(&manifest.window_title_pattern)
        .map_err(|e| anyhow::anyhow!("invalid window_title_pattern: {e}"))?;
    Ok(Pack {
        manifest,
        title_re,
        source: PackSource::Bundled, // overwritten by caller
        dir: PathBuf::new(),         // overwritten by caller
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_pack(root: &Path, id: &str, json: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pack.json"), json).unwrap();
    }

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "navisual-packs-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn loads_and_matches_by_title() {
        let root = tmp();
        write_pack(
            &root,
            "blender",
            r#"{"id":"blender","name":"Blender","window_title_pattern":"(?i)blender",
                "system_prompt_injection":"Blender 3D.","shortcuts":{"Move":"G","Render":"F12"}}"#,
        );
        let reg = PackRegistry::load(Some(&root), None);
        assert_eq!(reg.len(), 1);
        let pack = reg.get_active_pack("untitled.blend - Blender 4.2").unwrap();
        assert_eq!(pack.manifest.id, "blender");
        assert_eq!(pack.manifest.shortcuts.get("Move").unwrap(), "G");
        // No match → None.
        assert!(reg.get_active_pack("Document1 - Word").is_none());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prompt_injection_only_pack_loads_without_shortcuts() {
        let root = tmp();
        write_pack(
            &root,
            "turbotax",
            r#"{"id":"turbotax","window_title_pattern":"(?i)turbotax",
                "system_prompt_injection":"TurboTax web."}"#,
        );
        let reg = PackRegistry::load(Some(&root), None);
        let pack = reg.get_active_pack("TurboTax — Federal").unwrap();
        assert!(pack.manifest.shortcuts.is_empty());
        assert_eq!(pack.manifest.system_prompt_injection, "TurboTax web.");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A full pack with element_hints/icon_templates/workflows (deferred features) still loads.
        let root = tmp();
        write_pack(
            &root,
            "full",
            r#"{"id":"full","window_title_pattern":"x",
                "element_hints":[{"name":"Timeline","region":"bottom","role":"other"}],
                "icon_templates":["a","b"],"workflows":["w1"]}"#,
        );
        let reg = PackRegistry::load(Some(&root), None);
        assert_eq!(reg.len(), 1);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn malformed_pack_is_skipped_not_fatal() {
        let root = tmp();
        write_pack(&root, "bad", r#"{"id":"bad","window_title_pattern":"([unclosed"}"#); // bad regex
        write_pack(&root, "ok", r#"{"id":"ok","window_title_pattern":"ok"}"#);
        let reg = PackRegistry::load(Some(&root), None);
        assert_eq!(reg.len(), 1);
        assert!(reg.get_active_pack("ok app").is_some());
        fs::remove_dir_all(&root).ok();
    }

    // Live: print which bundled pack matches a real window TITLE and the injection block it
    // would add to the prompt. Confirms the title patterns hit real-world titles and don't
    // false-match. Run:
    //   TITLE="x - Google Chrome" cargo test --lib pack_match_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pack_match_live() {
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("packs");
        let reg = PackRegistry::load(None, Some(&bundled));
        let title = std::env::var("TITLE").unwrap_or_default();
        eprintln!("title = {title:?}");
        match reg.get_active_pack(&title) {
            Some(p) => {
                eprintln!("  matched pack: {}", p.manifest.id);
                let block = crate::ai::prompts::pack_context_block(
                    &p.manifest.target_app,
                    &p.manifest.system_prompt_injection,
                    &p.manifest.shortcuts,
                );
                eprintln!("--- injection block ---{block}--- end ---");
            }
            None => eprintln!("  no pack matched"),
        }
    }

    #[test]
    fn specific_pack_beats_lower_priority_fallback() {
        // A tax site open in a browser matches both patterns; the specific pack (default
        // priority 0) must win over the generic-browser fallback (priority 100) regardless
        // of which loaded first on disk.
        let root = tmp();
        write_pack(
            &root,
            "generic-browser",
            r#"{"id":"generic-browser","window_title_pattern":"(?i)chrome$","priority":100,
                "system_prompt_injection":"GENERIC"}"#,
        );
        write_pack(
            &root,
            "turbotax",
            r#"{"id":"turbotax","window_title_pattern":"(?i)turbotax","system_prompt_injection":"TT"}"#,
        );
        let reg = PackRegistry::load(Some(&root), None);
        let pack = reg.get_active_pack("TurboTax — Chrome").unwrap();
        assert_eq!(pack.manifest.id, "turbotax");
        // A plain browser title still hits the fallback.
        assert_eq!(
            reg.get_active_pack("Amazon — Chrome").unwrap().manifest.id,
            "generic-browser"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bundled_packs_are_valid() {
        // Load the packs that actually ship in src-tauri/packs/ — catches a malformed
        // pack.json or a bad window_title_pattern in a shipped pack at test time.
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("packs");
        let reg = PackRegistry::load(None, Some(&bundled));
        assert!(reg.len() >= 2, "expected the bundled packs to load (got {})", reg.len());
        // Smoke-test routing against representative titles.
        assert_eq!(
            reg.get_active_pack("untitled.blend - Blender 4.2").map(|p| p.manifest.id.as_str()),
            Some("blender")
        );
        assert_eq!(
            reg.get_active_pack("Amazon.com - Google Chrome").map(|p| p.manifest.id.as_str()),
            Some("generic-browser")
        );
        // Edge injects a zero-width space into its title ("Microsoft\u{200b} Edge"); the
        // pattern must still match it (regression for the live-found bug).
        assert_eq!(
            reg.get_active_pack("Inbox - Outlook - Microsoft\u{200b} Edge").map(|p| p.manifest.id.as_str()),
            Some("generic-browser")
        );
        // A non-browser window matches nothing.
        assert!(reg
            .get_active_pack("v0.6-plan.md - Navisual-workspace (Workspace) - Visual Studio Code")
            .is_none());
        // The Blender pack carries shortcuts; the browser pack is prompt-injection only.
        assert!(!reg.get_active_pack("x.blend - Blender").unwrap().manifest.shortcuts.is_empty());
    }

    #[test]
    fn user_pack_shadows_bundled_same_id() {
        let user = tmp();
        let bundled = tmp();
        write_pack(
            &user,
            "blender",
            r#"{"id":"blender","window_title_pattern":"(?i)blender","system_prompt_injection":"USER"}"#,
        );
        write_pack(
            &bundled,
            "blender",
            r#"{"id":"blender","window_title_pattern":"(?i)blender","system_prompt_injection":"BUNDLED"}"#,
        );
        let reg = PackRegistry::load(Some(&user), Some(&bundled));
        assert_eq!(reg.len(), 1);
        let pack = reg.get_active_pack("x.blend - Blender").unwrap();
        assert_eq!(pack.manifest.system_prompt_injection, "USER");
        assert_eq!(pack.source, PackSource::User);
        fs::remove_dir_all(&user).ok();
        fs::remove_dir_all(&bundled).ok();
    }
}
