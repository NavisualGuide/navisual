# AI Navigator — Python Sidecar

Runs as a child process of the Rust/Tauri backend. Handles AI routing, session
management, and voice I/O. Communicates via JSON-lines over stdin/stdout.

## Phase A (current)
- `ping` — health check
- `echo` — round-trip test
- `version` — Python + platform info

## Phase B (next)
- Migrate `legacy/src/ai/` → `sidecar/ai/`
- Migrate `legacy/src/core/` → `sidecar/core/`
- Add `send_guidance`, `start_session`, `trigger_correction` commands

## Dev

```bash
# Run standalone (reads from stdin, writes to stdout)
python sidecar/main.py

# Then type a JSON-line command:
{"id":"1","cmd":"ping"}
```

## Build (Phase G)

```powershell
pyinstaller --onefile --console sidecar/main.py --name ai-navigator-sidecar
```
