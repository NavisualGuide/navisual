"""Local OCR engine for AI Navigator (FALLBACK locator).

On Windows, uses Windows.Media.Ocr (built-in, zero-install, fast).
On other platforms, falls back to PaddleOCR.
Runs in a separate multiprocessing.Process to avoid blocking the Qt thread.

Only used when the Accessibility API fails to find the target element.
"""

import asyncio
import io
import logging
import multiprocessing as mp
import re
import sys
from difflib import SequenceMatcher
from typing import Optional

from PIL import Image
from pydantic import BaseModel

logger = logging.getLogger(__name__)

_PUNCT_RE = re.compile(r"^[\W_]+|[\W_]+$")


def _strip_punct(text: str) -> str:
    """Strip leading/trailing punctuation and whitespace from an OCR token.

    Handles curly quotes, backticks, apostrophes etc. that OCR sometimes
    attaches to a word when reading subtitle or inline-quoted text.
    Example: "'Continue'" → "Continue", '"Yes"' → "Yes"
    """
    return _PUNCT_RE.sub("", text).strip()


class OCRResult(BaseModel):
    """A single OCR detection result."""

    text: str
    bbox: tuple[int, int, int, int]  # (x, y, width, height)
    confidence: float


# ---------------------------------------------------------------------------
# Windows OCR backend (Windows.Media.Ocr — built into Windows 10/11)
# ---------------------------------------------------------------------------

class WindowsOCREngine:
    """Windows.Media.Ocr wrapper — native, zero-install, no OneDNN issues.

    Uses the Windows Runtime OCR engine, which is always available on
    Windows 10/11. Outputs line-level results with merged bounding boxes
    so that multi-word target_text ("Search Amazon") can be matched.
    """

    def __init__(self) -> None:
        self._engine = None

    def _ensure_initialized(self) -> None:
        if self._engine is None:
            import winrt.windows.media.ocr as win_ocr
            self._engine = win_ocr.OcrEngine.try_create_from_user_profile_languages()
            if self._engine is None:
                raise RuntimeError("Windows OCR engine unavailable (no recognizer languages installed?)")
            logger.info("Windows OCR engine initialized")

    def process_screenshot(self, image_bytes: bytes) -> list[OCRResult]:
        """Run OCR on a screenshot image and return line-level text with bboxes."""
        self._ensure_initialized()
        return asyncio.run(self._recognize_async(image_bytes))

    async def _recognize_async(self, image_bytes: bytes) -> list[OCRResult]:
        import winrt.windows.graphics.imaging as imaging
        import winrt.windows.storage.streams as streams

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

        ocr_results: list[OCRResult] = []
        for line in result.lines:
            words = list(line.words)
            if not words:
                continue

            # Merge all word bboxes in the line into one spanning rect
            xs = [w.bounding_rect.x for w in words]
            ys = [w.bounding_rect.y for w in words]
            x2s = [w.bounding_rect.x + w.bounding_rect.width for w in words]
            y2s = [w.bounding_rect.y + w.bounding_rect.height for w in words]
            x = int(min(xs))
            y = int(min(ys))
            w = int(max(x2s) - x)
            h = int(max(y2s) - y)
            text = " ".join(wrd.text for wrd in words).strip()

            if text:
                ocr_results.append(OCRResult(text=text, bbox=(x, y, w, h), confidence=1.0))

            # Also emit individual words so single-word searches match precisely
            for wrd in words:
                r = wrd.bounding_rect
                word_text = wrd.text.strip()
                if word_text and word_text != text:
                    ocr_results.append(OCRResult(
                        text=word_text,
                        bbox=(int(r.x), int(r.y), int(r.width), int(r.height)),
                        confidence=0.95,
                    ))

        return ocr_results

    @staticmethod
    def is_available() -> bool:
        if sys.platform != "win32":
            return False
        try:
            import winrt.windows.media.ocr  # noqa: F401
            import winrt.windows.graphics.imaging  # noqa: F401
            import winrt.windows.storage.streams  # noqa: F401
            return True
        except ImportError:
            return False


