"""Tests for the API router."""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from src.ai.api_router import APIRouter, BudgetExceededError
from src.ai.tool_schemas import NavigateStep, NavigateStepResponse
from src.config import Config
from src.core.cost_tracker import CostTracker


class TestAPIRouter:
    def _make_router(self, api_key="test-key", daily_cap=100000):
        config = MagicMock(spec=Config)
        config.api_provider = "anthropic"
        config.anthropic_api_key = api_key
        config.anthropic_model = "claude-sonnet-4-20250514"
        config.api_timeout_sec = 30
        config.api_max_retries = 1

        tracker = CostTracker(daily_cap=daily_cap, monthly_cap=1000000, safety_margin=1.0)
        return APIRouter(config=config, cost_tracker=tracker), tracker

    def test_is_available_with_key(self):
        router, _ = self._make_router(api_key="sk-ant-test")
        assert router.is_available

    def test_is_not_available_without_key(self):
        router, _ = self._make_router(api_key=None)
        assert not router.is_available

    @pytest.mark.asyncio
    async def test_budget_exceeded_raises(self):
        router, tracker = self._make_router(daily_cap=100)
        tracker.record_usage(90, 10)  # At cap

        with pytest.raises(BudgetExceededError):
            await router.send_guidance_request(
                user_text="test",
                screenshot_b64=None,
            )

    @pytest.mark.asyncio
    async def test_no_client_raises(self):
        router, _ = self._make_router(api_key=None)

        with pytest.raises(RuntimeError, match="No API client"):
            await router.send_guidance_request(
                user_text="test",
                screenshot_b64=None,
            )
