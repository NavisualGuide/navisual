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
    /// Coarse screen regions for named elements — tells the locator *where* an element lives
    /// so template matching searches a fixed window independent of the (sometimes mis-grounded)
    /// AI bbox. Static chrome (toolbars, menus, panels) is stable, so this is the robustness
    /// lever for icon matching on sparse-A11y apps. See [`region_to_fractional_rect`].
    #[serde(default)]
    pub element_hints: Vec<ElementHint>,
    /// Windows display scale the pack's icon crops were captured at (1.0 = 100 %, 1.5 = 150 %).
    /// The template-matching DPI prior centres its scale sweep on `target_monitor_scale ÷
    /// authoring_scale`, so a pack authored at 100 % still matches for a user at 150 %/200 %.
    /// Defaults to 1.0 (author at 100 % — the guidance in nav-packs.md §6.1).
    #[serde(default = "default_authoring_scale")]
    pub authoring_scale: f32,
}

/// Default `authoring_scale` when a pack omits it — 100 % (the documented authoring scale).
fn default_authoring_scale() -> f32 {
    1.0
}

/// A named UI element's coarse location (and role) within the app window.
#[derive(Debug, Clone, Deserialize)]
pub struct ElementHint {
    pub name: String,
    /// A named region ("left", "top-right", …) resolved by [`region_to_fractional_rect`].
    #[serde(default)]
    pub region: String,
    /// UI role of the element. Parsed from the pack but not yet consumed — reserved for biasing
    /// OCR role search (a future element_hints use); kept so packs can declare it now.
    #[serde(default)]
    #[allow(dead_code)]
    pub role: String,
}

/// Where a pack came from. User packs shadow bundled packs with the same `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    Bundled,
    User,
}

/// One icon-crop file discovered in a pack's `icons/` directory, for template matching
/// (Workstream B). `stem` is the lowercased filename without extension ("move_tool"), used to
/// associate the icon with an AI target ("Move tool").
#[derive(Debug, Clone)]
pub struct IconAsset {
    pub stem: String,
    pub path: PathBuf,
}

/// A loaded pack: its manifest plus the compiled title regex, provenance, and any icon crops.
#[derive(Debug, Clone)]
pub struct Pack {
    pub manifest: PackManifest,
    pub title_re: regex::Regex,
    pub source: PackSource,
    pub dir: PathBuf,
    /// Icon crops from `<dir>/icons/*.png|jpg` for template matching (empty for most packs).
    pub icons: Vec<IconAsset>,
}

impl Pack {
    /// Icons whose filename stem is associated with `target_text` (Workstream B Pass-3
    /// candidates). Capped so a sloppy match can't blow up latency — the locator still gates
    /// each by NCC, so over-selection only costs a few extra correlation passes.
    pub fn candidate_icons(&self, target_text: &str) -> Vec<&IconAsset> {
        self.icons
            .iter()
            .filter(|a| icon_stem_matches_target(&a.stem, target_text))
            .take(8)
            .collect()
    }

    /// Display scale the pack's icon crops were authored at, clamped to a sane range so a
    /// malformed manifest value can't yield a zero/negative or absurd DPI prior. See
    /// [`PackManifest::authoring_scale`].
    pub fn authoring_scale(&self) -> f32 {
        let s = self.manifest.authoring_scale;
        if s.is_finite() && s > 0.0 {
            s.clamp(0.5, 4.0)
        } else {
            1.0
        }
    }

    /// Coarse search region for `target_text` from the pack's `element_hints` (matched by name,
    /// same token-subset rule as icons). `None` when no hint matches — the locator then falls
    /// back to the AI bbox window. Returns the resolved fractional rect `[x0,y0,x1,y1]`.
    pub fn region_hint_for(&self, target_text: &str) -> Option<[f32; 4]> {
        self.manifest
            .element_hints
            .iter()
            .filter(|h| !h.region.is_empty() && icon_stem_matches_target(&h.name, target_text))
            .find_map(|h| region_to_fractional_rect(&h.region))
    }
}

/// Map a coarse region name to a fractional rect `[x0,y0,x1,y1]` (0..1) within the app window.
/// Edge strips/bands are deliberately generous so a hint reliably contains its chrome; corners
/// and center cover the rest. Unknown names return `None` (caller falls back to the AI bbox).
pub fn region_to_fractional_rect(region: &str) -> Option<[f32; 4]> {
    let rect = match region.trim().to_ascii_lowercase().as_str() {
        "left" => [0.00, 0.00, 0.18, 1.00],
        "right" => [0.82, 0.00, 1.00, 1.00],
        "top" => [0.00, 0.00, 1.00, 0.18],
        "bottom" => [0.00, 0.82, 1.00, 1.00],
        "center" => [0.25, 0.25, 0.75, 0.75],
        "top-left" => [0.00, 0.00, 0.30, 0.30],
        "top-right" => [0.70, 0.00, 1.00, 0.30],
        "bottom-left" => [0.00, 0.70, 0.30, 1.00],
        "bottom-right" => [0.70, 0.70, 1.00, 1.00],
        "top-center" => [0.25, 0.00, 0.75, 0.22],
        "bottom-center" => [0.25, 0.78, 0.75, 1.00],
        "left-center" | "center-left" => [0.00, 0.25, 0.22, 0.75],
        "right-center" | "center-right" => [0.78, 0.25, 1.00, 0.75],
        _ => return None,
    };
    Some(rect)
}

