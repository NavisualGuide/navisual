"""Tests for state summary management."""

from src.core.state import StateSummary


class TestStateSummary:
    def test_creation(self):
        state = StateSummary(summary_text="Blender open. Cube selected.")
        assert state.summary_text == "Blender open. Cube selected."
        assert state.turn_index == 0

    def test_to_context_string(self):
        state = StateSummary(summary_text="Amazon homepage. No search yet.")
        ctx = state.to_context_string()
        assert "Previous state:" in ctx
        assert "Amazon homepage" in ctx

    def test_str(self):
        state = StateSummary(summary_text="Testing state")
        assert str(state) == "Testing state"

    def test_turn_index(self):
        state = StateSummary(summary_text="test", turn_index=5)
        assert state.turn_index == 5
