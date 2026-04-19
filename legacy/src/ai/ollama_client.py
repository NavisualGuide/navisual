"""Ollama local model client for AI Navigator.

Uses the Ollama native REST API for local inference. Supports vision-capable
models (llama3.2-vision, llava, moondream) for screenshot analysis.

No API key required — runs entirely on the user's machine.
Install Ollama: https://ollama.com
Pull a vision model: `ollama pull llama3.2-vision`

Structured output uses JSON mode + schema in system prompt since tool_use
support varies across Ollama models.
"""

import json
import logging
from typing import Optional

import httpx

from src.ai.tool_schemas import NavigateStepResponse

logger = logging.getLogger(__name__)

# JSON schema embedded in the system prompt so Ollama models output
# valid navigate_step responses without requiring tool_use support.
OLLAMA_JSON_SCHEMA_PROMPT = """
You MUST respond with a single valid JSON object matching this exact schema:
{
  "steps": [
    {
      "instruction": "string — clear instruction for the user",
      "target_text": "string or null — exact UI element text to locate",
      "target_role": "button|tab|link|textbox|menuitem|checkbox|radio|combobox|slider|image|heading|other or null",
      "target_region": "top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|bottom-right or null",
      "target_zone_x": "integer 0-15 or null — column of 16x9 grid cell (0=left, 15=right)",
      "target_zone_y": "integer 0-8 or null — row of 16x9 grid cell (0=top, 8=bottom)",
      "overlay_type": "arrow|highlight|circle|none",
      "clipboard": "string or null — text to copy to clipboard",
      "checkpoint": true
    }
  ],
  "state_summary": "string — compact app state description",
  "needs_input": false
}

Rules:
- steps: array of 1-4 steps
- All string fields required unless marked "or null"
- overlay_type defaults to "arrow"
- checkpoint defaults to true
- Respond with JSON ONLY — no explanation text before or after
"""


class OllamaClient:
    """Ollama local model client for AI Navigator.

    Sends requests to the local Ollama server using the native chat API.
    Uses JSON mode for structured output (compatible with all models).
    Vision input is supported for models like llama3.2-vision and llava.

    Recommended models:
    - llama3.2-vision (11B) — best quality, requires ~8GB VRAM or 16GB RAM
    - llava:7b — lighter alternative, good for lower-end hardware
    - moondream — very lightweight, minimal quality
    """

    def __init__(
        self,
        base_url: str = "http://localhost:11434",
        model: str = "llama3.2-vision",
        timeout_sec: int = 60,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.model = model
        self.timeout_sec = timeout_sec
        self._client: Optional[httpx.AsyncClient] = None

    async def _ensure_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout_sec, connect=5.0),
                headers={"content-type": "application/json"},
            )
        return self._client

    async def send_message(
        self,
        messages: list[dict],
        screenshot_b64: Optional[str] = None,
        system_prompt: str = "",
    ) -> tuple[NavigateStepResponse, int, int]:
        """Send a message to the local Ollama server.

        Args:
            messages: Conversation history in Ollama chat format.
            screenshot_b64: Base64-encoded JPEG screenshot (optional).
            system_prompt: System prompt text.

        Returns:
            Tuple of (NavigateStepResponse, input_tokens, output_tokens).

        Raises:
            OllamaError: If the server is unreachable or returns an error.
        """
        client = await self._ensure_client()

        # Combine system prompt with JSON schema instruction
        full_system = (system_prompt + "\n\n" + OLLAMA_JSON_SCHEMA_PROMPT).strip()

        payload = {
            "model": self.model,
            "messages": messages,
            "system": full_system,
            "stream": False,
            "format": "json",
            "options": {
                "temperature": 0.1,  # Low temp for consistent structured output
            },
        }

        try:
            response = await client.post(f"{self.base_url}/api/chat", json=payload)

            if response.status_code == 200:
                return self._parse_response(response.json())

            error_body = response.text
            logger.error("Ollama error %d: %s", response.status_code, error_body[:200])
            raise OllamaError(
                f"Ollama returned status {response.status_code}: {error_body[:100]}"
            )

        except httpx.ConnectError:
            raise OllamaError(
                f"Cannot connect to Ollama at {self.base_url}. "
                "Is Ollama running? Start it with: ollama serve"
            )
        except httpx.TimeoutException:
            raise OllamaError(
                f"Ollama request timed out after {self.timeout_sec}s. "
                "The model may be loading. Try again in a moment."
            )

    def _parse_response(self, data: dict) -> tuple[NavigateStepResponse, int, int]:
        """Parse the Ollama chat response into NavigateStepResponse."""
        message = data.get("message", {})
        content = message.get("content", "")

        # Token counts from Ollama eval stats
        input_tokens = data.get("prompt_eval_count", 0)
        output_tokens = data.get("eval_count", 0)

        try:
            parsed = json.loads(content)
            response = NavigateStepResponse(**parsed)
            logger.debug(
                "Ollama navigate_step: %d steps, state='%s'",
                len(response.steps),
                response.state_summary[:50],
            )
            return response, input_tokens, output_tokens

        except json.JSONDecodeError as e:
            logger.error("Ollama returned invalid JSON: %s\nContent: %s", e, content[:300])
            raise OllamaError(f"Model returned invalid JSON: {e}")
        except Exception as e:
            logger.error("Failed to parse Ollama response: %s\nData: %s", e, str(parsed)[:200])
            raise OllamaError(f"Failed to parse navigate_step response: {e}")

    async def check_model_available(self) -> bool:
        """Check if the configured model is available on the local Ollama server."""
        client = await self._ensure_client()
        try:
            response = await client.get(f"{self.base_url}/api/tags")
            if response.status_code == 200:
                models = response.json().get("models", [])
                available = [m.get("name", "").split(":")[0] for m in models]
                model_name = self.model.split(":")[0]
                if model_name not in available:
                    logger.warning(
                        "Ollama model '%s' not found. Available: %s. "
                        "Run: ollama pull %s",
                        self.model, available, self.model,
                    )
                    return False
                return True
        except (httpx.ConnectError, httpx.TimeoutException):
            pass
        return False

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()

    @property
    def is_available(self) -> bool:
        """Always True — availability is checked at request time."""
        return True


class OllamaError(Exception):
    """Error from the Ollama local server."""
    pass


def build_ollama_messages(
    user_text: str,
    screenshot_b64: Optional[str] = None,
    state_summary: Optional[str] = None,
    conversation_history: Optional[list[dict]] = None,
) -> list[dict]:
    """Build the messages array for the Ollama chat API.

    Ollama's chat API uses a similar format to OpenAI but images
    are passed as a list in the 'images' field of the message.
    """
    messages = []

    # Convert conversation history (text-only turns)
    if conversation_history:
        for turn in conversation_history:
            role = turn.get("role", "user")
            content = turn.get("content", "")
            if isinstance(content, str):
                messages.append({"role": role, "content": content})

    # Build current user message
    parts = []
    if state_summary:
        parts.append(f"[Context] {state_summary}")
    parts.append(user_text)

    message: dict = {
        "role": "user",
        "content": "\n".join(parts),
    }

    # Attach screenshot as image if provided
    if screenshot_b64:
        message["images"] = [screenshot_b64]

    messages.append(message)
    return messages
