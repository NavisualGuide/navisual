# AI Navigator

**The AI guides, never overrides.**

AI Navigator is a cross-platform desktop application that guides users through computer tasks by observing their screen and providing real-time navigation instructions via audio and on-screen overlays. Unlike AI agents that take control, AI Navigator keeps the user in command — every click, keystroke, and decision is theirs.

## Features

- 👁️ **Observes, never acts** — Reads your screen but never moves the mouse or types
- 🗣️ **Real-time guidance** — Audio instructions + visual overlays adapted to what's on screen
- 🎯 **Smart element detection** — Local OCR + accessibility APIs find UI elements accurately
- 📋 **Multi-step sequences** — Reduce API calls by grouping sequential actions
- 💾 **Session persistence** — Save and resume tasks across sessions
- 🔐 **Privacy-first** — User controls capture; no heuristic detection of sensitive screens
- 🌍 **Cross-platform** — Windows (MVP), macOS/Linux (v0.3+)

## Quick Start

### Requirements

- Python 3.11+
- Windows 10+ (macOS/Linux in v0.3+)

### Installation

```bash
# Clone
git clone https://github.com/stevefu-ops/ai-navigator.git
cd ai-navigator

# Create venv
python -m venv venv
source venv/Scripts/activate  # Windows: venv\Scripts\activate

# Install
pip install -e ".[dev]"

# Configure
cp .env.example .env
# Edit .env with your Anthropic API key
```

### Run

```bash
python -m src.main
```

## Project Structure

```
ai-navigator/
├── src/
│   ├── core/           # Business logic (session, state, cost tracking)
│   ├── input/          # Screen capture, event detection, user input
│   ├── ai/             # API clients, tool schemas
│   ├── locator/        # Element locator (OCR + accessibility API)
│   ├── output/         # Overlay, TTS, clipboard
│   ├── ui/             # PySide6 UI windows
│   ├── main.py         # Entry point
│   └── config.py       # Configuration
├── tests/              # Unit & integration tests
├── docs/               # Documentation
├── AI-Navigator-Design-Document.md  # Full design spec
└── CLAUDE.md           # Project guide for Claude Code
```

## Status

**v0.1.0-alpha** — Scaffolding complete, MVP implementation starting.

### MVP Timeline (12 weeks)

| Weeks | Milestone |
|-------|-----------|
| 1–2 | Screen capture + event detection + chat UI |
| 3–4 | Anthropic API + multi-step sequences + state summarization |
| 5–6 | OCR integration + Element Locator + overlay rendering |
| 7–8 | Correction hotkey + session persistence + clipboard |
| 9–10 | End-to-end testing with browser tasks |
| 11 | Internal demo + feedback |
| 12 | v0.1 alpha release |

### MVP Scope

✅ **In scope:**
- Event-driven screen capture
- Text chat input
- Anthropic API with tool_use (structured output)
- Multi-step sequences with checkpoints
- Local OCR-based element locator
- On-screen overlay arrows & subtitles
- Correction hotkey (Ctrl+Shift+X)
- Session persistence (save/resume)
- Clipboard commands for CLI tasks
- Browser tasks only, Windows only

❌ **Not in v0.1:**
- TTS / voice input (v0.2)
- macOS / Linux (v0.3+)
- Complex apps like Blender (v0.3+)
- Nav-Packs (v0.3+)

## Configuration

Copy `.env.example` to `.env` and set your API keys:

```bash
ANTHROPIC_API_KEY=sk-ant-...
```

See [CLAUDE.md](CLAUDE.md) for detailed configuration options.

## Architecture

AI Navigator uses a six-layer model:

1. **Input** — Screen capture, event detection, user input
2. **Core Engine** — Session management, state, cost control, API routing
3. **Element Locator** — OCR + accessibility APIs find UI elements locally
4. **Output** — Overlay rendering, TTS, clipboard
5. **AI Backend** — Anthropic, OpenAI, or local models
6. **Platform Layer** — OS-specific APIs (screen capture, overlays, hotkeys)

Read [CLAUDE.md](CLAUDE.md) for detailed architecture, conventions, and status.

## Development

### Running Tests

```bash
pytest                    # All tests
pytest -v --cov         # Verbose + coverage
pytest tests/test_ocr.py # Specific test
```

### Code Quality

```bash
black src/              # Format
ruff check src/         # Lint
mypy src/               # Type check
```

### Building

```bash
pip install -e ".[dev]"
```

For production (v1.0+), we'll use PyInstaller:

```bash
pyinstaller --onefile src/main.py
```

## Privacy & Security

- **No screenshot persistence** — Images stay in RAM, never written to disk
- **User-controlled capture** — Pause/resume hotkey, app/URL blocklists
- **Encrypted API keys** — Stored in OS keychain
- **Local-first option** — Works fully offline with local models + Whisper
- **Input-transparent overlay** — Cannot intercept clicks or keystrokes

## Business Model

- **Community (Free)** — Own API key or local model
- **Personal Pro ($25–30/mo)** — Managed keys, full overlay, voice, session persistence, Nav-Packs
- **Enterprise (Custom)** — Custom Nav-Packs, SSO, audit logs, on-prem option

## Contributing

This is an early-stage project. Contributions welcome!

- **Issues:** [GitHub Issues](https://github.com/stevefu-ops/ai-navigator/issues)
- **Design discussions:** [GitHub Discussions](https://github.com/stevefu-ops/ai-navigator/discussions)

## License

FSL-1.1-Apache-2.0 (Functional Source License) — see LICENSE file. Source-available with a 2-year non-compete clause. Each version converts to Apache 2.0 two years after release.

## Design & Research

The full design document is available at [AI-Navigator-Design-Document.md](AI-Navigator-Design-Document.md). It covers:

- Detailed architecture & data flow
- Cost modeling & optimization strategies
- Token budget system with safety margins
- Privacy & security measures
- UI/UX design principles
- MVP plan & post-MVP roadmap
- Business model & pricing tiers

For developers and contributors, read [CLAUDE.md](CLAUDE.md) for a project guide.

## Roadmap

```
v0.1 (week 12)  MVP: browser tasks, Windows, text/overlay guidance (pip install)
v0.2 (month 5)  TTS + voice input, prompt caching, accessibility APIs
v0.3 (month 8)  Tauri/Rust rewrite, complex apps (Blender), macOS, Nav-Packs
v0.4 (month 10) Linux, plugin system, accessibility UX pass
v1.0 (month 12) MSIX packaging, native installer, public launch
```

## Questions?

- **GitHub Issues:** [Feature requests, bug reports](https://github.com/stevefu-ops/ai-navigator/issues)
- **Discussions:** [Design questions, ideas](https://github.com/stevefu-ops/ai-navigator/discussions)

---

**Status:** 🚧 Early-stage development. Not for production use yet.

**Last updated:** 2026-04-05
