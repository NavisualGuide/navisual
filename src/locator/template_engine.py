"""Template matching engine stub for AI Navigator v0.3.

Will use OpenCV matchTemplate for finding icon-only UI elements
that have no text labels (toolbar buttons, non-text controls).
"""

import logging
from typing import Optional

logger = logging.getLogger(__name__)


class TemplateEngine:
    """Icon/template matching engine (stub for v0.3).

    Will support:
    - OpenCV template matching against icon libraries
    - Per-app icon packs (shipped with Nav-Packs)
    - Scale-invariant matching for different DPI settings
    """

    def __init__(self) -> None:
        logger.info("TemplateEngine: stub initialized (available in v0.3)")

    def find(
        self,
        template_name: str,
        screenshot_bytes: bytes,
        region_hint: Optional[str] = None,
    ) -> tuple[int, int, int, int] | None:
        """Find a template/icon on the screenshot.

        Returns bounding box (x, y, width, height) or None.
        """
        return None

    @property
    def is_available(self) -> bool:
        return False
