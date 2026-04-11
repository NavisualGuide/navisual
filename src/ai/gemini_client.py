"""Google Gemini API client for AI Navigator.

Uses httpx for direct REST calls to the Gemini generateContent API with
function calling for structured navigate_step output. Supports vision
(screenshots) and multimodal conversation.

Free tier (Google AI Studio): ~1,500 requests/day with gemini-2.0-flash.
No credit card required for new users.
"""

import json
import logging
from typing import Callable, Optional

import httpx

from src.ai.tool_schemas import NavigateStepResponse

logger = logging.getLogger(__name__)

GEMINI_API_BASE = "https://generativelanguage.googleapis.com/v1beta/models"

# Gemini function declaration — mirrors NAVIGATE_STEP_TOOL in Anthropic format
# but uses Gemini's schema format (no $schema/$ref, uses OpenAPI 3.0 style)
GEMINI_NAVIGATE_STEP_FUNCTION = {
    "name": "navigate_step",
    "description": (
        "Provide navigation instructions for the user. Return one or more steps. "
        "Steps with checkpoint=true will wait for the user to complete the action before proceeding."
    ),
    "parameters": {
        "type": "object",
        "required": ["steps", "state_summary", "needs_input"],
        "properties": {
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["instruction", "checkpoint"],
                    "properties": {
                        "instruction": {
                            "type": "string",
                            "description": (
                                "The instruction shown to the user. "
                                "Be specific about visual appearance and position."
                            ),
                        },
                        "target_text": {
                            "type": "string",
                            "description": (
                                "Exact text label of the UI element to highlight. "
                                "Used by Accessibility API and OCR to locate it."
                            ),
                        },
                        "target_role": {
                            "type": "string",
                            "enum": [
                                "button", "tab", "link", "textbox", "menuitem",
                                "checkbox", "radio", "combobox", "slider",
                                "image", "heading", "other",
                            ],
                            "description": "The UI role/type of the target element.",
                        },
                        "target_region": {
                            "type": "string",
                            "enum": [
                                "top-left", "top-center", "top-right",
                                "center-left", "center", "center-right",
                                "bottom-left", "bottom-center", "bottom-right",
                            ],
                            "description": "Rough screen region to narrow the search.",
                        },
                        "overlay_type": {
                            "type": "string",
                            "enum": ["arrow", "highlight", "circle", "none"],
                            "description": "Type of visual overlay to draw on the target.",
                        },
                        "clipboard": {
                            "type": "string",
                            "description": "Text to copy to clipboard (CLI commands, text entry).",
                        },
                        "checkpoint": {
                            "type": "boolean",
                            "description": (
                                "If true, wait for screen change before advancing. "
                                "If false, auto-advance to the next step."
                            ),
                        },
                    },
                },
            },
            "state_summary": {
                "type": "string",
                "description": "Compact summary of current app state (not shown to user).",
            },
            "needs_input": {
                "type": "boolean",
                "description": "If true, AI needs the user to answer a question first.",
            },
            "request_full_screen": {
                "type": "boolean",
                "description": (
                    "Set true when the task requires seeing the full desktop "
                    "(Start Menu, taskbar, Desktop, OS dialogs). "
                    "The engine will serve the full virtual desktop screenshot next turn."
                ),
            },
        },
    },
}


