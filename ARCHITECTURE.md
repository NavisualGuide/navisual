# Architecture

This is a short technical tour of how Navisual works under the hood — enough to get oriented before reading the code. For a higher-level explanation of the product, see [README.md](README.md).

---

## Six-Layer Model

```
┌─ INPUT ────────────────────────────────────────────────┐
│ • Screen capture (BitBlt, active-window crop)          │
│ • Screen change detection (8×8 aHash, on-demand)       │
│ • Chat input + push-to-talk voice (Web Speech API)     │
└────────────────────────────────────────────────────────┘
                          ▼
┌─ CORE ENGINE ──────────────────────────────────────────┐
│ • AI router (Anthropic, Gemini, OpenAI, DeepSeek,      │
│   Qwen, Ollama, Managed)                               │
│ • Session manager (conversation + persistence)         │
│ • Cost tracker (token budgets, daily/monthly caps)     │
│ • Step sequencer (multi-step responses with checkpoints)│
└────────────────────────────────────────────────────────┘
                          ▼
┌─ ELEMENT LOCATOR (LOCAL) ──────────────────────────────┐
│ Core differentiator: AI returns TEXT descriptions;     │
│ local code finds EXACT screen positions.               │
│                                                        │
│ Strategies (in priority):                              │
│ 1. Windows UI Automation (UIA) — primary, <5 ms        │
│ 2. Windows.Media.Ocr — fallback, native resolution     │
│                                                        │
│ Output: tight bbox in screen coords, or "unavailable"  │
└────────────────────────────────────────────────────────┘
                          ▼
┌─ OUTPUT ───────────────────────────────────────────────┐
│ • Overlay (transparent canvas — pointer + caption)     │
│ • TTS (Windows SAPI, STA thread)                       │
│ • Clipboard (paste-target text only)                   │
│ • Chat history (panel UI)                              │
└────────────────────────────────────────────────────────┘
```

---

## The Guidance Loop

```
1. User types a task → frontend invokes guide()
2. Backend captures the active window (BitBlt, JPEG-encoded)
3. AI request: system prompt + screenshot + conversation history
4. AI replies via tool_use with 1–4 steps:
     { instruction, target_text, target_role, target_bbox,
       overlay_type, clipboard, checkpoint, state_summary }
5. Element locator finds target_text on the live screen
     → UIA first; OCR fallback if UIA misses
6. Overlay renders pointer at the locator's bbox + caption
7. TTS speaks the instruction
8. Wait for: screen change (Autopilot), user "Next", or "Wrong" hotkey
9. Loop
```

---

## Element Locator (the differentiator)

The AI cannot reliably estimate pixel coordinates — DPI scaling, window position, dynamic UI, and per-app layout variation make it unreliable. Instead, the AI returns a *text* description of the target element, and Navisual finds the pixels locally.

### Strategy 1 — Windows UI Automation (primary)

Walks the UIA tree of the captured window plus other visible top-level windows. Matches `target_text` against UIA element names using a three-pass approach:

- **Pass 1 & 2:** Anchored regex `(?i)^[\W_]*<target>[\W_]*$` for exact / near-exact name matches.
- **Pass 3:** Manual tree walk with substring matching for cases like `"Increase Font Size (Ctrl+>)"` where Pass 1/2 would fail.

Rejects container roles, off-screen elements, and length outliers (`name_len ≤ 4 × target_len`). Typical lookup: under 5 ms.

### Strategy 2 — Windows.Media.Ocr (fallback)

Used when UIA misses (icon-only buttons, custom-drawn UI, some Electron / Chromium apps). Built-in OS OCR — no model download, ~10–50 ms.

Match cascade:

1. **Exact** — `strip_punct(text).to_lower() == target.to_lower()`
2. **Word-boundary substring** — `\b<target>\b` either direction
3. **Fuzzy** — three tiers at LCS ratios 0.85, 0.75, 0.70

If the first pass with an AI-supplied bbox proximity filter finds no winner, retries without the filter (`nb-*` strategy prefix in the debug trace).

### Capture & PID-Union

The capture rect is the **union of all visible same-PID top-level windows on the target's monitor** — not just the foreground window's frame. This catches modal dialogs and popups that float outside the main window (WeChat's Storage dialog, Word's Find & Replace) which would otherwise be silently cropped and cause hallucinated coordinates.

