# Navisual — Project Guide

**Version:** 0.4.0-alpha
**Status:** v0.4 Phases A–E.7 complete (Tauri scaffold, full-Rust AI backend, screen capture + A11y + OCR + locator, overlay, guidance loop + chat UI, hotkeys, TTS, screen watcher, streaming, clipboard, needs_input reply UI, settings modal, voice input). Settings stored in %APPDATA%\com.navisual.app\.env. Next: packaging / internal tester distribution.
**License:** FSL-1.1-Apache-2.0 (Functional Source License, converts to Apache 2.0 after 2 years)
**Design Doc:** [Navisual-Design-Document.md](docs/Navisual-Design-Document.md) *(Note: This SDD is the main source of truth for future changes. Always update `CLAUDE.md` to sync with the SDD).*
**Settings:** [settings.md](docs/settings.md)
**Nav-Packs:** [nav-packs.md](docs/nav-packs.md)
**GitHub:** [NavisualGuide/navisual](https://github.com/NavisualGuide/navisual)

---

## Quick Summary

**Navisual** is a cross-platform desktop app that guides users through computer tasks by observing their screen and providing real-time navigation instructions (via audio/overlay). The user always stays in control — the AI never clicks, types, or acts.

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
| Session Manager | `src-tauri/src/ai/session.rs` | Lifecycle, persistence, conversation history | DONE |
| State Context | `src-tauri/src/ai/prompts.rs` | App state, window context, and Rule 17 | DONE |
| Cost Tracker | `src-tauri/src/ai/cost_tracker.rs` | Token budgets, daily/monthly caps | DONE |
| API Router | `src-tauri/src/ai/mod.rs` | Provider selection (Anthropic, Gemini, Ollama) | DONE |
| Screen Capture | `src-tauri/src/capture/mod.rs` | On-demand BitBlt, active-window crop | DONE |
| Screen Watcher | `src-tauri/src/screen_watcher.rs` | Event-driven aHash detection (500ms) | DONE |
| Element Locator | `src-tauri/src/locator/mod.rs` | Orchestrates OCR + A11y + templates | DONE |
| OCR Engine | `src-tauri/src/locator/ocr.rs` | **FALLBACK**: Windows.Media.Ocr (built-in) | DONE |
| A11y Engine | `src-tauri/src/locator/a11y.rs` | **PRIMARY**: Windows UIA element lookup (< 5ms) | DONE |
| Overlay Pipeline | `src-tauri/src/overlay.rs` | Configure/emit overlay updates to WebView | DONE |
| TTS Engine | `src-tauri/src/tts.rs` | Text-to-speech via Windows SAPI (STA thread) | DONE |
| Guidance Loop | `src-tauri/src/lib.rs` | `guide`, `next_step`, `send_correction` | DONE |
| Frontend Panel | `src/App.svelte` | Main chat UI, history, and consent logic | DONE |
| Frontend Overlay | `src/Overlay.svelte` | Transparent canvas for arrows/highlights | DONE |


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
   - Correction hotkey (Alt+E) → trigger correction handler
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
- System prompt: Navisual window self-awareness (minimize, not close)
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

### ✅ Completed (v0.3.0)
- **Token optimization** — API-send screenshot downscaled to 768×432 max (2 vision tiles, ~75% token reduction vs 1920×1080); active-window crop via `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` cuts tokens up to 80% more; self-process exclusion prevents crop to Navisual's own window
- **Extended model tiering** — Gemini Flash Lite for all automated screen-change re-queries; Gemini Flash (full) for initial and user-triggered requests
- **UI consolidation** — `MainWindow` + `FloatingWindow` replaced by single `ConsolidatedPanel` (`src/ui/panel_window.py`); two states: panel mode (360×540, full UI) and icon mode (56×56 draggable dot); `WDA_EXCLUDEFROMCAPTURE` applied so panel never appears in API screenshots
- **Checkpoint rework** — Checkpoint steps no longer auto-complete on screen change (too noisy in complex apps like OneNote ribbons). Completion is now explicit: user presses **→ Next** button or Alt+` (next-step hotkey). Next button re-queries the AI with `[User completed: '...']` context so it advances rather than repeating the same instruction
- **A11y multi-window search** — When Navisual is the foreground window (user clicked Next button), the A11y engine now searches all other desktop top-level windows instead of returning nothing. Fixes "arrow missing after Next" issue
- **A11y false-match fix** — `_search_descendants` regex changed from substring `(?i)Insert` to anchored `(?i)^Insert$`; prevents "Insert" matching "Insert Space", "Insert Row", etc.
- **Own-window crop exclusion** — `get_foreground_window_rect()` checks PID against `os.getpid()`; if Navisual is foreground, returns None so full desktop is sent to API
- **System prompt rule 13** — Added screen-scope rule: AI can set `request_full_screen=true` when it needs to see beyond the active window crop (Start Menu, taskbar, system dialogs)

### ✅ Completed (v0.3.0-patch — 2026-04-13)
- **Screen flash fix** — `WDA_EXCLUDEFROMCAPTURE` removed from both panel and overlay. Windows DWM was flashing both monitors on every 10fps mss capture while either window had that flag set. Replaced with software-based blanking in `prepare_api_image()`: panel blanked via `GetWindowRect(_panel_hwnd)`; overlay blanked via `set_overlay_bbox()` registry with 140px padding to cover arrow shaft + arrowhead.
- **Thinking animation** — Latest-instruction box cycles `Thinking` → `Thinking.` → `Thinking..` → `Thinking...` at 500ms while waiting for API. `begin_streaming_message()` moved to lazy-init on first streaming chunk so animation runs for the full wait duration.
- **System prompt rule 14 — Scrolling** — AI must emit a dedicated scroll step (no overlay, no target_text) before directing the user to click an element that is not visible on screen.
- **System prompt rule 15 — Unfamiliar software** — AI must confirm the software name via `needs_input=true` before navigating when the software is unrecognised.
- **System prompt rule 16 — Webpage install commands** — AI must extract and copy install commands from the current page rather than bouncing between pages.
- **`ocr_engine.py` syntax fix** — `@staticmethod` decorator was misplaced on `_BUTTON_LIKE_ROLES` class variable instead of `find_text()`, causing a `SyntaxError` on import.
- **`CHECKPOINT_AUTO_ADVANCE` default `true` → `false`** — auto-continue is now opt-in. Most users prefer on-demand help (ask → follow → ask again). Continuous auto-advance is a power-user setting for fully guided walkthroughs.

### ✅ Completed (v0.3.1-alpha — 2026-04-13)
- **Settings window** (`src/ui/settings_window.py`, new) — Modal dialog with Provider / Capture / Overlay tabs. Reads current `.env` on open; writes atomically on Apply; clears `@lru_cache` and emits `applied(new_config)` to push live changes. Provider tab: all four providers (Gemini, Anthropic, Ollama, OpenAI) with key fields greyed when inactive. Capture tab: auto-continue toggle + sensitivity slider only. Overlay tab: color picker, thickness/font/opacity sliders, duration spinbox with "auto" special-value.
- **Overlay black-screen fix (final)** — Changed overlay from show/hide lifecycle to **permanently visible**. Previously the pre-warm did `show()→hide()`, which released the DWM compositing surface; the next `show_overlay()` re-allocated it and flashed both monitors for ~5 s. Overlay now calls `show()` once at startup and stays visible. Since `paintEvent` produces zero output when `_overlay_type="none"` and `_show_subtitle=False`, the window is fully transparent when idle. `clear()` never calls `hide()`.
- **Esc to dismiss overlay** — `ConsolidatedPanel` gained `overlay_dismiss_requested = Signal()` wired to an Esc `QShortcut`. Connected to `overlay.clear()` in `Application._connect_signals()`. Users can dismiss a subtitle that covers content.
- **A11y regex fix — arrow-prefixed links** (`a11y_engine.py`) — Regex changed from anchored exact `(?i)^target$` to `(?i)^[\W_]*target[\W_]*$`. Allows optional leading/trailing non-word characters (Unicode arrows ←→, bullets, chevrons) so "← my claims" matches target "my claims". "Insert Space" still correctly rejected — "Space" is a word char and blocks `[\W_]*$`.
- **OCR link disambiguation** (`ocr_engine.py`) — Split `_BUTTON_LIKE_ROLES` into `_PREFER_LARGEST_ROLES` (`button`, `tab`, `menuitem`, `checkbox`, `radio`) and `_PREFER_SMALLEST_ROLES` (`link`). Real buttons are visually larger than inline text → prefer largest. Breadcrumb/nav links are smaller-font than headings sharing the same word → prefer smallest. Fixes overlay pointing at heading text instead of a navigation link.
- **OCR bbox padding** (`main.py`) — When OCR locates a `button`, `tab`, or `menuitem`, adds proportional padding (±25% width, ±33% height) so the overlay box covers the full clickable hit-area, not bare character bounds. Not applied to `link` (inline-sized, padding unhelpful).
- **OpenAI settings** (`settings_window.py`, `config.py`) — Added OpenAI section to Provider tab: API key password field + model dropdown (gpt-4o default, editable). Added `openai_model: str` field to `Config`. OpenAI support remains a stub in the engine; settings UI is complete.
- **Settings UX improvements** — Sensitivity label renamed "Trigger threshold" with plain-language tooltip (Low/Medium/High examples). Duration tooltip clarifies "auto = persists until next instruction or Esc".
- **Single-screen picker removed** — The `CAPTURE_SCREEN` config field and monitor selector UI were removed. With active-window crop enabled (default), the API image is already cropped to the foreground window regardless of monitor count — a per-monitor filter provides no cost or quality benefit.
- **force_full resolution raised** — `force_full` requests (Start Menu, taskbar, system dialogs) now cap at 1280×720 instead of 768×432. On a 2-monitor setup the old cap produced a 768×216 panoramic strip; 1280×720 gives readable quality for these rare requests (0–2 per session).
- **`capture_screen` config field removed** (`config.py`) — Field existed but `screen_capture.py` always captured `monitors[0]` (all monitors). Removed dead code.
- **Hotkeys redesigned — Alt+key** — All 6 hotkeys switched from Ctrl+Shift combos to single-modifier Alt+key for easy one-handed left-hand use (right hand on mouse). New defaults: Next=`alt+\``, Re-analyze=`alt+e`, Pause=`alt+s`, Toggle panel=`alt+q`, Talk=`alt+a`, Re-read=`alt+r`. Parser extended with `MOD_ALT` and `VK_OEM_3` (0xC0) for backtick.
- **Two new hotkeys: Talk + Re-read** — Talk (`alt+a`) triggers push-to-talk voice input globally (same as mic button in panel). Re-read (`alt+r`) replays the last instruction via TTS; starts TTS thread on demand even if `ENABLE_TTS=false`.
- **Hotkeys settings tab shipped** (`settings_window.py`) — All 6 hotkeys configurable in-app. Plain QLineEdit fields, all marked 🔄 restart-required. Adds `TALK_HOTKEY` and `REREAD_HOTKEY` env vars.
- **Zone-hint coordinate fix** (`screen_capture.py`, `main.py`) — Zone-hint overlay was placed at the wrong screen position when active-window crop is enabled. Root cause: the AI's zone coordinates are relative to the cropped API image (e.g., the VS Code window), but the zone-hint code computed positions using `_vd_width/_vd_height` (the full virtual desktop). Fix: `prepare_api_image()` now records `_last_api_crop_rect` (actual crop rect in virtual-desktop physical pixels) exposed via `get_last_api_crop_rect()`; the zone-hint calculation uses the crop rect dimensions so `zone=(10,0)` correctly maps inside the cropped window instead of across the full desktop.

### ✅ Completed (v0.4-alpha Phases A–C — 2026-04-19)

- **Phase A — scaffold** (commit e7e406f) — Tauri v2 + Svelte 5 (Vite SPA) + Rust backend + Python sidecar skeleton. Panel window spawns, invokes Rust commands.
- **Phase B — sidecar IPC** (commit 997f300) — Python AI layer (`ai/`, `core/`) migrated into `sidecar/`. JSON-lines protocol over stdin/stdout. Rust `Sidecar` type spawns the Python process and round-trips requests. `ping`, `echo`, `cost_report` dispatchers.
- **Phase C.1 — screen capture in Rust** (commit 2d4300d) — `xcap` crate for per-monitor capture, `image` crate for JPEG encoding, DWM `EXTENDED_FRAME_BOUNDS` for active-window crop. Tauri commands `capture_screen`, `capture_active_window`.
- **Phase C.2 — A11y locator in Rust** (commit 03859f7 + hardening this round) — `uiautomation` 0.24 crate. Same semantics as v0.3 Python: dash normalisation, anchored `^[\W_]*target[\W_]*$` regex (rejects "Insert Space" for target "Insert"), container-role rejection, off-screen guard. Multi-window search: z-order enumeration via `GetTopWindow`/`GetWindow(GW_HWNDNEXT)` with class-name blocklist (`Progman`, `WorkerW`, `Shell_TrayWnd`, IME classes…) skips shell and self; `collect_visible_top_windows(our_pid, 8)` caps at 8 real candidates so per-root timeout doesn't get diluted. `match_in_subtree` filter_fn swallows `get_name()` errors (was propagating, causing UIMatcher to abort on transient `E_ELEMENTNOTAVAILABLE`). Tauri command `locate_a11y`.
- **Phase C.3 — OCR + orchestrator** (commit 594484f) — `Windows.Media.Ocr` via the `windows` crate (`WriteBytes`/`StoreAsync`/`FlushAsync`/`BitmapDecoder`/`RecognizeAsync`). Line-level + word-level bboxes emitted so single-word targets get a tight box. `find_text` ported from v0.3 Python with identical strategies: exact → substring (MIN_SUBSTR_LEN=8) → fuzzy LCS ratio >0.7, 4%-screen-height button cap, 16×9 zone filter, nearby-text anchor. `LocateOptions` propagates role / nearby_text / zone / timeout. Orchestrator = A11y first, OCR fallback on captured active window. Tauri command `locate_element`.
- **Phase C hardening** (this commit) — Capture self-exclusion: `get_foreground_frame_rect()` checks PID; when our panel is foreground it walks z-order and returns the first non-self, non-shell window. Fixes the case where the user clicks the locate button (panel becomes foreground) and the OCR path would otherwise capture the panel's own contents instead of Task Manager / the target app. Also bumped A11y default timeouts: `locate_a11y` 100 → 1500 ms, `locate_element` A11y phase 150 → 500 ms; frontend `timeoutMs` 300 → 2000 / 300 → 800 so the backend default isn't overridden.

### ✅ Completed (v0.4 Phase D.1–D.2 — 2026-04-23)

- **Phase D.1 — overlay wiring**: `overlay.rs` configure() + emit_update() pipeline; `set_ignore_cursor_events(true)` for click-through (raw `SetWindowLongPtrW` does not propagate to WebView2 child HWND); Tauri capability updated to cover `"overlay"` window; canvas renderer in `src/Overlay.svelte` (box + arrow + subtitle).
- **Phase D.2 — guidance loop + chat UI**: `guide`, `next_step`, `send_correction` Tauri commands; GuidanceState in AppState; panel replaced with task input / instruction panel / Next→ / ✗ Wrong flow; OCR fuzzy threshold raised from 0.7 → 0.85 (prevented "Status"↔"Startup" false match).

### ✅ Completed (v0.4 Phase D.3 — 2026-04-24)

- **Global hotkeys** — `tauri-plugin-global-shortcut` registered from Svelte `onMount`: Alt+` (next step), Alt+E (correction), Alt+S (cancel/pause). Request-token pattern prevents stale AI responses from overwriting a cancelled state.
- **TTS — Windows SAPI** — `src-tauri/src/tts.rs`; `ISpVoice` COM object on a dedicated STA thread; `SPF_ASYNC | SPF_PURGEBEFORESPEAK` flags so each new instruction instantly cancels the previous one. `speak("")` silences on cancel. No Python dependency.
- **Window position tracker** — `src-tauri/src/track.rs`; 200 ms polling thread uses `WindowFromPoint` + `GetAncestor(GA_ROOT)` to find the containing HWND, stores element-relative bbox, re-emits overlay on window move/resize, clears on `IsIconic` (minimize), restores on un-minimize.
- **App close / quit** — `on_window_event(Destroyed)` on the panel window calls `std::process::exit(0)` so closing the window terminates the overlay and Python sidecar. `core:window:allow-close` capability added; capability windows fixed from `"main"` → `"panel"`.
- **Sidecar CWD fix** — sidecar spawn sets `current_dir` to the project root so `pydantic-settings` always finds `.env` regardless of how Tauri launches.
- **90 s AI timeout** — `tokio::time::timeout` wraps `send_guidance` and `trigger_correction` sidecar calls; hangs now surface as a user-visible error instead of blocking forever.
- **Cancel button** — replaces "Guide me" while thinking; drops stale responses via request token; stops TTS and clears overlay immediately.

### ✅ Completed (v0.4 Phase D.4 — 2026-04-25)

- **Svelte SPA Migration**: Stripped out SvelteKit and migrated the frontend to a pure Vite + Svelte SPA multi-page setup (`index.html` and `overlay.html`). This completely eliminates the 40+ second SSR warmup delay during development, reducing Vite startup to ~1 second while retaining all functionality.
- **E.1 UI Overhaul**: Redesigned Svelte panel to match the v0.3 ConsolidatedPanel design language.

### ✅ Completed (v0.4 Phase E.0 — 2026-04-26)

- **E.0 Rust Sidecar Rewrite**: Ported the Python sidecar logic to Rust. Removed the `sidecar/` directory, moving AI routing, Anthropic, Gemini, cost tracking, and session logic directly into `src-tauri/src/ai/`. This eliminates the need for Python as a runtime dependency.

### ✅ Completed (v0.4 Phase E.1–E.7 — 2026-04-30)

- **E.1 UI Overhaul** — already noted in Phase D.4 entry above.
- **E.2 Streaming responses** — Rust AI router streams SSE from Anthropic/Gemini via `reqwest`; `stream_chunk` Tauri event appended to `currentInstruction` in real time. Partial JSON instruction extracted in `ai/streaming.rs`.
- **E.3 Screen change detection + auto-advance** — `screen_watcher.rs` background thread; aHash 8×8 at JPEG q=30 every 500 ms; Hamming threshold 6/64; `screen_changed` Tauri event; 5 s debounce in Svelte; Pause/Resume guard.
- **E.4 Clipboard** — `execute_step()` in `lib.rs` writes `step.clipboard` via `arboard`; 📋 badge shown in step header.
- **E.5 `needs_input` reply UI** — dedicated `replyText` state; blue reply section renders when `phase === "needs_input"`; Enter key + Send button; `isReply: true` flag preserves session in Rust.
- **E.5b Next button context pass-through** — `lastCompletedInstruction` carried into `[User completed: "..."]` re-query; Rust guard prevents session reset on continuation calls.
- **E.6 Settings UI** — ⚙ modal with Provider / Screen Guide / Hotkeys / Audio tabs. `get_settings` / `save_settings` Tauri commands. Atomic `.env` write; empty key fields skip overwrite; in-process config reload via `router.reload_config()`. Emits `overlay:theme` event; `Overlay.svelte` re-parameterizes draw colors + line widths live.
- **E.7 Voice input** — 🎤 button in action row + `Alt+A` global hotkey; uses `SpeechRecognition` / `webkitSpeechRecognition` Web Speech API inside WebView2; transcript auto-submitted as task prompt; enabled/disabled via Settings → Audio. Language configurable (9 BCP-47 locales in dropdown).

### ✅ Completed (v0.4 Phase E.8–E.11 — 2026-05-05)

- **E.8 Restore Task Input** — User input is preserved in the text box if the AI call fails (e.g., window minimized), preventing re-typing.
- **E.9 Consent-Driven Full Screen** — Secure, user-granted permission flow for virtual desktop capture across all monitors. AI must explicitly request access; user must explicitly allow. Reverts to active-window mode immediately after use.
- **E.10 Context Awareness** — Focused window Title and Class injected into AI prompts. System prompt Rule 17 added to instruct AI to refuse guesses when the user deviates from the target application.
- **E.11 Error Resilience** — Mid-session capture failures (e.g. window minimized) no longer lock the UI or disable buttons; state reverts to previous phase; aggressive red error messages replaced with soft "system" warning icons.
- **UI terminology** — "Auto-advance" renamed to **Autopilot**; "Overlay" renamed to **Screen Guide**; "Subtitle" renamed to **Live caption** throughout all UI strings.
- **Settings: Show/Hide API key** — `get_settings` now returns actual stored API key (was always empty); Show/Hide uses `{#if}` blocks (distinct DOM elements) to reliably unmask password inputs in WebView2.
- **Settings: model dropdowns** — All provider model fields changed from `<input list="datalist">` to `<select>` so all options are always visible.
- **Settings: Gemini models updated** — `gemini-3.1-pro-preview`, `gemini-3.1-flash-lite-preview`, `gemini-3-flash-preview`, `gemini-2.5-flash`, `gemini-2.5-flash-lite`.
- **fast_model confirmed unused** — `anthropic_fast_model` / `gemini_fast_model` exist in `Config` struct but the Rust router never reads them; removed from Settings UI; reserved for future re-query tiering.
- **Header shows model only** — Title bar `header-provider` span shows the active model ID (e.g., `gemini-2.5-flash`) without the provider prefix.
- **Caption: fit-to-text + transparent** — `drawSubtitle` in `Overlay.svelte` now measures text width and sizes the strip to fit content (not full screen width); opacity reduced from 0.78 → 0.52.
- **Clear → Show overlay toggle** — ✕ Clear in quick menu clears screen guide + caption; button changes to 👁 Show which calls `restore_overlay` (new Tauri command) to re-emit the last stored overlay update.
- **`restore_overlay` Rust command** — `AppState.last_overlay` stores the last non-None `(OverlayKind, bbox, text)` emitted by `execute_step`; `restore_overlay` re-emits from this store.
- **App data dir for settings** — All persistent files moved to `app.path().app_data_dir()` (Windows: `%APPDATA%\com.navisual.app\`). `Config::load()` now accepts `Option<&Path>`; `save_settings` uses `state.env_path` (stored in `AppState`). Session files and usage.json also moved to the same directory. Fixes write-permission failures when installed to `Program Files`.

### 🚧 Next: v0.5 — Server + Monetization

**Known OCR limitation — Task Manager / high-DPI primary.** Windows.Media.Ocr wants ~30 px text for reliable reads; small-font nav items (Task Manager sidebar ~12–14 physical px) are at the reliability floor. The capture self-exclusion fix removes noise (only the target window is OCR'd), but the text pixel size is what it is. A 2× bilinear upscale of captures with width <1280 before OCR is the planned follow-up if Task Manager / other compact-UI apps miss too often in practice.

**Known A11y limitation.** Chromium-based webviews (Outlook PWA, Teams, some Electron apps) lazy-load accessibility trees; virtualised list items (e.g. email subject lines) are often absent from the UIA tree until focused. No clean fix — documented as expected.

### 🚧 Next: v0.3.1 / v0.4

### 📋 Upcoming Milestones

```
v0.3.1 — Remaining v0.3 items (Python, Windows):
  1. Settings window: in-app UI for API provider + key (no more .env editing) ✅ — see [settings.md](docs/settings.md)
  2. PyPI packaging: pip install navisual
  Note: single-screen picker was evaluated and removed — active-window crop makes it redundant.

v0.4 — DONE: Tauri/Rust rewrite (Phases A–E.7 + hardening):
  ✅ Full Rust backend: screen capture (BitBlt), A11y (UIA), OCR (Windows.Media.Ocr),
     AI router (Anthropic + Gemini streaming), TTS (Windows SAPI), hotkeys, overlay
  ✅ Session-level HWND storage — stable window targeting across z-order changes
  ✅ Option A Set-of-Marks grid with axis-label margin strips (font8x8)
  ✅ Consent-driven full-screen capture (E.9) / Virtual desktop restored via BitBlt
  Remaining: signed installer + EV code signing (blocked on server being ready first)

v0.5 — Server + Monetization: see [server-plan.md](docs/server-plan.md)
  S.1 Free trial proxy — Supabase Edge Function + Postgres; anonymous auth on first
       launch (no sign-up required); OpenRouter free Llama Vision; 50 free requests;
       anonymous session upgrades to real account in-place when user pays (S.2)
  S.2 Pay As You Go — Google OAuth upgrade + Stripe coins ($5 min, 1 coin = $0.20);
       Gemini Flash for paid requests; Billing tab in Settings
  S.3 Subscriptions — Stripe Subscription ($20/mo, $50/mo); monthly quota reset;
       Stripe Customer Portal for cancel/upgrade; cap-overflow dialog (5 choices)
  Installer — signed Windows installer + EV code signing once S.1 is deployed
  Infrastructure: Supabase (auth + DB + relay) + Stripe (payments) — 2 parties only

v0.6 — Complex Apps + Nav-Packs:
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
| Correction hotkey | ✓ | Alt+E → re-analysis |
| Session persistence | ✓ | Save/resume sessions |
| Clipboard commands | ✓ | For CLI tasks |
| TTS | ✓ | pyttsx3/Windows SAPI, `ENABLE_TTS=true` |
| Voice input (PTT) | ✓ | SpeechRecognition + Google STT, `ENABLE_VOICE_INPUT=true` |
| Multi-platform | ✗ | Windows only for MVP |

### 📅 Full Roadmap

```
v0.2  DONE — streaming + prompt caching + multi-monitor + model tiering + TTS + voice input
v0.3  DONE — token optimization + active-window crop + UI consolidation (ConsolidatedPanel)
      + checkpoint rework + multi-window A11y + A11y false-match fix
v0.3.1  single screen mode + settings window + PyPI packaging + subtitle persistence
v0.4  DONE — full Tauri/Rust rewrite (Phases A–E.7 + hardening); stable HWND targeting;
      SoM grid; restored full-screen capture (E.9) via consent loop
v0.5  Server + monetization: Supabase (auth + relay) + Stripe; free trial (anonymous
      auth, 50 req) + PAYG coins + subscriptions + signed installer
v0.6  Template matching + Nav-Packs v1 + Blender/SolidWorks + quantized local models
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
    next_step_hotkey: str = "alt+`"
    correction_hotkey: str = "alt+e"
    pause_hotkey: str = "alt+s"
    floating_window_hotkey: str = "alt+q"
    talk_hotkey: str = "alt+a"
    reread_hotkey: str = "alt+r"
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
git clone https://github.com/NavisualGuide/navisual.git
cd navisual

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

**Decision:** MVP ships as `pip install navisual`. Tauri native binary at v0.3 with EV code signing.

**Benefit:** Zero SmartScreen issues. MVP testers are developers who have Python.

### 10. FSL License (Functional Source License)

**Why:** MIT is too permissive (competitors clone freely). GPL prevents closed-source Pro tier. No license = ambiguous rights.

**Decision:** FSL-1.1-Apache-2.0. Source-available, 2-year non-compete, converts to Apache 2.0.

**Benefit:** Code is public (trust, transparency), but commercial rights protected during growth phase.

---

## Links & References

- **Design Document:** [Navisual-Design-Document.md](docs/Navisual-Design-Document.md) (§1–11 detailed specs)
- **GitHub:** [NavisualGuide/navisual](https://github.com/NavisualGuide/navisual)
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

- **Issues:** [GitHub Issues](https://github.com/NavisualGuide/navisual/issues)
- **Discussions:** [GitHub Discussions](https://github.com/NavisualGuide/navisual/discussions)

---

*Last updated: 2026-04-19*
