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

from ai.anthropic_client import AnthropicClient, build_messages
from ai.gemini_client import GeminiAPIError, GeminiClient, build_gemini_messages
from ai.ollama_client import OllamaClient, OllamaError, build_ollama_messages
from ai.openai_client import OpenAIClient
from ai.prompts import (
    INITIAL_CONTEXT_TEMPLATE,
    SESSION_RESUME_TEMPLATE,
    SYSTEM_PROMPT,
)
from ai.tool_schemas import NavigateStepResponse
from config import Config
from core.cost_tracker import CostTracker, ManagedCredit

if TYPE_CHECKING:
    from core.session import Session

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

    def __init__(
        self,
        config: Config,
        cost_tracker: CostTracker,
        managed_credit: Optional[ManagedCredit] = None,
    ) -> None:
        self._config = config
        self._cost_tracker = cost_tracker
        self._managed_credit = managed_credit
        self._anthropic_client: Optional[AnthropicClient] = None
        self._gemini_client: Optional[GeminiClient] = None
        self._ollama_client: Optional[OllamaClient] = None
        self._openai_client: Optional[OpenAIClient] = None
        self._managed_client: Optional[AnthropicClient | GeminiClient] = None

        self._init_clients()
        self._init_managed_client()

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
                    "API Router: Gemini client initialized (model: %s, fast model: %s)",
                    self._config.gemini_model,
                    self._config.gemini_fast_model,
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

    def _init_managed_client(self) -> None:
        """Initialize the managed (embedded) API client if a managed key is configured."""
        if not self._config.managed_api_key:
            return
        mp = self._config.managed_provider
        if mp == "gemini":
            self._managed_client = GeminiClient(
                api_key=self._config.managed_api_key,
                model=self._config.gemini_model,
                timeout_sec=self._config.api_timeout_sec,
                max_retries=self._config.api_max_retries,
            )
        elif mp == "anthropic":
            self._managed_client = AnthropicClient(
                api_key=self._config.managed_api_key,
                model=self._config.anthropic_model,
                timeout_sec=self._config.api_timeout_sec,
                max_retries=self._config.api_max_retries,
            )
        else:
            logger.error("Managed key: unknown provider '%s'", mp)
            return
        logger.info("Managed free-trial client ready (provider=%s, cap=%d tokens)", mp, self._config.managed_token_cap)

    def _has_user_client(self) -> bool:
        """Return True if the user has configured their own key for the active provider."""
        p = self._config.api_provider
        if p == "anthropic":
            return self._anthropic_client is not None
        if p == "gemini":
            return self._gemini_client is not None
        if p == "ollama":
            return True  # Ollama never needs a key
        if p == "openai":
            return self._openai_client is not None
        return False

    @property
    def managed_credit_remaining(self) -> Optional[int]:
        """Remaining managed credit in tokens, or None if no managed key is configured."""
        if self._managed_credit is None or self._config.managed_api_key is None:
            return None
        return self._managed_credit.remaining

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
        use_fast_model: bool = False,
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
        estimated_total = ESTIMATED_INPUT_TOKENS + ESTIMATED_OUTPUT_TOKENS

        # Determine whether to use managed key (no user key configured)
        use_managed = not self._has_user_client() and self._managed_client is not None
        if use_managed:
            if self._managed_credit and self._managed_credit.is_exhausted:
                raise ManagedCreditExhaustedError(
                    "Your free trial is complete. Add your API key in Settings → Provider."
                )
            if self._managed_credit and not self._managed_credit.can_spend(estimated_total):
                raise ManagedCreditExhaustedError(
                    "Your free trial is complete. Add your API key in Settings → Provider."
                )

        # Budget pre-check for user key (skip for Ollama — it's free/local)
        if not use_managed and provider != "ollama":
            if not self._cost_tracker.can_spend(estimated_total):
                raise BudgetExceededError(
                    "Token budget would be exceeded. "
                    f"Daily: {self._cost_tracker.daily_total}/{self._cost_tracker.daily_cap}, "
                    f"Monthly: {self._cost_tracker.monthly_total}/{self._cost_tracker.monthly_cap}"
                )

        conversation_history = session.get_conversation_for_api() if session else []

        # Route to provider (managed key takes over when no user key is set)
        if use_managed and self._managed_client:
            mp = self._config.managed_provider
            if mp == "gemini" and isinstance(self._managed_client, GeminiClient):
                messages = build_gemini_messages(
                    user_text=user_text,
                    screenshot_b64=screenshot_b64,
                    state_summary=state_summary,
                    conversation_history=conversation_history,
                )
                model_override = self._config.gemini_fast_model if use_fast_model else None
                try:
                    response, in_tokens, out_tokens = await self._managed_client.send_message(
                        messages=messages,
                        screenshot_b64=screenshot_b64,
                        system_prompt=SYSTEM_PROMPT,
                        on_text_chunk=on_text_chunk,
                        model_override=model_override,
                    )
                except GeminiAPIError as e:
                    raise RuntimeError(f"Managed Gemini error: {e}") from e
            elif mp == "anthropic" and isinstance(self._managed_client, AnthropicClient):
                messages = build_messages(
                    user_text=user_text,
                    screenshot_b64=screenshot_b64,
                    state_summary=state_summary,
                    conversation_history=conversation_history,
                )
                response, in_tokens, out_tokens = await self._managed_client.send_message(
                    messages=messages,
                    screenshot_b64=screenshot_b64,
                    system_prompt=SYSTEM_PROMPT,
                    on_text_chunk=on_text_chunk,
                )
            else:
                raise RuntimeError(f"Managed client type mismatch for provider '{mp}'")

        elif provider == "anthropic" and self._anthropic_client:
            messages = build_messages(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                conversation_history=conversation_history,
            )
            # Model tiering: use fast model for automated screen-change re-queries
            model_override = None
            if use_fast_model and self._config.anthropic_fast_model != self._config.anthropic_model:
                model_override = self._config.anthropic_fast_model
                logger.debug("Model tiering: using fast model %s", model_override)
            response, in_tokens, out_tokens = await self._anthropic_client.send_message(
                messages=messages,
                screenshot_b64=screenshot_b64,
                system_prompt=SYSTEM_PROMPT,
                on_text_chunk=on_text_chunk,
                model_override=model_override,
            )

        elif provider == "gemini" and self._gemini_client:
            messages = build_gemini_messages(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                conversation_history=conversation_history,
            )
            model_override = None
            if use_fast_model and self._config.gemini_fast_model != self._config.gemini_model:
                model_override = self._config.gemini_fast_model
            try:
                response, in_tokens, out_tokens = await self._gemini_client.send_message(
                    messages=messages,
                    screenshot_b64=screenshot_b64,
                    system_prompt=SYSTEM_PROMPT,
                    on_text_chunk=on_text_chunk,
                    model_override=model_override,
                )
            except GeminiAPIError as e:
                # 503 = primary model overloaded. Fall back to fast model if different.
                fast = self._config.gemini_fast_model
                if e.status_code == 503 and fast != (model_override or self._config.gemini_model):
                    logger.warning(
                        "Gemini primary model unavailable (503), falling back to fast model: %s", fast
                    )
                    response, in_tokens, out_tokens = await self._gemini_client.send_message(
                        messages=messages,
                        screenshot_b64=screenshot_b64,
                        system_prompt=SYSTEM_PROMPT,
                        on_text_chunk=on_text_chunk,
                        model_override=fast,
                    )
                else:
                    raise

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

        # Record usage against the appropriate tracker
        if use_managed and self._managed_credit:
            self._managed_credit.record_usage(in_tokens, out_tokens)
        else:
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
        use_fast_model: bool = False,
    ) -> NavigateStepResponse:
        """Send the first request for a new task."""
        user_text = INITIAL_CONTEXT_TEMPLATE.format(task_description=task_description)
        return await self.send_guidance_request(
            user_text=user_text,
            screenshot_b64=screenshot_b64,
            session=session,
            on_text_chunk=on_text_chunk,
            use_fast_model=use_fast_model,
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


class ManagedCreditExhaustedError(Exception):
    """Raised when the embedded free-trial credit is fully consumed."""
    pass
