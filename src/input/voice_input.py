"""Voice input for AI Navigator.

Uses SpeechRecognition + PyAudio for push-to-talk voice input.
Listens on a background thread; recognized text is passed to a callback
on the Qt main thread via a Signal.

Enable with ENABLE_VOICE_INPUT=true in .env.
Trigger with the floating window's microphone button or the voice hotkey.
"""

import logging
import queue
import threading
from typing import Callable, Optional

logger = logging.getLogger(__name__)


class VoiceInput:
    """Push-to-talk voice input using SpeechRecognition + Google STT.

    Architecture:
    - listen() is called from the main thread (e.g. button press)
    - Recording happens in a background thread so UI doesn't freeze
    - on_transcript callback is called with the recognized text
    - Uses Google Web Speech API (free, requires internet)

    Usage:
        voice = VoiceInput(on_transcript=engine.handle_user_message)
        voice.start()
        voice.listen()     # triggered by PTT button / hotkey
        voice.shutdown()
    """

    def __init__(
        self,
        on_transcript: Optional[Callable[[str], None]] = None,
        language: str = "en-US",
        energy_threshold: int = 300,
        pause_threshold: float = 0.8,
    ) -> None:
        self._on_transcript = on_transcript
        self._language = language
        self._energy_threshold = energy_threshold
        self._pause_threshold = pause_threshold
        self._listen_queue: queue.Queue[bool] = queue.Queue()
        self._thread: Optional[threading.Thread] = None
        self._available = False
        self._is_listening = False

        try:
            import speech_recognition as sr  # noqa: F401
            import pyaudio  # noqa: F401
            self._available = True
        except ImportError as e:
            logger.warning(
                "VoiceInput: %s — voice input disabled. "
                "Install with: pip install SpeechRecognition pyaudio", e
            )

    @property
    def is_available(self) -> bool:
        return self._available

    @property
    def is_listening(self) -> bool:
        return self._is_listening

    def start(self) -> None:
        """Start the voice input background thread."""
        if not self._available:
            return
        self._thread = threading.Thread(
            target=self._run_loop, daemon=True, name="voice-input"
        )
        self._thread.start()
        logger.info("VoiceInput started (language=%s)", self._language)

    def listen(self) -> None:
        """Trigger a single push-to-talk listen cycle (non-blocking).

        Call this from a button press or hotkey. The background thread will
        record audio, transcribe it, and call on_transcript with the result.
        """
        if not self._available or self._is_listening:
            return
        self._listen_queue.put(True)

    def shutdown(self) -> None:
        """Stop the background thread."""
        if self._thread and self._thread.is_alive():
            self._listen_queue.put(False)  # Sentinel
            self._thread.join(timeout=3)
        logger.info("VoiceInput stopped")

    def _run_loop(self) -> None:
        """Background thread: wait for listen triggers and process them."""
        import speech_recognition as sr

        recognizer = sr.Recognizer()
        recognizer.energy_threshold = self._energy_threshold
        recognizer.pause_threshold = self._pause_threshold
        recognizer.dynamic_energy_threshold = True

        while True:
            trigger = self._listen_queue.get()
            if trigger is False:  # Sentinel
                break

            self._is_listening = True
            logger.debug("VoiceInput: listening...")

            try:
                with sr.Microphone() as source:
                    # Brief ambient noise adjustment
                    recognizer.adjust_for_ambient_noise(source, duration=0.3)
                    try:
                        audio = recognizer.listen(source, timeout=8, phrase_time_limit=15)
                    except sr.WaitTimeoutError:
                        logger.debug("VoiceInput: no speech detected (timeout)")
                        self._is_listening = False
                        continue

                # Recognize using Google Web Speech API (free)
                try:
                    text = recognizer.recognize_google(audio, language=self._language)
                    logger.info("VoiceInput recognized: %s", text)
                    if text and self._on_transcript:
                        self._on_transcript(text)
                except sr.UnknownValueError:
                    logger.debug("VoiceInput: speech not understood")
                except sr.RequestError as e:
                    logger.warning("VoiceInput: recognition service error: %s", e)

            except Exception as e:
                logger.error("VoiceInput error: %s", e)
            finally:
                self._is_listening = False
