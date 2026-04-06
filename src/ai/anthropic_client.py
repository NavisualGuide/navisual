"""Anthropic API client for AI Navigator.

Uses httpx for direct REST calls to the Anthropic Messages API with tool_use
for structured output. Returns validated NavigateStepResponse objects.
"""

import asyncio
import base64
import logging
from typing import Optional

import httpx

from src.ai.tool_schemas import NAVIGATE_STEP_TOOL, NavigateStepResponse

logger = logging.getLogger(__name__)

ANTHROPIC_API_URL = "https://api.anthropic.com/v1/messages"
ANTHROPIC_VERSION = "2023-06-01"


class AnthropicClient:
    """Anthropic Messages API client with tool_use support.

    Sends screenshots + conversation history to Claude and parses
    the navigate_step tool call response into structured data.
    """

    def __init__(
        self,
        api_key: str,
        model: str = "claude-sonnet-4-20250514",
        timeout_sec: int = 30,
        max_retries: int = 3,
    ) -> None:
        self.api_key = api_key
        self.model = model
        self.timeout_sec = timeout_sec
        self.max_retries = max_retries
        self._client: Optional[httpx.AsyncClient] = None

    async def _ensure_client(self) -> httpx.AsyncClient:
        """Get or create the async HTTP client."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout_sec, connect=10.0),
                headers={
                    "x-api-key": self.api_key,
                    "anthropic-version": ANTHROPIC_VERSION,
                    "content-type": "application/json",
                },
            )
        return self._client

    async def send_message(
        self,
        messages: list[dict],
        screenshot_b64: Optional[str] = None,
        system_prompt: str = "",
    ) -> tuple[NavigateStepResponse, int, int]:
        """Send a message to the Anthropic API with tool_use.

        Args:
            messages: Conversation history in Anthropic format.
            screenshot_b64: Base64-encoded JPEG screenshot (optional).
            system_prompt: System prompt text.

        Returns:
            Tuple of (NavigateStepResponse, input_tokens, output_tokens).

        Raises:
            AnthropicAPIError: On API errors after retries.
        """
        client = await self._ensure_client()

        # Build the request payload
        payload = {
            "model": self.model,
            "max_tokens": 1024,
            "system": system_prompt,
            "tools": [NAVIGATE_STEP_TOOL],
            "tool_choice": {"type": "tool", "name": "navigate_step"},
            "messages": messages,
        }

        # Execute with retries
        last_error = None
        for attempt in range(1, self.max_retries + 1):
            try:
                response = await client.post(ANTHROPIC_API_URL, json=payload)

                if response.status_code == 200:
                    return self._parse_response(response.json())

                # Handle specific error codes
                if response.status_code == 429:  # Rate limit
                    retry_after = int(response.headers.get("retry-after", 5))
                    logger.warning("Rate limited, retrying in %ds (attempt %d/%d)",
                                   retry_after, attempt, self.max_retries)
                    await asyncio.sleep(retry_after)
                    continue
                elif response.status_code == 529:  # Overloaded
                    logger.warning("API overloaded, retrying in 10s (attempt %d/%d)",
                                   attempt, self.max_retries)
                    await asyncio.sleep(10)
                    continue
                else:
                    error_body = response.text
                    logger.error("API error %d: %s", response.status_code, error_body[:200])
                    last_error = AnthropicAPIError(response.status_code, error_body)
                    if response.status_code >= 500:
                        await asyncio.sleep(2 ** attempt)
                        continue
                    raise last_error

            except httpx.TimeoutException:
                logger.warning("API timeout (attempt %d/%d)", attempt, self.max_retries)
                last_error = AnthropicAPIError(0, "Request timed out")
                await asyncio.sleep(2 ** attempt)
            except httpx.HTTPError as e:
                logger.error("HTTP error: %s (attempt %d/%d)", e, attempt, self.max_retries)
                last_error = AnthropicAPIError(0, str(e))
                await asyncio.sleep(2 ** attempt)

        raise last_error or AnthropicAPIError(0, "Max retries exceeded")

    def _parse_response(self, data: dict) -> tuple[NavigateStepResponse, int, int]:
        """Parse the API response and extract the tool_use result.

        Returns:
            Tuple of (NavigateStepResponse, input_tokens, output_tokens).
        """
        # Extract token usage
        usage = data.get("usage", {})
        input_tokens = usage.get("input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)

        # Find the tool_use content block
        content_blocks = data.get("content", [])
        for block in content_blocks:
            if block.get("type") == "tool_use" and block.get("name") == "navigate_step":
                tool_input = block.get("input", {})
                response = NavigateStepResponse(**tool_input)
                logger.debug(
                    "Parsed navigate_step: %d steps, state='%s'",
                    len(response.steps), response.state_summary[:50],
                )
                return response, input_tokens, output_tokens

        # No tool_use block found — check for text response
        text_content = " ".join(
            block.get("text", "") for block in content_blocks if block.get("type") == "text"
        )
        if text_content:
            logger.warning("AI returned text instead of tool_use: %s", text_content[:100])

        raise AnthropicAPIError(
            0, "No navigate_step tool_use block in response"
        )

    async def close(self) -> None:
        """Close the HTTP client."""
        if self._client and not self._client.is_closed:
            await self._client.aclose()

    @property
    def is_available(self) -> bool:
        return bool(self.api_key)


class AnthropicAPIError(Exception):
    """Error from the Anthropic API."""

    def __init__(self, status_code: int, message: str) -> None:
        self.status_code = status_code
        self.message = message
        super().__init__(f"Anthropic API error ({status_code}): {message}")


def build_messages(
    user_text: str,
    screenshot_b64: Optional[str] = None,
    state_summary: Optional[str] = None,
    conversation_history: Optional[list[dict]] = None,
) -> list[dict]:
    """Build the messages array for the Anthropic API.

    Constructs proper content blocks with text and optional images.
    """
    messages = []

    # Include conversation history (text-only turns)
    if conversation_history:
        messages.extend(conversation_history)

    # Build the current user message
    content = []

    # Add state summary as context
    if state_summary:
        content.append({"type": "text", "text": f"[Context] {state_summary}"})

    # Add screenshot
    if screenshot_b64:
        content.append({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": screenshot_b64,
            },
        })

    # Add user text
    content.append({"type": "text", "text": user_text})

    messages.append({"role": "user", "content": content})

    return messages