/// Lowercased alphanumeric tokens of `s` ("Move tool" / "move_tool" → ["move","tool"]).
fn normalize_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether an icon filename `stem` is associated with the AI's `target_text`: one token set
/// fully contains the other ("move_tool" ↔ "Move tool"; "render_button" ⊇ "Render"). Keeps a
/// short generic target from latching everything while tolerating naming-style differences.
pub fn icon_stem_matches_target(stem: &str, target: &str) -> bool {
    let st = normalize_tokens(stem);
    let tt = normalize_tokens(target);
    if st.is_empty() || tt.is_empty() {
        return false;
    }
    st.iter().all(|t| tt.contains(t)) || tt.iter().all(|t| st.contains(t))
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
            Ok(pack) => {
                let icons = scan_icons(&path.join("icons"));
                out.push(Pack {
                    source,
                    dir: path,
                    icons,
                    ..pack
                });
            }
            Err(e) => log::warn!("skipping pack at {}: {e}", manifest_path.display()),
        }
    }
    out
}

/// Collect PNG/JPEG icon crops from a pack's `icons/` directory (empty if absent).
fn scan_icons(icons_dir: &Path) -> Vec<IconAsset> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(icons_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
            .unwrap_or(false);
        if !is_image {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.push(IconAsset {
                stem: stem.to_ascii_lowercase(),
                path: path.clone(),
            });
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
        icons: Vec::new(),           // populated by scan_dir
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
        let blender = reg.get_active_pack("x.blend - Blender").unwrap();
        assert!(!blender.manifest.shortcuts.is_empty());
        // The full Blender pack ships a toolbar icon set; each tool name resolves to its icon.
        assert!(blender.icons.len() >= 8, "expected the bundled Blender icons (got {})", blender.icons.len());
        for (target, want) in [
            ("Move tool", "move"),
            ("Rotate", "rotate"),
            ("Scale", "scale"),
            ("Annotate", "annotate"),
            ("Add Cube", "add_cube"),
        ] {
            let got: Vec<&str> = blender.candidate_icons(target).iter().map(|a| a.stem.as_str()).collect();
            assert_eq!(got, vec![want], "target {target:?} should resolve to exactly [{want}]");
        }
    }

    #[test]
    fn region_hints_resolve_by_name() {
        assert_eq!(region_to_fractional_rect("left"), Some([0.00, 0.00, 0.18, 1.00]));
        assert_eq!(region_to_fractional_rect("TOP-RIGHT"), Some([0.70, 0.00, 1.00, 0.30]));
        assert_eq!(region_to_fractional_rect("nonsense"), None);

        let root = tmp();
        let dir = root.join("p");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("pack.json"),
            r#"{"id":"p","window_title_pattern":"x",
                "element_hints":[
                  {"name":"Move","region":"left","role":"button"},
                  {"name":"Render Image","region":"top"}
                ]}"#,
        )
        .unwrap();
        let reg = PackRegistry::load(Some(&root), None);
        let pack = reg.get_active_pack("x window").unwrap();
        assert_eq!(pack.region_hint_for("Move tool"), Some([0.00, 0.00, 0.18, 1.00]));
        assert_eq!(pack.region_hint_for("Render"), Some([0.00, 0.00, 1.00, 0.18]));
        assert_eq!(pack.region_hint_for("Scale"), None); // no hint for Scale
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn authoring_scale_defaults_reads_and_clamps() {
        let root = tmp();
        let write = |id: &str, body: &str| {
            let d = root.join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("pack.json"), body).unwrap();
        };
        // Omitted → default 1.0; explicit 1.5 → read back; bogus 0 / negative → accessor clamps to 1.0.
        write("a", r#"{"id":"a","window_title_pattern":"aaa"}"#);
        write("b", r#"{"id":"b","window_title_pattern":"bbb","authoring_scale":1.5}"#);
        write("c", r#"{"id":"c","window_title_pattern":"ccc","authoring_scale":0}"#);
        write("d", r#"{"id":"d","window_title_pattern":"ddd","authoring_scale":-2.0}"#);

        let reg = PackRegistry::load(Some(&root), None);
        assert_eq!(reg.get_active_pack("aaa win").unwrap().authoring_scale(), 1.0);
        assert_eq!(reg.get_active_pack("bbb win").unwrap().authoring_scale(), 1.5);
        assert_eq!(reg.get_active_pack("ccc win").unwrap().authoring_scale(), 1.0);
        assert_eq!(reg.get_active_pack("ddd win").unwrap().authoring_scale(), 1.0);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn icon_stem_target_association() {
        assert!(icon_stem_matches_target("move_tool", "Move tool")); // naming-style diff
        assert!(icon_stem_matches_target("move_tool", "move")); // target ⊆ stem
        assert!(icon_stem_matches_target("render_button", "Render")); // target ⊆ stem
        assert!(!icon_stem_matches_target("select_tool", "Move tool")); // disjoint
        assert!(!icon_stem_matches_target("move_tool", "")); // empty target
        assert!(!icon_stem_matches_target("", "Move")); // empty stem
    }

    #[test]
    fn candidate_icons_filters_and_caps() {
        let root = tmp();
        let dir = root.join("p");
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(
            dir.join("pack.json"),
            r#"{"id":"p","window_title_pattern":"x"}"#,
        )
        .unwrap();
        for f in ["move_tool.png", "select_tool.png", "render_button.jpg", "notes.txt"] {
            fs::write(dir.join("icons").join(f), b"stub").unwrap();
        }
        let reg = PackRegistry::load(Some(&root), None);
        let pack = reg.get_active_pack("x window").unwrap();
        assert_eq!(pack.icons.len(), 3, "only image files become icons (notes.txt excluded)");
        let cands: Vec<_> = pack
            .candidate_icons("Move tool")
            .iter()
            .map(|a| a.stem.clone())
            .collect();
        assert_eq!(cands, vec!["move_tool".to_string()]);
        fs::remove_dir_all(&root).ok();
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
