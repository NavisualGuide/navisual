"""Correction handler for AI Navigator.

Processes "wrong instruction" signals from the user (via hotkey or button).
Captures a fresh screenshot, adds correction context, and requests
a re-analysis from the AI.
"""

import logging
from typing import TYPE_CHECKING

from src.ai.prompts import CORRECTION_CONTEXT
from src.input.screen_capture import capture_screenshot_b64

if TYPE_CHECKING:
    from src.ai.api_router import APIRouter
    from src.core.session import Session

logger = logging.getLogger(__name__)


class CorrectionHandler:
    """Handles user correction signals ("wrong" hotkey).

    When the user indicates the previous instruction was wrong:
    1. Capture fresh screenshot
    2. Build correction context
    3. Send to AI for re-analysis
    4. Replace current step sequence
    """

    def __init__(self, api_router: "APIRouter") -> None:
        self._api_router = api_router

    async def handle_correction(self, session: "Session") -> dict | None:
        """Process a correction request.

        Args:
            session: Current active session.

        Returns:
            NavigateStepResponse dict if successful, None on failure.
        """
        logger.info("Correction requested — capturing fresh screenshot")

        try:
            # 1. Capture fresh screenshot
            screenshot_b64, _img = capture_screenshot_b64()

            # 2. Build correction message
            correction_text = CORRECTION_CONTEXT

            # 3. Add correction turn to conversation
            session.add_turn(role="correction", content=correction_text)

            # 4. Get state summary for context
            state_summary = None
            if session.current_state_summary:
                state_summary = session.current_state_summary.summary_text

            # 5. Send to AI
            response = await self._api_router.send_guidance_request(
                user_text=correction_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                session=session,
            )

            logger.info(
                "Correction response: %d new steps",
                len(response.steps) if response else 0,
            )
            return response

        except Exception as e:
            logger.error("Correction handling failed: %s", e)
            return None
