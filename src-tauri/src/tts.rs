//! Text-to-speech via Windows SAPI (ISpVoice COM interface).
//!
//! Runs on a dedicated STA thread so the async runtime is never blocked.
//! Calling `speak(text)` purges any in-flight speech and starts the new text
//! immediately. Calling `speak("")` stops speech without starting a new one.

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;
    use std::thread;

    enum Msg {
        Speak(String),
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
    }

    impl Drop for TtsEngine {
        fn drop(&mut self) {
            let _ = self.tx.send(Msg::Quit);
        }
    }

    fn tts_thread(rx: mpsc::Receiver<Msg>) {
        use windows::Win32::Media::Speech::{ISpVoice, SpVoice};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
        };

        unsafe {
            // ISpVoice requires STA.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

            let voice: ISpVoice = match CoCreateInstance(&SpVoice, None, CLSCTX_ALL) {
                Ok(v) => v,
                Err(e) => {
                    log::error!("TTS: SpVoice CoCreateInstance failed: {e}");
                    return;
                }
            };

            // SPF_ASYNC (1) | SPF_PURGEBEFORESPEAK (2) — returns immediately,
            // cancels whatever is currently being spoken.
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
                    Msg::Quit => break,
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct TtsEngine;
    impl TtsEngine {
        pub fn new() -> Self {
            Self
        }
        pub fn speak(&self, _text: String) {}
    }
}

pub use imp::TtsEngine;
