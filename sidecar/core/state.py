"""State summary storage for AI Navigator.

Stores compact text summaries of application state from AI responses.
These replace old screenshots with ~100 tokens of text context.
"""

from datetime import datetime, timezone

from pydantic import BaseModel, Field


class StateSummary(BaseModel):
    """Compact text summary of current application state."""

    summary_text: str = Field(description="AI-generated state summary text.")
    timestamp: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    turn_index: int = Field(default=0, description="Which conversation turn generated this.")

    def to_context_string(self) -> str:
        """Format the state summary for inclusion in API payloads."""
        return f"Previous state: {self.summary_text}"

    def __str__(self) -> str:
        return self.summary_text
