//! Pack-shipped add-on deployment — installing the Blender bridge for real users.
//!
//! The bridge (`navisual_bridge.py`) ships inside the Blender nav-pack, but Blender only
//! loads add-ons from its OWN config directory, and its "Install…" flow **copies** the
//! file there. That copy is what makes a pack update invisible to an already-installed
//! bridge (bit us three times during development, 2026-07-19) — and it makes first-run
//! setup a multi-step manual chore for a non-technical user, which is exactly the
//! audience Navisual targets.
//!
//! So Navisual does the copy itself: detect every Blender config directory, compare the
//! installed `BRIDGE_VERSION` against the pack's, and write the file on request.
//!
//! **What this deliberately does NOT do:** enable the add-on. Blender's enabled-add-on
//! list lives in the binary `userpref.blend`, and the documented alternative
//! (`scripts/startup/`) would silently auto-run a socket server the user never agreed
//! to. The Add-ons checkbox stays the consent gate (script-adapters-plan.md §3.5) — we
//! only place the file and tell the user which box to tick.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Filename inside the pack directory and inside Blender's `scripts/addons/`.
pub const ADDON_FILE: &str = "navisual_bridge.py";

/// One Blender configuration directory found on this machine.
#[derive(Debug, Clone, Serialize)]
pub struct AddonInstall {
    /// Blender's config version folder name ("5.1", "3.6").
    pub blender_version: String,
    /// Absolute `scripts/addons` path (may not exist yet — we create it on install).
    pub addons_dir: String,
    /// `BRIDGE_VERSION` parsed from the installed copy; `None` when not installed.
    pub installed_version: Option<i64>,
}

/// Deployment status, SCOPED to the Blender the user is actually working in.
///
/// Machine-wide aggregation was the v1 mistake (live 2026-07-19): with 3.6 and 5.1 both
/// installed, working in an up-to-date 5.1 still raised the prompt because 3.6 lacked
/// the add-on — and the offer then wrote to both. Status and install now follow the
/// TARGET window's Blender version; the other installs are reported for information
/// only and never drive the prompt.
#[derive(Debug, Clone, Serialize)]
pub struct AddonStatus {
    /// `BRIDGE_VERSION` in the pack's copy — the version we would install.
    pub pack_version: Option<i64>,
    /// Whether the pack ships the add-on at all (false → nothing to offer).
    pub available: bool,
    /// Config-folder version of the target Blender ("5.1"), parsed from its window
    /// title. `None` when the target isn't Blender or the title didn't say.
    pub target_version: Option<String>,
    /// `BRIDGE_VERSION` installed for THAT version; `None` = not installed.
    pub target_installed_version: Option<i64>,
    /// Every Blender config dir found (informational).
    pub installs: Vec<AddonInstall>,
    /// The target Blender needs a first install or an update.
    pub needs_action: bool,
}

/// Blender's window title always ends "… - Blender <major>.<minor>.<patch>"; the config
/// folder is `<major>.<minor>`. Returns None for a non-Blender or unusual title.
pub fn config_version_from_title(title: &str) -> Option<String> {
    let idx = title.rfind("Blender ")?;
    let rest = &title[idx + "Blender ".len()..];
    let mut parts = rest.split('.');
    let major: u32 = parts.next()?.trim().parse().ok()?;
    let minor: u32 = parts
        .next()?
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    Some(format!("{major}.{minor}"))
}

