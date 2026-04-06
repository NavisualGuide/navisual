"""Chat input handler for AI Navigator.

Signal-based intermediary between the UI layer and the core engine.
Manages message submission and callback dispatching.
"""

import logging
from typing import Callable, Optional

logger = logging.getLogger(__name__)


class ChatInputHandler:
    """Handles user chat input and dispatches to registered callbacks.

    Acts as a bridge between UI widgets (main window, floating window)
    and the core guidance engine.
    """

    def __init__(self) -> None:
        self._callbacks: list[Callable[[str], None]] = []
        self._history: list[str] = []
        self._history_index: int = -1

    def on_message(self, callback: Callable[[str], None]) -> None:
        """Register a callback for when user submits a message."""
        self._callbacks.append(callback)

    def emit_message(self, text: str) -> None:
        """Called by the UI when user submits a message.

        Dispatches to all registered callbacks.
        """
        text = text.strip()
        if not text:
            return

        self._history.append(text)
        self._history_index = len(self._history)

        logger.info("User message: %s", text[:100])
        for callback in self._callbacks:
            try:
                callback(text)
            except Exception as e:
                logger.error("Chat input callback error: %s", e)

    def get_history_prev(self) -> Optional[str]:
        """Navigate to previous message in history (for up-arrow)."""
        if not self._history:
            return None
        self._history_index = max(0, self._history_index - 1)
        return self._history[self._history_index]

    def get_history_next(self) -> Optional[str]:
        """Navigate to next message in history (for down-arrow)."""
        if not self._history:
            return None
        self._history_index = min(len(self._history), self._history_index + 1)
        if self._history_index >= len(self._history):
            return ""
        return self._history[self._history_index]
