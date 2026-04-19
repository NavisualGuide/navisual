"""OpenAI API client stub for AI Navigator v0.2.

Will implement function_calling for structured output,
matching the same NavigateStepResponse interface as the Anthropic client.
"""

import logging

logger = logging.getLogger(__name__)


class OpenAIClient:
    """OpenAI API client (stub for v0.2).

    Will support:
    - GPT-4o with function_calling
    - Vision (image input)
    - Structured output via function definitions
    """

    def __init__(self, api_key: str | None = None) -> None:
        self.api_key = api_key
        logger.info("OpenAIClient: stub initialized (available in v0.2)")

    async def send_message(
        self,
        messages: list[dict],
        screenshot_b64: str | None = None,
        system_prompt: str = "",
        tools: list[dict] | None = None,
    ) -> dict:
        """Send a message to the OpenAI API."""
        raise NotImplementedError("OpenAI support will be available in v0.2")

    @property
    def is_available(self) -> bool:
        return self.api_key is not None
