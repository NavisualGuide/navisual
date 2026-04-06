"""Tests for session management and persistence."""

import json
import tempfile
from pathlib import Path

import pytest

from src.core.session import Session, SessionManager, Turn


class TestSession:
    def test_create_session(self):
        session = Session(task_description="Buy a USB cable")
        assert session.task_description == "Buy a USB cable"
        assert len(session.conversation) == 0
        assert session.total_tokens == 0

    def test_add_turn(self):
        session = Session()
        session.add_turn(role="user", content="Help me shop")
        session.add_turn(role="assistant", content="Click the search bar")

        assert len(session.conversation) == 2
        assert session.conversation[0].role == "user"
        assert session.conversation[1].role == "assistant"

    def test_update_state(self):
        session = Session()
        session.add_turn(role="user", content="test")
        session.update_state("Amazon homepage. Search bar visible.")

        assert session.current_state_summary is not None
        assert "Amazon homepage" in session.current_state_summary.summary_text

    def test_record_tokens(self):
        session = Session()
        session.record_tokens(100, 50)
        session.record_tokens(200, 75)

        assert session.token_usage["input"] == 300
        assert session.token_usage["output"] == 125
        assert session.total_tokens == 425

    def test_conversation_for_api(self):
        session = Session()
        session.add_turn(role="user", content="Help me")
        session.add_turn(role="assistant", content="Click here")
        session.add_turn(role="user", content="Done, what's next?")

        messages = session.get_conversation_for_api()
        assert len(messages) == 3
        assert messages[0]["role"] == "user"
        assert messages[1]["role"] == "assistant"

    def test_conversation_for_api_max_turns(self):
        session = Session()
        for i in range(20):
            session.add_turn(role="user", content=f"Message {i}")

        messages = session.get_conversation_for_api(max_turns=5)
        assert len(messages) == 5


class TestSessionManager:
    def test_create_and_save(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            manager = SessionManager(session_dir=Path(tmpdir))
            session = manager.create_session("Test task")

            path = manager.save_session()
            assert path.exists()

            data = json.loads(path.read_text())
            assert data["task_description"] == "Test task"

    def test_load_session(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            manager = SessionManager(session_dir=Path(tmpdir))
            session = manager.create_session("Loadable task")
            session.add_turn(role="user", content="Test message")
            manager.save_session()

            session_id = str(session.id)

            # Create new manager and load
            manager2 = SessionManager(session_dir=Path(tmpdir))
            loaded = manager2.load_session(session_id)

            assert loaded.task_description == "Loadable task"
            assert len(loaded.conversation) == 1

    def test_list_sessions(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            manager = SessionManager(session_dir=Path(tmpdir))

            manager.create_session("Task A")
            manager.save_session()

            manager.create_session("Task B")
            manager.save_session()

            sessions = manager.list_sessions()
            assert len(sessions) == 2

    def test_delete_session(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            manager = SessionManager(session_dir=Path(tmpdir))
            session = manager.create_session("Deletable")
            manager.save_session()

            assert manager.delete_session(str(session.id))
            assert len(manager.list_sessions()) == 0

    def test_load_nonexistent(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            manager = SessionManager(session_dir=Path(tmpdir))
            with pytest.raises(FileNotFoundError):
                manager.load_session("nonexistent-id")
