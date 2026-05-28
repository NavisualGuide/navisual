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
                .name("tts-winrt".into())
                .spawn(move || tts_thread(rx))
                .expect("tts thread spawn");
            Self { tx }
        }

        pub fn speak(&self, text: String, lang: String) {
            let _ = self.tx.send(Msg::Speak(text, lang));
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

        let voices = enum_voices();
        let mut preferred_id: Option<String> = None;
        let mut current_id: Option<String> = None;

        for msg in rx {
            match msg {
                Msg::Speak(text, lang) => {
                    if text.is_empty() {
                        // Empty text = stop any in-flight speech.
                        let _ = player.Pause();
                        continue;
                    }
                    let target = if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
                        detect_lang(&text)
                    } else {
                        lang_code_of_locale(&lang)
                    };
                    // A user-selected preferred voice is used as-is — picking a voice
                    // must be predictable. Auto (no preferred) picks an installed voice
                    // for the reply language, falling back to the first voice rather than
                    // going silent when language metadata is unavailable.
                    let chosen: Option<String> = if let Some(pid) = preferred_id.clone() {
                        Some(pid)
                    } else {
                        voices
                            .iter()
                            .find(|v| !v.lang.is_empty() && v.lang == target)
                            .map(|v| v.id.clone())
                            .or_else(|| {
                                if voices.iter().all(|v| v.lang.is_empty()) {
                                    voices.first().map(|v| v.id.clone())
                                } else {
                                    None
                                }
                            })
                    };
                    log::info!(
                        "[tts] lang={lang} target={target} preferred={preferred_id:?} chosen={chosen:?}"
                    );
                    let Some(id) = chosen else {
                        log::warn!("TTS: no installed voice for language '{target}' — caption only");
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
                    let _ = reply.send(enum_voices());
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
        pub fn speak(&self, _text: String, _lang: String) {}
        pub fn set_voice(&self, _voice_id: String) {}
        pub fn list_voices(&self) -> Vec<VoiceInfo> {
            vec![]
        }
    }
}

pub use imp::{TtsEngine, VoiceInfo};
