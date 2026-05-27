//! Text-to-speech via Windows SAPI (ISpVoice COM interface).
//!
//! Runs on a dedicated STA thread so the async runtime is never blocked.
//! Calling `speak(text)` purges any in-flight speech and starts the new text
//! immediately. Calling `speak("")` stops speech without starting a new one.
//! Calling `set_voice(token_id)` switches voices without restarting the thread.
//! Calling `list_voices()` blocks until the STA thread returns all installed voices.

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[derive(serde::Serialize, Clone)]
    pub struct VoiceInfo {
        pub id: String,
        pub name: String,
        /// Primary language code from the SAPI token (e.g. "en", "zh"); "" if unknown.
        pub lang: String,
    }

    enum Msg {
        /// (text, language): language is a BCP-47 locale, or "auto"/"" to detect.
        Speak(String, String),
        SetVoice(String),
        ListVoices(mpsc::SyncSender<Vec<VoiceInfo>>),
        Quit,
    }

    pub struct TtsEngine {
        tx: mpsc::Sender<Msg>,
    }

    impl TtsEngine {
        pub fn new() -> Self {
            let (tx, rx) = mpsc::channel();
            thread::Builder::new()
                .name("tts-sapi".into())
                .spawn(move || tts_thread(rx))
                .expect("tts thread spawn");
            Self { tx }
        }

        pub fn speak(&self, text: String, lang: String) {
            let _ = self.tx.send(Msg::Speak(text, lang));
        }

        /// Set the *preferred* voice (a SAPI token id). Empty = no preference
        /// (pure auto-by-language). Applied on the next speak, per utterance.
        pub fn set_voice(&self, voice_id: String) {
            let _ = self.tx.send(Msg::SetVoice(voice_id));
        }

        /// Block until the STA thread returns all installed TTS voices (up to 3 s).
        pub fn list_voices(&self) -> Vec<VoiceInfo> {
            let (tx, rx) = mpsc::sync_channel(1);
            let _ = self.tx.send(Msg::ListVoices(tx));
            rx.recv_timeout(Duration::from_secs(3)).unwrap_or_default()
        }
    }

    impl Drop for TtsEngine {
        fn drop(&mut self) {
            let _ = self.tx.send(Msg::Quit);
        }
    }

    unsafe fn pwstr_to_string(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let len = (0usize..).take_while(|&i| *ptr.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }

    /// Map a SAPI `Attributes\Language` value (LCID in hex, sometimes a
    /// ';'-separated list) to a primary 2-letter language code. "" if unknown.
    fn lcid_to_lang(lcid_str: &str) -> String {
        let first = lcid_str
            .split([';', ',', ' '])
            .find(|s| !s.is_empty())
            .unwrap_or("");
        let Ok(lcid) = u32::from_str_radix(first.trim(), 16) else {
            return String::new();
        };
        let code = match lcid & 0x3ff {
            0x04 => "zh",
            0x09 => "en",
            0x11 => "ja",
            0x12 => "ko",
            0x0a => "es",
            0x0c => "fr",
            0x07 => "de",
            0x16 => "pt",
            0x10 => "it",
            0x19 => "ru",
            0x01 => "ar",
            0x15 => "pl",
            0x13 => "nl",
            0x1f => "tr",
            0x2a => "vi",
            0x21 => "id",
            _ => "",
        };
        code.to_string()
    }

    /// Best-effort primary-language detection from script, for "auto" TTS.
    fn detect_lang(text: &str) -> String {
        let (mut kana, mut hangul, mut han, mut cyr, mut arab) =
            (false, false, false, false, false);
        for c in text.chars() {
            let u = c as u32;
            if (0x3040..=0x30ff).contains(&u) {
                kana = true;
            } else if (0xac00..=0xd7af).contains(&u) || (0x1100..=0x11ff).contains(&u) {
                hangul = true;
            } else if (0x4e00..=0x9fff).contains(&u) || (0x3400..=0x4dbf).contains(&u) {
                han = true;
            } else if (0x0400..=0x04ff).contains(&u) {
                cyr = true;
            } else if (0x0600..=0x06ff).contains(&u) {
                arab = true;
            }
        }
        let code = if kana {
            "ja"
        } else if hangul {
            "ko"
        } else if han {
            "zh"
        } else if cyr {
            "ru"
        } else if arab {
            "ar"
        } else {
            "en"
        };
        code.to_string()
    }

    /// Primary language subtag of a BCP-47 locale ("zh-CN" -> "zh").
    fn lang_code_of_locale(locale: &str) -> String {
        locale
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    /// Read a voice token's `Attributes\Language` and map it to a language code.
    unsafe fn token_language(t: &windows::Win32::Media::Speech::ISpObjectToken) -> String {
        use windows::Win32::System::Com::CoTaskMemFree;
        let attrs_w: Vec<u16> = "Attributes"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let Ok(attrs) = t.OpenKey(windows::core::PCWSTR(attrs_w.as_ptr())) else {
            return String::new();
        };
        let lang_w: Vec<u16> = "Language".encode_utf16().chain(std::iter::once(0)).collect();
        let Ok(p) = attrs.GetStringValue(windows::core::PCWSTR(lang_w.as_ptr())) else {
            return String::new();
        };
        let s = pwstr_to_string(p.0 as *const u16);
        CoTaskMemFree(Some(p.0 as *const _));
        lcid_to_lang(&s)
    }

    // Windows stores SAPI voices in two separate registry locations:
    // - Speech\Voices       — classic SAPI5 voices (David Desktop, Zira Desktop)
    // - Speech_OneCore\Voices — modern OneCore voices (Mark, Cortana, etc.)
    // We enumerate both and merge, deduplicating by ID.
    const ONECORE_VOICES: &str = "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech_OneCore\\Voices";
    const CLASSIC_VOICES: &str = "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech\\Voices";

    // Enumerate one token category into `out`, skipping IDs already in `seen`.
    unsafe fn collect_from_category(
        cat_path: &str,
        seen: &mut Vec<String>,
        out: &mut Vec<VoiceInfo>,
    ) {
        use windows::Win32::Media::Speech::{
            ISpObjectToken, ISpObjectTokenCategory, SpObjectTokenCategory,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

        let Ok(cat) =
            CoCreateInstance::<_, ISpObjectTokenCategory>(&SpObjectTokenCategory, None, CLSCTX_ALL)
        else {
            return;
        };

        let wide: Vec<u16> = cat_path.encode_utf16().chain(std::iter::once(0)).collect();
        let pcwstr = windows::core::PCWSTR(wide.as_ptr());
        if cat.SetId(pcwstr, false).is_err() {
            return;
        }

        let Ok(tokens) =
            cat.EnumTokens(windows::core::PCWSTR::null(), windows::core::PCWSTR::null())
        else {
            return;
        };

        loop {
            let mut token: Option<ISpObjectToken> = None;
            let mut fetched: u32 = 0;
            let hr = tokens.Next(1, &mut token, Some(&mut fetched));
            if fetched == 0 || hr.is_err() {
                break;
            }
            let Some(t) = token else { break };

            let id_raw = match t.GetId() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let id = pwstr_to_string(id_raw.0 as *const u16);
            CoTaskMemFree(Some(id_raw.0 as *const _));
            if id.is_empty() || seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());

            let name = match t.GetStringValue(windows::core::PCWSTR::null()) {
                Ok(p) => {
                    let s = pwstr_to_string(p.0 as *const u16);
                    CoTaskMemFree(Some(p.0 as *const _));
                    if s.is_empty() {
                        id.split('\\').next_back().unwrap_or("Unknown").to_string()
                    } else {
                        s
                    }
                }
                Err(_) => id.split('\\').next_back().unwrap_or("Unknown").to_string(),
            };
            let lang = token_language(&t);
            out.push(VoiceInfo { id, name, lang });
        }
    }

    // Enumerate installed SAPI voices. Uses OneCore (Windows 10+) which is a
    // superset of the classic engine — no duplicates. Falls back to classic only
    // if OneCore yields nothing (very old Windows 10 builds).
    unsafe fn sapi_enum_voices() -> Vec<VoiceInfo> {
        let mut seen = Vec::new();
        let mut voices = Vec::new();
        collect_from_category(ONECORE_VOICES, &mut seen, &mut voices);
        if voices.is_empty() {
            collect_from_category(CLASSIC_VOICES, &mut seen, &mut voices);
        }
        voices
    }

    // Find the token with `target_id` across both categories and call SetVoice.
    unsafe fn sapi_set_voice(voice: &windows::Win32::Media::Speech::ISpVoice, target_id: &str) {
        use windows::Win32::Media::Speech::{
            ISpObjectToken, ISpObjectTokenCategory, SpObjectTokenCategory,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

        for cat_path in [ONECORE_VOICES, CLASSIC_VOICES] {
            let Ok(cat) = CoCreateInstance::<_, ISpObjectTokenCategory>(
                &SpObjectTokenCategory,
                None,
                CLSCTX_ALL,
            ) else {
                continue;
            };

            let wide: Vec<u16> = cat_path.encode_utf16().chain(std::iter::once(0)).collect();
            let pcwstr = windows::core::PCWSTR(wide.as_ptr());
            if cat.SetId(pcwstr, false).is_err() {
                continue;
            }

            let Ok(tokens) =
                cat.EnumTokens(windows::core::PCWSTR::null(), windows::core::PCWSTR::null())
            else {
                continue;
            };

            loop {
                let mut token: Option<ISpObjectToken> = None;
                let mut fetched: u32 = 0;
                let hr = tokens.Next(1, &mut token, Some(&mut fetched));
                if fetched == 0 || hr.is_err() {
                    break;
                }
                let Some(t) = token else { break };

                let id_raw = match t.GetId() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let id = pwstr_to_string(id_raw.0 as *const u16);
                CoTaskMemFree(Some(id_raw.0 as *const _));

                if id == target_id {
                    if let Err(e) = voice.SetVoice(&t) {
                        log::warn!("TTS SetVoice failed: {e}");
                    }
                    return;
                }
            }
        }
        log::warn!("TTS: voice id not found in any category: {target_id}");
    }

    fn tts_thread(rx: mpsc::Receiver<Msg>) {
        use windows::Win32::Media::Speech::{ISpVoice, SpVoice};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
        };

        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let voice: ISpVoice = match CoCreateInstance(&SpVoice, None, CLSCTX_ALL) {
                Ok(v) => v,
                Err(e) => {
                    log::error!("TTS: SpVoice CoCreateInstance failed: {e}");
                    return;
                }
            };

            // SPF_ASYNC (1) | SPF_PURGEBEFORESPEAK (2)
            const FLAGS: u32 = 1 | 2;

            // Voices (with language) enumerated once. `preferred_id` is the user's
            // optional voice override; `current_id` is whatever SAPI is set to now,
            // so we only call SetVoice when the target language needs a switch.
            let voices = sapi_enum_voices();
            let mut preferred_id: Option<String> = None;
            let mut current_id: Option<String> = None;

            for msg in rx {
                match msg {
                    Msg::Speak(text, lang) => {
                        if text.is_empty() {
                            // Empty text = stop/purge any in-flight speech.
                            let stop = [0u16];
                            let _ = voice.Speak(windows::core::PCWSTR(stop.as_ptr()), FLAGS, None);
                            continue;
                        }
                        let target = if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
                            detect_lang(&text)
                        } else {
                            lang_code_of_locale(&lang)
                        };
                        // Preferred voice if it speaks the target language; else any
                        // installed voice for that language; else caption-only.
                        let chosen = preferred_id
                            .as_ref()
                            .filter(|pid| voices.iter().any(|v| &v.id == *pid && v.lang == target))
                            .cloned()
                            .or_else(|| {
                                voices.iter().find(|v| v.lang == target).map(|v| v.id.clone())
                            });
                        match chosen {
                            Some(id) => {
                                if current_id.as_deref() != Some(id.as_str()) {
                                    sapi_set_voice(&voice, &id);
                                    current_id = Some(id);
                                }
                                let wide: Vec<u16> =
                                    text.encode_utf16().chain(std::iter::once(0)).collect();
                                if let Err(e) =
                                    voice.Speak(windows::core::PCWSTR(wide.as_ptr()), FLAGS, None)
                                {
                                    log::warn!("TTS speak failed: {e}");
                                }
                            }
                            None => {
                                log::warn!(
                                    "TTS: no installed voice for language '{target}' — caption only"
                                );
                            }
                        }
                    }
                    Msg::SetVoice(id) => {
                        preferred_id = if id.is_empty() { None } else { Some(id) };
                    }
                    Msg::ListVoices(reply) => {
                        let _ = reply.send(sapi_enum_voices());
                    }
                    Msg::Quit => break,
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    #[derive(serde::Serialize, Clone)]
    pub struct VoiceInfo {
        pub id: String,
        pub name: String,
        pub lang: String,
    }
    pub struct TtsEngine;
    impl TtsEngine {
        pub fn new() -> Self {
            Self
        }
        pub fn speak(&self, _text: String, _lang: String) {}
        pub fn set_voice(&self, _voice_id: String) {}
        pub fn list_voices(&self) -> Vec<VoiceInfo> {
            vec![]
        }
    }
}

pub use imp::{TtsEngine, VoiceInfo};
