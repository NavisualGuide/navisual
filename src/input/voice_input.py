"""Voice input stub for AI Navigator v0.2.

Voice input will use Whisper (local or cloud) for speech-to-text.
This module provides the interface that will be implemented in v0.2.
"""

import logging

logger = logging.getLogger(__name__)


class VoiceInput:
    """Voice input handler (stub for v0.2).

    Will support:
    - Continuous listening mode
    - Push-to-talk via hotkey
    - Local STT via Whisper.cpp
    - Cloud STT via Whisper API / Deepgram
    """

    def __init__(self) -> None:
        logger.info("VoiceInput: stub initialized (available in v0.2)")

    async def start(self) -> None:
        """Start listening for voice input."""
        logger.info("VoiceInput.start(): not available until v0.2")

    async def stop(self) -> None:
        """Stop listening."""
        logger.info("VoiceInput.stop(): not available until v0.2")

    async def get_transcript(self) -> str | None:
        """Get the latest transcribed text, if any."""
        return None

    @property
    def is_available(self) -> bool:
        """Whether voice input is available."""
        return False
