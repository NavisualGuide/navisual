"""Self-contained Windows.Media.Ocr backend + ``find_text`` matcher.

Ported from ``legacy/src/locator/ocr_engine.py`` (Windows backend + matcher
only — PaddleOCR fallback omitted since the spike target is Windows). The
semantics mirror the shipping Rust implementation in
``src-tauri/src/locator/ocr.rs`` so results here generalise to production.

No external dependency besides Pillow + winrt.
"""

from __future__ import annotations

import asyncio
import io
import logging
import re
import sys
from dataclasses import dataclass
from difflib import SequenceMatcher
from typing import Optional

from PIL import Image

logger = logging.getLogger(__name__)

_PUNCT_RE = re.compile(r"^[\W_]+|[\W_]+$")

# Tunables copied verbatim from the shipping matcher so the spike records
# production-comparable numbers.
MAX_LABEL_LEN = 60
MIN_SUBSTR_LEN = 8
_PREFER_LARGEST_ROLES = frozenset({"button", "tab", "menuitem", "checkbox", "radio"})
_PREFER_SMALLEST_ROLES = frozenset({"link"})


@dataclass
class OCRResult:
    text: str
    bbox: tuple[int, int, int, int]  # (x, y, w, h)
    confidence: float


def _strip_punct(s: str) -> str:
    return _PUNCT_RE.sub("", s).strip()


# ---------------------------------------------------------------------------
# Windows.Media.Ocr backend
# ---------------------------------------------------------------------------

class WindowsOCRUnavailable(RuntimeError):
    """Raised on non-Windows platforms or when winrt is missing."""


class _WindowsOCR:
    def __init__(self) -> None:
        self._engine = None

    @staticmethod
    def is_available() -> bool:
        if sys.platform != "win32":
            return False
        try:
            import winrt.windows.media.ocr  # noqa: F401
            import winrt.windows.graphics.imaging  # noqa: F401
            import winrt.windows.storage.streams  # noqa: F401
        except ImportError:
            return False
        return True

    def _ensure(self) -> None:
        if self._engine is not None:
            return
        if not self.is_available():
            raise WindowsOCRUnavailable(
                "Windows.Media.Ocr is only available on Windows with the winrt "
                "packages installed. See tools/requirements.txt."
            )
        import winrt.windows.media.ocr as win_ocr  # noqa: WPS433
        self._engine = win_ocr.OcrEngine.try_create_from_user_profile_languages()
        if self._engine is None:
            raise RuntimeError("Windows OCR engine unavailable (no recognizer languages installed?)")

    def recognise(self, image_bytes: bytes) -> list[OCRResult]:
        self._ensure()
        # If called from inside a running event loop (e.g. bbox_spike.py's
        # asyncio.run(run_spike(...))), asyncio.run would explode. Offload
        # the OCR coroutine to a dedicated thread which gets its own loop.
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return asyncio.run(self._recognise_async(image_bytes))

        import threading
        result: list[OCRResult] = []
        error: list[BaseException] = []

        def _runner() -> None:
            try:
                result.extend(asyncio.run(self._recognise_async(image_bytes)))
            except BaseException as exc:  # noqa: BLE001
                error.append(exc)

        t = threading.Thread(target=_runner, daemon=True)
        t.start()
        t.join()
        if error:
            raise error[0]
        return result

    async def _recognise_async(self, image_bytes: bytes) -> list[OCRResult]:
        import winrt.windows.graphics.imaging as imaging  # noqa: WPS433
        import winrt.windows.storage.streams as streams    # noqa: WPS433

        img = Image.open(io.BytesIO(image_bytes)).convert("RGB")
        buf = io.BytesIO()
        img.save(buf, format="BMP")

        mem = streams.InMemoryRandomAccessStream()
        writer = streams.DataWriter(mem)
        writer.write_bytes(bytearray(buf.getvalue()))
        await writer.store_async()
        mem.seek(0)

        decoder = await imaging.BitmapDecoder.create_async(mem)
        bitmap = await decoder.get_software_bitmap_async()
        result = await self._engine.recognize_async(bitmap)

        out: list[OCRResult] = []
        for line in result.lines:
            words = list(line.words)
            if not words:
                continue
            xs = [w.bounding_rect.x for w in words]
            ys = [w.bounding_rect.y for w in words]
            x2s = [w.bounding_rect.x + w.bounding_rect.width for w in words]
            y2s = [w.bounding_rect.y + w.bounding_rect.height for w in words]
            x = int(min(xs))
            y = int(min(ys))
            w = int(max(x2s) - x)
            h = int(max(y2s) - y)
            text = " ".join(wrd.text for wrd in words).strip()
            if text and w > 0 and h > 0:
                out.append(OCRResult(text=text, bbox=(x, y, w, h), confidence=1.0))
            for wrd in words:
                wt = wrd.text.strip()
                if not wt or wt == text:
                    continue
                r = wrd.bounding_rect
                ww = int(r.width)
                hh = int(r.height)
                if ww <= 0 or hh <= 0:
                    continue
                out.append(OCRResult(
                    text=wt,
                    bbox=(int(r.x), int(r.y), ww, hh),
                    confidence=0.95,
                ))
        return out


