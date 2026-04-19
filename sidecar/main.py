"""AI Navigator — Python sidecar entry point.

Reads JSON-lines requests from stdin, dispatches to handlers, writes
JSON-lines responses to stdout. Any unhandled exception is reported as
an error response rather than crashing the process — the Rust backend
supervises lifecycle and will respawn on actual death.

Phase A scope: just `ping` and `echo`. Phase B adds AI routing and session.
"""

from __future__ import annotations

import json
import sys
from typing import Any

from dispatch import Dispatcher


def _emit(msg: dict[str, Any]) -> None:
    """Write one JSON-line response to stdout."""
    sys.stdout.write(json.dumps(msg, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main() -> None:
    dispatcher = Dispatcher()
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            req = json.loads(raw)
            req_id = req.get("id", "")
            cmd = req.get("cmd", "")
            result = dispatcher.handle(cmd, req)
            _emit({"id": req_id, "event": "response", **result})
        except Exception as e:  # noqa: BLE001 — boundary handler
            _emit(
                {
                    "id": req.get("id", "") if isinstance(req, dict) else "",
                    "event": "error",
                    "message": f"{type(e).__name__}: {e}",
                }
            )


if __name__ == "__main__":
    main()
