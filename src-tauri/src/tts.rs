//! Text-to-speech via the WinRT SpeechSynthesizer (Windows.Media.SpeechSynthesis).
//!
//! Uses the modern OneCore voice engine so the natural Windows 10/11 voices —
//! the ones in Settings → Speech (e.g. Kangkang, Xiaoxiao) — actually play. The
//! legacy SAPI `ISpVoice` engine can *enumerate* those tokens but silently
//! substitutes an old SAPI5 voice of the same language, so a picked voice never
//! took effect.
//!
//! Runs on a dedicated MTA thread. The synthesizer renders text to an audio
//! stream that a MediaPlayer plays; starting a new utterance replaces the
//! previous one. `speak("")` stops playback. `set_voice(id)` sets the preferred
//! voice (a WinRT VoiceInformation Id); empty = auto-by-language.

#[cfg(windows)]
mod imp {
    use std::future::IntoFuture;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use windows::core::HSTRING;
    use windows::Media::Core::MediaSource;
    use windows::Media::Playback::MediaPlayer;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;

    #[derive(serde::Serialize, Clone)]
    pub struct VoiceInfo {
        pub id: String,
        pub name: String,
        /// Primary language code (e.g. "en", "zh"); "" if unknown.
        pub lang: String,
    }

    enum Msg {
        /// (text, language, request_hint, fallback_locale): `language` is a BCP-47
        /// locale, or "auto"/"" to detect. `request_hint` is the user's ORIGINAL
        /// request text — Rule 19 pins the reply language to it, so in auto mode its
        /// script is a better signal than re-guessing from an ambiguous reply.
        /// `fallback_locale` is the OS UI locale (e.g. "fr-FR"), the last resort when
        /// both are Latin-ambiguous — see `detect_lang`.
        Speak(String, String, String, String),
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
                .name("tts-winrt".into())
                .spawn(move || tts_thread(rx))
                .expect("tts thread spawn");
            Self { tx }
        }

        pub fn speak(
            &self,
            text: String,
            lang: String,
            request_hint: String,
            fallback_locale: String,
        ) {
            let _ = self
                .tx
                .send(Msg::Speak(text, lang, request_hint, fallback_locale));
        }

        /// Set the *preferred* voice (a WinRT VoiceInformation Id). Empty = no
        /// preference (auto-by-language). Applied on the next speak.
        pub fn set_voice(&self, voice_id: String) {
            let _ = self.tx.send(Msg::SetVoice(voice_id));
        }

        /// Block until the worker returns all installed voices (up to 3 s).
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

