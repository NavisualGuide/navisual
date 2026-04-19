"""Step sequencer for AI Navigator.

Manages multi-step navigation sequences returned by the AI.
Advances through steps locally without API calls until a checkpoint
is reached, reducing round-trips by 2-4x.
"""

import logging
import time
from typing import Optional

from src.ai.tool_schemas import NavigateStep

logger = logging.getLogger(__name__)

# Minimum seconds a checkpoint step must be visible before a screen change
# can be treated as the user completing it.  Prevents instant-completion
# false-positives when a queued screen-change event fires right after load.
CHECKPOINT_MIN_DWELL_SEC = 1.0


class StepSequencer:
    """Manages and advances through multi-step navigation sequences.

    The AI returns 1-4 steps per response. Steps with checkpoint=False
    auto-advance after a delay. Steps with checkpoint=True wait for
    screen change or user input before advancing.

    Usage:
        sequencer = StepSequencer()
        sequencer.load_steps(response.steps)

        while not sequencer.is_complete:
            step = sequencer.current_step
            # ... render overlay, speak instruction ...

            if step.checkpoint:
                # Wait for screen change or user input
                await wait_for_trigger()
            else:
                # Auto-advance after delay
                await asyncio.sleep(2)

            sequencer.advance()
    """

    def __init__(self) -> None:
        self._steps: list[NavigateStep] = []
        self._current_index: int = 0
        self._step_shown_at: float = 0.0  # monotonic time when current step was loaded/advanced to

    def load_steps(self, steps: list[NavigateStep]) -> None:
        """Load a new step sequence from an AI response.

        Replaces any existing sequence.
        """
        self._steps = list(steps)
        self._current_index = 0
        self._step_shown_at = time.monotonic()
        logger.info("Loaded %d steps into sequencer", len(self._steps))
        if self._steps:
            logger.debug("First step: %s", self._steps[0].instruction[:80])

    @property
    def current_step(self) -> Optional[NavigateStep]:
        """Get the current step, or None if sequence is complete."""
        if self._current_index < len(self._steps):
            return self._steps[self._current_index]
        return None

    @property
    def current_index(self) -> int:
        """Current step index (0-based)."""
        return self._current_index

    @property
    def total_steps(self) -> int:
        """Total number of steps in the sequence."""
        return len(self._steps)

    @property
    def is_complete(self) -> bool:
        """Whether all steps have been processed."""
        return self._current_index >= len(self._steps)

    @property
    def is_at_checkpoint(self) -> bool:
        """Whether the current step is a checkpoint (wait for user action)."""
        step = self.current_step
        return step is not None and step.checkpoint

    @property
    def checkpoint_ready(self) -> bool:
        """True when the current checkpoint step has been visible long enough
        that a screen change can be treated as the user completing it.

        Guards against queued screen-change events firing instantly after
        a new step is loaded and falsely marking it as done.
        """
        return (time.monotonic() - self._step_shown_at) >= CHECKPOINT_MIN_DWELL_SEC

    @property
    def remaining_steps(self) -> int:
        """Number of steps remaining including current."""
        return max(0, len(self._steps) - self._current_index)

    def advance(self) -> Optional[NavigateStep]:
        """Move to the next step and return it.

        Returns None if the sequence is already complete.
        """
        if self.is_complete:
            return None

        self._current_index += 1
        self._step_shown_at = time.monotonic()
        step = self.current_step

        if step:
            logger.info(
                "Advanced to step %d/%d: %s",
                self._current_index + 1, len(self._steps),
                step.instruction[:60],
            )
        else:
            logger.info("Step sequence complete (%d steps)", len(self._steps))

        return step

    def reset(self) -> None:
        """Reset the sequencer (clear all steps)."""
        self._steps = []
        self._current_index = 0

    def get_progress(self) -> str:
        """Get a human-readable progress string."""
        if not self._steps:
            return ""
        return f"Step {self._current_index + 1} of {len(self._steps)}"
