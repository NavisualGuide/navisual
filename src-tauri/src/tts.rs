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
    }

    enum Msg {
        Speak(String),
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

        pub fn speak(&self, text: String) {
            let _ = self.tx.send(Msg::Speak(text));
        }

        /// Switch to the voice identified by `voice_id` (a SAPI token registry path).
        pub fn set_voice(&self, voice_id: String) {
            if !voice_id.is_empty() {
                let _ = self.tx.send(Msg::SetVoice(voice_id));
            }
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

    // Windows stores SAPI voices in two separate registry locations:
    // - Speech\Voices       — classic SAPI5 voices (David Desktop, Zira Desktop)
    // - Speech_OneCore\Voices — modern OneCore voices (Mark, Cortana, etc.)
    // We enumerate both and merge, deduplicating by ID.
    const ONECORE_VOICES: &str =
        "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech_OneCore\\Voices";
    const CLASSIC_VOICES: &str =
        "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech\\Voices";

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

        let Ok(cat) = CoCreateInstance::<_, ISpObjectTokenCategory>(
            &SpObjectTokenCategory, None, CLSCTX_ALL,
        ) else { return };

        let wide: Vec<u16> = cat_path.encode_utf16().chain(std::iter::once(0)).collect();
        let pcwstr = windows::core::PCWSTR(wide.as_ptr());
        if cat.SetId(pcwstr, false).is_err() { return }

        let Ok(tokens) = cat.EnumTokens(
            windows::core::PCWSTR::null(), windows::core::PCWSTR::null()
        ) else { return };

        loop {
            let mut token: Option<ISpObjectToken> = None;
            let mut fetched: u32 = 0;
            let hr = tokens.Next(1, &mut token, Some(&mut fetched));
            if fetched == 0 || hr.is_err() { break }
            let Some(t) = token else { break };

            let id_raw = match t.GetId() { Ok(p) => p, Err(_) => continue };
            let id = pwstr_to_string(id_raw.0 as *const u16);
            CoTaskMemFree(Some(id_raw.0 as *const _));
            if id.is_empty() || seen.contains(&id) { continue }
            seen.push(id.clone());

            let name = match t.GetStringValue(windows::core::PCWSTR::null()) {
                Ok(p) => {
                    let s = pwstr_to_string(p.0 as *const u16);
                    CoTaskMemFree(Some(p.0 as *const _));
                    if s.is_empty() { id.split('\\').next_back().unwrap_or("Unknown").to_string() } else { s }
                }
                Err(_) => id.split('\\').next_back().unwrap_or("Unknown").to_string(),
            };
            out.push(VoiceInfo { id, name });
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
    unsafe fn sapi_set_voice(
        voice: &windows::Win32::Media::Speech::ISpVoice,
        target_id: &str,
    ) {
        use windows::Win32::Media::Speech::{
            ISpObjectToken, ISpObjectTokenCategory, SpObjectTokenCategory,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

        for cat_path in [ONECORE_VOICES, CLASSIC_VOICES] {
            let Ok(cat) = CoCreateInstance::<_, ISpObjectTokenCategory>(
                &SpObjectTokenCategory, None, CLSCTX_ALL,
            ) else { continue };

            let wide: Vec<u16> = cat_path.encode_utf16().chain(std::iter::once(0)).collect();
            let pcwstr = windows::core::PCWSTR(wide.as_ptr());
            if cat.SetId(pcwstr, false).is_err() { continue }

            let Ok(tokens) = cat.EnumTokens(
                windows::core::PCWSTR::null(), windows::core::PCWSTR::null()
            ) else { continue };

            loop {
                let mut token: Option<ISpObjectToken> = None;
                let mut fetched: u32 = 0;
                let hr = tokens.Next(1, &mut token, Some(&mut fetched));
                if fetched == 0 || hr.is_err() { break }
                let Some(t) = token else { break };

                let id_raw = match t.GetId() { Ok(p) => p, Err(_) => continue };
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

            for msg in rx {
                match msg {
                    Msg::Speak(text) => {
                        let wide: Vec<u16> =
                            text.encode_utf16().chain(std::iter::once(0)).collect();
                        if let Err(e) = voice.Speak(
                            windows::core::PCWSTR(wide.as_ptr()),
                            FLAGS,
                            None,
                        ) {
                            log::warn!("TTS speak failed: {e}");
                        }
                    }
                    Msg::SetVoice(id) => {
                        sapi_set_voice(&voice, &id);
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
    }
    pub struct TtsEngine;
    impl TtsEngine {
        pub fn new() -> Self { Self }
        pub fn speak(&self, _text: String) {}
        pub fn set_voice(&self, _voice_id: String) {}
        pub fn list_voices(&self) -> Vec<VoiceInfo> { vec![] }
    }
}

pub use imp::{TtsEngine, VoiceInfo};
