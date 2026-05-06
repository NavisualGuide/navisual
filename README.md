# Navisual

**The AI guides, never overrides.**

Navisual is a Windows desktop app that guides you through computer tasks by watching your screen and giving real-time step-by-step instructions via on-screen overlays and audio. The AI never clicks, types, or takes control — every action is yours.

**Status:** v0.3.1-alpha — actively developed, suitable for developer testing.

---

## Tester Setup (Windows 11)

### 1. Install Python 3.11+

Download from [python.org](https://www.python.org/downloads/). During install, check **"Add Python to PATH"**.

Verify:
```
python --version   # must be 3.11 or higher
```

### 2. Get the code

```
git clone https://github.com/NavisualGuide/navisual.git
cd navisual
```

Or download the ZIP from GitHub and extract it.

### 3. Create a virtual environment

```
python -m venv venv
venv\Scripts\activate
```

### 4. Install dependencies

```
pip install -e .
```

### 5. Configure your API key

```
copy .env.example .env
```

Open `.env` and set your provider. **Gemini is the easiest for testers — free, no credit card:**

```env
API_PROVIDER=gemini
GEMINI_API_KEY=AIza-xxx        # Free key: https://aistudio.google.com/apikey
DAILY_TOKEN_CAP=1000000        # Raise the cap — default 100k is tight for testing
```

Alternatively, use Anthropic (Claude):

```env
API_PROVIDER=anthropic
ANTHROPIC_API_KEY=sk-ant-xxx
DAILY_TOKEN_CAP=1000000
```

### 6. Run

```
python -m src.main
```

Run as a **regular user** (not admin) — global hotkeys work without elevation on Windows 11.

---

## Using Navisual

The panel starts as a small draggable dot (icon mode). Click it to expand. Type your task in the input box, then follow the on-screen arrows and subtitles.

### Hotkeys

| Key | Action |
|-----|--------|
| `Alt+\`` | Next step / confirm completed |
| `Alt+E` | Wrong — re-analyze the current screen |
| `Alt+S` | Pause / resume capture |
| `Alt+Q` | Show / hide the panel |
| `Alt+A` | Push-to-talk voice input |
| `Alt+R` | Re-read last instruction aloud |

All hotkeys are configurable in **Settings → Hotkeys**.

### Settings

Click the gear icon in the panel to open Settings. You can change your API provider and key, adjust overlay appearance, and remap hotkeys — no `.env` editing required.

---

## Known Issues / Friction Points

| Issue | Fix |
|-------|-----|
| Panel not visible after launch | Look for a small orange dot — click it to expand |
| No spoken instructions | Add `ENABLE_TTS=true` to `.env` |
| `keyboard` raises PermissionError | Run as a normal user, not as Administrator |
| Arrow points to wrong place | Use `Alt+E` (Wrong) to trigger a re-analysis |

---

## Features (v0.3.1-alpha)

- **Observe, never act** — reads your screen, never moves the mouse or types
- **Real-time overlay arrows** — points at the exact UI element to click
- **Audio narration** — optional TTS via Windows SAPI
- **Voice input** — push-to-talk via Google Web Speech API
- **Multi-provider AI** — Gemini (free), Anthropic (Claude), Ollama (local), OpenAI
- **Multi-step sequences** — groups sequential actions to reduce API calls
- **Windows UI Automation** — primary element locator, < 5ms for browsers
- **Windows OCR** — built-in fallback, zero model downloads
- **Active-window crop** — sends only the relevant window to the AI (~80% token reduction)
- **Session persistence** — save and resume tasks
- **In-app settings** — configure provider, overlay, and hotkeys without editing `.env`
- **Correction hotkey** — `Alt+E` to re-analyze when the AI gets it wrong

---

## Project Structure

```
navisual/
├── src/
│   ├── main.py            # Entry point
│   ├── config.py          # Configuration
│   ├── core/              # Session, state, cost tracking
│   ├── input/             # Screen capture, event detection
│   ├── ai/                # API clients (Gemini, Anthropic, Ollama, OpenAI)
│   ├── locator/           # Element locator (A11y + OCR)
│   ├── output/            # Overlay, TTS, clipboard
│   └── ui/                # PySide6 windows
├── docs/
│   ├── Navisual-Design-Document.md   # Full design spec
│   ├── settings.md                        # Settings reference
│   └── nav-packs.md                       # Nav-Pack format spec
├── .env.example           # Config template
└── CLAUDE.md              # Developer / project guide
```

---

## Development

```bash
# Install with dev extras
pip install -e ".[dev]"

# Run tests
pytest

# Lint / format / type check
ruff check src/
black src/
mypy src/
```

---

## Roadmap

```
v0.3.1  Settings window + hotkeys redesign + bug fixes     ← current
v0.4    Signed Windows installer (embedded Python, no setup required)
v0.5    Template matching + Nav-Packs (Blender, SolidWorks)
v1.0    Microsoft Store + enterprise features + public launch
v1.x    macOS port + Linux port
```

---

## Privacy

- Screenshots are processed in RAM and never written to disk
- The AI receives only the active window crop (not your full desktop by default)
- Use `Alt+S` (Pause) to stop capture at any time
- Run fully offline with Ollama — no data leaves your machine

---

## License

[FSL-1.1-Apache-2.0](https://fsl.software/) — source-available with a 2-year non-compete clause. Each version converts to Apache 2.0 two years after release.

---

## Links

- **Issues / bugs:** [GitHub Issues](https://github.com/NavisualGuide/navisual/issues)
- **Design doc:** [Navisual-Design-Document.md](docs/Navisual-Design-Document.md)
- **Settings reference:** [settings.md](docs/settings.md)
- **Free Gemini key:** https://aistudio.google.com/apikey