# ---------------------------------------------------------------------------
# PaddleOCR backend (fallback for non-Windows or if Windows OCR unavailable)
# ---------------------------------------------------------------------------

class PaddleOCREngine:
    """PaddleOCR wrapper for text detection and bounding box extraction."""

    def __init__(self, lang: str = "en") -> None:
        self._lang = lang
        self._ocr = None

    def _ensure_initialized(self) -> None:
        if self._ocr is None:
            # Disable OneDNN via Python API before importing PaddleOCR.
            # PaddlePaddle 3.x has a PIR+OneDNN bug; FLAGS_use_mkldnn=0 prevents
            # OneDNN dispatch in the old executor (may not cover PIR executor).
            try:
                import paddle
                paddle.set_flags({"FLAGS_use_mkldnn": 0})
            except Exception:
                pass

            from paddleocr import PaddleOCR
            import inspect
            paddle_params = inspect.signature(PaddleOCR.__init__).parameters

            if "use_doc_orientation_classify" in paddle_params:
                # PaddleOCR 3.x: disable doc-processing sub-models to reduce
                # the number of OneDNN-affected models loaded at init.
                kwargs: dict = {
                    "lang": self._lang,
                    "use_doc_orientation_classify": False,
                    "use_doc_unwarping": False,
                    "use_textline_orientation": False,
                }
            else:
                # PaddleOCR 2.x
                kwargs = {"use_angle_cls": True, "lang": self._lang}
                if "use_gpu" in paddle_params:
                    kwargs["use_gpu"] = False
                if "show_log" in paddle_params:
                    kwargs["show_log"] = False

            self._ocr = PaddleOCR(**kwargs)
            logger.info("PaddleOCR initialized (lang=%s)", self._lang)

    def process_screenshot(self, image_bytes: bytes) -> list[OCRResult]:
        self._ensure_initialized()
        import numpy as np

        img = Image.open(io.BytesIO(image_bytes)).convert("RGB")
        img_array = np.array(img)

        # PaddleOCR 3.x removed the cls parameter (angle classification is init-time).
        try:
            results = self._ocr.ocr(img_array, cls=True)
        except TypeError:
            results = self._ocr.ocr(img_array)

        if not results or not results[0]:
            return []

        ocr_results = []
        for line in results[0]:
            try:
                if isinstance(line, dict):
                    # PaddleOCR 3.x dict format
                    points = line.get("det_poly") or line.get("bbox", [])
                    text = line.get("rec_text") or line.get("text", "")
                    confidence = float(line.get("rec_score", 0.0) or line.get("confidence", 0.0))
                else:
                    # PaddleOCR 2.x nested-list format
                    points, (text, confidence) = line

                if not points or not text:
                    continue

                xs = [p[0] for p in points]
                ys = [p[1] for p in points]
                x = int(min(xs)); y = int(min(ys))
                w = int(max(xs) - x); h = int(max(ys) - y)
                ocr_results.append(OCRResult(text=text, bbox=(x, y, w, h), confidence=float(confidence)))
            except Exception as e:
                logger.debug("Skipping malformed OCR line: %s", e)
                continue

        return ocr_results


# ---------------------------------------------------------------------------
# OCREngine: selects backend automatically
# ---------------------------------------------------------------------------

