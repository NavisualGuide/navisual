"""Local OCR engine for AI Navigator (FALLBACK locator).

Uses PaddleOCR for text detection and recognition. Runs in a separate
multiprocessing.Process to avoid blocking the main Qt thread (GIL mitigation).

Only used when the Accessibility API fails to find the target element.
"""

import io
import logging
import multiprocessing as mp
from difflib import SequenceMatcher
from typing import Optional

from PIL import Image
from pydantic import BaseModel

logger = logging.getLogger(__name__)


class OCRResult(BaseModel):
    """A single OCR detection result."""

    text: str
    bbox: tuple[int, int, int, int]  # (x, y, width, height)
    confidence: float


class OCREngine:
    """PaddleOCR wrapper for text detection and bounding box extraction.

    Designed to be used inside a worker process. The PaddleOCR model
    is initialized lazily on first use (downloads ~100MB of models).
    """

    def __init__(self, lang: str = "en") -> None:
        self._lang = lang
        self._ocr = None

    def _ensure_initialized(self) -> None:
        """Lazy-initialize PaddleOCR (must happen inside the worker process)."""
        if self._ocr is None:
            from paddleocr import PaddleOCR

            self._ocr = PaddleOCR(
                use_angle_cls=True,
                lang=self._lang,
                show_log=False,
                use_gpu=False,
            )
            logger.info("PaddleOCR initialized (lang=%s)", self._lang)

    def process_screenshot(self, image_bytes: bytes) -> list[OCRResult]:
        """Run OCR on a screenshot and return all detected text with bounding boxes.

        Args:
            image_bytes: PNG or JPEG image bytes.

        Returns:
            List of OCRResult with text, bbox, and confidence.
        """
        self._ensure_initialized()

        import numpy as np

        img = Image.open(io.BytesIO(image_bytes)).convert("RGB")
        img_array = np.array(img)

        results = self._ocr.ocr(img_array, cls=True)
        if not results or not results[0]:
            return []

        ocr_results = []
        for line in results[0]:
            points, (text, confidence) = line
            # Convert 4-point polygon to bounding box
            xs = [p[0] for p in points]
            ys = [p[1] for p in points]
            x = int(min(xs))
            y = int(min(ys))
            w = int(max(xs) - x)
            h = int(max(ys) - y)
            ocr_results.append(OCRResult(text=text, bbox=(x, y, w, h), confidence=confidence))

        return ocr_results

    @staticmethod
    def find_text(
        target_text: str,
        ocr_results: list[OCRResult],
        region_hint: Optional[str] = None,
        screen_width: int = 1920,
        screen_height: int = 1080,
        min_confidence: float = 0.5,
    ) -> Optional[OCRResult]:
        """Find the best match for target_text in OCR results.

        Uses case-insensitive substring matching, then fuzzy matching as fallback.

        Args:
            target_text: The text to find.
            ocr_results: Pre-computed OCR results.
            region_hint: Optional region to narrow search (e.g., "top-center").
            screen_width: Screen width for region filtering.
            screen_height: Screen height for region filtering.
            min_confidence: Minimum OCR confidence threshold.

        Returns:
            Best matching OCRResult, or None.
        """
        if not ocr_results or not target_text:
            return None

        target_lower = target_text.lower().strip()

        # Filter by confidence
        candidates = [r for r in ocr_results if r.confidence >= min_confidence]

        # Filter by region if hint provided
        if region_hint:
            candidates = _filter_by_region(candidates, region_hint, screen_width, screen_height)

        # Strategy 1: Exact match (case-insensitive)
        for r in candidates:
            if r.text.lower().strip() == target_lower:
                return r

        # Strategy 2: Substring match
        for r in candidates:
            if target_lower in r.text.lower() or r.text.lower() in target_lower:
                return r

        # Strategy 3: Fuzzy match (SequenceMatcher, > 0.7 ratio)
        best_match = None
        best_ratio = 0.0
        for r in candidates:
            ratio = SequenceMatcher(None, target_lower, r.text.lower().strip()).ratio()
            if ratio > best_ratio and ratio > 0.7:
                best_ratio = ratio
                best_match = r

        return best_match


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
        # Clear any stale request
        while not self._request_queue.empty():
            try:
                self._request_queue.get_nowait()
            except mp.queues.Empty:
                break
        self._request_queue.put(image_bytes)

    def get_results(self, timeout: float = 0.1) -> list[OCRResult]:
        """Get the latest OCR results (non-blocking with short timeout).

        Returns cached results if no new results are available.
        """
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
    engine = OCREngine(lang=lang)

    while True:
        try:
            image_bytes = request_queue.get()
            if image_bytes is None:  # Sentinel to stop
                break

            results = engine.process_screenshot(image_bytes)
            # Send results as dicts (Pydantic models aren't picklable across processes)
            result_dicts = [r.model_dump() for r in results]

            # Clear stale results
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

        region_parts = region.split("-")
        if len(region_parts) == 2:
            return row == region_parts[0] and col == region_parts[1]
        elif region == "center":
            return row == "center" and col == "center"
        return True

    filtered = [r for r in results if in_region(r)]
    # Fall back to all results if region filter eliminated everything
    return filtered if filtered else results
