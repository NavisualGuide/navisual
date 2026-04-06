"""Tests for the step sequencer."""

import pytest

from src.ai.tool_schemas import NavigateStep
from src.core.step_sequencer import StepSequencer


def _make_step(instruction: str, checkpoint: bool = False, **kwargs) -> NavigateStep:
    return NavigateStep(instruction=instruction, checkpoint=checkpoint, **kwargs)


class TestStepSequencer:
    def test_empty_sequencer(self):
        seq = StepSequencer()
        assert seq.is_complete
        assert seq.current_step is None
        assert seq.total_steps == 0

    def test_load_and_iterate(self):
        steps = [
            _make_step("Click the search bar", checkpoint=False),
            _make_step("Type 'USB cable'", checkpoint=False),
            _make_step("Press Enter", checkpoint=True),
        ]
        seq = StepSequencer()
        seq.load_steps(steps)

        assert seq.total_steps == 3
        assert seq.current_index == 0
        assert not seq.is_complete
        assert seq.current_step.instruction == "Click the search bar"

        # Advance
        next_step = seq.advance()
        assert next_step.instruction == "Type 'USB cable'"
        assert seq.current_index == 1
        assert not seq.is_at_checkpoint

        # Advance to checkpoint
        next_step = seq.advance()
        assert next_step.instruction == "Press Enter"
        assert seq.is_at_checkpoint

        # Advance past last step
        next_step = seq.advance()
        assert next_step is None
        assert seq.is_complete

    def test_checkpoint_detection(self):
        steps = [
            _make_step("Step 1", checkpoint=True),
            _make_step("Step 2", checkpoint=False),
        ]
        seq = StepSequencer()
        seq.load_steps(steps)

        assert seq.is_at_checkpoint

    def test_reset(self):
        seq = StepSequencer()
        seq.load_steps([_make_step("test")])
        seq.advance()
        seq.reset()

        assert seq.is_complete
        assert seq.total_steps == 0

    def test_load_replaces_previous(self):
        seq = StepSequencer()
        seq.load_steps([_make_step("old step")])
        seq.load_steps([_make_step("new step 1"), _make_step("new step 2")])

        assert seq.total_steps == 2
        assert seq.current_step.instruction == "new step 1"

    def test_remaining_steps(self):
        steps = [_make_step(f"Step {i}") for i in range(4)]
        seq = StepSequencer()
        seq.load_steps(steps)

        assert seq.remaining_steps == 4
        seq.advance()
        assert seq.remaining_steps == 3

    def test_progress_string(self):
        steps = [_make_step(f"Step {i}") for i in range(3)]
        seq = StepSequencer()
        seq.load_steps(steps)

        assert seq.get_progress() == "Step 1 of 3"
        seq.advance()
        assert seq.get_progress() == "Step 2 of 3"

    def test_empty_progress(self):
        seq = StepSequencer()
        assert seq.get_progress() == ""