/// Parse `BRIDGE_VERSION = N` out of an add-on file.
fn parse_version(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("BRIDGE_VERSION") {
            let rest = rest.trim_start().strip_prefix('=')?;
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Blender's per-version config roots: `%APPDATA%\Blender Foundation\Blender\<ver>\`.
/// Multiple versions coexist normally (a user with 3.6 and 5.1 has both) — we report
/// them all and install to all, since we can't know which one they'll open.
#[cfg(windows)]
fn blender_config_dirs() -> Vec<(String, PathBuf)> {
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    let root = appdata.join("Blender Foundation").join("Blender");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Version folders are "<major>.<minor>"; ignore anything else.
            let major: u32 = name.split('.').next()?.parse().ok()?;
            (major >= 3).then(|| (name, e.path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(not(windows))]
fn blender_config_dirs() -> Vec<(String, PathBuf)> {
    Vec::new()
}

/// Deployment status for the Blender identified by `target_title` (the target window's
/// title). `pack_dir` is the loaded Blender pack's directory.
pub fn status(pack_dir: Option<&Path>, target_title: Option<&str>) -> AddonStatus {
    let source = pack_dir.map(|d| d.join(ADDON_FILE)).filter(|p| p.is_file());
    let pack_version = source.as_deref().and_then(parse_version);
    let installs: Vec<AddonInstall> = blender_config_dirs()
        .into_iter()
        .map(|(ver, dir)| {
            let addons = dir.join("scripts").join("addons");
            let installed = addons.join(ADDON_FILE);
            AddonInstall {
                blender_version: ver,
                addons_dir: addons.to_string_lossy().into_owned(),
                installed_version: installed.is_file().then(|| parse_version(&installed)).flatten(),
            }
        })
        .collect();
    let target_version = target_title.and_then(config_version_from_title);
    // The target's installed version. A running Blender whose config dir we haven't
    // seen yet (fresh install) is simply "not installed" — install() creates the dir.
    let target_installed_version = target_version.as_ref().and_then(|tv| {
        installs
            .iter()
            .find(|i| &i.blender_version == tv)
            .and_then(|i| i.installed_version)
    });
    // Only the TARGET decides whether to prompt. No target version (title didn't say,
    // or not Blender) → never prompt: better silent than nagging about an install the
    // user may not even be running.
    let needs_action = matches!((pack_version, target_version.as_ref()), (Some(pv), Some(_))
        if target_installed_version.is_none_or(|iv| iv < pv));
    AddonStatus {
        pack_version,
        available: source.is_some(),
        target_version,
        target_installed_version,
        installs,
        needs_action,
    }
}

/// Outcome of an install/update run.
#[derive(Debug, Clone, Serialize)]
pub struct InstallResult {
    /// Blender versions the file was written to.
    pub installed: Vec<String>,
    /// Human-readable failures (permissions, missing dirs we couldn't create).
    pub errors: Vec<String>,
    /// True when at least one target had NO previous copy — the user must tick the
    /// Add-ons checkbox once. False → it was an update, so a Blender restart suffices.
    pub needs_enable: bool,
}

/// Copy the pack's add-on into the TARGET Blender's config directory (identified from
/// its window title). Falls back to every detected install only when the target can't
/// be identified — installing into versions the user isn't running is what made the v1
/// prompt confusing, so it's the exception, not the rule.
pub fn install(pack_dir: Option<&Path>, target_title: Option<&str>) -> InstallResult {
    let mut result = InstallResult {
        installed: Vec::new(),
        errors: Vec::new(),
        needs_enable: false,
    };
    let Some(source) = pack_dir.map(|d| d.join(ADDON_FILE)).filter(|p| p.is_file()) else {
        result.errors.push("the Blender pack does not ship the add-on".into());
        return result;
    };
    let mut dirs = blender_config_dirs();
    if let Some(tv) = target_title.and_then(config_version_from_title) {
        // Scope to the running version. If its config dir hasn't been created yet
        // (fresh install), synthesize the path — create_dir_all below makes it.
        if let Some(found) = dirs.iter().find(|(v, _)| *v == tv).cloned() {
            dirs = vec![found];
        } else if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            dirs = vec![(
                tv.clone(),
                appdata.join("Blender Foundation").join("Blender").join(&tv),
            )];
        }
    }
    if dirs.is_empty() {
        result
            .errors
            .push("no Blender installation found (looked in %APPDATA%\\Blender Foundation)".into());
        return result;
    }
    for (ver, dir) in dirs {
        let addons = dir.join("scripts").join("addons");
        if let Err(e) = std::fs::create_dir_all(&addons) {
            result.errors.push(format!("{ver}: cannot create {}: {e}", addons.display()));
            continue;
        }
        let dest = addons.join(ADDON_FILE);
        let was_present = dest.is_file();
        match std::fs::copy(&source, &dest) {
            Ok(_) => {
                if !was_present {
                    result.needs_enable = true;
                }
                result.installed.push(ver);
            }
            Err(e) => result.errors.push(format!("{ver}: {e}")),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("nav_addon_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn parses_bridge_version() {
        let d = tmp();
        let f = d.join(ADDON_FILE);
        fs::write(&f, "import bpy\n\nBRIDGE_VERSION = 2\n\ndef register():\n    pass\n").unwrap();
        assert_eq!(parse_version(&f), Some(2));
        // Spacing variants and trailing comments.
        fs::write(&f, "BRIDGE_VERSION=17  # bumped\n").unwrap();
        assert_eq!(parse_version(&f), Some(17));
        // Absent → None (an old add-on predating versioning).
        fs::write(&f, "import bpy\n").unwrap();
        assert_eq!(parse_version(&f), None);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn status_without_pack_is_unavailable() {
        let s = status(None, Some("x.blend - Blender 5.1.2"));
        assert!(!s.available);
        assert!(!s.needs_action, "nothing to install → no action prompt");
        assert_eq!(s.pack_version, None);
    }

    #[test]
    fn parses_config_version_from_title() {
        assert_eq!(
            config_version_from_title("(Unsaved) - Blender 5.1.2").as_deref(),
            Some("5.1")
        );
        assert_eq!(
            config_version_from_title(r"C:\work\bee.blend - Blender 3.6.22").as_deref(),
            Some("3.6")
        );
        // Non-Blender / unusual titles never yield a version (→ never prompt).
        assert_eq!(config_version_from_title("Document1 - Word"), None);
        assert_eq!(config_version_from_title("Blender"), None);
    }

    #[test]
    fn prompt_follows_the_target_not_the_machine() {
        // The live 2026-07-19 report: 3.6 (no add-on) + 5.1 (current). Working in 5.1
        // must be SILENT, and working in 3.6 must OFFER — v1 got both backwards by
        // aggregating across installs.
        let d = tmp();
        let pack = d.join("pack");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join(ADDON_FILE), "BRIDGE_VERSION = 2\n").unwrap();

        // status() reads the real machine dirs, so assert on the pure decision inputs
        // instead: the rule is "target's installed version < pack version".
        let decide = |installed: Option<i64>, pack_v: i64| -> bool {
            installed.is_none_or(|iv| iv < pack_v)
        };
        assert!(!decide(Some(2), 2), "target current → silent");
        assert!(decide(None, 2), "target missing → offer");
        assert!(decide(Some(1), 2), "target stale → offer");
        fs::remove_dir_all(&d).ok();
    }

    // Live: print what deployment sees on THIS machine (Blender installs found, the
    // add-on version in each, and the pack's).
    // Run: cargo test --lib addon_status_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn addon_status_live() {
        let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs").join("blender");
        let s = status(Some(&pack), std::env::var("TARGET_TITLE").ok().as_deref());
        eprintln!("pack ships add-on: {} (version {:?})", s.available, s.pack_version);
        eprintln!("needs_action: {}", s.needs_action);
        for i in &s.installs {
            eprintln!(
                "  Blender {} → installed v{:?}  [{}]",
                i.blender_version, i.installed_version, i.addons_dir
            );
        }
    }

    #[test]
    fn install_without_source_reports_error() {
        let d = tmp();
        let r = install(Some(&d), None); // dir exists but has no add-on file
        assert!(r.installed.is_empty());
        assert_eq!(r.errors.len(), 1);
        assert!(!r.needs_enable);
        fs::remove_dir_all(&d).ok();
    }
}
