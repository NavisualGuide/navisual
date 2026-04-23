"""Capture a fixture screenshot for the AI-bbox spike.

Usage
-----
  1. Open the target app (Task Manager, VS Code, etc.)
  2. Run this script:
       python tools/capture_fixture.py --name 01-taskmgr
  3. You have 3 seconds to click the target window before capture fires.

The script captures the foreground window using the same DWM
EXTENDED_FRAME_BOUNDS crop as the production Rust pipeline, then
downscales it to longer-side=768 preserving aspect ratio.

Output: fixtures/bboxes/<name>.jpg
"""

from __future__ import annotations

import argparse
import ctypes
import time
from pathlib import Path

from PIL import Image

# ---------------------------------------------------------------------------
# Windows DWM active-window capture (mirrors src-tauri/src/capture/win.rs)
# ---------------------------------------------------------------------------

DWMWA_EXTENDED_FRAME_BOUNDS = 9


def _get_foreground_rect() -> tuple[int, int, int, int] | None:
    """Return (left, top, right, bottom) of the foreground window using
    DWM extended frame bounds, falling back to GetWindowRect."""
    user32 = ctypes.windll.user32
    dwmapi = ctypes.windll.dwmapi

    hwnd = user32.GetForegroundWindow()
    if not hwnd:
        return None

    class RECT(ctypes.Structure):
        _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                    ("right", ctypes.c_long), ("bottom", ctypes.c_long)]

    rect = RECT()
    hr = dwmapi.DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        ctypes.byref(rect),
        ctypes.sizeof(rect),
    )
    if hr != 0:
        # Fallback to GetWindowRect
        if user32.GetWindowRect(hwnd, ctypes.byref(rect)) == 0:
            return None

    if rect.right <= rect.left or rect.bottom <= rect.top:
        return None
    return (rect.left, rect.top, rect.right, rect.bottom)


def capture_active_window() -> Image.Image:
    """Capture the current foreground window and return as PIL Image."""
    from PIL import ImageGrab  # lazy import so help text works without PIL

    rect = _get_foreground_rect()
    if rect is None:
        raise RuntimeError("Could not determine foreground window bounds.")

    img = ImageGrab.grab(bbox=rect, all_screens=True)
    return img.convert("RGB")


# ---------------------------------------------------------------------------
# Downscale — matches production pipeline (longer side capped at 768)
# ---------------------------------------------------------------------------

MAX_LONG_SIDE = 768


def downscale(img: Image.Image) -> Image.Image:
    """Resize so the longer side is MAX_LONG_SIDE, preserving aspect ratio."""
    w, h = img.size
    if max(w, h) <= MAX_LONG_SIDE:
        return img
    if w >= h:
        new_w = MAX_LONG_SIDE
        new_h = round(h * MAX_LONG_SIDE / w)
    else:
        new_h = MAX_LONG_SIDE
        new_w = round(w * MAX_LONG_SIDE / h)
    return img.resize((new_w, new_h), Image.LANCZOS)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Capture a fixture screenshot for the AI-bbox spike."
    )
    parser.add_argument(
        "--name", required=True,
        help="Output filename stem, e.g. '01-taskmgr'. Saved to fixtures/bboxes/<name>.jpg",
    )
    parser.add_argument(
        "--delay", type=float, default=3.0,
        help="Seconds to wait before capturing (default: 3). Use this time to "
             "click the target window.",
    )
    parser.add_argument(
        "--quality", type=int, default=90,
        help="JPEG quality (default: 90).",
    )
    args = parser.parse_args()

    out_dir = Path(__file__).resolve().parent.parent / "fixtures" / "bboxes"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{args.name}.jpg"

    if args.delay > 0:
        print(f"Click the target window now — capturing in {args.delay:.0f}s …")
        time.sleep(args.delay)

    print("Capturing …")
    img = capture_active_window()
    img = downscale(img)
    img.save(out_path, "JPEG", quality=args.quality)

    w, h = img.size
    print(f"Saved {out_path}  ({w}×{h} px)")
    print()
    print("Next: open fixtures/bboxes/README.md and create the matching .json sidecar.")


if __name__ == "__main__":
    main()
