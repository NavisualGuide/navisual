"""Text-to-speech engine for AI Navigator.

Uses Windows SAPI5 directly via comtypes for reliable multi-utterance playback.
pyttsx3 wraps SAPI5 but its cached engine singleton silently fails on the second
say() call — using comtypes directly bypasses the wrapper.

Runs in a dedicated daemon thread so speech never blocks the Qt event loop.
Enable with ENABLE_TTS=true in .env.
"""

import ctypes
import ctypes.wintypes
import logging
import queue
import sys
import threading
import time
from typing import Optional

logger = logging.getLogger(__name__)

# SAPI5 SpVoice constants
_SVSFlagsAsync = 1
_SVSFPurgeBeforeSpeak = 2
_SpeechRunStateSpeaking = 2

_PM_REMOVE = 0x0001


def _pump_messages() -> None:
    """Pump pending Windows messages on the calling thread.

    SAPI5 COM objects live in an STA apartment and need their host thread to
    pump messages so internal callbacks (word-boundary, end-stream, etc.) are
    dispatched locally.  Without pumping, COM routes these through the nearest
    STA that IS pumping — typically Qt's main-thread message loop — which
    causes the Qt UI to freeze while audio is playing.

    Calling this in the TTS polling loop keeps SAPI's callbacks on the TTS
    thread and away from Qt.

    No-op on non-Windows platforms.
    """
    if sys.platform != "win32":
        return
    try:
        msg = ctypes.wintypes.MSG()
        while ctypes.windll.user32.PeekMessageW(
            ctypes.byref(msg), None, 0, 0, _PM_REMOVE
        ):
            ctypes.windll.user32.TranslateMessage(ctypes.byref(msg))
            ctypes.windll.user32.DispatchMessageW(ctypes.byref(msg))
    except Exception:
        pass


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
        """Background thread: process the speech queue via SAPI5 comtypes."""
        try:
            import comtypes
            import comtypes.client
            # COINIT_MULTITHREADED (0x0): COM callbacks go through COM's internal
            # thread pool rather than the calling thread's message queue. This
            # prevents SAPI5 callbacks from being marshalled into Qt's main-thread
            # STA (which is what caused the UI freeze with CoInitialize/STA).
            _COINIT_MULTITHREADED = 0x0
            ctypes.windll.ole32.CoInitializeEx(None, _COINIT_MULTITHREADED)
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

                # Poll until speech finishes or stop() is called.
                # With COINIT_MULTITHREADED, SAPI runs via proxy in COM's thread
                # pool — no message pumping needed on this thread.
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
