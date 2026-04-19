"""Clipboard manager for AI Navigator.

Provides a thin wrapper around pyperclip for copying CLI commands
and generated text to the system clipboard.
"""

import logging

import pyperclip

logger = logging.getLogger(__name__)


def copy_to_clipboard(text: str) -> bool:
    """Copy text to the system clipboard.

    Returns True if successful, False otherwise.
    """
    try:
        pyperclip.copy(text)
        logger.info("Copied to clipboard: %s", text[:80] + ("..." if len(text) > 80 else ""))
        return True
    except pyperclip.PyperclipException as e:
        logger.error("Failed to copy to clipboard: %s", e)
        return False


def get_clipboard() -> str | None:
    """Get the current clipboard contents.

    Returns None if clipboard is empty or inaccessible.
    """
    try:
        return pyperclip.paste()
    except pyperclip.PyperclipException as e:
        logger.error("Failed to read clipboard: %s", e)
        return None
