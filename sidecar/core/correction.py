"""Correction handler for AI Navigator (sidecar).

Processes "wrong instruction" signals from the user. In v0.4 the Rust backend
owns screen capture and passes the base64 screenshot into the sidecar via IPC,
so this handler no longer captures on its own.
"""

import logging
from typing import TYPE_CHECKING, Optional

from ai.prompts import CORRECTION_CONTEXT

if TYPE_CHECKING:
    from ai.api_router import APIRouter
    from core.session import Session

logger = logging.getLogger(__name__)


class CorrectionHandler:
    """Handles user correction signals ("wrong" hotkey / button).

    Given a fresh screenshot (captured by the Rust side), builds correction
    context and requests re-analysis from the AI.
    """

    def __init__(self, api_router: "APIRouter") -> None:
        self._api_router = api_router

    async def handle_correction(
        self,
        session: "Session",
        screenshot_b64: Optional[str] = None,
    ) -> dict | None:
        """Process a correction request.

        Args:
            session: Current active session.
            screenshot_b64: Fresh screenshot provided by the Rust backend.

        Returns:
            NavigateStepResponse if successful, None on failure.
        """
        logger.info("Correction requested")

        try:
            correction_text = CORRECTION_CONTEXT
            session.add_turn(role="correction", content=correction_text)

            state_summary = None
            if session.current_state_summary:
                state_summary = session.current_state_summary.summary_text

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
