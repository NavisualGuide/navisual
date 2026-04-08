"""API router for AI Navigator.

Selects the AI backend (Anthropic / Gemini / Ollama / OpenAI), builds requests,
parses responses, and tracks token costs. Uses structured output
(tool_use / function_calling / JSON mode) for validated responses.

Supported providers:
  anthropic — Claude models via Anthropic API (requires ANTHROPIC_API_KEY)
  gemini    — Gemini models via Google AI Studio (requires GEMINI_API_KEY)
              Free tier: ~1,500 req/day — ideal for new users
  ollama    — Local models via Ollama (no API key, runs on-device)
              Best for privacy; requires `ollama serve` + a vision model
  openai    — OpenAI models (stub, v0.2)
"""

import logging
from typing import TYPE_CHECKING, Callable, Optional

from src.ai.anthropic_client import AnthropicClient, build_messages
from src.ai.gemini_client import GeminiClient, build_gemini_messages
from src.ai.ollama_client import OllamaClient, OllamaError, build_ollama_messages
from src.ai.openai_client import OpenAIClient
from src.ai.prompts import (
    INITIAL_CONTEXT_TEMPLATE,
    SESSION_RESUME_TEMPLATE,
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
        self._gemini_client: Optional[GeminiClient] = None
        self._ollama_client: Optional[OllamaClient] = None
        self._openai_client: Optional[OpenAIClient] = None

        self._init_clients()

    def _init_clients(self) -> None:
        """Initialize the appropriate API client based on config."""
        provider = self._config.api_provider

        if provider == "anthropic":
            if self._config.anthropic_api_key:
                self._anthropic_client = AnthropicClient(
                    api_key=self._config.anthropic_api_key,
                    model=self._config.anthropic_model,
                    timeout_sec=self._config.api_timeout_sec,
                    max_retries=self._config.api_max_retries,
                )
                logger.info(
                    "API Router: Anthropic client initialized (model: %s)",
                    self._config.anthropic_model,
                )
            else:
                logger.warning(
                    "API Router: provider='anthropic' but ANTHROPIC_API_KEY is not set"
                )

        elif provider == "gemini":
            if self._config.gemini_api_key:
                self._gemini_client = GeminiClient(
                    api_key=self._config.gemini_api_key,
                    model=self._config.gemini_model,
                    timeout_sec=self._config.api_timeout_sec,
                    max_retries=self._config.api_max_retries,
                )
                logger.info(
                    "API Router: Gemini client initialized (model: %s)",
                    self._config.gemini_model,
                )
            else:
                logger.warning(
                    "API Router: provider='gemini' but GEMINI_API_KEY is not set. "
                    "Get a free key at https://aistudio.google.com/apikey"
                )

        elif provider == "ollama":
            self._ollama_client = OllamaClient(
                base_url=self._config.ollama_base_url,
                model=self._config.ollama_model,
                timeout_sec=self._config.ollama_timeout_sec,
            )
            logger.info(
                "API Router: Ollama client initialized (model: %s @ %s)",
                self._config.ollama_model,
                self._config.ollama_base_url,
            )

        elif provider == "openai":
            if self._config.openai_api_key:
                self._openai_client = OpenAIClient(api_key=self._config.openai_api_key)
                logger.info("API Router: OpenAI client initialized")
            else:
                logger.warning(
                    "API Router: provider='openai' but OPENAI_API_KEY is not set"
                )

        else:
            logger.error("API Router: Unknown provider '%s'", provider)

    @property
    def is_available(self) -> bool:
        """Whether an API client is configured and available."""
        provider = self._config.api_provider
        if provider == "anthropic":
            return self._anthropic_client is not None and self._anthropic_client.is_available
        if provider == "gemini":
            return self._gemini_client is not None and self._gemini_client.is_available
        if provider == "ollama":
            return self._ollama_client is not None
        if provider == "openai":
            return self._openai_client is not None and self._openai_client.is_available
        return False

    @property
    def provider_name(self) -> str:
        """Human-readable provider + model string for UI display."""
        p = self._config.api_provider
        if p == "anthropic":
            return f"Claude ({self._config.anthropic_model})"
        if p == "gemini":
            return f"Gemini ({self._config.gemini_model})"
        if p == "ollama":
            return f"Ollama ({self._config.ollama_model})"
        if p == "openai":
            return "OpenAI"
        return p

    async def send_guidance_request(
        self,
        user_text: str,
        screenshot_b64: Optional[str] = None,
        state_summary: Optional[str] = None,
        session: Optional["Session"] = None,
        on_text_chunk: Optional[Callable[[str], None]] = None,
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
        provider = self._config.api_provider

        # Budget pre-check (skip for Ollama — it's free/local)
        if provider != "ollama":
            estimated_total = ESTIMATED_INPUT_TOKENS + ESTIMATED_OUTPUT_TOKENS
            if not self._cost_tracker.can_spend(estimated_total):
                raise BudgetExceededError(
                    "Token budget would be exceeded. "
                    f"Daily: {self._cost_tracker.daily_total}/{self._cost_tracker.daily_cap}, "
                    f"Monthly: {self._cost_tracker.monthly_total}/{self._cost_tracker.monthly_cap}"
                )

        conversation_history = session.get_conversation_for_api() if session else []

        # Route to provider
        if provider == "anthropic" and self._anthropic_client:
            messages = build_messages(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                conversation_history=conversation_history,
            )
            response, in_tokens, out_tokens = await self._anthropic_client.send_message(
                messages=messages,
                screenshot_b64=screenshot_b64,
                system_prompt=SYSTEM_PROMPT,
                on_text_chunk=on_text_chunk,
            )

        elif provider == "gemini" and self._gemini_client:
            messages = build_gemini_messages(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                conversation_history=conversation_history,
            )
            response, in_tokens, out_tokens = await self._gemini_client.send_message(
                messages=messages,
                screenshot_b64=screenshot_b64,
                system_prompt=SYSTEM_PROMPT,
                on_text_chunk=on_text_chunk,
            )

        elif provider == "ollama" and self._ollama_client:
            messages = build_ollama_messages(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                conversation_history=conversation_history,
            )
            try:
                response, in_tokens, out_tokens = await self._ollama_client.send_message(
                    messages=messages,
                    screenshot_b64=screenshot_b64,
                    system_prompt=SYSTEM_PROMPT,
                )
            except OllamaError as e:
                raise RuntimeError(str(e)) from e

        else:
            raise RuntimeError(
                f"No API client available for provider '{provider}'. "
                "Check your API key configuration in .env"
            )

        # Record usage (Ollama tokens are local so no financial cost)
        self._cost_tracker.record_usage(in_tokens, out_tokens)
        if session:
            session.record_tokens(in_tokens, out_tokens)

        if self._cost_tracker.is_approaching_limit():
            logger.warning(
                "Token budget approaching limit: %s", self._cost_tracker.get_usage_summary()
            )

        return response

    async def send_initial_request(
        self,
        task_description: str,
        screenshot_b64: Optional[str] = None,
        session: Optional["Session"] = None,
        on_text_chunk: Optional[Callable[[str], None]] = None,
    ) -> NavigateStepResponse:
        """Send the first request for a new task."""
        user_text = INITIAL_CONTEXT_TEMPLATE.format(task_description=task_description)
        return await self.send_guidance_request(
            user_text=user_text,
            screenshot_b64=screenshot_b64,
            session=session,
            on_text_chunk=on_text_chunk,
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
        if self._gemini_client:
            await self._gemini_client.close()
        if self._ollama_client:
            await self._ollama_client.close()


class BudgetExceededError(Exception):
    """Raised when a request would exceed the token budget."""
    pass
