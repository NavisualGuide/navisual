"""On-demand screen capture for AI Navigator.

Uses mss for fast, cross-platform screenshots. Provides both full-resolution
captures for API calls and low-resolution thumbnails for pixel-diff monitoring.
"""

import base64
import io
import logging
from typing import Optional

import mss
from PIL import Image

from src.config import get_config

logger = logging.getLogger(__name__)

# Module-level mss instance (reusable, thread-safe for reads)
_sct: Optional[mss.mss] = None


def _get_sct() -> mss.mss:
    """Get or create the mss screenshot instance."""
    global _sct
    if _sct is None:
        _sct = mss.mss()
    return _sct


def capture_screenshot() -> Image.Image:
    """Capture the full screen and return as a PIL Image.

    Resizes to max dimensions from config if the screen is larger.
    """
    config = get_config()
    sct = _get_sct()

    # Capture primary monitor (index 0 is all monitors, 1 is primary)
    monitor = sct.monitors[1] if len(sct.monitors) > 1 else sct.monitors[0]
    raw = sct.grab(monitor)

    img = Image.frombytes("RGB", (raw.width, raw.height), raw.rgb)

    # Resize if larger than max dimensions
    max_w, max_h = config.max_screenshot_width, config.max_screenshot_height
    if img.width > max_w or img.height > max_h:
        img.thumbnail((max_w, max_h), Image.Resampling.LANCZOS)

    return img


def capture_screenshot_b64(quality: int = 85) -> tuple[str, Image.Image]:
    """Capture screenshot and return as (base64 JPEG string, PIL Image).

    Uses JPEG for smaller payload size to the API.
    Returns both for local OCR use and API transmission.
    """
    img = capture_screenshot()
    buffer = io.BytesIO()
    img.save(buffer, format="JPEG", quality=quality)
    b64 = base64.b64encode(buffer.getvalue()).decode("utf-8")
    return b64, img


def capture_screenshot_png_b64() -> tuple[str, Image.Image]:
    """Capture screenshot and return as (base64 PNG string, PIL Image).

    PNG for highest quality (lossless). Larger payload but better for OCR.
    """
    img = capture_screenshot()
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    b64 = base64.b64encode(buffer.getvalue()).decode("utf-8")
    return b64, img


def capture_thumbnail() -> Image.Image:
    """Capture a low-resolution thumbnail for fast pixel-diff comparison.

    Used by the ScreenMonitor's diff worker at ~10fps.
    """
    config = get_config()
    sct = _get_sct()

    monitor = sct.monitors[1] if len(sct.monitors) > 1 else sct.monitors[0]
    raw = sct.grab(monitor)

    img = Image.frombytes("RGB", (raw.width, raw.height), raw.rgb)
    img = img.resize(
        (config.diff_thumbnail_width, config.diff_thumbnail_height),
        Image.Resampling.NEAREST,  # Fastest resize method
    )
    return img


def image_to_b64(img: Image.Image, fmt: str = "JPEG", quality: int = 85) -> str:
    """Convert a PIL Image to a base64-encoded string."""
    buffer = io.BytesIO()
    save_kwargs = {"format": fmt}
    if fmt.upper() == "JPEG":
        save_kwargs["quality"] = quality
    img.save(buffer, **save_kwargs)
    return base64.b64encode(buffer.getvalue()).decode("utf-8")


def image_to_bytes(img: Image.Image, fmt: str = "PNG") -> bytes:
    """Convert a PIL Image to raw bytes."""
    buffer = io.BytesIO()
    img.save(buffer, format=fmt)
    return buffer.getvalue()
