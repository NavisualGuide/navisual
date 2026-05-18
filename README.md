# Navisual

**The AI guides, never overrides.**

Navisual is a Windows desktop app that watches your screen and gives real-time step-by-step instructions via on-screen overlays and audio. The AI never clicks, types, or takes control — every action is yours.

**Status:** v0.5.x — public beta. Signed Windows installer at [navisualguide.com](https://navisualguide.com).  
**Website:** [navisualguide.com](https://navisualguide.com)

---

## Quick Start

**No API key required.** The app includes 50 free AI requests out of the box. Just build, launch, and start guiding.

1. Type your task — *"How do I export a PDF in Illustrator?"*
2. Follow the arrows and audio instructions on screen
3. Press ``Ctrl+` `` to confirm each step and advance

---

## Build from Source (Windows)

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) 18+
- [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (for Windows system crates)
- [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) — pre-installed on Windows 11

### Steps

```powershell
git clone https://github.com/NavisualGuide/navisual.git
cd navisual
npm install
npm run tauri dev      # development (hot reload)
npm run tauri build    # production binary
```

The production binary is placed in `src-tauri/target/release/`.

### Configuration

Settings are stored in `%APPDATA%\com.navisual.app\.env`. The app creates this file on first launch. You can also copy `.env.example` there to pre-configure it.

In development (`npm run tauri dev`), the project-root `.env` is used instead.

**To use your own API key** (optional — the free managed tier works without one):

```env
API_PROVIDER=gemini
GEMINI_API_KEY=AIza-xxx        # Free key: https://aistudio.google.com/apikey
```

Or use Anthropic:

```env
API_PROVIDER=anthropic
ANTHROPIC_API_KEY=sk-ant-xxx
```

All settings are also configurable in-app via **Settings** (gear icon) — no `.env` editing required.

---

## AI Providers

| Provider | Setup | Cost |
|----------|-------|------|
| **Managed (free)** | None — works on first launch | 50 free requests, then paid |
| Gemini | Free API key at [aistudio.google.com](https://aistudio.google.com/apikey) | Free tier available |
| Anthropic | API key at [console.anthropic.com](https://console.anthropic.com) | Pay per use |
| Ollama | [ollama.com](https://ollama.com) + `ollama pull llama3.2-vision` | Free, runs locally |
| OpenAI | API key at [platform.openai.com](https://platform.openai.com) | Pay per use |

---

## Hotkeys

| Key | Action |
|-----|--------|
| <kbd>Ctrl</kbd>+<kbd>`</kbd> | Next step / confirm completed |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Wrong — re-analyze the current screen |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> | Pause / resume |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Show / hide the panel |

All hotkeys are configurable in **Settings → Hotkeys**.

---

## Features

- **Observe, never act** — reads your screen, never moves the mouse or types
- **Screen Guide** — overlay indicators pointing at the exact UI element
- **Live captions** — subtitle strip showing the current instruction
- **Audio narration** — TTS via Windows SAPI (no install required)
- **Voice input** — push-to-talk via Web Speech API
- **Free managed tier** — 50 requests out of the box, no account needed
- **Multi-provider AI** — Gemini, Anthropic (Claude), Ollama (local), OpenAI, Managed
- **Windows UI Automation** — primary element locator, < 5ms for browsers
- **Windows OCR** — built-in fallback, zero model downloads
- **Active-window crop** — sends only the relevant window to the AI
- **Multi-step sequences** — groups sequential actions to reduce API calls
- **Session persistence** — state preserved across app restarts
- **Autopilot mode** — auto-advances on screen change without pressing next
- **In-app settings** — configure everything without editing files

---

## Architecture

For a short technical tour (data flow, element locator, key design decisions), see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Project Structure

```
navisual/
├── src/                        # Svelte frontend
│   ├── App.svelte              # Main panel UI
│   └── Overlay.svelte          # Transparent overlay canvas
├── src-tauri/                  # Rust backend
│   ├── src/
│   │   ├── lib.rs              # Tauri commands + guidance loop
│   │   ├── ai/                 # AI router (Anthropic, Gemini, Ollama, Managed)
│   │   ├── capture/            # Screen capture (BitBlt, active-window crop)
│   │   ├── locator/            # Element locator (UIA + OCR)
│   │   ├── overlay.rs          # Overlay pipeline
│   │   ├── tts.rs              # Windows SAPI TTS
│   │   ├── server.rs           # Supabase auth client
│   │   ├── track.rs            # Window tracker (HWND focus detection)
│   ├── Cargo.toml
│   └── tauri.conf.json
├── index.html                  # Panel window entry point
├── overlay.html                # Overlay window entry point
└── .env.example                # Config template
```

---

## Roadmap

```
v0.5    ✅ Free managed tier (Supabase relay, anonymous auth, 50 free requests)
        🔜 Pay-as-you-go coin purchases + signed Windows installer
v0.6    Template matching + Nav-Packs 
v1.0    Microsoft Store + enterprise features + public launch
v1.x    macOS port + Linux port
```

---

## Privacy

**What stays on your machine.** Local element matching (UI Automation + OCR), session history, settings, and cost tracking are all local-only. The AI returns *text* descriptions of UI elements; your machine finds the pixels — UIA / OCR coordinates are never sent to the AI.

**What gets sent to the AI.** Only a screenshot of the **active window** (active-window crop, by default) plus the conversation text.

| Provider | Where the screenshot + text go |
|---|---|
| **Managed (free, default)** | Supabase Edge Function → OpenRouter → NVIDIA Nemotron |
| BYOK Anthropic | `api.anthropic.com` |
| BYOK Gemini | `generativelanguage.googleapis.com` |
| BYOK OpenAI | `api.openai.com` |
| BYOK DeepSeek | `api.deepseek.com` (text only — no image is sent) |
| BYOK Qwen | `dashscope.aliyuncs.com` (or your configured workspace URL) |
| BYOK Ollama | `http://localhost:11434` (local — nothing leaves your machine) |

**Other notes.**

- Screenshots are held in memory only — never written to disk at default settings
- Full-screen capture (multi-monitor) requires explicit one-time permission per AI call
- Press <kbd>Ctrl</kbd>+<kbd>S</kbd> to stop all capture instantly
- Settings, sessions, and the Supabase anonymous-auth token live in `%APPDATA%\com.navisual.app\`
- Debug captures + locator traces are off by default; if enabled (developer-only `.env` flags), files older than 7 days are auto-deleted

---

## Contributing

Issues and pull requests welcome.

---

## License

[FSL-1.1-Apache-2.0](https://fsl.software/) — source-available. Each version converts to Apache 2.0 two years after its release date.
