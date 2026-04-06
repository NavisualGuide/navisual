"""Tests for the cost tracker."""

import json
import tempfile
from pathlib import Path

from src.core.cost_tracker import CostTracker


class TestCostTracker:
    def test_initial_state(self):
        tracker = CostTracker(daily_cap=1000, monthly_cap=10000)
        assert tracker.daily_total == 0
        assert tracker.monthly_total == 0

    def test_can_spend(self):
        tracker = CostTracker(daily_cap=1000, monthly_cap=10000, safety_margin=1.0)
        assert tracker.can_spend(500)
        assert tracker.can_spend(1000)
        assert not tracker.can_spend(1001)

    def test_safety_margin(self):
        tracker = CostTracker(daily_cap=1000, monthly_cap=10000, safety_margin=2.5)
        # 200 * 2.5 = 500 — should fit
        assert tracker.can_spend(200)
        # 500 * 2.5 = 1250 — exceeds daily cap
        assert not tracker.can_spend(500)

    def test_record_usage(self):
        tracker = CostTracker(daily_cap=10000, monthly_cap=100000, safety_margin=1.0)
        tracker.record_usage(100, 50)
        assert tracker.daily_total == 150
        assert tracker.monthly_total == 150

        tracker.record_usage(200, 75)
        assert tracker.daily_total == 425
        assert tracker.monthly_total == 425

    def test_approaching_limit(self):
        tracker = CostTracker(daily_cap=100, monthly_cap=1000, safety_margin=1.0)
        assert not tracker.is_approaching_limit()

        tracker.record_usage(85, 0)
        assert tracker.is_approaching_limit(threshold=0.8)

    def test_usage_summary(self):
        tracker = CostTracker(daily_cap=1000, monthly_cap=10000, safety_margin=1.0)
        tracker.record_usage(100, 50)

        summary = tracker.get_usage_summary()
        assert summary["daily_used"] == 150
        assert summary["daily_cap"] == 1000
        assert summary["daily_pct"] == 15.0

    def test_persistence(self):
        with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as f:
            path = Path(f.name)

        try:
            # Save
            tracker1 = CostTracker(daily_cap=1000, monthly_cap=10000, storage_path=path)
            tracker1.record_usage(100, 50)

            # Load
            tracker2 = CostTracker(daily_cap=1000, monthly_cap=10000, storage_path=path)
            assert tracker2.daily_total == 150
        finally:
            path.unlink(missing_ok=True)
