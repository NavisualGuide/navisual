"""Command dispatcher for the AI Navigator sidecar.

Phase A: ping / echo / version.
Phase B: send_guidance, start_session, trigger_correction, etc. — wires into
the migrated src.ai and src.core modules.
"""

from __future__ import annotations

import platform
import sys
from typing import Any, Callable

VERSION = "0.4.0-alpha"


class Dispatcher:
    def __init__(self) -> None:
        self._handlers: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
            "ping": self._ping,
            "echo": self._echo,
            "version": self._version,
            "shutdown": self._shutdown,
        }

    def handle(self, cmd: str, req: dict[str, Any]) -> dict[str, Any]:
        handler = self._handlers.get(cmd)
        if handler is None:
            return {"ok": False, "error": f"unknown command: {cmd}"}
        return handler(req)

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
        # The sidecar's supervisor (Rust) sees stdin close when it decides to
        # terminate. This handler exists so shutdown can be observed in logs.
        return {"ok": True, "shutting_down": True}
