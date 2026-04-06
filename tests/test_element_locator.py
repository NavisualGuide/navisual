"""Tests for the element locator orchestration."""

from unittest.mock import MagicMock, patch

from src.locator.a11y_engine import A11yResult
from src.locator.element_locator import ElementLocator, LocatorResult
from src.locator.ocr_engine import OCRResult


class TestElementLocator:
    def test_a11y_success_returns_immediately(self):
        """When A11y finds the element, OCR is never consulted."""
        locator = ElementLocator(enable_a11y=True, enable_ocr=False)

        # Mock A11y engine
        mock_result = A11yResult(bbox=(100, 200, 50, 30), name="Search", role="EditControl")
        locator._a11y_engine = MagicMock()
        locator._a11y_engine.is_available = True
        locator._a11y_engine.find_element.return_value = mock_result

        result = locator.locate("Search", target_role="textbox")

        assert result.bbox == (100, 200, 50, 30)
        assert result.method == "a11y"
        assert result.confidence == 1.0

    def test_a11y_miss_falls_back_to_ocr(self):
        """When A11y misses, pre-cached OCR results are used."""
        locator = ElementLocator(enable_a11y=True, enable_ocr=True)

        # Mock A11y miss
        locator._a11y_engine = MagicMock()
        locator._a11y_engine.is_available = True
        locator._a11y_engine.find_element.return_value = None

        # Mock OCR worker with cached results
        locator._ocr_worker = MagicMock()
        locator._ocr_worker.get_results.return_value = [
            OCRResult(text="Search Amazon", bbox=(150, 45, 120, 25), confidence=0.95),
            OCRResult(text="Cart", bbox=(900, 10, 40, 20), confidence=0.9),
        ]

        result = locator.locate("Search Amazon")

        assert result.bbox == (150, 45, 120, 25)
        assert result.method == "ocr"
        assert result.confidence == 0.95

    def test_both_miss_returns_none(self):
        """When both A11y and OCR miss, result has no bbox."""
        locator = ElementLocator(enable_a11y=True, enable_ocr=True)

        locator._a11y_engine = MagicMock()
        locator._a11y_engine.is_available = True
        locator._a11y_engine.find_element.return_value = None

        locator._ocr_worker = MagicMock()
        locator._ocr_worker.get_results.return_value = []

        result = locator.locate("Nonexistent Button")

        assert result.bbox is None
        assert result.method == "none"

    def test_a11y_disabled(self):
        """When A11y is disabled, go straight to OCR."""
        locator = ElementLocator(enable_a11y=False, enable_ocr=True)

        locator._ocr_worker = MagicMock()
        locator._ocr_worker.get_results.return_value = [
            OCRResult(text="Submit", bbox=(500, 400, 80, 30), confidence=0.88),
        ]

        result = locator.locate("Submit")

        assert result.bbox == (500, 400, 80, 30)
        assert result.method == "ocr"

    def test_empty_target_text(self):
        """Empty target text returns immediately with no match."""
        locator = ElementLocator(enable_a11y=True, enable_ocr=True)
        result = locator.locate("")
        assert result.bbox is None
        assert result.method == "none"
