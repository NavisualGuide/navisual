"""Token usage tracking and budget enforcement for AI Navigator.

Tracks daily and monthly token consumption, applies safety margins,
and enforces caps to prevent runaway costs.
"""

import json
import logging
from datetime import date, datetime, timezone
from pathlib import Path

from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)


class TokenUsage(BaseModel):
    """Persistent token usage record."""

    date: str = Field(default_factory=lambda: date.today().isoformat())
    daily_input: int = 0
    daily_output: int = 0
    monthly_input: int = 0
    monthly_output: int = 0
    month: str = Field(default_factory=lambda: date.today().strftime("%Y-%m"))
    last_updated: str = Field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )


class CostTracker:
    """Tracks token usage and enforces budget caps.

    Applies a safety margin multiplier to cost estimates during early versions
    to account for imperfect optimization (default 2.5x, reduce toward 1.0
    as optimizations mature).
    """

    def __init__(
        self,
        daily_cap: int = 100_000,
        monthly_cap: int = 5_000_000,
        safety_margin: float = 2.5,
        storage_path: Path | None = None,
    ):
        self.daily_cap = daily_cap
        self.monthly_cap = monthly_cap
        self.safety_margin = safety_margin
        self._storage_path = storage_path
        self._usage = TokenUsage()
        self._load()

    def _load(self) -> None:
        """Load persisted usage data."""
        if self._storage_path and self._storage_path.exists():
            try:
                data = json.loads(self._storage_path.read_text(encoding="utf-8"))
                self._usage = TokenUsage(**data)
                # Reset if day or month has changed
                today = date.today()
                if self._usage.date != today.isoformat():
                    self._usage.daily_input = 0
                    self._usage.daily_output = 0
                    self._usage.date = today.isoformat()
                current_month = today.strftime("%Y-%m")
                if self._usage.month != current_month:
                    self._usage.monthly_input = 0
                    self._usage.monthly_output = 0
                    self._usage.month = current_month
            except (json.JSONDecodeError, KeyError, ValueError) as e:
                logger.warning("Failed to load token usage data, resetting: %s", e)
                self._usage = TokenUsage()

    def _save(self) -> None:
        """Persist usage data to disk."""
        if self._storage_path:
            self._storage_path.parent.mkdir(parents=True, exist_ok=True)
            self._storage_path.write_text(self._usage.model_dump_json(indent=2), encoding="utf-8")

    @property
    def daily_total(self) -> int:
        """Total tokens used today."""
        return self._usage.daily_input + self._usage.daily_output

    @property
    def monthly_total(self) -> int:
        """Total tokens used this month."""
        return self._usage.monthly_input + self._usage.monthly_output

    def can_spend(self, estimated_tokens: int) -> bool:
        """Check if a request with estimated_tokens is within budget.

        Applies the safety margin multiplier to the estimate.
        """
        adjusted = int(estimated_tokens * self.safety_margin)
        within_daily = self.daily_total + adjusted <= self.daily_cap
        within_monthly = self.monthly_total + adjusted <= self.monthly_cap
        if not within_daily:
            logger.warning(
                "Daily token cap would be exceeded: %d + %d > %d",
                self.daily_total, adjusted, self.daily_cap,
            )
        if not within_monthly:
            logger.warning(
                "Monthly token cap would be exceeded: %d + %d > %d",
                self.monthly_total, adjusted, self.monthly_cap,
            )
        return within_daily and within_monthly

    def record_usage(self, input_tokens: int, output_tokens: int) -> None:
        """Record actual token usage from an API response."""
        self._usage.daily_input += input_tokens
        self._usage.daily_output += output_tokens
        self._usage.monthly_input += input_tokens
        self._usage.monthly_output += output_tokens
        self._usage.last_updated = datetime.now(timezone.utc).isoformat()
        self._save()
        logger.debug(
            "Token usage recorded: +%d in, +%d out (daily total: %d, monthly: %d)",
            input_tokens, output_tokens, self.daily_total, self.monthly_total,
        )

    def is_approaching_limit(self, threshold: float = 0.8) -> bool:
        """Check if usage is approaching the cap (default 80%)."""
        daily_pct = self.daily_total / self.daily_cap if self.daily_cap > 0 else 0
        monthly_pct = self.monthly_total / self.monthly_cap if self.monthly_cap > 0 else 0
        return daily_pct >= threshold or monthly_pct >= threshold

    def get_usage_summary(self) -> dict:
        """Return a summary of current usage for display."""
        return {
            "daily_used": self.daily_total,
            "daily_cap": self.daily_cap,
            "daily_pct": round(self.daily_total / self.daily_cap * 100, 1) if self.daily_cap else 0,
            "monthly_used": self.monthly_total,
            "monthly_cap": self.monthly_cap,
            "monthly_pct": round(self.monthly_total / self.monthly_cap * 100, 1) if self.monthly_cap else 0,
        }
