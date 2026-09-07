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
    use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceGender};

    #[derive(serde::Serialize, Clone)]
    pub struct VoiceInfo {
        pub id: String,
        pub name: String,
        /// Primary language code (e.g. "en", "zh"); "" if unknown.
        pub lang: String,
        /// Full BCP-47 tag as reported by the voice (e.g. "en-US") — SSML's xml:lang.
        pub locale: String,
        /// 1 = male, 2 = female, 0 = unknown. Used to keep every language in one
        /// instruction sounding like the SAME person.
        pub gender: u8,
        /// A Windows "Natural" (neural) voice. Far better than the legacy ones and
        /// preferred whenever installed — none are, by default.
        pub natural: bool,
    }

    const GENDER_UNKNOWN: u8 = 0;
    const GENDER_MALE: u8 = 1;
    const GENDER_FEMALE: u8 = 2;

    enum Msg {
        /// (text, language, request_hint, fallback_locale): `language` is a BCP-47
        /// locale, or "auto"/"" to detect. `request_hint` is the user's ORIGINAL
        /// request text — the LANGUAGE rule pins the reply language to it, so in auto
        /// mode its script is a better signal than re-guessing from an ambiguous reply.
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
    ///    the LANGUAGE rule pins the reply language to the request, so when the reply is
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
            let gender = match vi.Gender() {
                Ok(VoiceGender::Male) => GENDER_MALE,
                Ok(VoiceGender::Female) => GENDER_FEMALE,
                _ => GENDER_UNKNOWN,
            };
            // Windows 11's neural voices report no distinct flag; "Natural" in the
            // display name is how they identify themselves ("Microsoft Aria (Natural)").
            let natural = name.to_ascii_lowercase().contains("natural");
            out.push(VoiceInfo {
                id,
                name,
                lang: lang_code_of_locale(&langtag),
                locale: langtag,
                gender,
                natural,
            });
        }
        out
    }

    /// Which writing system a character belongs to. `Neutral` covers spaces, digits
    /// and punctuation, which join whichever run they fall inside rather than
    /// splitting it — otherwise "Ctrl+B" between two Chinese clauses would become
    /// three separate voice segments.
    #[derive(PartialEq, Clone, Copy, Debug)]
    enum Script {
        Neutral,
        Latin,
        Han,
        Kana,
        Hangul,
        Cyrillic,
        Arabic,
    }

    fn script_of(c: char) -> Script {
        let u = c as u32;
        if (0x3040..=0x30ff).contains(&u) {
            Script::Kana
        } else if (0xac00..=0xd7af).contains(&u) || (0x1100..=0x11ff).contains(&u) {
            Script::Hangul
        } else if (0x4e00..=0x9fff).contains(&u) || (0x3400..=0x4dbf).contains(&u) {
            Script::Han
        } else if (0x0400..=0x04ff).contains(&u) {
            Script::Cyrillic
        } else if (0x0600..=0x06ff).contains(&u) {
            Script::Arabic
        } else if c.is_alphabetic() {
            Script::Latin
        } else {
            Script::Neutral
        }
    }

    fn lang_for_script(sc: Script) -> Option<&'static str> {
        match sc {
            Script::Han => Some("zh"),
            Script::Kana => Some("ja"),
            Script::Hangul => Some("ko"),
            Script::Cyrillic => Some("ru"),
            Script::Arabic => Some("ar"),
            _ => None,
        }
    }

    /// Split text into consecutive runs of one script. Neutral characters extend the
    /// current run, so punctuation and spacing never fragment a sentence.
    fn script_runs(text: &str) -> Vec<(Script, String)> {
        let mut runs: Vec<(Script, String)> = Vec::new();
        for c in text.chars() {
            let sc = script_of(c);
            match runs.last_mut() {
                // Neutral joins whatever is open; a matching script continues it.
                Some((cur, buf)) if sc == Script::Neutral || sc == *cur => buf.push(c),
                _ => runs.push((if sc == Script::Neutral { Script::Latin } else { sc }, c.to_string())),
            }
        }
        runs
    }

    /// Best installed voice for `lang`, preferring a Natural (neural) voice and then
    /// one matching `want_gender`. Gender is the point: Windows substitutes a voice of
    /// its own choosing for text the current voice cannot pronounce, and on a default
    /// install that turns a male English instruction into a female Chinese one halfway
    /// through (reported live 2026-09-07). Ranking, not filtering — a language always
    /// beats a gender, since the wrong gender is odd but the wrong language is silence.
    fn pick_voice<'a>(voices: &'a [VoiceInfo], lang: &str, want_gender: u8) -> Option<&'a VoiceInfo> {
        voices
            .iter()
            .filter(|v| !v.lang.is_empty() && v.lang == lang)
            .max_by_key(|v| {
                (
                    v.natural,
                    want_gender != GENDER_UNKNOWN && v.gender == want_gender,
                )
            })
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    /// One SSML document that names a voice per segment, so a mixed-language
    /// instruction is spoken by voices we chose rather than by whatever Windows
    /// substitutes mid-sentence.
    fn build_ssml(segments: &[(String, String)], xml_lang: &str) -> String {
        let lang = if xml_lang.is_empty() { "en-US" } else { xml_lang };
        let mut out = format!(
            "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"{}\">",
            xml_escape(lang)
        );
        for (voice, text) in segments {
            out.push_str(&format!(
                "<voice name=\"{}\">{}</voice>",
                xml_escape(voice),
                xml_escape(text)
            ));
        }
        out.push_str("</speak>");
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
                    // The user's preferred voice also sets the GENDER for every
                    // auto-pick, so choosing a male voice does not yield a female one
                    // the moment the reply switches language.
                    let want_gender = preferred.map(|v| v.gender).unwrap_or(GENDER_UNKNOWN);
                    let chosen: Option<String> = match preferred {
                        Some(v) if v.lang.is_empty() || v.lang == target => Some(v.id.clone()),
                        _ => pick_voice(&voices, &target, want_gender)
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
                            // Cloned: `id` is still needed below to resolve the primary
                            // voice's gender/name for the mixed-script SSML path.
                            current_id = Some(id.clone());
                        } else {
                            log::warn!("TTS: voice id not found: {id}");
                        }
                    }
                    // Mixed-language handling. A voice reads Latin text no matter what
                    // language it is (a Chinese voice says "OneNote" fine), but an English
                    // voice cannot pronounce Han at all — so Windows silently substitutes
                    // one of its own, which is where a male English instruction acquired a
                    // female Chinese clause halfway through.
                    //
                    // So: split only the runs whose script this voice genuinely cannot
                    // read, and name a same-gender voice for them in SSML. When the text is
                    // single-script — the overwhelmingly common case — nothing changes and
                    // the plain-text path runs exactly as before.
                    let primary = voices.iter().find(|v| v.id == id);
                    let primary_gender = primary.map(|v| v.gender).unwrap_or(GENDER_UNKNOWN);
                    let primary_name = primary.map(|v| v.name.clone()).unwrap_or_default();
                    let primary_locale = primary.map(|v| v.locale.clone()).unwrap_or_default();

                    let mut segments: Vec<(String, String)> = Vec::new();
                    let mut needs_ssml = false;
                    if !primary_name.is_empty() {
                        for (sc, run) in script_runs(&text) {
                            let foreign = lang_for_script(sc).filter(|l| *l != target);
                            let voice = match foreign
                                .and_then(|l| pick_voice(&voices, l, primary_gender))
                            {
                                Some(v) => {
                                    needs_ssml = true;
                                    v.name.clone()
                                }
                                None => primary_name.clone(),
                            };
                            match segments.last_mut() {
                                Some((last_voice, buf)) if *last_voice == voice => buf.push_str(&run),
                                _ => segments.push((voice, run)),
                            }
                        }
                    }

                    let stream = if needs_ssml {
                        let ssml = build_ssml(&segments, &primary_locale);
                        log::info!(
                            "[tts] mixed-script: {} segment(s), voices {:?}",
                            segments.len(),
                            segments.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>()
                        );
                        match synth.SynthesizeSsmlToStreamAsync(&HSTRING::from(ssml.as_str())) {
                            Ok(op) => match block_on(op.into_future()) {
                                Ok(st) => Some(st),
                                Err(e) => {
                                    // Never lose the utterance to an SSML problem — fall
                                    // back to plain text, which is what shipped before.
                                    log::warn!("TTS: SSML synthesize failed ({e}) — plain text");
                                    None
                                }
                            },
                            Err(e) => {
                                log::warn!("TTS: SSML synthesize failed ({e}) — plain text");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(stream) = stream {
                        let ct = stream.ContentType().unwrap_or_default();
                        match MediaSource::CreateFromStream(&stream, &ct) {
                            Ok(source) => {
                                let _ = player.SetSource(&source);
                                let _ = player.Play();
                            }
                            Err(e) => log::warn!("TTS: CreateFromStream failed: {e}"),
                        }
                        continue;
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

    #[cfg(test)]
    mod tests {
        use super::*;

        fn v(name: &str, lang: &str, gender: u8, natural: bool) -> VoiceInfo {
            VoiceInfo {
                id: format!("id-{name}"),
                name: name.into(),
                lang: lang.into(),
                locale: format!("{lang}-XX"),
                gender,
                natural,
            }
        }

        #[test]
        fn neutral_chars_do_not_split_runs() {
            // "Ctrl+B" must stay in one Latin run — punctuation and digits join the
            // run they sit in rather than starting new voice segments.
            let runs = script_runs("Press Ctrl+B now");
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].0, Script::Latin);
        }

        #[test]
        fn mixed_script_splits_at_the_script_boundary() {
            let runs = script_runs("Click 清理 to continue");
            let scripts: Vec<Script> = runs.iter().map(|(s, _)| *s).collect();
            assert_eq!(scripts, vec![Script::Latin, Script::Han, Script::Latin]);
            // The trailing space after the Han run attaches to it, not to the next run.
            assert!(runs[1].1.starts_with('清'));
        }

        #[test]
        fn same_gender_wins_within_a_language() {
            // The live case: an en-US male primary with three zh-CN voices installed,
            // two female. Windows picked a female one; we must pick Kangkang.
            let voices = vec![
                v("Huihui", "zh", GENDER_FEMALE, false),
                v("Yaoyao", "zh", GENDER_FEMALE, false),
                v("Kangkang", "zh", GENDER_MALE, false),
            ];
            let got = pick_voice(&voices, "zh", GENDER_MALE).unwrap();
            assert_eq!(got.name, "Kangkang");
        }

        #[test]
        fn natural_outranks_gender() {
            // A neural voice of the "wrong" gender still sounds far better than a
            // legacy one of the right gender.
            let voices = vec![
                v("Kangkang", "zh", GENDER_MALE, false),
                v("Xiaoxiao Natural", "zh", GENDER_FEMALE, true),
            ];
            let got = pick_voice(&voices, "zh", GENDER_MALE).unwrap();
            assert_eq!(got.name, "Xiaoxiao Natural");
        }

        #[test]
        fn language_is_never_traded_for_gender() {
            let voices = vec![v("David", "en", GENDER_MALE, false), v("Huihui", "zh", GENDER_FEMALE, false)];
            assert_eq!(pick_voice(&voices, "zh", GENDER_MALE).unwrap().name, "Huihui");
            assert!(pick_voice(&voices, "ja", GENDER_MALE).is_none());
        }

        #[test]
        fn ssml_escapes_and_names_each_voice() {
            let segs = vec![
                ("Microsoft David".to_string(), "Click <Save> & wait ".to_string()),
                ("Microsoft Kangkang".to_string(), "清理".to_string()),
            ];
            let x = build_ssml(&segs, "en-US");
            assert!(x.starts_with("<speak version=\"1.0\""));
            assert!(x.contains("xml:lang=\"en-US\""));
            assert!(x.contains("<voice name=\"Microsoft David\">"));
            assert!(x.contains("<voice name=\"Microsoft Kangkang\">清理</voice>"));
            assert!(x.contains("&lt;Save&gt; &amp; wait"));
            assert!(x.ends_with("</speak>"));
        }

        /// Proves Windows ACCEPTS the SSML we generate — the one thing unit tests
        /// cannot settle, since a malformed document fails only at synthesis time.
        /// Renders to a stream; plays nothing.
        #[test]
        #[ignore = "needs a desktop session with voices installed: cargo test --lib -- --ignored ssml_renders"]
        fn ssml_renders_on_this_machine() {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let voices = enum_voices();
            let en = pick_voice(&voices, "en", GENDER_MALE).expect("an en voice");
            let zh = pick_voice(&voices, "zh", en.gender).expect("a zh voice");
            eprintln!("primary={} ({}), secondary={} ({})", en.name, en.gender, zh.name, zh.gender);
            assert_eq!(zh.gender, en.gender, "same-gender pick should be available here");
            let segs = vec![
                (en.name.clone(), "Click ".to_string()),
                (zh.name.clone(), "清理".to_string()),
                (en.name.clone(), " to continue.".to_string()),
            ];
            let ssml = build_ssml(&segs, &en.locale);
            eprintln!("{ssml}");
            let synth = SpeechSynthesizer::new().expect("synth");
            let op = synth
                .SynthesizeSsmlToStreamAsync(&HSTRING::from(ssml.as_str()))
                .expect("SSML call accepted");
            let stream = block_on(op.into_future()).expect("SSML rendered");
            assert!(stream.Size().unwrap_or(0) > 0, "rendered stream is empty");
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
        pub locale: String,
        pub gender: u8,
        pub natural: bool,
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