class GeminiClient:
    """Google Gemini API client with function calling support.

    Uses the Gemini generateContent REST API directly via httpx.
    Supports multimodal input (text + screenshots).

    Free tier via Google AI Studio:
    - gemini-2.0-flash: 1,500 req/day, 1M tokens/min — sufficient for MVP testing
    - No credit card required
    """

    def __init__(
        self,
        api_key: str,
        model: str = "gemini-2.0-flash",
        timeout_sec: int = 30,
        max_retries: int = 3,
    ) -> None:
        self.api_key = api_key
        self.model = model
        self.timeout_sec = timeout_sec
        self.max_retries = max_retries
        self._client: Optional[httpx.AsyncClient] = None

    @property
    def api_url(self) -> str:
        return f"{GEMINI_API_BASE}/{self.model}:generateContent?key={self.api_key}"

    @property
    def stream_url(self) -> str:
        return f"{GEMINI_API_BASE}/{self.model}:streamGenerateContent?alt=sse&key={self.api_key}"

    async def _ensure_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout_sec, connect=10.0),
                headers={"content-type": "application/json"},
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
        """Send a message to the Gemini API with streaming function calling.

        Uses streamGenerateContent (SSE) so instruction text can be shown
        as it arrives. Accumulates function call args across chunks and parses
        the complete response at the end.

        Args:
            messages: Conversation history in Gemini 'contents' format.
            screenshot_b64: Base64-encoded JPEG screenshot (optional).
            system_prompt: System instruction text.
            on_text_chunk: Optional callback with each new instruction text fragment.

        Returns:
            Tuple of (NavigateStepResponse, input_tokens, output_tokens).

        Raises:
            GeminiAPIError: On API errors after retries.
        """
        import asyncio

        client = await self._ensure_client()
        effective_model = model_override or self.model
        if model_override and model_override != self.model:
            logger.debug("Gemini model tiering: using %s", effective_model)

        payload: dict = {
            "contents": messages,
            "tools": [{"function_declarations": [GEMINI_NAVIGATE_STEP_FUNCTION]}],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": ["navigate_step"],
                }
            },
        }

        if system_prompt:
            payload["systemInstruction"] = {"parts": [{"text": system_prompt}]}

        last_error: Optional[Exception] = None
        for attempt in range(1, self.max_retries + 1):
            try:
                url = f"{GEMINI_API_BASE}/{effective_model}:streamGenerateContent?alt=sse&key={self.api_key}"
                async with client.stream("POST", url, json=payload) as resp:
                    if resp.status_code == 429:
                        logger.warning("Gemini rate limited (attempt %d/%d)", attempt, self.max_retries)
                        await asyncio.sleep(5 * attempt)
                        continue
                    elif resp.status_code != 200:
                        error_body = await resp.aread()
                        logger.error("Gemini API error %d: %s", resp.status_code, error_body[:200])
                        last_error = GeminiAPIError(resp.status_code, error_body.decode())
                        if resp.status_code >= 500:
                            await asyncio.sleep(2 ** attempt)
                            continue
                        raise last_error

                    return await self._stream_response(resp, on_text_chunk)

            except httpx.TimeoutException:
                logger.warning("Gemini timeout (attempt %d/%d)", attempt, self.max_retries)
                last_error = GeminiAPIError(0, "Request timed out")
                await asyncio.sleep(2 ** attempt)
            except httpx.HTTPError as e:
                logger.error("Gemini HTTP error: %s", e)
                last_error = GeminiAPIError(0, str(e))
                await asyncio.sleep(2 ** attempt)

        raise last_error or GeminiAPIError(0, "Max retries exceeded")

    async def _stream_response(
        self,
        resp: httpx.Response,
        on_text_chunk: Optional[Callable[[str], None]],
    ) -> tuple[NavigateStepResponse, int, int]:
        """Process the Gemini SSE stream.

        Gemini streaming returns complete JSON objects per SSE event (not deltas).
        Each event is a full candidate with partial or complete function call args.
        We accumulate function call args across events and use the last complete one.
        """
        accumulated_args: dict = {}
        input_tokens = 0
        output_tokens = 0
        emitted_instruction_len = 0

        async for line in resp.aiter_lines():
            if not line.startswith("data: "):
                continue
            data_str = line[6:].strip()
            if not data_str or data_str == "[DONE]":
                continue
            try:
                event = json.loads(data_str)
            except json.JSONDecodeError:
                continue

            # Extract usage
            usage = event.get("usageMetadata", {})
            if usage:
                input_tokens = usage.get("promptTokenCount", input_tokens)
                output_tokens = usage.get("candidatesTokenCount", output_tokens)

            # Extract function call args from this chunk
            candidates = event.get("candidates", [])
            for candidate in candidates:
                parts = candidate.get("content", {}).get("parts", [])
                for part in parts:
                    if "functionCall" in part:
                        fn = part["functionCall"]
                        if fn.get("name") == "navigate_step":
                            # Gemini streams complete (or increasingly complete) args
                            args = fn.get("args", {})
                            if args:
                                accumulated_args = args

                            # Emit new instruction text if callback provided
                            if on_text_chunk and accumulated_args:
                                steps = accumulated_args.get("steps", [])
                                if steps and isinstance(steps, list):
                                    instruction = steps[0].get("instruction", "")
                                    if len(instruction) > emitted_instruction_len:
                                        new_text = instruction[emitted_instruction_len:]
                                        on_text_chunk(new_text)
                                        emitted_instruction_len = len(instruction)

        if not accumulated_args:
            raise GeminiAPIError(0, "No navigate_step function call in Gemini stream")

        try:
            response = NavigateStepResponse(**accumulated_args)
            logger.debug("Gemini navigate_step: %d steps", len(response.steps))
            return response, input_tokens, output_tokens
        except Exception as e:
            raise GeminiAPIError(0, f"Failed to parse Gemini function args: {e}")

    def _parse_response(self, data: dict) -> tuple[NavigateStepResponse, int, int]:
        """Parse the Gemini generateContent response."""
        usage = data.get("usageMetadata", {})
        input_tokens = usage.get("promptTokenCount", 0)
        output_tokens = usage.get("candidatesTokenCount", 0)

        candidates = data.get("candidates", [])
        if not candidates:
            raise GeminiAPIError(0, "No candidates in Gemini response")

        parts = candidates[0].get("content", {}).get("parts", [])
        for part in parts:
            if "functionCall" in part:
                fn_call = part["functionCall"]
                if fn_call.get("name") == "navigate_step":
                    args = fn_call.get("args", {})
                    response = NavigateStepResponse(**args)
                    logger.debug(
                        "Gemini navigate_step: %d steps, state='%s'",
                        len(response.steps),
                        response.state_summary[:50],
                    )
                    return response, input_tokens, output_tokens

        # Check for text fallback
        text_parts = [p.get("text", "") for p in parts if "text" in p]
        if text_parts:
            logger.warning("Gemini returned text instead of function call: %s", text_parts[0][:100])

        raise GeminiAPIError(0, "No navigate_step function call in Gemini response")

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()

    @property
    def is_available(self) -> bool:
        return bool(self.api_key)


