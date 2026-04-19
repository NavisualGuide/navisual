"""AI Navigator — Python sidecar entry point.

Reads JSON-lines requests from stdin, dispatches to handlers, writes
JSON-lines responses to stdout. Any unhandled exception is reported as
an error response rather than crashing the process — the Rust backend
supervises lifecycle and will respawn on actual death.

Phase B: handlers are async (AI calls need await). Requests are handled
sequentially for now; streaming and concurrent in-flight calls come later.
"""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any

from dispatch import Dispatcher


def _emit(msg: dict[str, Any]) -> None:
    """Write one JSON-line response to stdout."""
    sys.stdout.write(json.dumps(msg, ensure_ascii=False) + "\n")
    sys.stdout.flush()


async def _amain() -> None:
    dispatcher = Dispatcher()
    loop = asyncio.get_running_loop()
    while True:
        raw = await loop.run_in_executor(None, sys.stdin.readline)
        if raw == "":
            break  # EOF — Rust closed stdin
        raw = raw.strip()
        if not raw:
            continue
        req: Any = None
        try:
            req = json.loads(raw)
            req_id = req.get("id", "")
            cmd = req.get("cmd", "")
            result = await dispatcher.handle(cmd, req)
            _emit({"id": req_id, "event": "response", **result})
        except Exception as e:  # noqa: BLE001 — boundary handler
            _emit(
                {
                    "id": req.get("id", "") if isinstance(req, dict) else "",
                    "event": "error",
                    "message": f"{type(e).__name__}: {e}",
                }
            )


def main() -> None:
    try:
        asyncio.run(_amain())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