class OCREngine:
    """Selects Windows OCR (primary on Windows) or PaddleOCR (fallback).

    Designed to run inside a worker process.
    """

    def __init__(self, lang: str = "en") -> None:
        if WindowsOCREngine.is_available():
            self._backend: WindowsOCREngine | PaddleOCREngine = WindowsOCREngine()
            logger.info("OCREngine: using Windows.Media.Ocr backend")
        else:
            self._backend = PaddleOCREngine(lang=lang)
            logger.info("OCREngine: using PaddleOCR backend (lang=%s)", lang)

    def process_screenshot(self, image_bytes: bytes) -> list[OCRResult]:
        """Run OCR on a screenshot and return all detected text with bounding boxes."""
        return self._backend.process_screenshot(image_bytes)

    @staticmethod
    # Roles that are typically rendered as visually distinct, larger elements.
    # When target_role matches one of these, prefer larger bounding-box matches.
    _BUTTON_LIKE_ROLES: frozenset[str] = frozenset({
        "button", "link", "tab", "menuitem", "checkbox", "radio",
    })

    def find_text(
        target_text: str,
        ocr_results: list[OCRResult],
        region_hint: Optional[str] = None,
        screen_width: int = 1920,
        screen_height: int = 1080,
        min_confidence: float = 0.5,
        target_role: Optional[str] = None,
    ) -> Optional[OCRResult]:
        """Find the best match for target_text in OCR results.

        Uses case-insensitive substring matching, then fuzzy matching as fallback.

        When target_role is button-like (button, link, tab, …) and multiple
        exact matches exist, prefers the one with the largest bounding box.
        Buttons are visually larger than inline text that happens to share the
        same word (e.g. TurboTax "Click a Fix button" vs the blue Fix button).
        """
        if not ocr_results or not target_text:
            return None

        target_lower = target_text.lower().strip()
        prefer_largest = target_role in OCREngine._BUTTON_LIKE_ROLES

        # Filter by confidence
        candidates = [r for r in ocr_results if r.confidence >= min_confidence]

        # Filter out strings too long to be a UI label.
        # Subtitle/instruction text rendered on screen by the overlay is read by OCR
        # and can match the target word (e.g. "Continue" inside "Click the Continue button").
        # Real UI element labels are never more than ~60 characters.
        MAX_LABEL_LEN = 60
        candidates = [r for r in candidates if len(r.text.strip()) <= MAX_LABEL_LEN]

        # Filter by region if hint provided
        if region_hint:
            candidates = _filter_by_region(candidates, region_hint, screen_width, screen_height)

        # Strategy 1: Exact match (case-insensitive, punctuation-stripped).
        # Collect ALL exact matches; when target_role is button-like, pick the
        # largest bounding box — buttons are bigger than inline text that shares
        # the same word (e.g. "Fix" in a sentence vs the blue Fix button).
        exact_matches = [
            r for r in candidates
            if _strip_punct(r.text).lower() == target_lower
        ]
        if exact_matches:
            if prefer_largest and len(exact_matches) > 1:
                return max(exact_matches, key=lambda r: r.bbox[2] * r.bbox[3])
            return exact_matches[0]

        # Strategy 2: Substring match
        # Strip surrounding punctuation before comparing — OCR sometimes reads subtitle
        # fragments like "'Continue'" or '"Yes"' as separate word-level results, which
        # would otherwise match the real target text via fuzzy matching.
        # Guard: only allow an OCR token to match as a substring of the target if it's
        # at least 6 chars — short tokens like "in", "with", "for" appear everywhere
        # in body text and produce false positives.
        MIN_SUBSTR_LEN = 8
        substr_matches = []
        for r in candidates:
            r_clean = _strip_punct(r.text).lower()
            if not r_clean:
                continue
            if target_lower in r_clean or (r_clean in target_lower and len(r_clean) >= MIN_SUBSTR_LEN):
                substr_matches.append(r)
        if substr_matches:
            if prefer_largest and len(substr_matches) > 1:
                return max(substr_matches, key=lambda r: r.bbox[2] * r.bbox[3])
            return substr_matches[0]

        # Strategy 3: Fuzzy match (SequenceMatcher, > 0.7 ratio)
        # Use punctuation-stripped text so "'Continue'" scores as "Continue".
        best_match = None
        best_ratio = 0.0
        for r in candidates:
            r_clean = _strip_punct(r.text).lower()
            if not r_clean:
                continue
            ratio = SequenceMatcher(None, target_lower, r_clean).ratio()
            if ratio > best_ratio and ratio > 0.7:
                best_ratio = ratio
                best_match = r

        return best_match


