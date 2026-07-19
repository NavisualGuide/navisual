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

/// Deployment status across every Blender install detected.
#[derive(Debug, Clone, Serialize)]
pub struct AddonStatus {
    /// `BRIDGE_VERSION` in the pack's copy — the version we would install.
    pub pack_version: Option<i64>,
    /// Whether the pack ships the add-on at all (false → nothing to offer).
    pub available: bool,
    pub installs: Vec<AddonInstall>,
    /// Any Blender config dir where the add-on is missing or older than the pack's.
    pub needs_action: bool,
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

/// Current deployment status. `pack_dir` is the loaded Blender pack's directory.
pub fn status(pack_dir: Option<&Path>) -> AddonStatus {
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
    let needs_action = pack_version.is_some_and(|pv| {
        installs
            .iter()
            .any(|i| i.installed_version.is_none_or(|iv| iv < pv))
    });
    AddonStatus {
        pack_version,
        available: source.is_some(),
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

/// Copy the pack's add-on into every detected Blender config directory.
pub fn install(pack_dir: Option<&Path>) -> InstallResult {
    let mut result = InstallResult {
        installed: Vec::new(),
        errors: Vec::new(),
        needs_enable: false,
    };
    let Some(source) = pack_dir.map(|d| d.join(ADDON_FILE)).filter(|p| p.is_file()) else {
        result.errors.push("the Blender pack does not ship the add-on".into());
        return result;
    };
    let dirs = blender_config_dirs();
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
        let s = status(None);
        assert!(!s.available);
        assert!(!s.needs_action, "nothing to install → no action prompt");
        assert_eq!(s.pack_version, None);
    }

    // Live: print what deployment sees on THIS machine (Blender installs found, the
    // add-on version in each, and the pack's).
    // Run: cargo test --lib addon_status_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn addon_status_live() {
        let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs").join("blender");
        let s = status(Some(&pack));
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
        let r = install(Some(&d)); // dir exists but has no add-on file
        assert!(r.installed.is_empty());
        assert_eq!(r.errors.len(), 1);
        assert!(!r.needs_enable);
        fs::remove_dir_all(&d).ok();
    }
}
