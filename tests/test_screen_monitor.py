"""Tests for screen monitor (event detection, pHash comparison)."""

import imagehash
from PIL import Image

from src.input.screen_monitor import ScreenChangeEvent, ScreenMonitor


class TestScreenMonitor:
    def test_callback_registration(self):
        monitor = ScreenMonitor()
        events = []
        monitor.on_change(lambda e: events.append(e))
        assert len(monitor._callbacks) == 1

    def test_pause_resume(self):
        monitor = ScreenMonitor()
        assert not monitor.is_paused

        monitor._paused = True
        assert monitor.is_paused

    def test_force_check_emits_event(self):
        monitor = ScreenMonitor()
        events = []
        monitor.on_change(lambda e: events.append(e))

        monitor.force_check()
        assert len(events) == 1
        assert events[0].source == "user"

    def test_phash_comparison(self):
        """Test that similar images have small Hamming distance.

        pHash works on DCT coefficients of the image, so we need
        structurally different images (gradients, patterns) rather than
        just random noise or solid colors.
        """
        import numpy as np

        # Create a gradient image (dark left to bright right)
        gradient = np.tile(np.linspace(0, 255, 160, dtype=np.uint8), (90, 1))
        arr1 = np.stack([gradient, gradient, gradient], axis=2)
        img1 = Image.fromarray(arr1)

        # Same gradient with a tiny brightness shift — structurally identical
        arr2 = np.clip(arr1.astype(np.int16) + 3, 0, 255).astype(np.uint8)
        img2 = Image.fromarray(arr2)

        # Inverted gradient — structurally opposite
        arr3 = 255 - arr1
        img3 = Image.fromarray(arr3)

        hash1 = imagehash.phash(img1)
        hash2 = imagehash.phash(img2)
        hash3 = imagehash.phash(img3)

        # Similar images should have small distance
        assert hash1 - hash2 < 5

        # Structurally opposite images should have larger distance
        assert hash1 - hash3 > hash1 - hash2

    def test_screen_change_event(self):
        event = ScreenChangeEvent(source="diff", change_pct=0.15)
        assert event.source == "diff"
        assert event.change_pct == 0.15
