"""Event-driven screen change detection for AI Navigator.

Monitors for meaningful screen changes using a multi-layer approach:
1. Fast pixel-diff at ~10fps in a separate process (GIL mitigation)
2. pHash comparison for deduplication
3. Idle fallback timer

Triggers API calls only when something meaningful changes on screen.
"""

import logging
import multiprocessing as mp
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Callable, Optional

import imagehash
from PIL import Image

logger = logging.getLogger(__name__)


@dataclass
class ScreenChangeEvent:
    """Emitted when a meaningful screen change is detected."""

    timestamp: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    phash: str = ""
    change_pct: float = 0.0
    source: str = "diff"  # "diff", "idle", "user"


class ScreenMonitor:
    """Event-driven screen change detector.

    Runs pixel-diff in a separate process at ~10fps. The main process
    polls results via QTimer and dispatches callbacks.
    """

    def __init__(
        self,
        diff_threshold: float = 0.05,
        phash_threshold: int = 5,
        idle_timeout_sec: int = 10,
        diff_fps: int = 10,
        thumbnail_size: tuple[int, int] = (160, 90),
    ) -> None:
        self._diff_threshold = diff_threshold
        self._phash_threshold = phash_threshold
        self._idle_timeout_sec = idle_timeout_sec
        self._diff_fps = diff_fps
        self._thumbnail_size = thumbnail_size

        self._callbacks: list[Callable[[ScreenChangeEvent], None]] = []
        self._event_queue: mp.Queue = mp.Queue(maxsize=10)
        self._control_queue: mp.Queue = mp.Queue(maxsize=5)
        self._process: Optional[mp.Process] = None
        self._running = False
        self._paused = False
        self._last_phash: Optional[str] = None
        self._last_event_time: float = time.time()

    def on_change(self, callback: Callable[[ScreenChangeEvent], None]) -> None:
        """Register a callback for screen change events."""
        self._callbacks.append(callback)

    def start(self) -> None:
        """Start the screen monitoring process."""
        self._running = True
        self._process = mp.Process(
            target=_diff_worker_loop,
            args=(
                self._event_queue,
                self._control_queue,
                self._diff_threshold,
                self._diff_fps,
                self._thumbnail_size,
            ),
            daemon=True,
            name="screen-diff-worker",
        )
        self._process.start()
        logger.info("Screen monitor started (PID: %s)", self._process.pid)

    def stop(self) -> None:
        """Stop the screen monitoring process."""
        self._running = False
        if self._process and self._process.is_alive():
            self._control_queue.put("stop")
            self._process.join(timeout=5)
            if self._process.is_alive():
                self._process.terminate()
        logger.info("Screen monitor stopped")

    def pause(self) -> None:
        """Pause screen monitoring (privacy control)."""
        self._paused = True
        self._control_queue.put("pause")
        logger.info("Screen monitor paused")

    def resume(self) -> None:
        """Resume screen monitoring."""
        self._paused = False
        self._control_queue.put("resume")
        logger.info("Screen monitor resumed")

    def toggle_pause(self) -> bool:
        """Toggle pause state. Returns True if now paused."""
        if self._paused:
            self.resume()
        else:
            self.pause()
        return self._paused

    @property
    def is_paused(self) -> bool:
        return self._paused

    def poll(self) -> None:
        """Poll for events from the diff worker. Call this from a QTimer (~50ms).

        Dispatches ScreenChangeEvent to registered callbacks.
        """
        if not self._running or self._paused:
            return

        events_dispatched = 0
        while not self._event_queue.empty():
            try:
                event_data = self._event_queue.get_nowait()
                event = ScreenChangeEvent(**event_data)

                # pHash dedup: skip if the screen hasn't changed meaningfully
                if self._last_phash and event.phash:
                    if not self._is_phash_different(event.phash):
                        continue

                self._last_phash = event.phash
                self._last_event_time = time.time()

                for callback in self._callbacks:
                    try:
                        callback(event)
                    except Exception as e:
                        logger.error("Screen change callback error: %s", e)

                events_dispatched += 1
            except mp.queues.Empty:
                break

        # Idle fallback: if no events for idle_timeout_sec, emit a manual check
        if not events_dispatched and self._running and not self._paused:
            elapsed = time.time() - self._last_event_time
            if elapsed >= self._idle_timeout_sec:
                self._last_event_time = time.time()
                idle_event = ScreenChangeEvent(source="idle")
                for callback in self._callbacks:
                    try:
                        callback(idle_event)
                    except Exception as e:
                        logger.error("Idle check callback error: %s", e)

    def _is_phash_different(self, new_phash: str) -> bool:
        """Compare new pHash with the last one using Hamming distance."""
        if not self._last_phash:
            return True
        try:
            hash1 = imagehash.hex_to_hash(self._last_phash)
            hash2 = imagehash.hex_to_hash(new_phash)
            distance = hash1 - hash2
            return distance > self._phash_threshold
        except (ValueError, TypeError):
            return True

    def force_check(self) -> None:
        """Force a screen change event (e.g., after user action)."""
        event = ScreenChangeEvent(source="user")
        self._last_event_time = time.time()
        for callback in self._callbacks:
            try:
                callback(event)
            except Exception as e:
                logger.error("Force check callback error: %s", e)


def _diff_worker_loop(
    event_queue: mp.Queue,
    control_queue: mp.Queue,
    diff_threshold: float,
    fps: int,
    thumbnail_size: tuple[int, int],
) -> None:
    """Main loop for the pixel-diff worker process.

    Captures low-res thumbnails and compares consecutive frames.
    Runs at ~10fps with minimal CPU usage.
    """
    import numpy as np
    import mss

    interval = 1.0 / fps
    sct = mss.mss()
    monitor = sct.monitors[1] if len(sct.monitors) > 1 else sct.monitors[0]

    prev_frame = None
    paused = False

    while True:
        # Check for control messages
        while not control_queue.empty():
            try:
                msg = control_queue.get_nowait()
                if msg == "stop":
                    return
                elif msg == "pause":
                    paused = True
                elif msg == "resume":
                    paused = False
                    prev_frame = None  # Reset comparison on resume
            except Exception:
                break

        if paused:
            time.sleep(0.1)
            continue

        start = time.monotonic()

        try:
            # Capture low-res thumbnail
            raw = sct.grab(monitor)
            img = Image.frombytes("RGB", (raw.width, raw.height), raw.rgb)
            img = img.resize(thumbnail_size, Image.Resampling.NEAREST)
            frame = np.array(img, dtype=np.float32)

            if prev_frame is not None:
                # Pixel-diff: percentage of pixels that changed
                diff = np.abs(frame - prev_frame)
                change_pct = float(np.mean(diff > 25) )  # Threshold per-pixel change

                if change_pct > diff_threshold:
                    # Compute pHash for dedup
                    phash = str(imagehash.phash(img))

                    event_data = {
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "phash": phash,
                        "change_pct": round(change_pct, 4),
                        "source": "diff",
                    }

                    # Non-blocking put
                    if not event_queue.full():
                        event_queue.put(event_data)

            prev_frame = frame

        except Exception:
            pass  # Screen capture can fail during transitions

        # Maintain target FPS
        elapsed = time.monotonic() - start
        sleep_time = interval - elapsed
        if sleep_time > 0:
            time.sleep(sleep_time)