class GeminiAPIError(Exception):
    def __init__(self, status_code: int, message: str) -> None:
        self.status_code = status_code
        self.message = message
        super().__init__(f"Gemini API error ({status_code}): {message}")


def build_gemini_messages(
    user_text: str,
    screenshot_b64: Optional[str] = None,
    state_summary: Optional[str] = None,
    conversation_history: Optional[list[dict]] = None,
) -> list[dict]:
    """Build the contents array for the Gemini API.

    Gemini uses 'parts' within each content block.
    Images use 'inlineData' with base64 encoding.
    """
    contents = []

    # Convert conversation history (text-only turns)
    if conversation_history:
        for turn in conversation_history:
            role = turn.get("role", "user")
            # Gemini uses 'model' instead of 'assistant'
            gemini_role = "model" if role == "assistant" else "user"
            content_text = turn.get("content", "")
            if isinstance(content_text, str):
                contents.append({
                    "role": gemini_role,
                    "parts": [{"text": content_text}],
                })

    # Build current user message parts
    parts = []

    if state_summary:
        parts.append({"text": f"[Context] {state_summary}"})

    if screenshot_b64:
        parts.append({
            "inlineData": {
                "mimeType": "image/jpeg",
                "data": screenshot_b64,
            }
        })

    parts.append({"text": user_text})

    contents.append({"role": "user", "parts": parts})
    return contents
