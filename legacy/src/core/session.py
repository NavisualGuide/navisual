"""Session management and persistence for AI Navigator.

Manages the lifecycle of guidance sessions: init → active → paused → done.
Supports save/resume to JSON files for crash recovery and task continuation.
"""

import json
import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from uuid import UUID, uuid4

from pydantic import BaseModel, Field

from src.ai.tool_schemas import NavigateStep
from src.core.state import StateSummary

logger = logging.getLogger(__name__)


class Turn(BaseModel):
    """A single conversation turn."""

    role: str = Field(description="user, assistant, or correction")
    content: str = Field(description="Message text")
    screenshot_hash: Optional[str] = Field(default=None, description="pHash for dedup")
    timestamp: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))


class Session(BaseModel):
    """A complete guidance session with conversation history and state."""

    id: UUID = Field(default_factory=uuid4)
    task_description: str = Field(default="", description="What the user wants to accomplish")
    conversation: list[Turn] = Field(default_factory=list)
    current_state_summary: Optional[StateSummary] = None
    current_step_sequence: list[NavigateStep] = Field(default_factory=list)
    current_step_index: int = 0
    token_usage: dict = Field(default_factory=lambda: {"input": 0, "output": 0})
    started_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    last_active_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    def add_turn(self, role: str, content: str, screenshot_hash: Optional[str] = None) -> None:
        """Add a conversation turn."""
        self.conversation.append(
            Turn(role=role, content=content, screenshot_hash=screenshot_hash)
        )
        self.last_active_at = datetime.now(timezone.utc)

    def update_state(self, summary_text: str) -> None:
        """Update the state summary from an AI response."""
        self.current_state_summary = StateSummary(
            summary_text=summary_text,
            turn_index=len(self.conversation),
        )

    def record_tokens(self, input_tokens: int, output_tokens: int) -> None:
        """Record token usage for this session."""
        self.token_usage["input"] = self.token_usage.get("input", 0) + input_tokens
        self.token_usage["output"] = self.token_usage.get("output", 0) + output_tokens

    def get_conversation_for_api(self, max_turns: int = 10) -> list[dict]:
        """Get recent conversation history formatted for the API.

        Returns the last N turns as Anthropic-format messages.
        Only includes text (no screenshots — those are replaced by state summaries).
        """
        messages = []
        recent = self.conversation[-max_turns:] if len(self.conversation) > max_turns else self.conversation

        for turn in recent:
            if turn.role == "correction":
                # Corrections are sent as user messages with special context
                messages.append({"role": "user", "content": turn.content})
            elif turn.role in ("user", "assistant"):
                messages.append({"role": turn.role, "content": turn.content})

        return messages

    @property
    def total_tokens(self) -> int:
        """Total tokens used in this session."""
        return self.token_usage.get("input", 0) + self.token_usage.get("output", 0)


class SessionManager:
    """Manages session lifecycle and persistence.

    Sessions are saved as JSON files in the configured session directory.
    """

    def __init__(self, session_dir: Path) -> None:
        self.session_dir = session_dir
        self.session_dir.mkdir(parents=True, exist_ok=True)
        self._current_session: Optional[Session] = None

    @property
    def current_session(self) -> Optional[Session]:
        return self._current_session

    def create_session(self, task_description: str) -> Session:
        """Create and activate a new session."""
        session = Session(task_description=task_description)
        self._current_session = session
        logger.info("Created session %s: %s", session.id, task_description[:60])
        return session

    def save_session(self, session: Optional[Session] = None) -> Path:
        """Save a session to disk. Returns the file path."""
        session = session or self._current_session
        if session is None:
            raise ValueError("No session to save")

        file_path = self.session_dir / f"{session.id}.json"
        file_path.write_text(session.model_dump_json(indent=2), encoding="utf-8")
        logger.info("Session saved: %s", file_path)
        return file_path

    def load_session(self, session_id: str) -> Session:
        """Load a session from disk by ID."""
        file_path = self.session_dir / f"{session_id}.json"
        if not file_path.exists():
            raise FileNotFoundError(f"Session not found: {session_id}")

        session = Session.model_validate_json(file_path.read_text(encoding="utf-8"))
        self._current_session = session
        logger.info("Session loaded: %s", session_id)
        return session

    def list_sessions(self) -> list[dict]:
        """List all saved sessions with metadata."""
        sessions = []
        for path in sorted(self.session_dir.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True):
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
                sessions.append({
                    "id": data.get("id", path.stem),
                    "task_description": data.get("task_description", "Unknown"),
                    "last_active_at": data.get("last_active_at", ""),
                    "turns": len(data.get("conversation", [])),
                    "total_tokens": data.get("token_usage", {}).get("input", 0)
                    + data.get("token_usage", {}).get("output", 0),
                })
            except (json.JSONDecodeError, KeyError) as e:
                logger.warning("Failed to read session %s: %s", path, e)
        return sessions

    def delete_session(self, session_id: str) -> bool:
        """Delete a saved session. Returns True if deleted."""
        file_path = self.session_dir / f"{session_id}.json"
        if file_path.exists():
            file_path.unlink()
            logger.info("Session deleted: %s", session_id)
            if self._current_session and str(self._current_session.id) == session_id:
                self._current_session = None
            return True
        return False
