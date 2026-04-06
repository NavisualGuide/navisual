# AI Navigator — Project Guide

**Version:** 0.1.0-alpha
**Status:** Scaffolding complete. MVP implementation starting.
**Design Doc:** [AI-Navigator-Design-Document.md](AI-Navigator-Design-Document.md)
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
│ 2. Local OCR (EasyOCR) - fallback, works on any app    │
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
| Voice Input | `input/voice_input.py` | Stub for v0.2 | STUB |
| API Router | `ai/api_router.py` | Provider selection, request building | TODO |
| Anthropic Client | `ai/anthropic.py` | Anthropic API (tool_use) | TODO |
| OpenAI Client | `ai/openai_client.py` | OpenAI API (function_calling) | STUB |
| Tool Schemas | `ai/tool_schemas.py` | navigate_step tool definition | TODO |
| Element Locator | `locator/element_locator.py` | Orchestrates OCR + A11y + templates | TODO |
| OCR Engine | `locator/ocr_engine.py` | EasyOCR wrapper, text → bbox | TODO |
| A11y Engine | `locator/a11y_engine.py` | Stub for v0.2 (UIA on Windows) | STUB |
| Overlay Renderer | `output/overlay.py` | Qt frameless window for overlays | TODO |
| Clipboard Manager | `output/clipboard.py` | System clipboard access | TODO |
| TTS Engine | `output/tts.py` | Stub for v0.2 | STUB |
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

### ✅ Completed
- Design document v0.2 (comprehensive)
- Project structure scaffolded
- pyproject.toml configured
- GitHub repo initialized & pushed
- CLAUDE.md (this file)

### 🚧 In Progress
- Starting MVP implementation

### 📋 MVP Milestones

```
Week 1-2:  Screen capture + event detection + basic chat UI
Week 3-4:  Anthropic API + tool_use + state summarization
Week 5-6:  EasyOCR integration + Element Locator + overlay rendering
Week 7-8:  Correction hotkey + session persistence + clipboard
Week 9-10: End-to-end testing (browser tasks)
Week 11:   Internal demo + feedback
Week 12:   v0.1 alpha release
```

### 🎯 MVP Scope (v0.1)

| Feature | In? | Notes |
|---------|-----|-------|
| Event-driven screen capture | ✓ | OS events + pixel-diff + idle fallback |
| Text chat input | ✓ | PySide6 window |
| Anthropic API (tool_use) | ✓ | Structured output |
| Multi-step sequences | ✓ | 1-4 steps per response, checkpoints |
| Local OCR (Element Locator) | ✓ | EasyOCR for overlay positioning |
| Overlay arrows | ✓ | Based on OCR-found positions |
| Correction hotkey | ✓ | Ctrl+Shift+X → re-analysis |
| Session persistence | ✓ | Save/resume sessions |
| Clipboard commands | ✓ | For CLI tasks |
| TTS / Voice input | ✗ | v0.2 |
| Accessibility API | ✗ | v0.2 (OCR sufficient for browsers) |
| Multi-platform | ✗ | Windows only for MVP |

### 📅 Post-MVP Roadmap

```
v0.2  TTS + voice input (paired) + prompt caching + Accessibility API (UIA)
v0.3  Blender/complex apps + template matching + local model support + macOS
v0.4  Linux + Nav-Packs + accessibility UX pass + plugin system
v1.0  Rust/Tauri rewrite + public launch
```

---

## Configuration

### Environment Variables

```bash
# .env
ANTHROPIC_API_KEY=sk-ant-...
OPENAI_API_KEY=sk-...           # Optional, for testing
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

---

## Links & References

- **Design Document:** [AI-Navigator-Design-Document.md](AI-Navigator-Design-Document.md) (§1–11 detailed specs)
- **GitHub:** [stevefu-ops/ai-navigator](https://github.com/stevefu-ops/ai-navigator)
- **Anthropic API:** https://docs.anthropic.com (tool_use, vision, caching)
- **EasyOCR:** https://github.com/JaidedAI/EasyOCR
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