    /// Strong-script language signal: Some("ja"/"ko"/"zh"/"ru"/"ar") when the text
    /// contains an unambiguous non-Latin script, None for Latin/none (en/fr/es/de/…
    /// all share Latin script and can't be told apart from the text alone).
    fn strong_script_lang(text: &str) -> Option<&'static str> {
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
        if kana {
            Some("ja")
        } else if hangul {
            Some("ko")
        } else if han {
            Some("zh")
        } else if cyr {
            Some("ru")
        } else if arab {
            Some("ar")
        } else {
            None
        }
    }

    /// Best-effort primary-language detection for "auto" TTS, in priority order:
    ///
    /// 1. A strong non-Latin script in the REPLY text itself — the voice must be able
    ///    to pronounce what's actually on screen (an English voice can't read Han at
    ///    all), so this always wins.
    /// 2. A strong non-Latin script in the user's ORIGINAL request (`request_hint`) —
    ///    Rule 19 pins the reply language to the request, so when the reply is
    ///    Latin-ambiguous (e.g. "Press Ctrl+B" answering a Chinese request) the
    ///    request's language is the user's actual language, better than guessing from
    ///    the OS locale. (Design suggestion #7, 2026-07-13: the request language is
    ///    known at guide() time — use it instead of re-detecting from the reply.)
    /// 3. The primary subtag of `fallback_locale` (the OS UI locale) — Latin scripts
    ///    can't be distinguished from text alone (audit C7: a French reply under
    ///    `auto` always got an English voice before this existed).
    /// 4. "en".
    fn detect_lang(text: &str, request_hint: &str, fallback_locale: &str) -> String {
        if let Some(l) = strong_script_lang(text) {
            return l.to_string();
        }
        if let Some(l) = strong_script_lang(request_hint) {
            return l.to_string();
        }
        let fb = lang_code_of_locale(fallback_locale);
        if fb.is_empty() {
            "en".to_string()
        } else {
            fb
        }
    }

    /// Minimal single-thread blocking executor — drives a WinRT async operation
    /// to completion on this (MTA) thread. The op completes on a thread-pool
    /// thread and wakes us, so there is no self-deadlock.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};
        struct ThreadWaker(std::thread::Thread);
        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }
        let mut fut = Box::pin(fut);
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    /// Primary language subtag of a BCP-47 locale ("zh-CN" -> "zh").
    fn lang_code_of_locale(locale: &str) -> String {
        locale
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    /// Enumerate every installed voice (OneCore + classic) via the WinRT engine.
    fn enum_voices() -> Vec<VoiceInfo> {
        let mut out = Vec::new();
        let Ok(all) = SpeechSynthesizer::AllVoices() else {
            return out;
        };
        let count = all.Size().unwrap_or(0);
        for i in 0..count {
            let Ok(vi) = all.GetAt(i) else { continue };
            let id = vi.Id().map(|h| h.to_string()).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let name = vi.DisplayName().map(|h| h.to_string()).unwrap_or_default();
            let langtag = vi.Language().map(|h| h.to_string()).unwrap_or_default();
            out.push(VoiceInfo {
                id,
                name,
                lang: lang_code_of_locale(&langtag),
            });
        }
        out
    }

    /// Point the synthesizer at the voice whose Id matches `id`. Returns whether it was found+set.
    fn set_voice_by_id(synth: &SpeechSynthesizer, id: &str) -> bool {
        let Ok(all) = SpeechSynthesizer::AllVoices() else {
            return false;
        };
        let count = all.Size().unwrap_or(0);
        for i in 0..count {
            let Ok(vi) = all.GetAt(i) else { continue };
            if vi.Id().map(|h| h.to_string()).unwrap_or_default() == id {
                return synth.SetVoice(&vi).is_ok();
            }
        }
        false
    }

    fn tts_thread(rx: mpsc::Receiver<Msg>) {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

        // MTA so block_on() on the synthesize async never deadlocks — the op
        // completes on a thread-pool thread and wakes us.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let synth = match SpeechSynthesizer::new() {
            Ok(s) => s,
            Err(e) => {
                log::error!("TTS: SpeechSynthesizer::new failed: {e}");
                return;
            }
        };
        let player = match MediaPlayer::new() {
            Ok(p) => p,
            Err(e) => {
                log::error!("TTS: MediaPlayer::new failed: {e}");
                return;
            }
        };

        // Refreshed lazily: re-enumerated on ListVoices (the user just opened Settings —
        // the natural moment to pick up a newly-installed voice) and on a resolve miss
        // during Speak, so a voice installed mid-session becomes usable without a restart
        // (audit 2026-07-12 C2 — the list used to be a one-time boot snapshot, so a voice
        // that appeared in the Settings dropdown via ListVoices' own fresh enumeration
        // still silently failed to play against this stale copy).
        let mut voices = enum_voices();
        let mut preferred_id: Option<String> = None;
        let mut current_id: Option<String> = None;

        for msg in rx {
            match msg {
                Msg::Speak(text, lang, request_hint, fallback_locale) => {
                    if text.is_empty() {
                        // Empty text = stop any in-flight speech.
                        let _ = player.Pause();
                        continue;
                    }
                    let target = if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
                        detect_lang(&text, &request_hint, &fallback_locale)
                    } else {
                        lang_code_of_locale(&lang)
                    };
                    // If a preferred voice is set but not in our cached list, re-enumerate
                    // once before treating it as unset — it may have been installed since
                    // boot (C2). Cheap and only fires on an actual miss.
                    if let Some(pid) = preferred_id.as_deref() {
                        if !voices.iter().any(|v| v.id == pid) {
                            voices = enum_voices();
                        }
                    }
                    // Resolve the preferred voice against the installed list — a stale
                    // id (e.g. saved before the WinRT migration) is treated as unset
                    // instead of silently leaving the OS default voice in place.
                    let preferred = preferred_id
                        .as_deref()
                        .and_then(|pid| voices.iter().find(|v| v.id == pid));
                    // The preferred voice applies to replies in its own language only.
                    // A reply in a different language auto-picks an installed voice for
                    // that language — handing e.g. Chinese text to an English preferred
                    // voice produces silence, which reads as "TTS is broken" (a real
                    // report: VOICE_LANGUAGE=auto + preferred Zira en-US + zh reply).
                    let chosen: Option<String> = match preferred {
                        Some(v) if v.lang.is_empty() || v.lang == target => Some(v.id.clone()),
                        _ => voices
                            .iter()
                            .find(|v| !v.lang.is_empty() && v.lang == target)
                            .map(|v| v.id.clone())
                            .or_else(|| {
                                // No language metadata at all → first voice beats silence.
                                if voices.iter().all(|v| v.lang.is_empty()) {
                                    voices.first().map(|v| v.id.clone())
                                } else {
                                    None
                                }
                            }),
                    };
                    log::info!(
                        "[tts] lang={lang} target={target} preferred={preferred_id:?} chosen={chosen:?}"
                    );
                    let Some(id) = chosen else {
                        log::warn!(
                            "TTS: no installed voice for language '{target}' — caption only"
                        );
                        continue;
                    };
                    if current_id.as_deref() != Some(id.as_str()) {
                        if set_voice_by_id(&synth, &id) {
                            current_id = Some(id);
                        } else {
                            log::warn!("TTS: voice id not found: {id}");
                        }
                    }
                    // Render to an audio stream and play it (replaces any current utterance).
                    match synth.SynthesizeTextToStreamAsync(&HSTRING::from(text.as_str())) {
                        Ok(op) => match block_on(op.into_future()) {
                            Ok(stream) => {
                                let ct = stream.ContentType().unwrap_or_default();
                                match MediaSource::CreateFromStream(&stream, &ct) {
                                    Ok(source) => {
                                        let _ = player.SetSource(&source);
                                        let _ = player.Play();
                                    }
                                    Err(e) => log::warn!("TTS: CreateFromStream failed: {e}"),
                                }
                            }
                            Err(e) => log::warn!("TTS: synthesize failed: {e}"),
                        },
                        Err(e) => log::warn!("TTS: synthesize failed: {e}"),
                    }
                }
                Msg::SetVoice(id) => {
                    preferred_id = if id.is_empty() { None } else { Some(id) };
                }
                Msg::ListVoices(reply) => {
                    // Re-enumerate and update the cache so a mid-session-installed voice
                    // is both returned to the Settings dropdown AND usable by Speak (C2).
                    voices = enum_voices();
                    let _ = reply.send(voices.clone());
                }
                Msg::Quit => break,
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
        pub fn speak(
            &self,
            _text: String,
            _lang: String,
            _request_hint: String,
            _fallback_locale: String,
        ) {
        }
        pub fn set_voice(&self, _voice_id: String) {}
        pub fn list_voices(&self) -> Vec<VoiceInfo> {
            vec![]
        }
    }
}

pub use imp::{TtsEngine, VoiceInfo};
