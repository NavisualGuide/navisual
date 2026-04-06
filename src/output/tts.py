"""Text-to-speech stub for AI Navigator v0.2.

TTS will provide audio narration of navigation instructions.
Ships paired with voice input in v0.2.
"""

import logging

logger = logging.getLogger(__name__)


class TTSEngine:
    """Text-to-speech engine (stub for v0.2).

    Will support:
    - Local TTS via Piper TTS or system TTS
    - Cloud TTS via OpenAI TTS / ElevenLabs
    - Configurable voice and speed
    """

    def __init__(self) -> None:
        logger.info("TTSEngine: stub initialized (available in v0.2)")

    async def speak(self, text: str) -> None:
        """Speak the given text aloud."""
        logger.debug("TTSEngine.speak(): would say '%s' (not available until v0.2)", text[:50])

    def stop(self) -> None:
        """Stop any ongoing speech."""
        pass

    @property
    def is_available(self) -> bool:
        """Whether TTS is available."""
        return False