# ---------------------------------------------------------------------------
# OCRWorker: runs OCREngine in a separate process
# ---------------------------------------------------------------------------

class OCRWorker:
    """Runs OCR in a separate process to avoid blocking the Qt main thread.

    Communication via multiprocessing.Queue.
    """

    def __init__(self, lang: str = "en") -> None:
        self._lang = lang
        self._request_queue: mp.Queue = mp.Queue(maxsize=2)
        self._result_queue: mp.Queue = mp.Queue(maxsize=2)
        self._process: Optional[mp.Process] = None
        self._latest_results: list[OCRResult] = []

    def start(self) -> None:
        """Start the OCR worker process."""
        self._process = mp.Process(
            target=_ocr_worker_loop,
            args=(self._request_queue, self._result_queue, self._lang),
            daemon=True,
            name="ocr-worker",
        )
        self._process.start()
        logger.info("OCR worker process started (PID: %s)", self._process.pid)

    def stop(self) -> None:
        """Stop the OCR worker process."""
        if self._process and self._process.is_alive():
            self._request_queue.put(None)  # Sentinel to stop
            self._process.join(timeout=5)
            if self._process.is_alive():
                self._process.terminate()
            logger.info("OCR worker process stopped")

    def submit(self, image_bytes: bytes) -> None:
        """Submit a screenshot for OCR processing (non-blocking)."""
        while not self._request_queue.empty():
            try:
                self._request_queue.get_nowait()
            except mp.queues.Empty:
                break
        self._request_queue.put(image_bytes)

    def get_results(self, timeout: float = 0.1) -> list[OCRResult]:
        """Get the latest OCR results (non-blocking). Returns cached if no new results."""
        try:
            while not self._result_queue.empty():
                raw_results = self._result_queue.get_nowait()
                self._latest_results = [OCRResult(**r) for r in raw_results]
        except mp.queues.Empty:
            pass
        return self._latest_results

    @property
    def is_running(self) -> bool:
        return self._process is not None and self._process.is_alive()


def _ocr_worker_loop(
    request_queue: mp.Queue,
    result_queue: mp.Queue,
    lang: str,
) -> None:
    """Main loop for the OCR worker process."""
    import os
    # Belt-and-suspenders: also set env vars for PaddleOCR fallback path
    os.environ.setdefault("FLAGS_use_mkldnn", "0")
    os.environ.setdefault("PADDLE_DISABLE_ONEDNN", "True")
    os.environ.setdefault("PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK", "True")

    engine = OCREngine(lang=lang)

    while True:
        try:
            image_bytes = request_queue.get()
            if image_bytes is None:  # Sentinel to stop
                break

            results = engine.process_screenshot(image_bytes)
            result_dicts = [r.model_dump() for r in results]

            # Clear stale results before publishing
            while not result_queue.empty():
                try:
                    result_queue.get_nowait()
                except Exception:
                    break
            result_queue.put(result_dicts)
        except Exception as e:
            logging.error("OCR worker error: %s", e)


def _filter_by_region(
    results: list[OCRResult],
    region: str,
    screen_width: int,
    screen_height: int,
) -> list[OCRResult]:
    """Filter OCR results by rough screen region."""
    third_w = screen_width / 3
    third_h = screen_height / 3

    def in_region(r: OCRResult) -> bool:
        cx = r.bbox[0] + r.bbox[2] / 2
        cy = r.bbox[1] + r.bbox[3] / 2
        col = "left" if cx < third_w else ("right" if cx > 2 * third_w else "center")
        row = "top" if cy < third_h else ("bottom" if cy > 2 * third_h else "center")
        parts = region.split("-")
        if len(parts) == 2:
            return row == parts[0] and col == parts[1]
        elif region == "center":
            return row == "center" and col == "center"
        return True

    filtered = [r for r in results if in_region(r)]
    return filtered if filtered else results
