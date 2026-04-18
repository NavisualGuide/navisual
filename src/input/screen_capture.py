"""On-demand screen capture for AI Navigator.

Uses mss for fast, cross-platform screenshots. Provides both full-resolution
captures for API calls and low-resolution thumbnails for pixel-diff monitoring.
"""

import base64
import io
import logging
import sys
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

    # Capture the full virtual desktop (all monitors combined).
    # monitors[0] is the bounding box of all screens; monitors[1] is primary only.
    # Using monitors[0] ensures apps on secondary monitors are visible to the AI.
    monitor = sct.monitors[0]
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

    monitor = sct.monitors[0]  # Full virtual desktop (all monitors)
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


def capture_screenshot_raw() -> Image.Image:
    """Capture the full virtual desktop at native resolution (no downscaling).

    Used as input to prepare_api_image() before cropping/downscaling.
    """
    sct = _get_sct()
    monitor = sct.monitors[0]
    raw = sct.grab(monitor)
    return Image.frombytes("RGB", (raw.width, raw.height), raw.rgb)


# Last foreground window rect that belonged to a different process.
# Used so that when AI Navigator itself is focused (user typed / clicked → Next),
# we still crop to the target app instead of sending the full virtual desktop.
_last_target_window_rect: Optional[tuple[int, int, int, int]] = None

# The actual crop rect used in the most recent prepare_api_image() call,
# in virtual-desktop physical pixels (x, y, w, h).  None when not cropped.
# Zone-hint overlay uses this to map AI zone coords (relative to the cropped
# API image) back to real screen positions.
_last_api_crop_rect: Optional[tuple[int, int, int, int]] = None


def get_last_api_crop_rect() -> Optional[tuple[int, int, int, int]]:
    """Return the crop rect used in the last prepare_api_image() call.

    Returns (x, y, w, h) in virtual-desktop physical pixels, or None if the
    last API image was not cropped (full virtual desktop was sent).
    """
    return _last_api_crop_rect

# HWND of the AI Navigator panel window.
# When set, prepare_api_image() blacks out that rect in the captured image so
# the panel never appears in screenshots sent to the AI — without needing
# WDA_EXCLUDEFROMCAPTURE, which causes DWM to flash both monitors on every capture.
_panel_hwnd: Optional[int] = None

# Current overlay bbox (x, y, w, h) in virtual-desktop physical pixels.
# Set by the overlay when it shows/clears so prepare_api_image() can blank it.
_overlay_bbox: Optional[tuple[int, int, int, int]] = None


def set_panel_hwnd(hwnd: int) -> None:
    """Register the AI Navigator panel's HWND for software-based exclusion from API images."""
    global _panel_hwnd
    _panel_hwnd = hwnd


def set_overlay_bbox(bbox: Optional[tuple[int, int, int, int]]) -> None:
    """Register (or clear) the current overlay bbox for software-based exclusion from API images."""
    global _overlay_bbox
    _overlay_bbox = bbox


