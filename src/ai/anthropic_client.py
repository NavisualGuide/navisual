"""Anthropic API client for AI Navigator.

Uses httpx for direct REST calls to the Anthropic Messages API with tool_use
for structured output. Supports streaming via SSE for fast first-token display.
Returns validated NavigateStepResponse objects.
"""

import asyncio
import json
import logging
import re
from typing import Callable, Optional

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
                    "anthropic-beta": "prompt-caching-2024-07-31",
                    "content-type": "application/json",
                },
            )
        return self._client

    async def send_message(
        self,
        messages: list[dict],
        screenshot_b64: Optional[str] = None,
        system_prompt: str = "",
        on_text_chunk: Optional[Callable[[str], None]] = None,
        model_override: Optional[str] = None,
    ) -> tuple[NavigateStepResponse, int, int]:
        """Send a message to the Anthropic API with tool_use streaming.

        Streams the response via SSE. As the tool input JSON arrives in chunks,
        extracts the first instruction text incrementally and calls on_text_chunk
        for each new character — so the UI can show text as it arrives.

        Args:
            messages: Conversation history in Anthropic format.
            screenshot_b64: Base64-encoded JPEG screenshot (optional).
            system_prompt: System prompt text.
            on_text_chunk: Optional callback called with each new instruction
                           text fragment as it streams in.

        Returns:
            Tuple of (NavigateStepResponse, input_tokens, output_tokens).

        Raises:
            AnthropicAPIError: On API errors after retries.
        """
        client = await self._ensure_client()

        # Use cache_control on system prompt + tool schema — both are large and
        # constant across every request. Anthropic charges 10% of input token cost
        # for cache writes and only 10% for cache reads (90% cheaper on hits).
        # Minimum cacheable block = 1024 tokens; system prompt + tool schema easily
        # exceeds this. Cache lifetime is 5 minutes (extended on each hit).
        cached_tool = dict(NAVIGATE_STEP_TOOL)
        cached_tool["cache_control"] = {"type": "ephemeral"}

        effective_model = model_override or self.model
        payload = {
            "model": effective_model,
            "max_tokens": 1024,
            "stream": True,
            "system": [
                {
                    "type": "text",
                    "text": system_prompt,
                    "cache_control": {"type": "ephemeral"},
                }
            ],
            "tools": [cached_tool],
            "tool_choice": {"type": "tool", "name": "navigate_step"},
            "messages": messages,
        }

        last_error = None
        for attempt in range(1, self.max_retries + 1):
            try:
                async with client.stream("POST", ANTHROPIC_API_URL, json=payload) as resp:
                    if resp.status_code == 429:
                        retry_after = int(resp.headers.get("retry-after", 5))
                        logger.warning("Rate limited, retrying in %ds (attempt %d/%d)",
                                       retry_after, attempt, self.max_retries)
                        await asyncio.sleep(retry_after)
                        continue
                    elif resp.status_code == 529:
                        logger.warning("API overloaded, retrying in 10s (attempt %d/%d)",
                                       attempt, self.max_retries)
                        await asyncio.sleep(10)
                        continue
                    elif resp.status_code != 200:
                        error_body = await resp.aread()
                        logger.error("API error %d: %s", resp.status_code, error_body[:200])
                        last_error = AnthropicAPIError(resp.status_code, error_body.decode())
                        if resp.status_code >= 500:
                            await asyncio.sleep(2 ** attempt)
                            continue
                        raise last_error

                    result = await self._stream_response(resp, on_text_chunk)
                    return result

            except httpx.TimeoutException:
                logger.warning("API timeout (attempt %d/%d)", attempt, self.max_retries)
                last_error = AnthropicAPIError(0, "Request timed out")
                await asyncio.sleep(2 ** attempt)
            except httpx.HTTPError as e:
                logger.error("HTTP error: %s (attempt %d/%d)", e, attempt, self.max_retries)
                last_error = AnthropicAPIError(0, str(e))
                await asyncio.sleep(2 ** attempt)

        raise last_error or AnthropicAPIError(0, "Max retries exceeded")

    async def _stream_response(
        self,
        resp: httpx.Response,
        on_text_chunk: Optional[Callable[[str], None]],
    ) -> tuple[NavigateStepResponse, int, int]:
        """Process the SSE stream, calling on_text_chunk as instruction text arrives.

        Anthropic streams tool input as `input_json_delta` events containing
        partial JSON. We accumulate these and extract the `instruction` field
        text incrementally using a simple state machine — no JSON parser needed
        until the stream is complete.
        """
        accumulated_json = ""
        input_tokens = 0
        output_tokens = 0

        # State for incremental instruction extraction
        instruction_prefix = '"instruction": "'
        emitted_instruction_len = 0  # chars of instruction already sent to callback
        in_instruction = False        # currently inside an instruction value

        async for line in resp.aiter_lines():
            if not line.startswith("data: "):
                continue
            data_str = line[6:]
            if data_str == "[DONE]":
                break
            try:
                event = json.loads(data_str)
            except json.JSONDecodeError:
                continue

            event_type = event.get("type", "")

            # Accumulate tool input JSON deltas
            if event_type == "content_block_delta":
                delta = event.get("delta", {})
                if delta.get("type") == "input_json_delta":
                    chunk = delta.get("partial_json", "")
                    accumulated_json += chunk

                    # Incrementally emit instruction text to callback
                    if on_text_chunk and chunk:
                        _emit_instruction_chunks(
                            accumulated_json, chunk,
                            emitted_instruction_len, in_instruction,
                            instruction_prefix, on_text_chunk,
                        )
                        # Update state
                        if not in_instruction and instruction_prefix in accumulated_json:
                            in_instruction = True
                        if in_instruction:
                            visible = _extract_visible_instruction(accumulated_json)
                            emitted_instruction_len = len(visible)

            elif event_type == "message_delta":
                usage = event.get("usage", {})
                output_tokens = usage.get("output_tokens", output_tokens)

            elif event_type == "message_start":
                usage = event.get("message", {}).get("usage", {})
                input_tokens = usage.get("input_tokens", 0)
                cache_read = usage.get("cache_read_input_tokens", 0)
                cache_write = usage.get("cache_creation_input_tokens", 0)
                if cache_read:
                    logger.info("Prompt cache HIT: %d tokens read (saved ~90%% cost)", cache_read)
                elif cache_write:
                    logger.info("Prompt cache WRITE: %d tokens cached for future requests", cache_write)

        return self._parse_tool_json(accumulated_json), input_tokens, output_tokens

    def _parse_tool_json(self, accumulated_json: str) -> NavigateStepResponse:
        """Parse the fully accumulated tool input JSON into a NavigateStepResponse."""
        try:
            data = json.loads(accumulated_json)
            return NavigateStepResponse(**data)
        except (json.JSONDecodeError, Exception) as e:
            raise AnthropicAPIError(0, f"Failed to parse tool input JSON: {e}\n{accumulated_json[:200]}")

    def _parse_response(self, data: dict) -> tuple[NavigateStepResponse, int, int]:
        """Parse a non-streaming API response (kept for fallback compatibility)."""
        usage = data.get("usage", {})
        input_tokens = usage.get("input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)

        content_blocks = data.get("content", [])
        for block in content_blocks:
            if block.get("type") == "tool_use" and block.get("name") == "navigate_step":
                tool_input = block.get("input", {})
                response = NavigateStepResponse(**tool_input)
                return response, input_tokens, output_tokens

        text_content = " ".join(
            block.get("text", "") for block in content_blocks if block.get("type") == "text"
        )
        if text_content:
            logger.warning("AI returned text instead of tool_use: %s", text_content[:100])
        raise AnthropicAPIError(0, "No navigate_step tool_use block in response")

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


# ---------------------------------------------------------------------------
# Streaming helpers
# ---------------------------------------------------------------------------

# Matches the start of any instruction value in the partial tool JSON
_INSTRUCTION_RE = re.compile(r'"instruction":\s*"')


def _extract_visible_instruction(partial_json: str) -> str:
    """Extract the first instruction string from partial (possibly incomplete) JSON.

    Scans forward from the `"instruction": "` marker and collects characters
    up to the first unescaped `"` or end of string, whichever comes first.
    """
    m = _INSTRUCTION_RE.search(partial_json)
    if not m:
        return ""
    start = m.end()
    result = []
    i = start
    while i < len(partial_json):
        ch = partial_json[i]
        if ch == "\\" and i + 1 < len(partial_json):
            # Handle escape sequences — convert \" to " etc.
            next_ch = partial_json[i + 1]
            if next_ch == '"':
                result.append('"')
            elif next_ch == 'n':
                result.append('\n')
            elif next_ch == 't':
                result.append('\t')
            else:
                result.append(next_ch)
            i += 2
            continue
        if ch == '"':
            break  # end of string value
        result.append(ch)
        i += 1
    return "".join(result)


def _emit_instruction_chunks(
    accumulated_json: str,
    _new_chunk: str,
    already_emitted: int,
    in_instruction: bool,
    prefix: str,
    callback: Callable[[str], None],
) -> None:
    """Emit any new instruction characters since the last call."""
    if not in_instruction and prefix not in accumulated_json:
        return
    visible = _extract_visible_instruction(accumulated_json)
    if len(visible) > already_emitted:
        new_text = visible[already_emitted:]
        if new_text:
            callback(new_text)


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
