"""API router for AI Navigator.

Selects the AI backend (Anthropic / OpenAI / local), builds requests,
parses responses, and tracks token costs. Uses structured output
(tool_use / function_calling) for validated responses.
"""

import logging
from typing import TYPE_CHECKING, Optional

from src.ai.anthropic_client import AnthropicClient, build_messages
from src.ai.openai_client import OpenAIClient
from src.ai.prompts import (
    INITIAL_CONTEXT_TEMPLATE,
    SESSION_RESUME_TEMPLATE,
    STATE_CONTEXT_TEMPLATE,
    SYSTEM_PROMPT,
)
from src.ai.tool_schemas import NavigateStepResponse
from src.config import Config
from src.core.cost_tracker import CostTracker

if TYPE_CHECKING:
    from src.core.session import Session

logger = logging.getLogger(__name__)

# Rough estimate: tokens per API call (for budget pre-check)
ESTIMATED_INPUT_TOKENS = 2500
ESTIMATED_OUTPUT_TOKENS = 500


class APIRouter:
    """Routes AI requests to the appropriate backend provider.

    Handles:
    - Provider selection based on config
    - Request building with screenshots, state summaries, and conversation history
    - Response parsing into NavigateStepResponse
    - Token cost tracking
    """

    def __init__(self, config: Config, cost_tracker: CostTracker) -> None:
        self._config = config
        self._cost_tracker = cost_tracker
        self._anthropic_client: Optional[AnthropicClient] = None
        self._openai_client: Optional[OpenAIClient] = None

        self._init_clients()

    def _init_clients(self) -> None:
        """Initialize the appropriate API client based on config."""
        if self._config.api_provider == "anthropic" and self._config.anthropic_api_key:
            self._anthropic_client = AnthropicClient(
                api_key=self._config.anthropic_api_key,
                model=self._config.anthropic_model,
                timeout_sec=self._config.api_timeout_sec,
                max_retries=self._config.api_max_retries,
            )
            logger.info("API Router: Anthropic client initialized (model: %s)", self._config.anthropic_model)
        elif self._config.api_provider == "openai" and self._config.openai_api_key:
            self._openai_client = OpenAIClient(api_key=self._config.openai_api_key)
            logger.info("API Router: OpenAI client initialized")
        else:
            logger.warning(
                "API Router: No API key configured for provider '%s'. "
                "Set ANTHROPIC_API_KEY or OPENAI_API_KEY in .env",
                self._config.api_provider,
            )

    @property
    def is_available(self) -> bool:
        """Whether an API client is configured and available."""
        if self._config.api_provider == "anthropic":
            return self._anthropic_client is not None and self._anthropic_client.is_available
        elif self._config.api_provider == "openai":
            return self._openai_client is not None and self._openai_client.is_available
        return False

    async def send_guidance_request(
        self,
        user_text: str,
        screenshot_b64: Optional[str] = None,
        state_summary: Optional[str] = None,
        session: Optional["Session"] = None,
    ) -> NavigateStepResponse:
        """Send a guidance request to the AI backend.

        Args:
            user_text: User's prompt or system-generated context.
            screenshot_b64: Base64-encoded screenshot.
            state_summary: Current state summary text.
            session: Current session for conversation history.

        Returns:
            NavigateStepResponse with steps, state_summary, and needs_input.

        Raises:
            BudgetExceededError: If token budget would be exceeded.
            RuntimeError: If no API client is available.
        """
        # Budget pre-check
        estimated_total = ESTIMATED_INPUT_TOKENS + ESTIMATED_OUTPUT_TOKENS
        if not self._cost_tracker.can_spend(estimated_total):
            raise BudgetExceededError(
                "Token budget would be exceeded. "
                f"Daily: {self._cost_tracker.daily_total}/{self._cost_tracker.daily_cap}, "
                f"Monthly: {self._cost_tracker.monthly_total}/{self._cost_tracker.monthly_cap}"
            )

        # Build messages
        conversation_history = session.get_conversation_for_api() if session else []
        messages = build_messages(
            user_text=user_text,
            screenshot_b64=screenshot_b64,
            state_summary=state_summary,
            conversation_history=conversation_history,
        )

        # Route to provider
        if self._anthropic_client and self._anthropic_client.is_available:
            response, in_tokens, out_tokens = await self._anthropic_client.send_message(
                messages=messages,
                screenshot_b64=screenshot_b64,
                system_prompt=SYSTEM_PROMPT,
            )
        else:
            raise RuntimeError(
                f"No API client available for provider '{self._config.api_provider}'. "
                "Check your API key configuration."
            )

        # Record usage
        self._cost_tracker.record_usage(in_tokens, out_tokens)
        if session:
            session.record_tokens(in_tokens, out_tokens)

        # Warn if approaching limit
        if self._cost_tracker.is_approaching_limit():
            logger.warning("Token budget approaching limit: %s", self._cost_tracker.get_usage_summary())

        return response

    async def send_initial_request(
        self,
        task_description: str,
        screenshot_b64: Optional[str] = None,
        session: Optional["Session"] = None,
    ) -> NavigateStepResponse:
        """Send the first request for a new task."""
        user_text = INITIAL_CONTEXT_TEMPLATE.format(task_description=task_description)
        return await self.send_guidance_request(
            user_text=user_text,
            screenshot_b64=screenshot_b64,
            session=session,
        )

    async def send_resume_request(
        self,
        state_summary: str,
        screenshot_b64: Optional[str] = None,
        session: Optional["Session"] = None,
    ) -> NavigateStepResponse:
        """Send a resume request after session restore."""
        user_text = SESSION_RESUME_TEMPLATE.format(state_summary=state_summary)
        return await self.send_guidance_request(
            user_text=user_text,
            screenshot_b64=screenshot_b64,
            session=session,
        )

    async def close(self) -> None:
        """Close all API clients."""
        if self._anthropic_client:
            await self._anthropic_client.close()


class BudgetExceededError(Exception):
    """Raised when a request would exceed the token budget."""

    pass