def _get_panel_rect_in_image(img: Image.Image) -> Optional[tuple[int, int, int, int]]:
    """Return the panel's bounding box in image-local pixel coords, or None."""
    if _panel_hwnd is None or sys.platform != "win32":
        return None
    try:
        import ctypes

        class _RECT(ctypes.Structure):
            _fields_ = [
                ("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long),
            ]

        rect = _RECT()
        ctypes.windll.user32.GetWindowRect(_panel_hwnd, ctypes.byref(rect))
        x, y = rect.left, rect.top
        w = rect.right - rect.left
        h = rect.bottom - rect.top
        if w <= 0 or h <= 0:
            return None
        # Convert virtual coords to image-local coords
        sct = _get_sct()
        ox = sct.monitors[0]["left"]
        oy = sct.monitors[0]["top"]
        ix = x - ox
        iy = y - oy
        ix2 = ix + w
        iy2 = iy + h
        # Clamp to image bounds
        ix = max(0, min(ix, img.width))
        iy = max(0, min(iy, img.height))
        ix2 = max(0, min(ix2, img.width))
        iy2 = max(0, min(iy2, img.height))
        if ix2 > ix and iy2 > iy:
            return (ix, iy, ix2, iy2)
    except Exception:
        pass
    return None


def get_foreground_window_rect() -> Optional[tuple[int, int, int, int]]:
    """Return the foreground window bounds in virtual-desktop physical pixels (x, y, w, h).

    Uses DwmGetWindowAttribute with DWMWA_EXTENDED_FRAME_BOUNDS to get the
    precise window rect, excluding the invisible drop-shadow region.
    Falls back to GetWindowRect if DWM fails.

    When AI Navigator is the foreground window (user clicked the panel or typed
    into the input box), returns the LAST known target-app rect so the AI still
    sees the target app rather than the full virtual desktop (which would show a
    black WDA_EXCLUDEFROMCAPTURE rectangle where AI Navigator sits).

    Returns None only if no target window has been seen yet or bounds are invalid.
    Windows only — returns None on other platforms.
    """
    global _last_target_window_rect

    if sys.platform != "win32":
        return None

    import ctypes
    import ctypes.wintypes
    import os

    hwnd = ctypes.windll.user32.GetForegroundWindow()
    if not hwnd:
        return _last_target_window_rect

    # If the foreground window is our own process, reuse the last known target
    # rect so the AI always sees the target app, never our own panel.
    pid = ctypes.c_ulong(0)
    ctypes.windll.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    if pid.value == os.getpid():
        return _last_target_window_rect

    class _RECT(ctypes.Structure):
        _fields_ = [
            ("left", ctypes.c_long), ("top", ctypes.c_long),
            ("right", ctypes.c_long), ("bottom", ctypes.c_long),
        ]

    rect = _RECT()
    DWMWA_EXTENDED_FRAME_BOUNDS = 9
    hr = ctypes.windll.dwmapi.DwmGetWindowAttribute(
        hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, ctypes.byref(rect), ctypes.sizeof(rect)
    )
    if hr != 0:
        # DWM unavailable — fall back to GetWindowRect
        ctypes.windll.user32.GetWindowRect(hwnd, ctypes.byref(rect))

    x, y = rect.left, rect.top
    w = rect.right - rect.left
    h = rect.bottom - rect.top
    if w <= 0 or h <= 0:
        return _last_target_window_rect

    result = (x, y, w, h)
    _last_target_window_rect = result
    return result


def prepare_api_image(
    raw_img: Image.Image,
    force_full: bool = False,
    quality: int = 85,
) -> str:
    """Prepare a raw screenshot for API transmission with token optimization.

    Steps:
    1. Crop to the foreground window (if enable_active_window_crop is True and
       force_full is False) — reduces tokens by up to 80% on typical tasks.
    2. Downscale to max_api_screenshot_width × max_api_screenshot_height
       (768×432 by default = 2 vision tiles, ~75% token reduction vs 1920×1080).

    Args:
        raw_img: Full-resolution virtual desktop image from capture_screenshot_raw().
        force_full: Skip window crop even if enable_active_window_crop is True.
                    Set when the AI needs to see the full desktop.
        quality: JPEG quality for the output (default 85).

    Returns:
        Base64-encoded JPEG string for the AI API.
    """
    config = get_config()
    img = raw_img

    # Black out the AI Navigator panel and overlay before any cropping so they
    # never appear in API images — software approach that avoids DWM flash from
    # WDA_EXCLUDEFROMCAPTURE.
    panel_rect = _get_panel_rect_in_image(img)
    if panel_rect or _overlay_bbox:
        img = img.copy()
        from PIL import ImageDraw
        draw = ImageDraw.Draw(img)
        if panel_rect:
            draw.rectangle(panel_rect, fill=(0, 0, 0))
        if _overlay_bbox:
            # Expand slightly to cover arrow shaft and arrowhead drawn outside bbox
            ox, oy, ow, oh = _overlay_bbox
            sct = _get_sct()
            origin_x = sct.monitors[0]["left"]
            origin_y = sct.monitors[0]["top"]
            pad = 140  # arrow offset is 130px; add margin
            ix  = max(0, ox - origin_x - pad)
            iy  = max(0, oy - origin_y - pad)
            ix2 = min(img.width,  ox - origin_x + ow + pad)
            iy2 = min(img.height, oy - origin_y + oh + pad)
            if ix2 > ix and iy2 > iy:
                draw.rectangle((ix, iy, ix2, iy2), fill=(0, 0, 0))
        del draw

    global _last_api_crop_rect
    cropped = False
    if config.enable_active_window_crop and not force_full:
        win_rect = get_foreground_window_rect()
        if win_rect:
            # The mss image starts at pixel (0,0) = virtual coord (monitors[0].left, monitors[0].top).
            # Convert virtual coords to image-local coords by subtracting the virtual origin.
            sct = _get_sct()
            ox = sct.monitors[0]["left"]
            oy = sct.monitors[0]["top"]
            wx, wy, ww, wh = win_rect
            ix = wx - ox
            iy = wy - oy
            ix2 = ix + ww
            iy2 = iy + wh
            # Clamp to image bounds
            ix = max(0, min(ix, img.width))
            iy = max(0, min(iy, img.height))
            ix2 = max(0, min(ix2, img.width))
            iy2 = max(0, min(iy2, img.height))
            if ix2 > ix and iy2 > iy:
                img = img.crop((ix, iy, ix2, iy2))
                cropped = True
                # Record the actual crop rect in virtual-desktop coords so
                # zone-hint overlay can map AI zone coords back to screen.
                _last_api_crop_rect = (ox + ix, oy + iy, ix2 - ix, iy2 - iy)

    if not cropped:
        _last_api_crop_rect = None

    # Downscale to API max dimensions.
    # force_full requests (Start Menu, taskbar, system dialogs) get a larger cap
    # because the full-desktop context matters more than token savings, and these
    # requests are rare (typically 0–2 per session on browser tasks).
    if force_full:
        max_w = config.max_api_full_screenshot_width
        max_h = config.max_api_full_screenshot_height
    else:
        max_w = config.max_api_screenshot_width
        max_h = config.max_api_screenshot_height
    pre_w, pre_h = img.width, img.height
    if img.width > max_w or img.height > max_h:
        img = img.copy()  # thumbnail() is in-place — avoid mutating raw_img
        img.thumbnail((max_w, max_h), Image.Resampling.LANCZOS)

    logger.debug(
        "API image: %dx%d → %dx%d (%.0f%% scale, cropped=%s, force_full=%s)",
        pre_w, pre_h, img.width, img.height,
        100.0 * img.width / pre_w,
        cropped, force_full,
    )
    return image_to_b64(img, fmt="JPEG", quality=quality)


def capture_for_guidance(
    force_full: bool = False,
) -> tuple[str, Image.Image]:
    """Single-capture entry point for the guidance loop.

    Captures the virtual desktop once and derives two images:
    - api_b64: cropped (foreground window) + downscaled to API max dims for token reduction.
    - ocr_img: full virtual desktop downscaled to max_screenshot dims for local OCR.

    Args:
        force_full: Send the full desktop to the API (skip window crop).

    Returns:
        (api_b64, ocr_img)
    """
    config = get_config()
    raw_img = capture_screenshot_raw()

    # OCR image: downscale to max_screenshot dims (unchanged from prior behaviour)
    ocr_img = raw_img.copy()
    max_w, max_h = config.max_screenshot_width, config.max_screenshot_height
    if ocr_img.width > max_w or ocr_img.height > max_h:
        ocr_img.thumbnail((max_w, max_h), Image.Resampling.LANCZOS)

    # API image: crop + downscale for token efficiency
    api_b64 = prepare_api_image(raw_img, force_full=force_full)

    return api_b64, ocr_img