The panel and overlay windows are blanked from the captured image via software rect-fill, so they never appear in the screenshot sent to the AI.

---

## Why This Design

| Decision | Why |
|---|---|
| AI returns text, local code finds positions | Pixel estimation by LLMs is unreliable; OS APIs are exact and fast |
| Event-driven detection, not polling | Polling wastes API calls during idle; OS events + on-demand aHash respond in <500 ms |
| Multi-step sequences with checkpoints | 2–4× fewer API calls per task |
| `tool_use` / `function_calling` for structured output | API validates schema; malformed responses rejected upstream, not in user-facing handlers |
| Active-window crop by default | ~80% image-token reduction vs full-desktop capture |
| User controls privacy (no heuristic redaction) | Heuristics are unreliable. Pause hotkey, BYOK Ollama, explicit consent for full-screen capture |

---

## Streaming, Caching, and Cost

- **Streaming** — Anthropic + Gemini stream SSE; instruction renders word-by-word.
- **Prompt caching** — Anthropic `cache_control: ephemeral` on system prompt; 90% savings on cached portion.
- **Model tiering** — Each provider has a `model` (initial / user-triggered) and a `fast_model` (screen-change re-queries) field. Reserved for future use.
- **Cost cap** — Daily + monthly token caps with a safety margin; hitting a cap blocks further requests and surfaces a "cap exceeded" error.

---

## Persistence

| File | Path | Purpose |
|---|---|---|
| `.env` | `%LOCALAPPDATA%\com.navisual.app\` | User settings (atomic write on Settings → Save) |
| `sessions/<uuid>.json` | `%LOCALAPPDATA%\com.navisual.app\` | Conversation history + state summary, resumable across launches |
| `usage.json` | `%LOCALAPPDATA%\com.navisual.app\` | Token usage for cost-cap enforcement |
| `supabase_session.json` | `%LOCALAPPDATA%\com.navisual.app\` | Anonymous-auth JWT for the free managed tier |
| `debug/` | `%LOCALAPPDATA%\com.navisual.app\` | Off by default; opt-in via `.env` flags. Cleaned at startup if >7 days old. |
| `locate_log.jsonl` | `%LOCALAPPDATA%\com.navisual.app\` | Off by default; same cleanup rule |

Screenshots themselves are **not persisted** — chat thumbnails and the lightbox image live in process memory only.

---

## Frontend ↔ Backend

- **Panel window** (`src/App.svelte`) — Vite + Svelte 5 (runes). Chat, settings, screen-change polling.
- **Overlay window** (`src/Overlay.svelte`) — Transparent always-on-top canvas. Receives `overlay:update` events from Rust and renders pointer / caption.
- **Tauri commands** wire frontend → Rust: `guide`, `next_step`, `send_correction`, `get_settings`, `save_settings`, `get_chat_full_screenshot`, `list_target_windows`, `pin_target_window`, etc.
- **Tauri events** push state from Rust → frontend: `stream_chunk` (streaming instruction text), `balance_update`, `trial_exhausted`, `overlay:update`, `screen_changed`.

---

## Key Files

```
src-tauri/src/
├── lib.rs                    Tauri command surface + guidance loop
├── ai/
│   ├── router.rs             Provider selection
│   ├── anthropic.rs, gemini.rs, openai.rs, deepseek.rs,
│   │   qwen.rs, ollama.rs, managed.rs
│   ├── session.rs            Persistent sessions
│   ├── cost_tracker.rs       Token caps
│   ├── bbox.rs               AI-bbox → screen-coord translation
│   └── prompts.rs            System prompt + correction prompt
├── capture/
│   ├── mod.rs                Capture API surface
│   └── win.rs                Windows BitBlt + PID-union + DWM frame
├── locator/
│   ├── mod.rs                Orchestrator (A11y → OCR)
│   ├── a11y.rs               UIA matcher (3-pass)
│   ├── ocr.rs                Windows.Media.Ocr + fuzzy cascade
│   ├── hit_test.rs           WindowFromPoint denylist
│   └── trace.rs              Debug trace serialization
├── overlay.rs                Transparent overlay window plumbing
├── tts.rs                    SAPI STA thread + voice enumeration
├── track.rs                  HWND focus / move / resize tracking
└── server.rs                 Supabase anonymous-auth client

src/
├── App.svelte                Panel UI
└── Overlay.svelte            Transparent canvas (pointer + caption)
```
