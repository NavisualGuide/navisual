# AI Navigator — Project Guide

**Version:** 0.2.0
**Status:** v0.2 complete. Streaming, prompt caching, multi-monitor, model tiering, TTS, voice input all shipped.
**License:** FSL-1.1-Apache-2.0 (Functional Source License, converts to Apache 2.0 after 2 years)
**Design Doc:** [AI-Navigator-Design-Document.md](docs/AI-Navigator-Design-Document.md)
**GitHub:** [stevefu-ops/ai-navigator](https://github.com/stevefu-ops/ai-navigator)

---

## Quick Summary

**AI Navigator** is a cross-platform desktop app that guides users through computer tasks by observing their screen and providing real-time navigation instructions (via audio/overlay). The user always stays in control — the AI never clicks, types, or acts.

**Slogan:** *The AI guides, never overrides.*

---

## Architecture Overview

### Six-Layer Model

```
┌─ INPUT LAYER ──────────────────────────────────────────┐
│ • Screen capture (on-demand via event detection)        │
│ • Screen change detector (OS events + pixel-diff)       │
│ • Chat input + voice input (v0.2)                       │
└────────────────────────────────────────────────────────┘
                          ▼
┌─ CORE ENGINE ───────────────────────────────────────────┐
│ • Session manager (state + conversation + persistence)  │
│ • State summarizer (compact text for context)           │
│ • API router (multi-provider support)                   │
│ • Cost controller (token budgets + safety margin)       │
│ • Correction handler (user "wrong" signal processing)   │
│ • Step sequencer (advances multi-step sequences)        │
└────────────────────────────────────────────────────────┘
                          ▼
┌─ ELEMENT LOCATOR (LOCAL) ───────────────────────────────┐
│ Core differentiator: AI returns TEXT descriptions,       │
│ local OCR/A11y finds EXACT screen positions.             │
│                                                         │
│ Strategies (in priority):                               │
│ 1. OS Accessibility APIs (UIA/AX/AT-SPI2) - fastest    │
│ 1. OS Accessibility API (UIA) - PRIMARY, < 5ms           │
│ 2. Local OCR (PaddleOCR) - FALLBACK, works on any app   │
│ 3. Template matching (icons) - v0.3                    │
│                                                         │
│ Output: exact bbox or "not found" → graceful fallback  │
└────────────────────────────────────────────────────────┘
                          ▼
┌─ OUTPUT LAYER ─────────────────────────────────────────┐
│ • Overlay renderer (arrows, highlights, subtitles)      │
│ • TTS engine (v0.2)                                     │
│ • Clipboard manager (CLI commands)                      │
│ • Chat window (conversation display)                    │
└────────────────────────────────────────────────────────┘
```

### Component Map

| Component | File | Purpose | Status |
|-----------|------|---------|--------|
| Session Manager | `core/session.py` | Lifecycle, persistence, conversation history | TODO |
| State Summarizer | `core/state.py` | Compact app state for API context | TODO |
| Cost Tracker | `core/cost_tracker.py` | Token budgets, safety margins | TODO |
| Correction Handler | `core/correction.py` | Re-analysis on user "wrong" signal | TODO |
| Step Sequencer | `core/step_sequencer.py` | Advance through multi-step responses locally | TODO |
| Screen Capture | `input/screen_capture.py` | On-demand screenshots | TODO |
| Screen Monitor | `input/screen_monitor.py` | Event-driven detection | TODO |
| Chat Input | `input/chat_input.py` | User prompt input | TODO |
| Voice Input | `input/voice_input.py` | Push-to-talk via SpeechRecognition + Google STT | DONE |
| API Router | `ai/api_router.py` | Provider selection, request building | DONE |
| Anthropic Client | `ai/anthropic_client.py` | Anthropic API (tool_use) | DONE |
| Gemini Client | `ai/gemini_client.py` | Google Gemini API (function calling) | DONE |
| Ollama Client | `ai/ollama_client.py` | Local Ollama inference (JSON mode) | DONE |
| OpenAI Client | `ai/openai_client.py` | OpenAI API (function_calling) | STUB |
| Tool Schemas | `ai/tool_schemas.py` | navigate_step tool definition | DONE |
| Element Locator | `locator/element_locator.py` | Orchestrates OCR + A11y + templates | TODO |
| OCR Engine | `locator/ocr_engine.py` | **FALLBACK**: PaddleOCR wrapper, text → bbox | TODO |
| A11y Engine | `locator/a11y_engine.py` | **PRIMARY**: Windows UIA element lookup (< 5ms) | TODO |
| Overlay Renderer | `output/overlay.py` | Qt frameless window for overlays | TODO |
| Clipboard Manager | `output/clipboard.py` | System clipboard access | TODO |
| TTS Engine | `output/tts.py` | Text-to-speech via pyttsx3 (Windows SAPI) | DONE |
| Main Window | `ui/main_window.py` | Chat UI (PySide6) | TODO |
| Floating Window | `ui/floating_window.py` | Hotkey-activated input + correction button | TODO |

---

## Data Flow

### The Guidance Loop (Event-Driven)

```
1. User types prompt or screen change detected
   ↓
2. Capture screenshot
   ↓
3. Build API payload:
   - System prompt + user prompt
   - Current screenshot
   - State summary (from prior turn)
   - Cached image reference (optional)
   ↓
4. Call AI API with tool_use (Anthropic) / function_calling (OpenAI)
   ↓
5. AI responds via navigate_step tool:
   - steps: [
       {instruction, target_text, target_region, overlay_type, clipboard, checkpoint},
       ...
     ]
   - state_summary
   - needs_input
   ↓
6. Element Locator finds target_text on live screen (OCR/A11y)
   ↓
7. Render overlay (arrow/highlight/subtitle at exact bbox)
   ↓
8. Speak instruction + show subtitle
   ↓
9. Wait for:
   - Screen change → loop to step 1
   - Correction hotkey (Ctrl+Shift+X) → trigger correction handler
   - User voice/chat → loop to step 1
   - Next-step hotkey → advance to next step in sequence
```

### Key Optimizations

| Optimization | Savings | When |
|--------------|---------|------|
| Screenshot dedup (pHash) | 50-70% fewer API calls | MVP |
| State summaries (text only) | Replace old images with ~100 tokens | MVP |
| Multi-step sequences | 2-4x fewer API calls per task | MVP |
| Event-driven detection | Eliminate idle polling | MVP |
| Prompt caching (Anthropic) | 90% cheaper on system prompt | v0.2 |
| Model tiering (Claude Haiku) | For simple change detection | v0.2 |
| API-send screenshot downscaling | 75% image token reduction (768×432 cap) | v0.3 |
| Active window crop | Up to 80% image token cut | v0.3 |
| Extended model tiering | Gemini Flash (free) for re-queries; Haiku for mid-sequence | v0.3 |

---

## Code Conventions

### File Organization

```
src/
├── main.py                   # Entry point
├── config.py                 # Settings, API keys, hotkey bindings
├── core/                     # Business logic
├── input/                    # User input + screen capture
├── ai/                       # API clients + tool schemas
├── locator/                  # Element locating (OCR + A11y)
├── output/                   # UI rendering + audio
└── ui/                       # PySide6 windows
```

### Python Style

- **Python 3.11+** — Type hints required (`mypy` strict mode eventual goal)
- **Async/await** — Use `asyncio` for non-blocking I/O (API calls, screen capture)
- **Dataclasses** — For config, state, responses (see `pydantic` for validation)
- **Logging** — `logging` module, INFO level default
- **Testing** — `pytest` + `pytest-asyncio` for async tests

### Naming Conventions

| Type | Convention | Example |
|------|-----------|---------|
| Classes | PascalCase | `ScreenMonitor`, `ElementLocator` |
| Functions | snake_case | `run_guidance_loop()` |
| Constants | UPPER_SNAKE_CASE | `DEFAULT_CAPTURE_INTERVAL_MS` |
| Private | `_leading_underscore` | `_process_ocr_results()` |
| Config keys | snake_case | `api_key`, `capture_interval_ms` |

### Imports & Organization

```python
# Standard library
import asyncio
from pathlib import Path

# Third-party
from pydantic import BaseModel
import numpy as np

# Local
from src.core.session import Session
from src.locator.element_locator import ElementLocator
```

### Async Patterns

```python
# Coroutines for I/O-heavy operations
async def capture_screenshot() -> Image:
    """Non-blocking screen capture."""
    ...

# Event loop in main
async def main():
    await run_guidance_loop()

if __name__ == "__main__":
    asyncio.run(main())
```

---

## Current Status

### ✅ Completed (v0.1.0-alpha)
- Design document v0.2 (comprehensive)
- All six layers implemented with 47 passing tests
- Anthropic API (tool_use), PaddleOCR, Windows UIA A11y
- PySide6 UI (main window + floating window + overlay)
- Session persistence, correction hotkey, clipboard
- Multi-step sequencer with checkpoint support
- Cost tracker (daily/monthly caps with safety margin)
- First real-world test: Amazon + SolidWorks

### ✅ Completed (v0.1.1)
- Multi-provider AI: Gemini Flash (free tier) + Ollama (local) + Anthropic
- System prompt: generic browser language (no Edge/Chrome/Firefox specifics)
- System prompt: AI Navigator window self-awareness (minimize, not close)
- Input box stays enabled during API calls — messages queue automatically
- Screen change auto-advance: mid-sequence steps now advance without user prompt
- Screen change re-query: when sequence complete + screen changes, AI re-queries (debounced 5s)
- Window geometry in state context so AI knows where the Navigator window is
- Startup message shows active provider + model

### ✅ Completed (v0.1.2)
- System prompt rule 12: always respond in English (fixes Chinese/locale responses)
- Overlay visibility: white contrasting outline under all overlay types (visible on any background)
- .env.example fully rewritten with all providers, all model options, all settings

### ✅ Completed (v0.1.3)
- OCR fix: `show_log` and `use_gpu` arguments conditionally included via `inspect.signature`
  (both removed in newer PaddleOCR versions)
- Race condition fix: `_is_processing` set synchronously in `handle_screen_change` before
  scheduling async API calls, preventing duplicate calls from rapid screen-change events
- `_handle_checkpoint_completed` resets `_is_processing` in non-API branches so UI stays responsive
- A11y engine: replaced invalid `PropertyCondition`/`PropertyId` API with `Control(RegexName=...)`
- A11y engine: window/titlebar/pane controls excluded; 4× name-length guard prevents browser tab
  title false matches; search depth increased to 12 (fast) / 8 (slow) for Chrome's deep DOM
- System prompt rule 3: `target_text` limited to 1–5 words max

### ✅ Completed (v0.1.4)
- OCR backend replaced on Windows: `Windows.Media.Ocr` (built-in Windows 10/11, via `winrt`)
  is now primary. Eliminates PaddlePaddle 3.x PIR+OneDNN `ConvertPirAttribute2RuntimeAttribute`
  bug that crashed every OCR inference. ~10ms vs ~150ms, zero model downloads.
- PaddleOCR retained as fallback for non-Windows platforms (macOS/Linux in future)
- `winrt-Windows.Media.Ocr` and related packages added to `pyproject.toml`
- OCR results: line-level merged bbox + individual word bboxes for precise single-word matching
- PaddleOCR 3.x compatibility: dict result format, `cls` try/except, `use_doc_orientation_classify`
  flags to reduce model load and limit OneDNN exposure

### ✅ Completed (v0.2.0)
- **Streaming responses** — instructions render word-by-word as they arrive (Anthropic + Gemini)
- **Prompt caching** — Anthropic beta header + `cache_control: ephemeral` on system + tools (90% cheaper for system prompt)
- **Multi-monitor support** — overlay spans virtual desktop union; active screen detection for subtitle positioning
- **Model tiering** — Haiku (`anthropic_fast_model`) for screen-change re-queries; Sonnet for initial/user-triggered guidance
- **TTS** — pyttsx3 via Windows SAPI; queue-draining so only latest instruction is spoken; enabled via `ENABLE_TTS=true`
- **Voice input** — push-to-talk via SpeechRecognition + PyAudio + Google Web Speech API; mic button in floating window; `ENABLE_VOICE_INPUT=true`; transcript thread-safe via Qt signal

### 🚧 Next: v0.3

### 📋 Upcoming Milestones

```
v0.3 — Stability + Efficiency (Python, Windows):
  1. Bug fixes: overlay coordinate (OCR image-space offset), subtitle persistence
  2. Token optimization:
       - API-send screenshot downscaling (768×432 max, separate from local capture)
       - Active window crop before API send: GetWindowRect pre-API crop only
         (~20 lines). Do NOT rearchitect the monitor pipeline — that ships in Rust.
       - Extended model tiering: Gemini Flash for all automated re-queries
  3. Single screen mode: user picks one screen to capture; halves image token cost
       and eliminates multi-monitor coordinate complexity for most use cases
  4. Settings window: in-app UI to choose API provider + enter API key;
       no more .env editing for beta testers; stored in ~/.ai-navigator/settings.json
  5. UI consolidation: merge chat + floating control into one window;
       keyboard shortcut legend; draggable subtitle overlay
  6. PyPI packaging: publish to PyPI (pip install ai-navigator)

v0.4 — Distribution (Windows):
  1. Signed Windows installer: embedded Python + NSIS/WiX + OV cert (~$100/yr)
       — no Python required on user machine; interim before full Tauri rewrite
  2. EV code signing ($400/yr) — builds SmartScreen reputation
  3. Tauri/Rust rewrite: Rust backend (screen capture, A11y, hotkeys, tray,
       overlay via native layered window); Svelte web frontend (chat UI,
       settings); Python AI layer retained as bundled sidecar (PyInstaller
       binary) — JSON-lines IPC over stdin/stdout, screenshots via temp file.
       Single binary ~5MB (Tauri) + ~40MB sidecar. See SDD §2.5.
  4. Full window-tracking pipeline (Rust): SetWinEventHook tracks window moves;
       capture scoped to target window only; coordinates become window-relative
       (no DPR matrix, no virtual origin); IVirtualDesktopManager for virtual
       desktop detection; IsIconic for minimise/restore handling

v0.5 — Complex Apps + Nav-Packs:
  1. Template matching: OpenCV matchTemplate for icon-only UI elements
  2. Nav-Packs v1: pack format + loader; built-in packs for Blender + SolidWorks
  3. Community pack submission format (GitHub-based)
  4. Quantized local model improvements (LLaVA-Next, Qwen-VL-Chat)

v1.0 — Public Launch (Windows):
  1. MSIX packaging → Microsoft Store submission
  2. Enterprise features: SSO (SAML 2.0 / Azure AD), audit logs
  3. Plugin system for third-party Nav-Pack developers
  4. Full Nav-Pack library (Pro + community)
  5. Browser Companion extension (Chrome, Pro feature): Chrome Extension MV3 +
       native messaging bridge; DOM getBoundingClientRect() replaces OCR for
       browser tasks (~99% accuracy); MutationObserver replaces pixel-diff for
       SPA navigation. See SDD §7.6.

v1.x — Platform Expansion (post-public-launch):
  1. macOS (clean port — strong API parity):
       A11y: AXUIElement; OCR: Vision.framework (built-in, ~10ms);
       window bounds/tracking: CGWindowListCopyWindowInfo + AX notifications;
       window capture: CGWindowListCreateImage; .pkg + Apple Developer cert
  2. Linux X11 (workable, fragmented):
       A11y: AT-SPI2 (pyatspi); OCR: Tesseract; window tracking: XSelectInput;
       AppImage / Flatpak distribution
  3. Linux Wayland: requires XWayland — Wayland deliberately blocks cross-process
       window position queries and capture. Not natively supportable.
```

### 🎯 MVP Scope (v0.1)

| Feature | In? | Notes |
|---------|-----|-------|
| Event-driven screen capture | ✓ | OS events + pixel-diff + idle fallback |
| Text chat input | ✓ | PySide6 window |
| Anthropic API (tool_use) | ✓ | Structured output |
| Multi-step sequences | ✓ | 1-4 steps per response, checkpoints |
| OS Accessibility API (UIA) | ✓ | **Primary** element locator (< 5ms for browsers) |
| Local OCR fallback (PaddleOCR) | ✓ | Fallback when A11y tree unavailable |
| Overlay arrows | ✓ | Positioned by A11y (primary) or OCR (fallback) |
| Correction hotkey | ✓ | Ctrl+Shift+X → re-analysis |
| Session persistence | ✓ | Save/resume sessions |
| Clipboard commands | ✓ | For CLI tasks |
| TTS | ✓ | pyttsx3/Windows SAPI, `ENABLE_TTS=true` |
| Voice input (PTT) | ✓ | SpeechRecognition + Google STT, `ENABLE_VOICE_INPUT=true` |
| Multi-platform | ✗ | Windows only for MVP |

### 📅 Full Roadmap

```
v0.2  DONE — streaming + prompt caching + multi-monitor + model tiering + TTS + voice input
v0.3  Bug fixes + token optimization + single screen mode + settings window
      + UI consolidation + PyPI packaging
v0.4  Signed Windows installer + EV code signing + Tauri/Rust rewrite
v0.5  Template matching + Nav-Packs v1 + Blender/SolidWorks + quantized local models
v1.0  MSIX (Microsoft Store) + enterprise (SSO, audit logs) + plugin system + public launch
v1.x  macOS port + Linux port  (after public launch)
```

### 🔍 Known Issues / Future Improvements

| Issue | Priority | Notes |
|-------|----------|-------|
| Response speed | High | Fix: streaming (v0.2) + prompt caching (v0.2) |
| Single-monitor only | Medium | Multi-monitor: v0.2 |
| Ollama vision quality | Medium | Improve with better quantized models |
| Daily token cap blocks testing | Low | Set `DAILY_TOKEN_CAP=1000000` in .env |
| Screen-change re-query too eager | Low | Tune `_screen_change_requery_cooldown_sec` if noisy |

---

## Configuration

### Environment Variables

```bash
# .env

# --- Provider selection ---
# Options: anthropic | gemini | ollama | openai
API_PROVIDER=anthropic

# --- Anthropic (Claude) ---
ANTHROPIC_API_KEY=sk-ant-...
# ANTHROPIC_MODEL=claude-haiku-4-5-20251001   # Fast & cheap
# ANTHROPIC_MODEL=claude-sonnet-4-6           # Default — balanced
# ANTHROPIC_MODEL=claude-opus-4-6             # Most capable

# --- Google Gemini (free tier for new users) ---
# GEMINI_API_KEY=AIza...        # Free key: https://aistudio.google.com/apikey
# GEMINI_MODEL=gemini-2.0-flash  # Default — free tier, multimodal, fast

# --- Ollama (local, no API key, runs on-device) ---
# OLLAMA_BASE_URL=http://localhost:11434
# OLLAMA_MODEL=llama3.2-vision   # Requires: ollama pull llama3.2-vision

# --- OpenAI (v0.2) ---
OPENAI_API_KEY=sk-...           # Optional, stub for now

# --- Budget (raise for testing) ---
DAILY_TOKEN_CAP=1000000         # Default 100k is tight for development
COST_SAFETY_MARGIN=1.2          # Default 2.5x is conservative

LOG_LEVEL=INFO
DEBUG_MODE=false
```

### Config File

```python
# src/config.py
class Config:
    # API
    api_provider: str = "anthropic"  # or "openai"
    api_timeout_sec: int = 30

    # Screen capture
    capture_interval_ms: int = 2000  # Fallback only
    max_screenshot_size: tuple = (1920, 1080)

    # OCR
    ocr_model: str = "english"
    ocr_confidence_threshold: float = 0.5

    # Overlay
    overlay_color: str = "#FF6B35"
    overlay_thickness: int = 2

    # Token budget
    daily_cap_tokens: int = 100000
    monthly_cap_tokens: int = 5000000
    safety_margin: float = 2.5

    # Hotkeys
    correction_hotkey: str = "ctrl+shift+x"
    pause_hotkey: str = "ctrl+shift+p"
    next_step_hotkey: str = "ctrl+shift+n"
```

---

## Testing

### Run Tests

```bash
pytest                      # All tests
pytest tests/test_ocr.py    # Specific test
pytest -v --cov            # Verbose + coverage
```

### Test Structure

```
tests/
├── test_screen_capture.py
├── test_screen_monitor.py
├── test_element_locator.py
├── test_api_router.py
├── test_session.py
├── test_state_summary.py
└── test_step_sequencer.py
```

---

## Development Workflow

### Getting Started

```bash
# 1. Clone repo
git clone https://github.com/stevefu-ops/ai-navigator.git
cd ai-navigator

# 2. Create venv
python -m venv venv
source venv/Scripts/activate  # Windows: venv\Scripts\activate

# 3. Install deps
pip install -e ".[dev]"

# 4. Set up env
cp .env.example .env
# Edit .env with your API keys

# 5. Run main app
python -m src.main

# 6. Run tests
pytest
```

### Branch Strategy

- `main` — stable, release-ready code
- `dev` — integration branch for v0.2+ features
- `feature/xxx` — individual features (PR before merge)

### Commit Messages

```
Short summary (imperative, <70 chars)

Longer explanation (wrap at 80 chars):
- What was changed
- Why it was changed
- Any relevant design decisions
```

---

## Key Design Decisions

### 1. AI Returns Text, Local Code Finds Positions

**Why:** AI cannot reliably estimate pixel coordinates (DPI scaling, window position, dynamic UI). Instead, AI returns `target_text: "Modeling"` and local OCR finds exact bbox.

**Benefit:** Overlay always points to the right place, even if user moves windows.

### 2. Event-Driven Detection, Not Polling

**Why:** Polling every 2s wastes API calls during idle periods and feels sluggish.

**Decision:** Use OS accessibility events (instant), fast local pixel-diff (10fps, ~1ms), user signals (hotkeys/voice).

**Benefit:** Responsive (< 500ms to first instruction) + cheap (50-70% fewer API calls).

### 3. Multi-Step Sequences with Checkpoints

**Why:** Many tasks have obvious sequential micro-actions. Waiting for an API call between each is slow.

**Decision:** AI returns 1-4 steps per response; system advances locally until checkpoint, then re-queries API.

**Benefit:** 2-4x fewer API calls per task.

### 4. Tool Use for Structured Output

**Why:** Raw JSON prompting is fragile (malformed JSON, silent failures).

**Decision:** Use Anthropic's `tool_use` / OpenAI's `function_calling` for validated schema.

**Benefit:** Invalid responses rejected by API, not by our app.

### 5. User Controls Privacy (No Heuristics)

**Why:** Heuristic detection of "sensitive" screens is unreliable (false negatives = liability, false positives = annoying).

**Decision:** User has explicit controls: pause hotkey, app/URL blocklists, capture indicator.

**Benefit:** Trustworthy, no hidden behavior.

### 6. MVP Browser-Only, Windows-Only

**Why:** Blender/complex apps + macOS/Linux = too much surface area for a first release.

**Decision:** Browser tasks only (Amazon, TurboTax web, Google Forms). Windows only.

**Benefit:** Can ship v0.1 in 12 weeks, learn from real users, then expand.

### 7. Multi-Process Architecture (GIL Mitigation)

**Why:** Python's GIL prevents true multithreading for CPU work. Running 10fps pixel-diff + OCR on the same thread as Qt freezes the UI.

**Decision:** CPU-heavy work (OCR, pHash, screen diff) runs in separate `multiprocessing.Process` workers. Main process only handles Qt UI + asyncio I/O. Communication via `multiprocessing.Queue`.

**Benefit:** UI never stalls. OCR runs in parallel with API calls — by the time the API returns `target_text`, OCR results are already cached.

### 8. PaddleOCR over EasyOCR

**Why:** EasyOCR depends on PyTorch (~500MB+, 2GB+ with CUDA). CPU inference is 200-500ms.

**Decision:** Use PaddleOCR — ~50-150ms on CPU, ~100MB dependency, no CUDA needed.

**Benefit:** 2-3x faster OCR, 5x smaller dependency footprint.

### 9. pip install for MVP (No PyInstaller)

**Why:** A PyInstaller `.exe` doing screen capture + hotkeys + clipboard = SmartScreen blocks it as malware.

**Decision:** MVP ships as `pip install ai-navigator`. Tauri native binary at v0.3 with EV code signing.

**Benefit:** Zero SmartScreen issues. MVP testers are developers who have Python.

### 10. FSL License (Functional Source License)

**Why:** MIT is too permissive (competitors clone freely). GPL prevents closed-source Pro tier. No license = ambiguous rights.

**Decision:** FSL-1.1-Apache-2.0. Source-available, 2-year non-compete, converts to Apache 2.0.

**Benefit:** Code is public (trust, transparency), but commercial rights protected during growth phase.

---

## Links & References

- **Design Document:** [AI-Navigator-Design-Document.md](docs/AI-Navigator-Design-Document.md) (§1–11 detailed specs)
- **GitHub:** [stevefu-ops/ai-navigator](https://github.com/stevefu-ops/ai-navigator)
- **Anthropic API:** https://docs.anthropic.com (tool_use, vision, caching)
- **PaddleOCR:** https://github.com/PaddlePaddle/PaddleOCR
- **PySide6:** https://doc.qt.io/qtforpython-6/
- **Prompt Caching:** https://docs.anthropic.com/en/docs/build-a-system-with-claude/architecture (cost control)

---

## Quick Commands

```bash
# Install dev dependencies
pip install -e ".[dev]"

# Run linter
ruff check src/

# Format code
black src/

# Type check
mypy src/

# Run tests
pytest

# Build distribution (v1.0+)
python -m build

# Run app
python -m src.main
```

---

## Contact & Questions

- **Issues:** [GitHub Issues](https://github.com/stevefu-ops/ai-navigator/issues)
- **Discussions:** [GitHub Discussions](https://github.com/stevefu-ops/ai-navigator/discussions)

---

*Last updated: 2026-04-05*
