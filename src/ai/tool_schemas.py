"""Tool schema definitions for AI Navigator's structured output.

Defines the navigate_step tool used by Anthropic (tool_use) and OpenAI (function_calling)
to return validated, structured navigation instructions.
"""

from enum import Enum
from typing import Optional

from pydantic import BaseModel, Field


class OverlayType(str, Enum):
    ARROW = "arrow"
    HIGHLIGHT = "highlight"
    CIRCLE = "circle"
    NONE = "none"


class TargetRole(str, Enum):
    BUTTON = "button"
    TAB = "tab"
    LINK = "link"
    TEXTBOX = "textbox"
    MENUITEM = "menuitem"
    CHECKBOX = "checkbox"
    RADIO = "radio"
    COMBOBOX = "combobox"
    SLIDER = "slider"
    IMAGE = "image"
    HEADING = "heading"
    OTHER = "other"


class TargetRegion(str, Enum):
    TOP_LEFT = "top-left"
    TOP_CENTER = "top-center"
    TOP_RIGHT = "top-right"
    CENTER_LEFT = "center-left"
    CENTER = "center"
    CENTER_RIGHT = "center-right"
    BOTTOM_LEFT = "bottom-left"
    BOTTOM_CENTER = "bottom-center"
    BOTTOM_RIGHT = "bottom-right"


class NavigateStep(BaseModel):
    """A single navigation instruction step."""

    instruction: str = Field(description="Instruction shown/spoken to the user.")
    target_text: Optional[str] = Field(
        default=None,
        description="Exact text label of the UI element. Used by A11y API and OCR to find it.",
    )
    target_role: Optional[TargetRole] = Field(
        default=None,
        description="UI role of the target element for precise A11y queries.",
    )
    target_region: Optional[TargetRegion] = Field(
        default=None,
        description="Rough screen region to narrow search.",
    )
    overlay_type: OverlayType = Field(
        default=OverlayType.ARROW,
        description="Type of visual overlay to draw.",
    )
    clipboard: Optional[str] = Field(
        default=None,
        description="Text to copy to clipboard (CLI commands, text entry).",
    )
    checkpoint: bool = Field(
        default=True,
        description="If true, wait for user action before advancing to next step.",
    )


class NavigateStepResponse(BaseModel):
    """Full response from the AI via the navigate_step tool."""

    steps: list[NavigateStep] = Field(description="1-4 navigation steps.")
    state_summary: str = Field(description="Compact state summary for context tracking.")
    needs_input: bool = Field(
        default=False,
        description="If true, AI needs user to answer a question before proceeding.",
    )


# Anthropic tool_use schema (sent in the API request)
NAVIGATE_STEP_TOOL = {
    "name": "navigate_step",
    "description": (
        "Provide navigation instructions for the user. Return one or more steps. "
        "Steps with checkpoint=true will wait for the user to complete the action before proceeding."
    ),
    "input_schema": {
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
                                "The instruction shown/spoken to the user. "
                                "Be specific about visual appearance and position."
                            ),
                        },
                        "target_text": {
                            "type": "string",
                            "description": (
                                "Exact text label of the UI element to highlight. "
                                "Used by Accessibility API and OCR to find the element."
                            ),
                        },
                        "target_role": {
                            "type": "string",
                            "enum": [r.value for r in TargetRole],
                            "description": "The UI role/type of the target element.",
                        },
                        "target_region": {
                            "type": "string",
                            "enum": [r.value for r in TargetRegion],
                            "description": "Rough screen region to narrow search.",
                        },
                        "overlay_type": {
                            "type": "string",
                            "enum": [o.value for o in OverlayType],
                            "description": "Type of visual overlay to draw on the target.",
                        },
                        "clipboard": {
                            "type": "string",
                            "description": "Text to copy to clipboard. Null if not applicable.",
                        },
                        "checkpoint": {
                            "type": "boolean",
                            "description": (
                                "If true, wait for user action (screen change) before "
                                "showing the next step. If false, auto-advance after a delay."
                            ),
                        },
                    },
                },
            },
            "state_summary": {
                "type": "string",
                "description": "Compact summary of current app state. Not shown to user.",
            },
            "needs_input": {
                "type": "boolean",
                "description": "If true, AI needs the user to answer a question.",
            },
        },
    },
}
