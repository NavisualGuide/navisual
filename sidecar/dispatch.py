"""Command dispatcher for the AI Navigator sidecar.

Phase A: ping / echo / version / shutdown.
Phase B: start_session / send_guidance / trigger_correction — wires into the
migrated `ai/` and `core/` modules. Async-capable; main.py runs the IO loop.

Session state lives in-process. A single APIRouter + CostTracker is created
lazily on first AI call and shared across sessions.
"""

from __future__ import annotations

import inspect
import logging
import platform
import sys
from typing import Any, Awaitable, Callable, Optional

VERSION = "0.4.0-alpha"

logger = logging.getLogger(__name__)


class Dispatcher:
    def __init__(self) -> None:
        self._handlers: dict[str, Callable[[dict[str, Any]], Any]] = {
            # Phase A
            "ping": self._ping,
            "echo": self._echo,
            "version": self._version,
            "shutdown": self._shutdown,
            # Phase B
            "start_session": self._start_session,
            "send_guidance": self._send_guidance,
            "trigger_correction": self._trigger_correction,
            "end_session": self._end_session,
            "list_sessions": self._list_sessions,
        }

        # Phase B state (lazy-init on first AI call)
        self._config = None  # type: ignore[assignment]
        self._cost_tracker = None  # type: ignore[assignment]
        self._managed_credit = None  # type: ignore[assignment]
        self._api_router = None  # type: ignore[assignment]
        self._correction_handler = None  # type: ignore[assignment]
        self._sessions: dict[str, Any] = {}

    async def handle(self, cmd: str, req: dict[str, Any]) -> dict[str, Any]:
        handler = self._handlers.get(cmd)
        if handler is None:
            return {"ok": False, "error": f"unknown command: {cmd}"}
        result = handler(req)
        if inspect.isawaitable(result):
            result = await result
        return result

    # ---------- Phase A ----------

    def _ping(self, _req: dict[str, Any]) -> dict[str, Any]:
        return {"ok": True, "pong": True, "version": VERSION}

    def _echo(self, req: dict[str, Any]) -> dict[str, Any]:
        return {"ok": True, "text": req.get("text", "")}

    def _version(self, _req: dict[str, Any]) -> dict[str, Any]:
        return {
            "ok": True,
            "version": VERSION,
            "python": sys.version.split()[0],
            "platform": platform.platform(),
        }

    def _shutdown(self, _req: dict[str, Any]) -> dict[str, Any]:
        return {"ok": True, "shutting_down": True}

    # ---------- Phase B ----------

    def _ensure_ai_ready(self) -> None:
        """Lazy-init Config, CostTracker, APIRouter on first AI call."""
        if self._api_router is not None:
            return
        from config import get_config
        from core.cost_tracker import CostTracker, ManagedCredit
        from ai.api_router import APIRouter
        from core.correction import CorrectionHandler

        self._config = get_config()
        self._cost_tracker = CostTracker(
            daily_cap=self._config.daily_token_cap,
            monthly_cap=self._config.monthly_token_cap,
            safety_margin=self._config.cost_safety_margin,
        )
        self._managed_credit = None
        if self._config.managed_api_key:
            self._managed_credit = ManagedCredit(cap=self._config.managed_token_cap)
        self._api_router = APIRouter(
            config=self._config,
            cost_tracker=self._cost_tracker,
            managed_credit=self._managed_credit,
        )
        self._correction_handler = CorrectionHandler(self._api_router)
        logger.info("sidecar AI ready (provider=%s)", self._api_router.provider_name)

    def _get_or_create_session(self, session_id: Optional[str], task: str = ""):
        from core.session import Session
        if session_id and session_id in self._sessions:
            return self._sessions[session_id]
        sess = Session(task_description=task)
        self._sessions[str(sess.id)] = sess
        return sess

    async def _start_session(self, req: dict[str, Any]) -> dict[str, Any]:
        """Create a new session. Returns session_id + provider info."""
        task = req.get("task", "")
        self._ensure_ai_ready()
        sess = self._get_or_create_session(None, task=task)
        return {
            "ok": True,
            "session_id": str(sess.id),
            "provider": self._api_router.provider_name,
            "managed_credit_remaining": self._api_router.managed_credit_remaining,
        }

    async def _end_session(self, req: dict[str, Any]) -> dict[str, Any]:
        sid = req.get("session_id", "")
        if sid in self._sessions:
            del self._sessions[sid]
            return {"ok": True, "ended": sid}
        return {"ok": False, "error": f"unknown session: {sid}"}

    async def _list_sessions(self, _req: dict[str, Any]) -> dict[str, Any]:
        return {"ok": True, "sessions": list(self._sessions.keys())}

    async def _send_guidance(self, req: dict[str, Any]) -> dict[str, Any]:
        """Send a guidance request. Accepts session_id + user_text + optional
        screenshot_b64 + state_summary + use_fast_model.
        """
        self._ensure_ai_ready()
        sid = req.get("session_id", "")
        if sid not in self._sessions:
            return {"ok": False, "error": f"unknown session: {sid}"}
        sess = self._sessions[sid]

        user_text = req.get("user_text", "")
        screenshot_b64 = req.get("screenshot_b64")
        state_summary = req.get("state_summary")
        use_fast = bool(req.get("use_fast_model", False))

        try:
            response = await self._api_router.send_guidance_request(
                user_text=user_text,
                screenshot_b64=screenshot_b64,
                state_summary=state_summary,
                session=sess,
                use_fast_model=use_fast,
            )
        except Exception as e:
            logger.exception("send_guidance failed")
            return {"ok": False, "error": f"{type(e).__name__}: {e}"}

        return {
            "ok": True,
            "response": response.model_dump() if hasattr(response, "model_dump") else response,
            "usage": {
                "daily_total": self._cost_tracker.daily_total,
                "daily_cap": self._cost_tracker.daily_cap,
                "managed_remaining": self._api_router.managed_credit_remaining,
            },
        }

    async def _trigger_correction(self, req: dict[str, Any]) -> dict[str, Any]:
        """Re-query with correction context. Rust passes a fresh screenshot."""
        self._ensure_ai_ready()
        sid = req.get("session_id", "")
        if sid not in self._sessions:
            return {"ok": False, "error": f"unknown session: {sid}"}
        sess = self._sessions[sid]
        screenshot_b64 = req.get("screenshot_b64")

        response = await self._correction_handler.handle_correction(
            session=sess,
            screenshot_b64=screenshot_b64,
        )
        if response is None:
            return {"ok": False, "error": "correction failed"}
        return {
            "ok": True,
            "response": response.model_dump() if hasattr(response, "model_dump") else response,
        }
