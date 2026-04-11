"""Text-to-speech engine for AI Navigator.

Uses Windows SAPI5 directly via comtypes for reliable multi-utterance playback.
pyttsx3 wraps SAPI5 but its cached engine singleton silently fails on the second
say() call — using comtypes directly bypasses the wrapper.

Runs in a dedicated daemon thread so speech never blocks the Qt event loop.
Enable with ENABLE_TTS=true in .env.
"""

import logging
import queue
import threading
import time
from typing import Optional

logger = logging.getLogger(__name__)

# SAPI5 SpVoice constants
_SVSFlagsAsync = 1
_SVSFPurgeBeforeSpeak = 2
_SpeechRunStateSpeaking = 2


class TTSEngine:
    """Text-to-speech engine using Windows SAPI5 via comtypes.

    Speech runs in a dedicated thread. comtypes SAPI5 is used directly
    (not via pyttsx3) because pyttsx3's cached engine singleton silently
    fails after the first runAndWait() completes on Windows.

    Usage:
        tts = TTSEngine(rate=175)
        tts.start()
        tts.speak("Click the Search button at the top of the page.")
        tts.stop()        # interrupt current speech
        tts.shutdown()    # stop thread and release engine
    """

    def __init__(self, rate: int = 175, volume: float = 1.0) -> None:
        self._rate = rate
        self._volume = volume
        self._queue: queue.Queue[Optional[str]] = queue.Queue()
        self._thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        self._available = False

        try:
            import comtypes.client  # noqa: F401
            self._available = True
        except ImportError:
            logger.warning(
                "TTSEngine: comtypes not available — TTS disabled. "
                "comtypes should be installed with uiautomation."
            )

    @property
    def is_available(self) -> bool:
        return self._available

    def start(self) -> None:
        """Start the TTS background thread."""
        if not self._available:
            return
        self._thread = threading.Thread(
            target=self._run_loop, daemon=True, name="tts-worker"
        )
        self._thread.start()
        logger.info("TTSEngine started (rate=%d, volume=%.1f)", self._rate, self._volume)

    def speak(self, text: str) -> None:
        """Queue text for speech. Returns immediately (non-blocking).

        If speech is already queued, the new text replaces it so the user
        hears the latest instruction rather than a backlog of old ones.
        """
        if not self._available:
            return
        # Drain old queued items — only keep the latest instruction
        while not self._queue.empty():
            try:
                self._queue.get_nowait()
            except queue.Empty:
                break
        self._queue.put(text)

    def stop(self) -> None:
        """Interrupt any speech currently being spoken."""
        self._stop_event.set()

    def shutdown(self) -> None:
        """Stop the TTS thread and release the engine."""
        self._stop_event.set()
        if self._thread and self._thread.is_alive():
            self._queue.put(None)  # Sentinel
            self._thread.join(timeout=3)
        logger.info("TTSEngine stopped")

    def _run_loop(self) -> None:
        """Background thread: process the speech queue via SAPI5 comtypes.

        Creates one SAPI5 SpVoice COM object for the lifetime of the thread
        (COM STA — must be created and used in the same thread). Speaks
        asynchronously and polls for completion so stop() can interrupt
        mid-utterance without blocking.
        """
        try:
            import comtypes
            import comtypes.client
            comtypes.CoInitialize()  # STA for this thread
        except Exception as e:
            logger.error("TTSEngine: COM init failed: %s", e)
            self._available = False
            return

        try:
            voice = comtypes.client.CreateObject("SAPI.SpVoice")
            # SAPI5 rate: -10 to +10. Map from WPM (default 170 WPM ≈ rate 0).
            sapi_rate = max(-10, min(10, round((self._rate - 170) / 25)))
            voice.Rate = sapi_rate
            voice.Volume = int(self._volume * 100)
            logger.debug("SAPI5 voice ready (sapi_rate=%d, volume=%d)", sapi_rate, voice.Volume)
        except Exception as e:
            logger.error("TTSEngine: SAPI5 SpVoice creation failed: %s", e)
            self._available = False
            return

        while True:
            try:
                text = self._queue.get(timeout=1.0)
                if text is None:  # Sentinel — shut down
                    break

                logger.debug("TTS speaking: %s", text[:60])
                self._stop_event.clear()
                voice.Speak(text, _SVSFlagsAsync)

                # Poll until speech finishes or stop() is called
                while True:
                    try:
                        if voice.Status.RunningState != _SpeechRunStateSpeaking:
                            break
                    except Exception:
                        break
                    if self._stop_event.is_set():
                        voice.Speak("", _SVSFPurgeBeforeSpeak | _SVSFlagsAsync)
                        break
                    time.sleep(0.05)

            except queue.Empty:
                continue
            except Exception as e:
                logger.warning("TTS speak error: %s", e, exc_info=True)
