"""Tests for screen capture utilities."""

import base64
from unittest.mock import MagicMock, patch

from PIL import Image

from src.input.screen_capture import image_to_b64, image_to_bytes


class TestScreenCapture:
    def test_image_to_b64_jpeg(self):
        """Test base64 encoding of a PIL image."""
        img = Image.new("RGB", (100, 100), color=(255, 0, 0))
        b64 = image_to_b64(img, fmt="JPEG")

        # Should be valid base64
        decoded = base64.b64decode(b64)
        assert len(decoded) > 0

        # Should be a valid JPEG
        result = Image.open(__import__("io").BytesIO(decoded))
        assert result.size == (100, 100)

    def test_image_to_b64_png(self):
        img = Image.new("RGB", (50, 50), color=(0, 255, 0))
        b64 = image_to_b64(img, fmt="PNG")
        decoded = base64.b64decode(b64)
        assert decoded[:4] == b'\x89PNG'

    def test_image_to_bytes(self):
        img = Image.new("RGB", (10, 10), color=(0, 0, 255))
        raw = image_to_bytes(img, fmt="PNG")
        assert isinstance(raw, bytes)
        assert raw[:4] == b'\x89PNG'
