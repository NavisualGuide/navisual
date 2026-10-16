//! Key combos a step names that another program holds for itself.
//!
//! A global hotkey takes its keys from every app. In L1 step 3 (2026-10-12) the model told the user
//! to press Alt+F9 -- Word's field codes -- and NVIDIA's overlay, which holds Alt+F9 for Record,
//! opened its dialog instead. It took the foreground without being seen and covered the next four
//! tasks. Navisual releases its own hotkeys when a step names them (App.svelte); it cannot release
//! another program's, but it can find out: `RegisterHotKey` on a combo another program holds fails
//! with `ERROR_HOTKEY_ALREADY_REGISTERED`. Measured on the founder's machine: Alt+F9 and Alt+Z
//! (NVIDIA) and Win+Shift+S (Windows) held, Alt+F8 free.
//!
//! The check is a measurement, not a list of programs (rule 16). A combo with the Windows key is
//! skipped: those belong to Windows by design, and a step naming one (Win+Shift+S, a screenshot)
//! means the program that holds it. What a check finds goes to the panel, which tells the user, and
//! into the next request's `[Keyboard]` line, which tells the model the key never reached the app.

use parking_lot::Mutex;

/// Held combos found since the last request, for the next one's `[Keyboard]` line.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Modifier bits as `RegisterHotKey` takes them, and the virtual key, for a canonical combo
/// ("alt+f9", "ctrl+shift+e" -- the form `combosInText` in `lib/hotkey.ts` produces). `None` for a
/// combo with the Windows key, or one this cannot parse.
fn parse(combo: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 0x1;
    const MOD_CONTROL: u32 = 0x2;
    const MOD_SHIFT: u32 = 0x4;
    let mut mods = 0u32;
    let mut parts: Vec<&str> = combo.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    // "ctrl++" names the plus key.
    if combo.trim_end().ends_with("++") {
        parts.push("+");
    }
    let key = parts.pop()?;
    for m in parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            _ => return None, // "win", or anything unknown
        }
    }
    if mods == 0 {
        return None;
    }
    let k = key.to_ascii_lowercase();
    let vk = match k.as_str() {
        "esc" => 0x1B,
        "delete" => 0x2E,
        "enter" => 0x0D,
        "space" => 0x20,
        "tab" => 0x09,
        "home" => 0x24,
        "end" => 0x23,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "backquote" => 0xC0,
        "-" => 0xBD,
        "=" | "+" => 0xBB,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        ";" => 0xBA,
        _ if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k.to_ascii_uppercase().as_bytes()[0] as u32,
        _ if k.starts_with('f') => match k[1..].parse::<u32>() {
            Ok(n @ 1..=24) => 0x6F + n,
            _ => return None,
        },
        _ => return None,
    };
    Some((mods, vk))
}

/// Whether another program holds this combo: register it for a moment, and release it at once.
#[cfg(windows)]
fn held_elsewhere(mods: u32, vk: u32) -> bool {
    use windows::Win32::Foundation::ERROR_HOTKEY_ALREADY_REGISTERED;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    };
    // Any id: a thread-level hotkey (no window), gone again before this returns.
    const PROBE_ID: i32 = 0x4E56;
    let flags = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
    match unsafe { RegisterHotKey(None, PROBE_ID, flags, vk) } {
        Ok(()) => {
            let _ = unsafe { UnregisterHotKey(None, PROBE_ID) };
            false
        }
        Err(e) => e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult(),
    }
}

#[cfg(not(windows))]
fn held_elsewhere(_mods: u32, _vk: u32) -> bool {
    false
}

/// The combos in `combos` another program holds. Each is kept for the next request's `[Keyboard]`
/// line, and logged.
pub fn probe(combos: &[String]) -> Vec<String> {
    let mut held = Vec::new();
    for c in combos {
        let Some((mods, vk)) = parse(c) else { continue };
        if held_elsewhere(mods, vk) {
            log::info!("[hotkeys] a step names {c}; another program holds it, so the key will not reach the app");
            held.push(c.clone());
        }
    }
    if !held.is_empty() {
        let mut pending = PENDING.lock();
        for c in &held {
            if !pending.contains(c) {
                pending.push(c.clone());
            }
        }
    }
    held
}

/// "alt+f9" -> "Alt+F9", for the model and the log.
pub fn display(combo: &str) -> String {
    combo
        .split('+')
        .map(|p| match p {
            "backquote" => "`".to_string(),
            "pageup" => "Page Up".to_string(),
            "pagedown" => "Page Down".to_string(),
            _ if p.len() > 1 && p.starts_with('f') && p[1..].chars().all(|c| c.is_ascii_digit()) => p.to_ascii_uppercase(),
            _ => {
                let mut cs = p.chars();
                match cs.next() {
                    Some(f) => f.to_ascii_uppercase().to_string() + cs.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The `[Keyboard]` line for the next request, once: what was found since the last one.
pub fn take_note() -> Option<String> {
    let held: Vec<String> = std::mem::take(&mut *PENDING.lock());
    if held.is_empty() {
        return None;
    }
    let keys: Vec<String> = held.iter().map(|c| display(c)).collect();
    log::info!("[hotkeys] reported to AI: {}", keys.join(", "));
    Some(crate::ai::prompts::held_keys_note(&keys))
}

#[cfg(test)]
mod tests {
    use super::{display, parse};

    #[test]
    fn combos_parse_as_register_hot_key_takes_them() {
        assert_eq!(parse("alt+f9"), Some((0x1, 0x78)));
        assert_eq!(parse("ctrl+shift+e"), Some((0x2 | 0x4, 0x45)));
        assert_eq!(parse("ctrl+backquote"), Some((0x2, 0xC0)));
        assert_eq!(parse("ctrl+1"), Some((0x2, 0x31)));
        assert_eq!(parse("ctrl+pagedown"), Some((0x2, 0x22)));
        // The Windows key's combos belong to Windows; a bare key is not a hotkey.
        assert_eq!(parse("win+shift+s"), None);
        assert_eq!(parse("f9"), None);
        assert_eq!(parse("ctrl+f25"), None);
        assert_eq!(parse("ctrl+nonsense"), None);
    }

    #[test]
    fn combos_display_as_people_write_them() {
        assert_eq!(display("alt+f9"), "Alt+F9");
        assert_eq!(display("ctrl+shift+e"), "Ctrl+Shift+E");
        assert_eq!(display("ctrl+backquote"), "Ctrl+`");
    }

    /// On the founder's machine NVIDIA's overlay holds Alt+F9 and Alt+F8 is free (measured
    /// 2026-10-13). Machine-specific, so run by hand: `cargo test --lib held_keys -- --ignored`.
    #[test]
    #[ignore]
    fn live_alt_f9_is_held_here_and_alt_f8_is_not() {
        let found = super::probe(&["alt+f9".to_string(), "alt+f8".to_string()]);
        assert_eq!(found, vec!["alt+f9".to_string()]);
        let note = super::take_note().unwrap();
        assert!(note.contains("Alt+F9"), "{note}");
        assert!(super::take_note().is_none(), "the line is sent once");
    }
}
