"""Element Locator orchestrator for AI Navigator.

The core differentiator: AI returns TEXT descriptions, this module
finds EXACT screen positions using a prioritized fallback chain:

1. OS Accessibility API (UIA) — PRIMARY, < 5ms for browsers
2. Local OCR (PaddleOCR) — FALLBACK, pre-cached in parallel
3. Template Matching — FUTURE (v0.3), for icon-only elements

If all strategies fail, returns None — the overlay shows subtitle-only.
"""

import logging
from difflib import SequenceMatcher
from typing import Optional

from pydantic import BaseModel

from src.locator.a11y_engine import A11yEngine
from src.locator.ocr_engine import OCREngine, OCRResult, OCRWorker, WindowsOCREngine

logger = logging.getLogger(__name__)


class LocatorResult(BaseModel):
    """Result from the element location pipeline."""

    bbox: Optional[tuple[int, int, int, int]] = None  # (x, y, width, height)
    method: str = "none"  # "a11y", "ocr", "none"
    confidence: float = 0.0
    element_name: str = ""


class ElementLocator:
    """Orchestrates the A11y → OCR → subtitle fallback chain.

    Usage:
        locator = ElementLocator()
        locator.start()

        # At the start of each guidance turn, pre-cache OCR:
        locator.start_ocr_precache(screenshot_bytes)

        # After API returns target_text + target_role:
        result = locator.locate("Search Amazon", target_role="textbox", target_region="top-center")

        # result.bbox is the exact position, or None for subtitle fallback
    """

    def __init__(
        self,
        enable_a11y: bool = True,
        enable_ocr: bool = True,
        ocr_lang: str = "en",
        ocr_confidence_threshold: float = 0.5,
        a11y_timeout_ms: int = 100,
    ) -> None:
        self._a11y_engine = A11yEngine() if enable_a11y else None
        self._ocr_worker = OCRWorker(lang=ocr_lang) if enable_ocr else None
        self._ocr_confidence_threshold = ocr_confidence_threshold
        self._a11y_timeout_ms = a11y_timeout_ms
        self._screen_width = 1920
        self._screen_height = 1080
        # Synchronous OCR engine for fresh-screenshot fallback (last-resort only).
        # Only initialised on Windows where Windows.Media.Ocr is available.
        self._sync_ocr: Optional[WindowsOCREngine] = (
            WindowsOCREngine() if WindowsOCREngine.is_available() else None
        )

    def start(self) -> None:
        """Start background workers (OCR process)."""
        if self._ocr_worker:
            self._ocr_worker.start()
            logger.info("Element Locator started (A11y: %s, OCR: %s)",
                        self._a11y_engine is not None and self._a11y_engine.is_available,
                        self._ocr_worker is not None)

    def stop(self) -> None:
        """Stop background workers."""
        if self._ocr_worker:
            self._ocr_worker.stop()
        logger.info("Element Locator stopped")

    def set_screen_size(self, width: int, height: int) -> None:
        """Update screen dimensions for OCR region filtering."""
        self._screen_width = width
        self._screen_height = height

    def start_ocr_precache(self, screenshot_bytes: bytes) -> None:
        """Submit a screenshot for OCR pre-indexing.

        Call this at the START of each guidance turn (before the API call).
        By the time the API returns, OCR results will be ready.
        """
        if self._ocr_worker and self._ocr_worker.is_running:
            self._ocr_worker.submit(screenshot_bytes)
            logger.debug("OCR pre-cache submitted")

    def locate(
        self,
        target_text: str,
        target_role: Optional[str] = None,
        target_region: Optional[str] = None,
        nearby_text: Optional[str] = None,
        zone_x: Optional[int] = None,
        zone_y: Optional[int] = None,
        screenshot_bytes: Optional[bytes] = None,
    ) -> LocatorResult:
        """Find a UI element on screen using the A11y → OCR fallback chain.

        Args:
            target_text: Exact text label to find (from AI response).
            target_role: UI role (button, tab, link, etc.) for A11y filtering.
            target_region: Rough screen region hint for OCR filtering.
            nearby_text: Short unique string adjacent to the target; used by OCR
                         to disambiguate when target_text appears multiple times.
            zone_x: Column (0-15) of the 16×9 grid cell reported by the AI.
            zone_y: Row (0-8) of the 16×9 grid cell reported by the AI.

        Returns:
            LocatorResult with bbox (or None if not found).
        """
        if not target_text:
            return LocatorResult()

        # Strategy 1: Accessibility API (PRIMARY, < 5ms)
        if self._a11y_engine and self._a11y_engine.is_available:
            a11y_result = self._a11y_engine.find_element(
                target_text=target_text,
                target_role=target_role,
                timeout_ms=self._a11y_timeout_ms,
            )
            if a11y_result:
                # Zone sanity check: if AI provided a zone, verify the A11y result
                # lands within ±2 cells of it.  Browser chrome elements (tab strip,
                # toolbar) can have the same A11y name as web-content elements and
                # appear first in the UIA tree.  Rejecting a zone-mismatched A11y
                # result lets OCR (which does filter by zone) find the right one.
                if zone_x is not None and zone_y is not None:
                    cell_w = self._screen_width  / 16
                    cell_h = self._screen_height / 9
                    cx = a11y_result.bbox[0] + a11y_result.bbox[2] / 2
                    cy = a11y_result.bbox[1] + a11y_result.bbox[3] / 2
                    result_zx = int(cx / cell_w)
                    result_zy = int(cy / cell_h)
                    if abs(result_zx - zone_x) > 2 or abs(result_zy - zone_y) > 2:
                        logger.info(
                            "A11y result '%s' at zone (%d,%d) rejected — expected zone (%d,%d); trying OCR",
                            a11y_result.name, result_zx, result_zy, zone_x, zone_y,
                        )
                        a11y_result = None

            if a11y_result:
                logger.info(
                    "Element found via A11y: '%s' at %s",
                    a11y_result.name, a11y_result.bbox,
                )
                return LocatorResult(
                    bbox=a11y_result.bbox,
                    method="a11y",
                    confidence=a11y_result.confidence,
                    element_name=a11y_result.name,
                )
            logger.info("A11y miss for '%s' (role=%s), trying OCR fallback", target_text, target_role)

        # Strategy 2: OCR fallback (pre-cached results)
        def _ocr_search(ocr_results: list[OCRResult], label: str) -> Optional[LocatorResult]:
            match = OCREngine.find_text(
                target_text=target_text,
                ocr_results=ocr_results,
                region_hint=target_region,
                screen_width=self._screen_width,
                screen_height=self._screen_height,
                min_confidence=self._ocr_confidence_threshold,
                target_role=target_role,
                nearby_text=nearby_text,
                zone_x=zone_x,
                zone_y=zone_y,
            )
            if match:
                logger.info(
                    "Element found via %s OCR: '%s' at %s (confidence: %.2f)",
                    label, match.text, match.bbox, match.confidence,
                )
                return LocatorResult(
                    bbox=match.bbox,
                    method="ocr",
                    confidence=match.confidence,
                    element_name=match.text,
                )
            # Diagnostic: log the 5 closest candidates so we can see what OCR found
            if ocr_results:
                tl = target_text.lower()
                ranked = sorted(
                    ocr_results,
                    key=lambda r: SequenceMatcher(None, tl, r.text.lower()).ratio(),
                    reverse=True,
                )[:5]
                logger.debug(
                    "%s OCR miss for '%s' — closest candidates: %s",
                    label,
                    target_text,
                    [(r.text, round(SequenceMatcher(None, tl, r.text.lower()).ratio(), 2))
                     for r in ranked],
                )
            return None

        if self._ocr_worker:
            ocr_results = self._ocr_worker.get_results()
            if ocr_results:
                result = _ocr_search(ocr_results, "cached")
                if result:
                    return result
            logger.info("OCR miss for '%s'", target_text)

        # Strategy 3: Fresh synchronous OCR (last resort — handles stale cache).
        # Triggered when cached OCR misses AND caller provided a recent screenshot.
        # Windows.Media.Ocr runs in ~10 ms so blocking briefly here is acceptable.
        if screenshot_bytes and self._sync_ocr is not None:
            try:
                fresh_results = self._sync_ocr.process_screenshot(screenshot_bytes)
                result = _ocr_search(fresh_results, "fresh-sync")
                if result:
                    # Also update the worker cache so the next locate() within the
                    # same step doesn't repeat the stale-cache miss.
                    if self._ocr_worker:
                        self._ocr_worker.submit(screenshot_bytes)
                    return result
                logger.info("Fresh-sync OCR also miss for '%s'", target_text)
            except Exception as e:
                logger.debug("Fresh-sync OCR error: %s", e)

        # Strategy 4: Not found — overlay will use subtitle-only
        logger.info("Element not found: '%s' — falling back to subtitle", target_text)
        return LocatorResult()