_OCR_SINGLETON: Optional[_WindowsOCR] = None


def run_windows_ocr(image_bytes: bytes) -> list[OCRResult]:
    """Run Windows.Media.Ocr on the given image bytes.

    Raises :class:`WindowsOCRUnavailable` on non-Windows / missing winrt.
    """
    global _OCR_SINGLETON
    if _OCR_SINGLETON is None:
        _OCR_SINGLETON = _WindowsOCR()
    return _OCR_SINGLETON.recognise(image_bytes)


# ---------------------------------------------------------------------------
# ``find_text`` matcher — mirrors legacy/ and src-tauri/ exactly
# ---------------------------------------------------------------------------

def find_text(
    target_text: str,
    ocr_results: list[OCRResult],
    *,
    target_role: Optional[str] = None,
    nearby_text: Optional[str] = None,
    zone: Optional[tuple[int, int]] = None,
    screen_width: int = 1920,
    screen_height: int = 1080,
    min_confidence: float = 0.5,
) -> Optional[OCRResult]:
    """Best-match OCR result for ``target_text``.

    Preserves all production semantics:
      - Exact → substring (``MIN_SUBSTR_LEN``) → fuzzy (SequenceMatcher > 0.7).
      - 4%-screen-height button cap for ``_PREFER_LARGEST_ROLES``.
      - Smallest-bbox preference for ``_PREFER_SMALLEST_ROLES``.
      - Punctuation-stripped comparisons.
      - Optional 16×9 zone filter (±1 cell tolerance) + nearby-text anchor.
    """
    if not target_text or not ocr_results:
        return None

    target_lower = target_text.lower().strip()
    prefer_largest = target_role in _PREFER_LARGEST_ROLES
    prefer_smallest = target_role in _PREFER_SMALLEST_ROLES

    # Nearby anchor (centre of best-matching OCR result for ``nearby_text``).
    anchor: Optional[tuple[float, float]] = None
    if nearby_text:
        nt_lower = nearby_text.lower().strip()
        best_ratio = 0.5
        for r in ocr_results:
            rc = _strip_punct(r.text).lower()
            if not rc:
                continue
            if nt_lower in rc or rc in nt_lower:
                ratio = 1.0
            else:
                ratio = SequenceMatcher(None, nt_lower, rc).ratio()
            if ratio > best_ratio:
                best_ratio = ratio
                bx, by, bw, bh = r.bbox
                anchor = (bx + bw / 2.0, by + bh / 2.0)

    def _prox(r: OCRResult) -> float:
        if anchor is None:
            return 0.0
        ax, ay = anchor
        rx, ry, rw, rh = r.bbox
        return (rx + rw / 2.0 - ax) ** 2 + (ry + rh / 2.0 - ay) ** 2

    button_height_cap = max(40, int(screen_height * 0.04))

    candidates = [
        r for r in ocr_results
        if r.confidence >= min_confidence and len(r.text.strip()) <= MAX_LABEL_LEN
    ]

    if zone is not None and screen_width > 0 and screen_height > 0:
        zx, zy = zone
        cw = screen_width / 16.0
        ch = screen_height / 9.0
        x0 = max(0.0, (zx - 1) * cw)
        x1 = min(screen_width, (zx + 2) * cw)
        y0 = max(0.0, (zy - 1) * ch)
        y1 = min(screen_height, (zy + 2) * ch)
        filt = [
            r for r in candidates
            if x0 <= r.bbox[0] + r.bbox[2] / 2.0 <= x1
            and y0 <= r.bbox[1] + r.bbox[3] / 2.0 <= y1
        ]
        if filt:
            candidates = filt

    def _pick(pool: list[OCRResult]) -> Optional[OCRResult]:
        if not pool:
            return None
        if anchor is not None:
            return min(pool, key=_prox)
        if len(pool) > 1:
            if prefer_largest:
                plausible = [r for r in pool if r.bbox[3] <= button_height_cap]
                use = plausible if plausible else pool
                return max(use, key=lambda r: r.bbox[2] * r.bbox[3])
            if prefer_smallest:
                return min(pool, key=lambda r: r.bbox[2] * r.bbox[3])
        return pool[0]

    # Strategy 1: exact (case + punct insensitive)
    exact = [r for r in candidates if _strip_punct(r.text).lower() == target_lower]
    if exact:
        return _pick(exact)

    # Strategy 2: substring (either direction), with MIN_SUBSTR_LEN guard.
    substr = []
    for r in candidates:
        rc = _strip_punct(r.text).lower()
        if not rc:
            continue
        if target_lower in rc or (rc in target_lower and len(rc) >= MIN_SUBSTR_LEN):
            substr.append(r)
    if substr:
        return _pick(substr)

    # Strategy 3: fuzzy SequenceMatcher > 0.7
    best = None
    best_ratio = 0.7
    for r in candidates:
        rc = _strip_punct(r.text).lower()
        if not rc:
            continue
        ratio = SequenceMatcher(None, target_lower, rc).ratio()
        if ratio > best_ratio:
            best_ratio = ratio
            best = r
    return best
