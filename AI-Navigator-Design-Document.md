# AI Navigator — Software Design Document

**Version:** 0.7
**Date:** 2026-04-08
**Slogan:** *The AI guides, never overrides.*

---

## 1. Overview

AI Navigator is a cross-platform desktop application that acts as a real-time, AI-powered guidance system for computer tasks. Unlike AI agents that take control, AI Navigator observes the user's screen and delivers step-by-step navigation instructions via audio narration and on-screen overlays. The user always remains in control — every click, keystroke, and decision is theirs.

### 1.1 Core Principles

| Principle | Description |
|-----------|-------------|
| **Observe, never act** | AI Navigator reads the screen but never moves the mouse, types, or executes commands. |
| **Guide in real-time** | Instructions adapt to what is currently on screen, not a pre-recorded script. |
| **Human-in-the-loop** | The user decides when to proceed, deviate, or ask for clarification. |
| **Cost-aware** | Aggressive local summarization + API caching to minimize token spend. |
| **Privacy-first** | Screenshots are processed and discarded; only text summaries persist. User controls what is captured. |
| **Graceful degradation** | If overlay positioning fails, fall back to text/audio descriptions. Never show a broken arrow. |

### 1.2 Target Use Cases

**MVP (v0.1):** Browser-based tasks only.
- Online shopping, form-filling, tax filing
- Web-based admin panels, SaaS tools

**Post-MVP (v0.2+):**
- 3D modeling in Blender, SolidWorks, Fusion 360
- CLI / terminal workflows (git, docker, system admin)
- Learning new software (Photoshop, Excel, CAD tools)
- Enterprise internal-tool onboarding

**Accessibility applications (v0.3+):**
- Assistive guidance for users with cognitive disabilities or low tech-literacy
- Software onboarding for aging populations
- Potential for grants, government contracts, and partnerships with accessibility-focused organizations

> **Design note:** AI Navigator's core loop — observe screen, describe what to do, point where to click — is structurally identical to an intelligent screen reader. Accessibility should be treated as a first-class use case, not an afterthought. UI text sizes, TTS quality, and instruction clarity all benefit from this framing.

---

## 2. System Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        AI Navigator                          │
│                                                              │
│  ┌────────────┐  ┌────────────────┐  ┌─────────────────────┐ │
│  │  Input      │  │  Core Engine   │  │  Output Layer       │ │
│  │  Layer      │──│                │──│                     │ │
│  │            │  │                │  │  • Overlay renderer  │ │
│  │ • Screen   │  │ • Session mgr  │  │  • TTS engine        │ │
│  │   capture  │  │ • State        │  │  • Clipboard mgr     │ │
│  │ • Screen   │  │   summarizer   │  │  • Chat window       │ │
│  │   change   │  │ • API router   │  │                     │ │
│  │   detector │  │ • Cost ctrl    │  │                     │ │
│  │ • Voice    │  │ • Correction   │  │                     │ │
│  │   input    │  │   handler      │  │                     │ │
│  │ • Chat     │  │                │  │                     │ │
│  │   window   │  │                │  │                     │ │
│  └────────────┘  └────────────────┘  └─────────────────────┘ │
│                         │                                     │
│         ┌───────────────┼───────────────┐                     │
│         │               │               │                     │
│  ┌──────┴───────┐ ┌─────┴──────┐ ┌──────┴──────┐             │
│  │ Platform     │ │ Local OCR  │ │ Element     │             │
│  │ Layer        │ │ Engine     │ │ Locator     │             │
│  │ (OS-specific)│ │(EasyOCR /  │ │(OCR + A11y  │             │
│  │ • Screen cap │ │ Tesseract) │ │ + template) │             │
│  │ • Audio out  │ └────────────┘ └─────────────┘             │
│  │ • Overlay    │                                             │
│  │ • Hotkeys    │                                             │
│  │ • Clipboard  │                                             │
│  │ • A11y APIs  │                                             │
│  └──────────────┘                                             │
└──────────────────────────────────────────────────────────────┘
                        │
                        ▼
            ┌───────────────────────┐
            │   AI Backend(s)       │
            │  • Anthropic API      │
            │  • OpenAI API         │
            │  • Local model (Ollama│
            │    / llama.cpp)       │
            └───────────────────────┘
```

### 2.1 Component Breakdown

#### Input Layer

| Component | Responsibility | Platform Notes |
|-----------|---------------|----------------|
| **Screen Capture** | On-demand screenshots triggered by the screen change detector | Win: DXGI / Win32 `BitBlt`; Mac: `CGWindowListCreateImage`; Linux: PipeWire / X11 `XGetImage` |
| **Screen Change Detector** | Event-driven monitoring: OS accessibility events (window focus, UI changes), fast local pixel-diff (~10fps), and user-action signals (hotkey press). Triggers API calls only when meaningful changes occur. Replaces fixed-interval polling. | See §3.1 |
| **Voice Input** | Continuous or push-to-talk mic capture → speech-to-text | Local: Whisper.cpp; Cloud: Whisper API / Deepgram |
| **Chat Window** | Floating, hotkey-activated text input for typed prompts | Cross-platform UI framework (see §2.3) |

#### Core Engine

| Component | Responsibility |
|-----------|---------------|
| **Session Manager** | Manages the lifecycle of a guidance session: init → active → paused → done. Holds conversation history. Supports **session persistence** — can save and restore sessions (see §3.4). |
| **State Summarizer** | After each AI response, extracts a compact text summary of current application state (the "hidden state"). Replaces stale screenshots with text to reduce token cost. |
| **API Router** | Selects the AI backend (Anthropic / OpenAI / local), manages API keys, applies prompt caching headers, handles retries. Uses **structured output** (tool_use / function calling) instead of raw JSON prompting (see §6). |
| **Cost Controller** | Tracks token usage per session and per billing period. Enforces caps. Decides when to use cached context vs. fresh image. Applies a **2–3x safety margin** to cost estimates during early versions to account for imperfect optimization. |
| **Correction Handler** | Processes "wrong instruction" signals from the user (hotkey or voice). Triggers a re-analysis with additional context: "The user reports the previous instruction was incorrect." (see §3.5) |

#### Output Layer

| Component | Responsibility | Platform Notes |
|-----------|---------------|----------------|
| **Overlay Renderer** | Draws arrows, highlights, bounding boxes, and subtitle-style text on top of all windows. Positions are determined by the **Element Locator** (local OCR/A11y), not by AI-estimated coordinates. Falls back to subtitle-only if element cannot be located. | Win: layered `HWND` with `WS_EX_TRANSPARENT`; Mac: `NSPanel` with `NSWindowLevelFloating`; Linux: X11 override-redirect / Wayland layer-shell |
| **TTS Engine** | Converts instruction text to speech | Local: Piper TTS / system TTS; Cloud: OpenAI TTS / ElevenLabs |
| **Clipboard Manager** | For CLI/text-editor tasks: places generated commands/text into the system clipboard and notifies the user | Native clipboard APIs |
| **Chat Window (output)** | Shows conversation history, choices, and clarification prompts | Same widget as input |

#### Element Locator (critical component)

The AI never estimates pixel coordinates. Instead, it returns **text descriptions** of UI elements. The Element Locator finds exact screen positions locally.

```
┌─────────────────┐         ┌──────────────────────────────────────┐
│   AI (cloud)     │         │   Element Locator (runs locally)     │
│                  │         │                                      │
│  INPUT:          │         │  INPUT:                              │
│  - screenshot    │         │  - target_text from AI               │
│  - context       │         │  - target_role (optional: "button",  │
│                  │         │    "tab", "link", "textbox")         │
│  OUTPUT:         │         │  - region_hint from AI               │
│  - text: "Click  │         │                                      │
│    the Modeling  │────────▶│  STRATEGY (in priority):             │
│    tab at the    │         │                                      │
│    top of the    │         │  ┌─ 1. Accessibility API (< 5ms) ──┐│
│    screen"       │         │  │ UIA / AX / AT-SPI2              ││
│                  │         │  │ Query widget tree by name+role   ││
│  - target_role:  │         │  │ → Returns exact bbox instantly   ││
│    "tab"         │         │  └──────────────┬───────────────────┘│
│                  │         │        found? ──┤                    │
│  (NO pixel       │         │     yes ↓       │ no ↓               │
│   coordinates)   │         │   USE BBOX   ┌──┴──────────────────┐│
│                  │         │              │ 2. OCR fallback      ││
│                  │         │              │    (50-150ms)        ││
│                  │         │              │ PaddleOCR on screen  ││
│                  │         │              │ Match target_text    ││
│                  │         │              └──────────┬───────────┘│
│                  │         │              found? ────┤            │
│                  │         │           yes ↓         │ no ↓       │
│                  │         │         USE BBOX    SUBTITLE ONLY   │
└─────────────────┘         └──────────────────────────────────────┘
```

**Detection strategies, in strict priority order:**

| Priority | Strategy | Latency | How | Best for | Limitation |
|----------|----------|---------|-----|----------|------------|
| **1 (primary)** | **OS Accessibility API** | **< 5ms** | Win: UI Automation (UIA) via `uiautomation` or `comtypes`. Queries the foreground app's widget tree for element name, role, and bounding box. | Browsers (excellent trees), Qt/GTK apps, Office apps | Not all apps expose good trees (Blender, games, Electron with poor a11y) |
| **2 (fallback)** | **Local OCR** | 10–150ms | OCR on the screenshot. Find all text + bounding boxes. Match AI's `target_text` against results. Runs in a **separate process** (see §2.4). **On Windows**, uses `Windows.Media.Ocr` (built-in, ~10ms, no model downloads). **On macOS/Linux (future)**, falls back to PaddleOCR (~50-150ms on CPU). | Any app with visible text labels, when A11y tree is unavailable or sparse | Can't find icon-only buttons |
| **3 (future, v0.3)** | **Template Matching** | ~50ms | OpenCV `matchTemplate` against a library of known UI icons/widgets. | Toolbar icons, non-text elements | Requires pre-built icon library per app; fragile across themes |

**Why Accessibility API is primary for MVP:**
- The MVP targets **browser tasks only**. Chrome, Firefox, and Edge expose some of the richest accessibility trees of any application class.
- UIA returns element name, role (`button`, `tab`, `link`, `textbox`), bounding box, and state (`enabled`, `focused`) — all in **< 5ms**.
- This eliminates the entire OCR latency budget from the critical path for browser tasks.
- The AI can also return `target_role` (e.g., "tab", "button") which makes the UIA query more precise.

**When does OCR kick in?**
- The A11y tree returns no match (element name doesn't match `target_text`)
- The A11y tree is empty or unavailable (e.g., app doesn't support UIA)
- The matched A11y element has no bounding box (rare, but possible)
- OCR runs in a separate process in parallel with the API call (§2.4), so when A11y fails, OCR results are already cached — no additional wait.

**Graceful degradation:**
- A11y finds the target (< 5ms) → draw overlay arrow at exact position. **This is the normal path for browser tasks.**
- A11y fails, OCR finds the target (already cached) → draw overlay arrow at OCR position.
- Both fail → show subtitle-only instruction: *"I can't pinpoint the button — look for 'Modeling' near the top of the Blender window."*
- The user never sees an arrow pointing at empty space.

**Window movement is handled naturally:** Both UIA and OCR query the *current* state — UIA queries the live widget tree (positions update instantly when windows move), OCR scans the live screenshot. Neither depends on the screenshot the AI analyzed.

#### Platform Layer

A thin abstraction (trait/interface) per OS capability:

```
trait ScreenCapture    { fn capture_full() -> Image; fn capture_region(rect) -> Image; }
trait ScreenMonitor    { fn on_change(callback); fn on_focus_change(callback); }
trait OverlayWindow    { fn show(elements: Vec<OverlayElement>); fn hide(); }
trait GlobalHotkey     { fn register(combo, callback); fn unregister(combo); }
trait ClipboardAccess  { fn set_text(s: &str); fn get_text() -> String; }
trait AudioOutput      { fn speak(text: &str, voice: VoiceConfig); fn stop(); }
trait AccessibilityAPI { fn find_element(name: &str, role: Option<&str>) -> Option<UIElement>; }
```

### 2.2 Technology Choices — Production Target (v1.0)

| Layer | Technology | Rationale |
|-------|-----------|-----------|
| **Application language** | Rust | Cross-platform, safe, fast, good FFI. Single binary distribution. No SmartScreen issues with native code. |
| **UI framework** | Tauri v2 (Rust + web frontend) | Native shell with web UI for the chat panel. Access to system APIs via Rust backend. Small binary (~5 MB). |
| **Frontend (chat/settings)** | Svelte or React (lightweight) | Rendered inside Tauri webview. |
| **Overlay rendering** | Native OS APIs called from Rust | Tauri doesn't support transparent overlays well — use raw platform calls via `winit` / `raw-window-handle` or a separate lightweight overlay process. |
| **Screen capture** | `scrap` crate (Rust) or `xcap` | Cross-platform, GPU-accelerated on Windows. |
| **Local OCR** | PaddleOCR via FFI or Tesseract | PaddleOCR: ~50-150ms on CPU vs EasyOCR's 200-500ms. Lighter dependency than PyTorch. |
| **Local STT** | `whisper.cpp` via FFI | Runs on CPU/GPU, no cloud dependency. |
| **Local TTS** | `piper-rs` or system TTS | Offline capable. |
| **AI SDK** | HTTP client (`reqwest`) | Direct REST calls to Anthropic/OpenAI. Thin wrapper, no heavy SDK dependency. Uses tool_use / function calling for structured output. |

### 2.3 Technology Choices — MVP (v0.1, Python)

The MVP is built in Python for prototyping speed. Key considerations:

- **Python + Qt/PySide6**: Rich library ecosystem, natural fit for PaddleOCR (also Python).
- **Tauri/Rust migration at v0.3** (not v1.0) to solve the distribution/SmartScreen problem before public launch.
- **CPU-heavy work in separate processes** to avoid GIL contention with Qt UI (see §2.4).

### 2.4 Concurrency Architecture (GIL Mitigation)

Python's Global Interpreter Lock (GIL) prevents true multithreading for CPU-bound work. Running 10fps pixel-diffing and OCR on the same thread as PySide6's Qt event loop will cause the UI to stutter or freeze. The solution is a **multi-process architecture**:

```
┌──────────────────────────────────────────────────────────┐
│  MAIN PROCESS (Qt event loop)                             │
│                                                          │
│  • PySide6 UI rendering (chat window, overlay)           │
│  • asyncio event loop for I/O (API calls via httpx)      │
│  • Receives results from worker processes via Queues     │
│  • Never does CPU-heavy work                             │
└─────────────┬────────────────────────────────────────────┘
              │  multiprocessing.Queue (results)
              │
    ┌─────────┴──────────┬───────────────────────┐
    │                    │                       │
┌───┴──────────┐  ┌──────┴──────────┐  ┌─────────┴────────┐
│ Screen       │  │ OCR Worker      │  │ Diff Worker       │
│ Capture      │  │ (Process)       │  │ (Process)         │
│ Worker       │  │                 │  │                   │
│ (Process)    │  │ • Win: WinOCR   │  │ • 10fps low-res   │
│              │  │   (~10ms/frame) │  │   pixel-diff      │
│ • mss screen │  │ • Other: Paddle │  │ • pHash compare   │
│   capture    │  │   (~50-150ms)   │  │ • Emits "changed" │
│ • On-demand  │  │ • Text → bbox   │  │   events to main  │
│              │  │   mapping       │  │   process          │
└──────────────┘  └─────────────────┘  └──────────────────┘
```

**Key rules:**
1. **Main process**: Only Qt UI + asyncio I/O. No CPU work.
2. **Diff Worker**: Runs in a separate `multiprocessing.Process`. Captures low-res thumbnails at 10fps, compares with pHash. Sends "screen changed" events to main process via `multiprocessing.Queue`.
3. **OCR Worker**: Runs in a separate `multiprocessing.Process`. Receives full screenshots, returns text + bounding box results. Runs in **parallel with the API call** — by the time the API returns `target_text`, OCR results are already cached.
4. **Communication**: All inter-process communication via `multiprocessing.Queue` (non-blocking on the main thread via `QTimer` polling or `asyncio` integration).

```python
# Pseudocode: A11y is instant; OCR pre-caches in parallel as fallback
async def guidance_turn(screenshot):
    # Start OCR pre-indexing and API call concurrently
    ocr_future = ocr_worker.submit(screenshot)     # Separate process, ~50-150ms
    api_future = call_anthropic_api(screenshot)     # Async I/O, ~1.2-2.0s

    # API returns target_text + target_role
    api_response = await api_future

    # Step 1: Try Accessibility API FIRST (< 5ms, on main process — it's I/O, not CPU)
    bbox = a11y_engine.find(
        name=api_response.target_text,
        role=api_response.target_role   # e.g., "button", "tab"
    )

    # Step 2: If A11y failed, use pre-cached OCR results (already done by now)
    if bbox is None:
        ocr_results = await ocr_future  # Already finished during API wait
        bbox = ocr_results.find(api_response.target_text)

    # Step 3: Render overlay or fall back to subtitle
    if bbox:
        render_overlay(bbox)
    else:
        render_subtitle(api_response.instruction)
```

This pattern ensures:
- **Browser tasks (MVP)**: A11y finds the element in < 5ms after API returns. OCR never needed.
- **Complex apps (v0.3+)**: If A11y tree is sparse, OCR results are already cached from the parallel pre-index.
- UI never freezes (GIL never blocked by CPU work — A11y queries are I/O, not CPU)
- 10fps pixel-diff runs smoothly in its own process

---

## 3. Data Flow

### 3.1 Main Guidance Loop (Event-Driven)

The guidance loop is **event-driven**, not polling-based. API calls are triggered by meaningful screen changes or user input — not on a fixed timer.

```
 User types prompt: "Help me buy a USB-C cable on Amazon"
          │
          ▼
 ┌──────────────────┐
 │ 1. Capture        │ ← Screenshot of current screen
 │    screenshot      │
 └────────┬───────────┘
          │
          ▼
 ┌──────────────────┐
 │ 2. Build API      │ ← System prompt + user prompt
 │    payload         │   + screenshot (new)
 │                    │   + cached_state_summary (text from prior turn)
 │                    │   + (optional) cached image reference
 └────────┬───────────┘
          │
          ▼
 ┌──────────────────────────────────────────────────────────────┐
 │ 3. In PARALLEL (see §2.4):                                   │
 │                                                              │
 │  ┌─ API Call (async I/O) ────────────────────────────────┐   │
 │  │ Call AI API (tool_use / function calling)              │   │
 │  │ → Sends to Anthropic / OpenAI / local                 │   │
 │  │                                                       │   │
 │  │ AI responds via structured tool call:                  │   │
 │  │                                                       │   │
 │  │ navigate_step(                                        │   │
 │  │   steps = [                                           │   │
 │  │     {                                                 │   │
 │  │       instruction: "Click the search bar...",         │   │
 │  │       target_text: "Search Amazon",                   │   │
 │  │       target_region: "top-center",                    │   │
 │  │       overlay_type: "highlight",                      │   │
 │  │       checkpoint: false                               │   │
 │  │     },                                                │   │
 │  │     {                                                 │   │
 │  │       instruction: "Type 'USB-C cable'...",           │   │
 │  │       target_text: null,                              │   │
 │  │       clipboard: "USB-C cable",                       │   │
 │  │       checkpoint: true                                │   │
 │  │     }                                                 │   │
 │  │   ],                                                  │   │
 │  │   state_summary: "Amazon.com open. Homepage...",      │   │
 │  │   needs_input: false                                  │   │
 │  │ )                                                     │   │
 │  └───────────────────────────────────────────────────────┘   │
 │                                                              │
 │  ┌─ OCR Pre-index (separate process, ~50-150ms) ─────────┐  │
 │  │ PaddleOCR scans ENTIRE screenshot immediately.         │  │
 │  │ Caches ALL text + bounding boxes found on screen.      │  │
 │  │ Finishes BEFORE the API call returns.                  │  │
 │  │ → Result: {"Search Amazon": bbox(612,42,798,68), ...}  │  │
 │  └───────────────────────────────────────────────────────┘  │
 │                                                              │
 │  Both finish → instant lookup: match target_text to cached   │
 │  OCR results. No sequential OCR wait after API response.     │
 └────────┬─────────────────────────────────────────────────────┘
          │
          ▼
 ┌──────────────────────────────────────────────────────────────┐
 │ 4. Locate element + Render output                            │
 │                                                              │
 │  • Stream subtitle as API response arrives                   │
 │    → User sees text at ~0.8s (via API streaming)             │
 │                                                              │
 │  • Once full response received:                              │
 │    a) Try Accessibility API (< 5ms, instant for browsers)    │
 │       → Found? Draw overlay arrow at exact bbox              │
 │    b) A11y miss? Use pre-cached OCR results (already done)   │
 │       → Found? Draw overlay arrow at OCR bbox                │
 │    c) Both miss? Keep subtitle only:                         │
 │       "Look for 'Search Amazon' at the top of the page"      │
 │                                                              │
 │  • Overlay arrow appears at ~1.5-2s (API is the bottleneck,  │
 │    not the element locator)                                  │
 │  • (if CLI) copy to clipboard                                │
 └────────┬─────────────────────────────────────────────────────┘
          │
          ▼
 ┌──────────────────────────────────────────────────────────┐
 │ 6. Advance through step sequence locally                  │
 │                                                          │
 │  Screen change detected:                                 │
 │   a) At checkpoint step → advance + re-query if done     │
 │   b) Mid-sequence (non-checkpoint) → auto-advance to     │
 │      next step (no API call). Screen change is treated   │
 │      as implicit user confirmation.                      │
 │   c) Sequence complete + screen changed → re-query AI    │
 │      (debounced: only once per 5 seconds to avoid noise) │
 │                                                          │
 │  User voice/chat/hotkey input → always triggers loop     │
 │  User types while AI is processing → message queued,     │
 │  sent automatically when current response finishes       │
 └────────┬─────────────────────────────────────────────────┘
          │  (screen changed, user spoke, or sequence complete)
          ▼
        Loop back to step 1
```

### 3.2 Screen Change Detection (Event-Driven)

Instead of polling every 2 seconds, the system uses layered event detection:

```
┌─────────────────────────────────────────────────────┐
│              Screen Change Detector                  │
│                                                      │
│  Layer 1: OS Events (instant, free)                  │
│  • Window focus change                               │
│  • Window resize / move                              │
│  • Accessibility tree mutation events                │
│  → Trigger: immediate screenshot + possible API call │
│                                                      │
│  Layer 2: Fast Local Pixel-Diff (~10fps, cheap)      │
│  • Capture low-res thumbnail (160x90)                │
│  • Compare with previous thumbnail                   │
│  • If > 5% pixels changed → trigger full screenshot  │
│  → Cost: ~1ms per check, no API involved             │
│                                                      │
│  Layer 3: User Action Signal (instant)               │
│  • User presses "next step" hotkey                   │
│  • User sends chat/voice message                     │
│  • User presses "wrong" correction hotkey            │
│  → Trigger: immediate screenshot + API call          │
│                                                      │
│  Layer 4: Idle Fallback (slow, safety net)           │
│  • If no events for 10 seconds during active session │
│  • Take a screenshot and check via pHash             │
│  • Catches changes that OS events missed             │
│  → Fallback only, not the primary mechanism          │
└─────────────────────────────────────────────────────┘
```

**Why this matters for cost and UX:**
- No wasted API calls during idle periods (user reading, thinking)
- Instant response when user completes a step (< 500ms to detect change)
- The 10fps pixel-diff is a local operation — no API cost

### 3.3 State Management

```
Session {
    id: UUID,
    task_description: String,         // "Buy a USB-C cable on Amazon"
    conversation: Vec<Turn>,          // Full chat history (text only)
    current_state_summary: String,    // "Amazon > search results > page 1 > no item selected"
    current_step_sequence: Vec<Step>, // Multi-step sequence from last AI response
    current_step_index: int,          // Which step the user is on
    cached_image_id: Option<String>,  // API-side cache reference
    token_usage: TokenCounter,
    started_at: Timestamp,
    last_active_at: Timestamp,        // For session persistence
}

Turn {
    role: User | Assistant | Correction,  // Correction = "wrong" signal
    content: String,
    screenshot_hash: Option<String>,
    timestamp: Timestamp,
}

Step {
    instruction: String,              // What to show/speak to user
    target_text: Option<String>,      // For Element Locator to find
    target_region: Option<String>,    // Rough area hint: "top-left", "center", etc.
    overlay_type: String,             // "arrow", "highlight", "circle", "none"
    clipboard: Option<String>,        // Text to copy to clipboard
    checkpoint: bool,                 // If true, wait for screen change before advancing
}
```

### 3.4 Session Persistence & Resume

Sessions can be saved to disk and resumed later (e.g., after a crash, app restart, or next day).

```
Saved session file (~/.ai-navigator/sessions/{id}.json):
{
    "id": "abc-123",
    "task_description": "File 2025 tax return on TurboTax",
    "state_summary": "TurboTax > W-2 entry > employer #1 done, employer #2 not started",
    "conversation_history": [...],   // Text only, no images
    "token_usage": { "input": 12500, "output": 3200 },
    "last_active_at": "2026-04-05T14:30:00Z"
}
```

On resume:
1. Load saved session.
2. Capture fresh screenshot.
3. Send to AI with: *"Resuming session. Last known state: {state_summary}. Here is the current screen. Assess whether the state is still valid and provide the next instruction."*

This costs one API call to re-sync. No old screenshots needed.

### 3.5 User Correction Flow

When the AI gives a wrong instruction, the user needs a fast way to signal this without typing a paragraph.

```
User presses correction hotkey (default: Ctrl+Shift+X)
    │
    ▼
┌──────────────────────────────────────────────────┐
│  Correction Handler                               │
│                                                   │
│  1. Capture fresh screenshot immediately          │
│  2. Add correction context to API call:           │
│     "The user pressed the 'wrong' button,         │
│      indicating the previous instruction was       │
│      incorrect or they cannot find the element.    │
│      Analyze the current screen carefully and      │
│      provide a corrected instruction. Describe     │
│      the target element differently."              │
│  3. Send to API (fresh screenshot + correction     │
│     context + state summary)                       │
│  4. New response replaces the current step         │
│     sequence                                       │
└──────────────────────────────────────────────────┘
```

The correction hotkey is also available as a button in the floating window for mouse users.

**Why this is critical:** Without a correction mechanism, users who receive a wrong instruction have to type "that's wrong, I don't see that button" — which is slow and breaks flow. A single hotkey press gets them back on track in ~2 seconds.

### 3.6 Screenshot Deduplication

Before sending a screenshot to the API:
1. Compute a perceptual hash (pHash) of the new screenshot.
2. Compare with the previous screenshot's hash.
3. If similarity > 95%, skip the API call (nothing changed).
4. This works alongside event-driven detection as a safety net to avoid duplicate API calls when OS events fire but the screen content hasn't meaningfully changed.

---

## 4. API & Token Management

### 4.1 Cost Model

Costs below are based on April 2026 public pricing (approximate).

| Operation | Anthropic (Claude Sonnet 4) | OpenAI (GPT-4o) |
|-----------|-----------------------------|------------------|
| Input text (per 1M tokens) | $3.00 | $2.50 |
| Output text (per 1M tokens) | $15.00 | $10.00 |
| Image input (per image ~1000x700) | ~800 tokens ≈ $0.0024 | ~765 tokens ≈ $0.0019 |
| Cached input (per 1M tokens) | $0.30 (90% discount) | $1.25 (50% discount) |

#### Per-Interaction Cost Estimate

| Scenario | Tokens In | Tokens Out | Cost |
|----------|-----------|------------|------|
| **First turn** (system prompt + image + user prompt) | ~2,500 | ~500 (multi-step) | ~$0.015 |
| **Follow-up turn** (cached system + state summary + new image) | ~1,500 (500 cached) | ~400 | ~$0.009 |
| **Text-only follow-up** (cached system + state summary, no image) | ~800 (500 cached) | ~200 | ~$0.004 |
| **Correction turn** (fresh image + correction context) | ~2,000 | ~400 | ~$0.012 |

> **Early-stage safety margin:** The estimates above assume optimizations (dedup, caching, summarization) work perfectly. During v0.1–v0.3, apply a **2–3x multiplier** to all cost projections. Budget for $0.03/turn average, not $0.01. As optimizations mature, actual costs will converge toward the ideal estimates.

#### Session Cost Estimate (with safety margin applied)

| Task Complexity | Turns | Ideal Cost | Budgeted Cost (2.5x) |
|----------------|-------|------------|----------------------|
| Simple (5-turn browser task) | 5 | $0.04 – $0.06 | $0.10 – $0.15 |
| Medium (15-turn guided workflow) | 15 | $0.10 – $0.15 | $0.25 – $0.40 |
| Complex (40-turn tax filing) | 40 | $0.25 – $0.40 | $0.60 – $1.00 |

> **Note on multi-step sequences (§3.1):** Because the AI can return 2–4 steps per turn, a "15-turn session" in the new design may only need 8–10 actual API calls. This offsets the safety margin.

#### Monthly Cost Per Active User (estimated, with safety margin)

| Usage Tier | Sessions/month | Avg API calls | Monthly API cost (budgeted) |
|-----------|---------------|---------------|----------------------------|
| Light | 20 | ~80 | $3 – $5 |
| Moderate | 60 | ~300 | $10 – $18 |
| Heavy | 150 | ~700 | $25 – $45 |

### 4.2 Cost Optimization Strategies

```
Priority  Strategy                         Savings         In MVP?
───────── ──────────────────────────────── ──────────────  ───────
1         Event-driven capture (not poll)  Eliminates idle calls   Yes
2         Screenshot dedup (pHash)         50-70% fewer API calls  Yes
3         Text state summaries             Replace old images      Yes
4         Multi-step sequences             2-4x fewer API calls    Yes
5         API prompt caching               90% cheaper on system prompt  v0.2
6         Model tiering                    Use Haiku for change detection  v0.2
7         Local model fallback             $0 for capable GPUs     v0.3
```

### 4.3 Token Budget System

```python
class TokenBudget:
    daily_cap: int          # e.g., 100,000 tokens/day for free tier
    monthly_cap: int        # e.g., 5,000,000 tokens/month for Pro
    current_daily: int
    current_monthly: int
    safety_margin: float    # 2.5 during v0.x, reduce toward 1.0 as optimizations mature

    def can_spend(self, estimated_tokens: int) -> bool:
        adjusted = int(estimated_tokens * self.safety_margin)
        return (self.current_daily + adjusted <= self.daily_cap and
                self.current_monthly + adjusted <= self.monthly_cap)

    def on_limit_approaching(self, threshold=0.8):
        # Warn user at 80% usage
        # Suggest switching to local model if available
        pass

    def on_limit_reached(self):
        # Options: pause, switch to local model, warn user
        # Never silently fail
        pass
```

### 4.4 Multi-Provider Strategy

Four providers are supported as of v0.1.1. Provider is selected via `API_PROVIDER` in `.env`.

| Provider | Key Required | Free Tier | Vision | Structured Output | Best For |
|----------|-------------|-----------|--------|-------------------|----------|
| **anthropic** | `ANTHROPIC_API_KEY` | No | Yes | `tool_use` (native) | Highest guidance quality |
| **gemini** | `GEMINI_API_KEY` | Yes (~1,500 req/day) | Yes | Function calling (native) | New users — zero cost to start |
| **ollama** | None | Yes (local) | Yes (llama3.2-vision) | JSON mode + schema prompt | Privacy-first, offline use |
| **openai** | `OPENAI_API_KEY` | No | Yes | `function_calling` (v0.2) | Alternative cloud provider |

```
┌─────────────────────────────────────────────────────┐
│                    API Router                        │
│                                                      │
│  provider = config.API_PROVIDER                      │
│                                                      │
│  "anthropic" → AnthropicClient (tool_use)            │
│  "gemini"    → GeminiClient (function calling)       │
│               Free tier via Google AI Studio         │
│               ~1,500 req/day, no credit card         │
│  "ollama"    → OllamaClient (JSON mode)              │
│               Local inference, no API cost           │
│               Requires: ollama serve + vision model  │
│  "openai"    → OpenAIClient (function_calling, v0.2) │
│                                                      │
│  Budget check skipped for Ollama (free/local).       │
│  All cloud providers: pre-flight can_spend() check.  │
└─────────────────────────────────────────────────────┘
```

**Recommended onboarding path for new users:**
1. Start with Gemini free tier (`API_PROVIDER=gemini`, free key from Google AI Studio)
2. Upgrade to Anthropic Sonnet for higher quality when ready to pay
3. Switch to Ollama for privacy-sensitive tasks at any time

**Anthropic model selection** (set via `ANTHROPIC_MODEL`):

| Model | Speed | Cost | Quality | Use case |
|-------|-------|------|---------|----------|
| `claude-haiku-4-5-20251001` | Fastest | ~20× cheaper than Sonnet | Good | Development/testing, simple tasks |
| `claude-sonnet-4-6` | Balanced | — | High | Default — production guidance |
| `claude-opus-4-6` | Slower | ~5× more than Sonnet | Highest | Complex multi-app workflows |

---

## 5. Privacy & Security

### 5.1 Data Handling

| Data | Stored Locally? | Sent to Cloud? | Retention |
|------|----------------|----------------|-----------|
| Screenshots | In-memory only (RAM) | Current frame only, to API | Discarded after AI response |
| State summaries | In-memory (session); optionally persisted to disk for resume | Yes, as text context | Session end or user deletes |
| Conversation text | Local log (opt-in) | Yes, to API | User-controlled |
| API keys | Local encrypted store | Never | Until user deletes |
| Voice audio | In-memory buffer | To STT service (if cloud) | Discarded after transcription |
| Session files | Local disk (opt-in) | Never | User-controlled |

### 5.2 Security Measures

- **No screenshot persistence**: Images never touch disk unless user explicitly enables session recording.
- **Local-first option**: Voice + vision can run entirely offline (Whisper.cpp + local LLM) for air-gapped / sensitive environments.
- **API key encryption**: Stored using OS keychain (Windows Credential Vault / macOS Keychain / libsecret on Linux).
- **Overlay isolation**: The overlay window is input-transparent — it cannot intercept clicks or keystrokes.

### 5.3 User-Controlled Privacy (replaces "sensitive screen detection")

Instead of unreliable heuristic detection of sensitive screens, the user has explicit control:

| Control | Description | Default |
|---------|-------------|---------|
| **Pause hotkey** | `Ctrl+Shift+P` — instantly stops all screen capture. Overlay shows "Paused" indicator. | Always available |
| **Resume hotkey** | Same hotkey toggles resume. | — |
| **App blocklist** | User can list apps where capture is never active (e.g., "1Password", "KeePass"). Checked against foreground window title. | Empty |
| **URL blocklist** | For browser tasks: user can list URL patterns where capture pauses (e.g., `*.bank.com`, `*/login*`). Checked via browser tab title or accessibility API. | Empty |
| **Capture indicator** | A small persistent icon (like a camera LED) shows when screen capture is active. User always knows. | On |

> **Why not auto-detect?** Heuristic detection of "sensitive" screens is unreliable. A false negative (failing to detect a banking site) creates liability. A false positive (pausing during normal use) is annoying. Explicit user control is simpler, more trustworthy, and avoids both failure modes.

---

## 6. Prompt Design

### 6.1 Structured Output via Tool Use

Instead of asking the AI to return raw JSON (which is fragile and fails silently on malformed output), we use the AI provider's **structured output** mechanism:

- **Anthropic:** `tool_use` — define a tool schema, AI "calls" it with validated parameters.
- **OpenAI:** `function_calling` — same concept, different API surface.
- **Local models:** Fall back to JSON mode with validation + retry on parse failure.

This gives us **schema validation for free** — if the AI returns invalid parameters, the API itself reports an error, rather than our app crashing on malformed JSON.

### 6.2 Tool Schema Definition

```json
{
  "name": "navigate_step",
  "description": "Provide navigation instructions for the user. Return one or more steps. Steps with checkpoint=true will wait for the user to complete the action before proceeding.",
  "input_schema": {
    "type": "object",
    "required": ["steps", "state_summary", "needs_input"],
    "properties": {
      "steps": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["instruction", "checkpoint"],
          "properties": {
            "instruction": {
              "type": "string",
              "description": "The instruction shown/spoken to the user. Be specific about visual appearance and position."
            },
            "target_text": {
              "type": "string",
              "description": "Exact text label of the UI element to highlight. Used by Accessibility API and OCR to find the element. Null if no specific element."
            },
            "target_role": {
              "type": "string",
              "enum": ["button", "tab", "link", "textbox", "menuitem", "checkbox", "radio", "combobox", "slider", "image", "heading", "other"],
              "description": "The UI role/type of the target element. Used to narrow Accessibility API queries. Improves match accuracy."
            },
            "target_region": {
              "type": "string",
              "enum": ["top-left", "top-center", "top-right", "center-left", "center", "center-right", "bottom-left", "bottom-center", "bottom-right"],
              "description": "Rough screen region to narrow OCR search."
            },
            "overlay_type": {
              "type": "string",
              "enum": ["arrow", "highlight", "circle", "none"],
              "description": "Type of visual overlay to draw on the target element."
            },
            "clipboard": {
              "type": "string",
              "description": "Text to copy to clipboard (for CLI commands or text entry). Null if not applicable."
            },
            "checkpoint": {
              "type": "boolean",
              "description": "If true, wait for the user to complete this action (screen change detected) before showing the next step. If false, show the next step after a short delay."
            }
          }
        }
      },
      "state_summary": {
        "type": "string",
        "description": "Compact summary of current application state for context tracking. Not shown to user."
      },
      "needs_input": {
        "type": "boolean",
        "description": "If true, the AI needs the user to answer a question or make a choice before proceeding."
      }
    }
  }
}
```

### 6.3 System Prompt (Core)

```
You are AI Navigator, a real-time guidance assistant. You observe the user's
screen and provide step-by-step navigation instructions. You NEVER perform
actions — the user does everything.

Rules:
1. Provide 1-4 steps per response. Group small sequential actions (click, type,
   press Enter) into one response to reduce round-trips.
2. Mark the last meaningful action in a sequence as checkpoint=true so the system
   waits for the user to complete it before calling you again.
3. Refer to UI elements by their EXACT visible text label in target_text and their
   UI role in target_role (e.g., "button", "tab", "link"). These are used by the
   Accessibility API and OCR to find the element on screen. Also describe the
   element's visual appearance and approximate position in the instruction text.
4. NEVER output pixel coordinates. You do not know the exact position of elements.
5. If the screen shows the user completed the step, acknowledge and move forward.
6. If the screen shows something unexpected, describe what you see and suggest
   how to recover.
7. For CLI/terminal tasks, provide the exact command in the clipboard field.
8. Output a state_summary for internal context tracking (not shown to the user).
9. If you need clarification, set needs_input=true and ask a short question in
   the instruction field.
10. BROWSER REFERENCES: Refer to web browsers generically — say "open your browser"
    or "click your browser in the taskbar", never by specific name (Edge, Chrome,
    Firefox). The user chooses their own browser.
11. AI NAVIGATOR WINDOW: If you see the "AI Navigator" window (your own interface)
    is covering important screen elements, tell the user to minimize or move it —
    NEVER to close it. Closing the app ends the session.
12. LANGUAGE: Always respond in English, regardless of the user's system language,
    browser language, or the language of any text visible on screen.

Use the navigate_step tool for all responses.
```

The state summary sent with each follow-up request also includes the current AI Navigator window geometry (position + size), so the model can reason about whether the app window is occluding important content.

---

## 7. MVP Plan

### 7.1 MVP Scope (v0.1)

**Goal:** Single-platform (Windows) prototype that can guide a user through **browser-based tasks** end-to-end.

**Focus decision:** The MVP targets browser tasks exclusively. Browsers have excellent accessibility API support, predictable layouts, and text-heavy UIs that work well with OCR. Complex apps like Blender have sparse accessibility trees and icon-heavy UIs — they are explicitly out of scope until v0.2+ when template matching and Nav-Packs are available.

| Feature | Status | Notes |
|---------|--------|-------|
| Event-driven screen capture | ✅ v0.1 | OS events + pixel-diff + idle fallback |
| Text chat input | ✅ v0.1 | PySide6 main window + floating window |
| AI API — Anthropic (tool_use) | ✅ v0.1 | Structured output, primary provider |
| AI API — Google Gemini Flash | ✅ v0.1.1 | Free tier, function calling, multimodal |
| AI API — Ollama (local) | ✅ v0.1.1 | No API key, JSON mode, vision-capable models |
| AI API — OpenAI | Stub | v0.2 |
| Multi-step sequences | ✅ v0.1 | AI returns 1-4 steps per response |
| Screen change auto-advance | ✅ v0.1.1 | Mid-sequence steps advance automatically on screen change |
| Input queuing during processing | ✅ v0.1.1 | User can type while AI is thinking; messages queued |
| OS Accessibility API (UIA) | ✅ v0.1 | **Primary** element locator (< 5ms) |
| Local OCR fallback | ✅ v0.1.4 | Windows: `Windows.Media.Ocr` (built-in, ~10ms). macOS/Linux fallback: PaddleOCR |
| Overlay arrows | ✅ v0.1 | Positioned by A11y (primary) or OCR (fallback) |
| Subtitle fallback | ✅ v0.1 | When both A11y and OCR fail |
| Correction hotkey | ✅ v0.1 | Ctrl+Shift+X triggers re-analysis |
| Screenshot dedup (pHash) | ✅ v0.1 | Cost control |
| State summarization | ✅ v0.1 | Core to cost control |
| Window self-awareness | ✅ v0.1.1 | AI Navigator window geometry in state context; rules 10+11 in prompt |
| Generic browser references | ✅ v0.1.1 | System prompt rule — no Edge/Chrome/Firefox specifics |
| English-only responses | ✅ v0.1.2 | System prompt rule 12 — always English regardless of system locale |
| Overlay visibility (contrast) | ✅ v0.1.2 | White outline under all overlay types; visible on any background |
| Session persistence (save/resume) | ✅ v0.1 | JSON file |
| Clipboard commands | ✅ v0.1 | For CLI tasks |
| Pause/resume capture hotkey | ✅ v0.1 | Privacy control |
| Streaming responses | ❌ | v0.2 — biggest speed win |
| TTS audio output | ❌ | v0.2 |
| Voice input | ❌ | v0.2 |
| Prompt caching | ❌ | v0.2 |
| Multi-monitor support | ❌ | v0.2 |
| Template matching (icons) | ❌ | v0.3 |
| Multi-platform (macOS/Linux) | ❌ | Windows only for MVP |
| Cost dashboard | ❌ | v0.2 |
| Nav-Packs | ❌ | v0.3 |

### 7.2 Milestones

```
✅ Week 1-2:   Project scaffold + event-driven screen capture + basic chat UI
✅ Week 3-4:   API integration (Anthropic tool_use) + multi-step sequences
               + state summarization + prompt engineering
✅ Week 5-6:   UIA integration (primary) + PaddleOCR fallback + Element Locator
               + overlay arrow rendering + subtitle fallback
✅ Week 7-8:   Correction hotkey + session persistence + clipboard manager
               + screenshot dedup + pause/resume hotkey
✅ Week 9-10:  47 tests passing. First real-world tests: Amazon + SolidWorks
✅ Week 11:    v0.1.0-alpha released

✅ v0.1.1 (2026-04-06):
   - Gemini Flash provider (free tier for new users)
   - Ollama provider (local, no API key)
   - System prompt rules: generic browser + window self-awareness
   - Screen change auto-advance (mid-sequence steps)
   - Input queuing during processing (input box stays enabled)
   - Window geometry in state context

✅ v0.1.2 (2026-04-07):
   - System prompt rule 12: always respond in English (fixes Chinese/locale response)
   - Overlay visibility: white contrasting outline under all overlay types
     (arrow/highlight/circle now visible on any background color)
   - Overlay stroke thickness increased: 4px colored + 10px white outline
   - .env.example fully rewritten with all providers, models, and settings

✅ v0.1.3 (2026-04-08):
   - OCR fix: `show_log` and `use_gpu` arguments conditionally included via
     `inspect.signature` (both removed in newer PaddleOCR versions)
   - Race condition fix: `_is_processing` set synchronously before scheduling
     async API calls, preventing duplicate calls from rapid screen-change events
   - A11y fix: replaced invalid `auto.PropertyCondition`/`auto.PropertyId` API
     with correct `Control(RegexName=...)` uiautomation API
   - A11y fix: window/titlebar/pane controls excluded from element matches;
     4× name-length filter prevents matching browser tab titles as elements
   - System prompt rule 3 updated: `target_text` limited to 1–5 words max

✅ v0.1.4 (2026-04-08):
   - OCR backend replaced: `Windows.Media.Ocr` (built-in Windows 10/11 API via
     `winrt`) is now primary on Windows. PaddleOCR retained as fallback for
     non-Windows platforms only. Eliminates PaddlePaddle 3.x PIR+OneDNN bug
     (`ConvertPirAttribute2RuntimeAttribute` errors on every inference call).
     Windows OCR is ~10ms vs ~150ms for PaddleOCR, with zero model downloads.
   - `winrt-Windows.Media.Ocr` + related winrt packages added to pyproject.toml
   - OCR result format: line-level bboxes (merged words) + individual word bboxes
     emitted in parallel so both single-word and multi-word `target_text` match

🚧 v0.2 (next):
   - Streaming responses (subtitle < 500ms — biggest perceived speed win)
   - Prompt caching (90% cost reduction on system prompt)
   - TTS + voice input (paired)
   - Multi-monitor support
   - Model tiering (Haiku for change detection)
   - Cost dashboard
```

### 7.3 MVP Tech Stack

| Component | Choice | Notes |
|-----------|--------|-------|
| Language | Python 3.11+ | |
| UI | PySide6 (Qt) | Main process only (see §2.4) |
| Screen capture | `mss` (Python, cross-platform) | In capture worker process |
| Screen change detection | `mss` low-res capture at 10fps + `imagehash` pHash | In diff worker process (bypasses GIL) |
| Accessibility API | `uiautomation` (Windows UIA wrapper) | **Primary** element locator. < 5ms. No extra dependency on Windows. |
| Local OCR (fallback) | `Windows.Media.Ocr` (Windows 10/11 built-in) via `winrt` | **Primary OCR on Windows**: ~10ms, zero model downloads, no dependency issues. Falls back to `paddleocr` on non-Windows platforms. In OCR worker process. |
| Image hashing | `imagehash` (pHash) | |
| API client | `httpx` (async) | Streaming + tool_use for structured output |
| Concurrency | `multiprocessing` (Process + Queue) | CPU work in separate processes; asyncio for I/O |
| Overlay | Qt frameless transparent window (`Qt.WindowStaysOnTopHint + Qt.FramelessWindowHint + Qt.WA_TranslucentBackground`) | |
| Clipboard | `pyperclip` | |
| Session storage | JSON files in `~/.ai-navigator/sessions/` | |
| Packaging | `pip install ai-navigator` (PyPI) | No PyInstaller for MVP — avoids SmartScreen (see §7.5) |

### 7.4 Roadmap

```
v0.2  Streaming responses + prompt caching + TTS + voice input (paired)
      + multi-monitor support + model tiering + cost dashboard
v0.3  Tauri/Rust rewrite of core (solves SmartScreen) + EV code signing
      + Blender / complex-app support + template matching for icons
      + quantized local model improvements + macOS port + Nav-Packs
v0.4  Linux port + plugin system + enterprise features (SSO, audit logs)
      + accessibility-focused UX pass (large text, high contrast, screen reader compat)
v1.0  Public launch + MSIX packaging for Microsoft Store + native installer
```

> **Note on streaming (v0.2 priority #1):** The biggest user-perceived speed issue is waiting for the full API response before showing anything. Streaming renders the instruction subtitle as tokens arrive (< 500ms for first token vs. 2s for full response). This single change will make the app feel substantially faster without any model or infrastructure changes.

> **Note on v0.2 voice:** TTS and voice input are shipped in the same release. Users who want to talk to the navigator expect it to talk back. Shipping one without the other creates a broken interaction model.

> **Note on Tauri migration at v0.3 (not v1.0):** The Python/PyInstaller distribution problem (§7.5) means the Tauri rewrite must happen before any broad public distribution. v0.3 is the target, giving ~6 months of Python-based development for rapid iteration before investing in the native rewrite.

### 7.5 Distribution Strategy (SmartScreen Mitigation)

**The Problem:** A PyInstaller `.exe` that requests global keyboard hooks, accesses the clipboard, and continuously captures the screen is indistinguishable from malware to Windows Defender SmartScreen. It will be blocked for nearly all users.

**Strategy by phase:**

| Phase | Distribution | Target Audience | SmartScreen Risk |
|-------|-------------|-----------------|------------------|
| **v0.1 alpha** | `pip install ai-navigator` (PyPI) | Developers, technical testers who have Python installed | **None** — no `.exe`, no SmartScreen |
| **v0.2 beta** | Still PyPI. Optional: unsigned `.exe` with manual instructions for Windows Defender exceptions | Power users willing to whitelist | **Medium** — but audience accepts it |
| **v0.3** | **Tauri native binary** + EV code signing ($400/yr) | Early adopters | **Low** — native binary + signed certificate builds reputation |
| **v1.0** | MSIX packaging (Microsoft Store) + direct download (signed) | General public | **None** — Store apps bypass SmartScreen |

**Why `pip install` for MVP:**
- MVP alpha testers are developers/power-users who already have Python
- Zero SmartScreen issues — there is no `.exe`
- Fast iteration — no build/package step during development
- EasyOCR/PaddleOCR install naturally via pip

**EV Code Signing timeline:**
- Purchase EV certificate at v0.3 launch (~$400/year)
- Sign all Tauri binaries
- SmartScreen reputation builds over ~1000 installs
- By v1.0 launch, reputation is established

---

## 8. Business Model — Cost & Pricing Framework

### 8.1 Tiers

| Tier | Price | AI Backend | Token Cap | Features |
|------|-------|-----------|-----------|----------|
| **Community** | Free | BYOK (own API key) or local model | None (user pays API directly) | Core guidance, subtitle overlay only, no session persistence, no Nav-Packs |
| **Personal Pro** | $25–30/month | Managed (our keys) | ~5M tokens/month (~200 medium sessions) | Full overlay (arrows + highlights), session persistence + resume, TTS, voice input, basic Nav-Packs, priority support |
| **Enterprise** | Custom | Managed + on-prem option | Custom | Custom Nav-Packs, SSO, audit logs, private deployment, dedicated support |

### 8.2 Feature Gating (Community vs. Pro)

To prevent the free tier from fully cannibalizing Pro subscriptions, quality-of-life features are gated:

| Feature | Community (Free) | Personal Pro |
|---------|-------------------|-------------|
| Core guidance loop | Yes | Yes |
| Subtitle instructions | Yes | Yes |
| OCR overlay (arrows, highlights) | Basic (arrows only) | Full (arrows + highlights + circles) |
| Multi-step sequences | Max 2 steps/turn | Up to 4 steps/turn |
| Session persistence / resume | No | Yes |
| Voice input + TTS | No | Yes |
| Nav-Packs | No | Yes (bundled basic pack) |
| Correction hotkey | Yes | Yes |
| Cost dashboard | Basic | Detailed with history |
| Support | Community forum | Email + priority |

> **Principle:** The free tier must be genuinely useful (core guidance works), but Pro should feel noticeably smoother and more powerful. Users who invest time in AI Navigator should naturally want to upgrade.

### 8.3 Operational Cost Per Pro User (with safety margin)

| Item | Monthly Cost (early) | Monthly Cost (mature) |
|------|---------------------|-----------------------|
| API tokens (avg usage, 2.5x margin) | $12 – $20 | $5 – $8 |
| Infrastructure (servers, auth, billing) | $1 – $2 | $1 – $2 |
| **Total COGS per user** | **$13 – $22** | **$6 – $10** |
| **Pro price** | **$25 – $30** | **$25 – $30** |
| **Gross margin (early)** | **10% – 45%** | — |
| **Gross margin (mature)** | — | **55% – 75%** |

> **Pricing rationale:** At $12-20/month (original plan), gross margins during the early period (before optimizations mature) would be near-zero or negative for heavy users. At $25-30/month, even worst-case early users are sustainable. As optimizations improve, margins expand significantly. This also positions the product as a professional tool, not a toy.

> **Usage-based fallback:** If a Pro user exceeds 5M tokens/month, offer overage at $5/1M tokens (below retail API pricing, still profitable). This protects against heavy outliers without hard-cutting their access.

### 8.4 Nav-Packs (Future Revenue)

"Nav-Packs" are curated prompt bundles + overlay templates for specific applications:
- **Blender Nav-Pack**: Knows Blender's UI layout, icon library for template matching, common workflows, hotkeys.
- **Tax Filing Nav-Pack**: Understands TurboTax / IRS forms, knows what fields mean, seasonal availability.
- **Enterprise Nav-Pack**: Custom-built for a company's internal tools. Requires onboarding engagement.

Free tier gets generic guidance. Pro gets basic Nav-Packs. Enterprise gets custom packs.

Nav-Packs can also be sold individually ($5–10 one-time) for Community users who want app-specific guidance without a full Pro subscription.

---

## 9. Project Structure (MVP)

```
ai-navigator/
├── README.md
├── pyproject.toml              # Python project config (dependencies, scripts)
├── src/
│   ├── main.py                 # Entry point
│   ├── config.py               # Settings, API keys, paths, hotkey bindings
│   ├── core/
│   │   ├── session.py          # Session lifecycle management + persistence
│   │   ├── state.py            # State summary storage
│   │   ├── cost_tracker.py     # Token usage tracking with safety margin
│   │   ├── correction.py       # Correction handler (re-analysis on "wrong" signal)
│   │   └── step_sequencer.py   # Advances through multi-step sequences locally
│   ├── input/
│   │   ├── screen_capture.py   # Screenshot capture (on-demand)
│   │   ├── screen_monitor.py   # Event-driven screen change detection
│   │   ├── voice_input.py      # (stub for v0.2)
│   │   └── chat_input.py       # Chat text input handling
│   ├── ai/
│   │   ├── api_router.py       # Provider selection + request building
│   │   ├── anthropic_client.py # Anthropic API client (tool_use)
│   │   ├── gemini_client.py    # Google Gemini client (function calling, free tier)
│   │   ├── ollama_client.py    # Ollama local model client (JSON mode)
│   │   ├── openai_client.py    # OpenAI API client (stub, v0.2)
│   │   ├── prompts.py          # System prompts (rules 1-11)
│   │   └── tool_schemas.py     # navigate_step tool schema definition
│   ├── locator/
│   │   ├── element_locator.py  # Orchestrates A11y → OCR → template fallback chain
│   │   ├── a11y_engine.py      # PRIMARY: OS Accessibility API (UIA on Windows, < 5ms)
│   │   ├── ocr_engine.py       # FALLBACK: PaddleOCR wrapper, text → bbox mapping
│   │   └── template_engine.py  # (stub for v0.3) Icon template matching
│   ├── output/
│   │   ├── overlay.py          # Screen overlay renderer (arrows, highlights, subtitles)
│   │   ├── tts.py              # Text-to-speech (stub for v0.2)
│   │   └── clipboard.py        # Clipboard manager
│   └── ui/
│       ├── main_window.py      # Main chat window (PySide6)
│       └── floating_window.py  # Hotkey-activated floating input + correction button
├── assets/
│   └── icons/
├── sessions/                   # Default session storage directory
├── tests/
│   ├── test_screen_capture.py
│   ├── test_screen_monitor.py
│   ├── test_api_router.py
│   ├── test_element_locator.py
│   ├── test_step_sequencer.py
│   ├── test_state_summary.py
│   └── test_session_persistence.py
└── docs/
    └── design.md               # This document
```

---

## 10. Key Risks & Mitigations

| # | Risk | Impact | Mitigation |
|---|------|--------|------------|
| 1 | **Element locator fails to find target** | Arrow points nowhere or overlay is absent | Three-tier fallback: Accessibility API (< 5ms, primary) → OCR (50-150ms, fallback) → subtitle-only. User never sees a broken arrow. |
| 2 | **High API cost per session (early)** | Operating at a loss for heavy users | 2.5x safety margin in budget. Usage-based overage pricing. Pro priced at $25-30 to absorb variance. |
| 3 | **Screen capture permission denied** | App is useless | Clear onboarding flow with OS-specific setup wizard. On macOS, guide user through System Preferences > Privacy > Screen Recording. |
| 4 | **Latency (API round-trip)** | Guidance feels sluggish, user loses patience | Split latency target: subtitle < 1s (via streaming), overlay arrow < 2s. UIA locates elements in < 5ms (no OCR wait on critical path for browser tasks). OCR fallback runs in parallel with API call. Multi-step sequences reduce round-trips 2-4x. |
| 5 | **Privacy backlash / trust** | Users won't install or will uninstall | Explicit user controls (pause hotkey, app blocklist, capture indicator). Local-first option. No disk persistence of screenshots. Source-available (FSL) community edition. |
| 6 | **Cross-platform overlay differences** | Buggy or inconsistent UX | Abstract overlay behind platform trait. MVP is Windows-only — invest in getting one platform right before expanding. |
| 7 | **Multi-step sequence goes stale** | AI's step 3 is wrong because user did something unexpected at step 2 | Checkpoint system. At each checkpoint, re-capture screen and validate before advancing. If screen doesn't match expected state, discard remaining steps and call AI. |
| 8 | **Wrong instruction with no recourse** | User frustrated, loses trust | Correction hotkey (Ctrl+Shift+X) in MVP. One press triggers re-analysis. Also available as button in floating window. |
| 9 | **Community tier too generous** | Pro subscriptions cannibalized | Feature gating: no session persistence, no voice, no Nav-Packs, limited multi-step in free tier. Free must be useful, Pro must be noticeably better. |
| 10 | **App crash mid-task** | User loses all progress | Session persistence saves state summary + conversation to disk. Resume costs one API call to re-sync. |
| 11 | **Windows SmartScreen blocks .exe** | Users can't install the app; app appears to be malware | MVP ships as `pip install` (no .exe). Tauri native binary at v0.3. EV code signing at v0.3. MSIX/Microsoft Store at v1.0. See §7.5. |
| 12 | **Python GIL causes UI stutter** | Chat window freezes during OCR/screen-diff; app feels broken | Multi-process architecture (§2.4): CPU work in `multiprocessing.Process`, Qt event loop stays on main process. Zero CPU-heavy work on main thread. |
| 13 | **Code cloned without commercial rights** | Competitor ships a clone; no legal recourse | FSL (Functional Source License) — source-available with 2-year non-compete. Converts to MIT/Apache after 2 years. See §12. |
| 14 | **OCR dependency too large** | App download/install size balloons to 2GB+ with PyTorch/CUDA | Use PaddleOCR (CPU-only, ~100MB) instead of EasyOCR (PyTorch, 500MB+). No CUDA dependency for MVP. |

---

## 11. Success Metrics (MVP)

| Metric | Target | How Measured |
|--------|--------|-------------|
| Task completion rate | > 70% of started browser tasks completed | Session logs (task_description → final state_summary) |
| **Subtitle latency** | **< 1s** from screen change to subtitle text appearing (via API streaming) | Timestamps in session log |
| **Overlay arrow latency (A11y path)** | **< 2s** from screen change to overlay arrow (API is the bottleneck, not locator) | Timestamps in session log |
| **Overlay arrow latency (OCR fallback)** | **< 2.5s** from screen change to overlay arrow (when A11y tree unavailable) | Timestamps in session log |
| A11y hit rate (browser tasks) | > 85% of elements found via UIA without OCR fallback | Element Locator logs |
| Avg. session cost (API) | < $0.40 for a 15-turn session (with safety margin) | Token counter |
| Element Locator accuracy | > 90% of overlay arrows point to correct element (A11y + OCR combined) | Manual QA testing on sample tasks |
| UIA query time | < 10ms per element lookup | Performance profiling |
| OCR processing time (fallback) | < 150ms per frame (PaddleOCR on CPU) | Performance profiling |
| Screenshot dedup hit rate | > 50% (API calls avoided) | Counter in screen monitor |
| Correction rate | < 20% of turns trigger correction hotkey | Session logs |
| User satisfaction (survey) | > 4/5 | Post-session survey |
| Session resume success | > 90% of resumed sessions correctly re-sync | Manual QA |
| UI responsiveness | No Qt event loop stalls > 50ms | Performance profiling (main process) |

---

## 12. Licensing Strategy

### 12.1 The Problem

Shipping code on a public GitHub repository without a license means "all rights reserved" (no one can legally use it), but enforcement is impractical. Conversely, using MIT/Apache allows anyone — including well-funded competitors — to clone the product and sell it.

For a solo developer building a commercial product with an open development model, neither extreme works.

### 12.2 Recommended License: FSL (Functional Source License)

**FSL** is a source-available license designed for exactly this situation. Used by HashiCorp, Sentry, and others.

| Aspect | FSL Terms |
|--------|-----------|
| **Source code** | Public and readable on GitHub |
| **Non-competing use** | Anyone can use, modify, and deploy for any purpose that does not compete with AI Navigator |
| **Competing use** | Prohibited during the non-compete period |
| **Non-compete period** | 2 years from each version's release date |
| **After 2 years** | Each version automatically converts to MIT or Apache 2.0 (your choice) |
| **Contributor license** | Contributors grant you a license to use their contributions commercially |

### 12.3 How This Maps to Business Tiers

| Tier | License Implication |
|------|---------------------|
| **Community (Free)** | Users run the FSL-licensed code with their own API keys. They can modify it for personal/non-competing use. |
| **Personal Pro** | Proprietary service layer on top of FSL code (managed API keys, Nav-Packs, priority features). |
| **Enterprise** | Separate commercial license agreement. May include on-prem deployment rights, custom Nav-Packs, and SLA. |

### 12.4 What Counts as "Competing"

The FSL requires you to define what "competing" means. For AI Navigator:

> **Competing use** means offering a product or service that provides AI-powered screen guidance, navigation instructions, or overlay-based user assistance as a primary function — substantially similar to AI Navigator.

> **Non-competing use** includes: using AI Navigator internally at a company, integrating AI Navigator into a product where screen guidance is not the primary function, academic research, personal use, and contributing to the AI Navigator project.

### 12.5 Action Items

1. **Before any more public code:** Add `LICENSE.md` to the repository with the FSL text.
2. **Add a `CONTRIBUTING.md`** with a Contributor License Agreement (CLA) or Developer Certificate of Origin (DCO).
3. **Add license header** to all source files.
4. **Choose the conversion license:** MIT (simpler, more permissive) or Apache 2.0 (includes patent grant). Recommend **Apache 2.0** for the patent protection.

> **Reference:** https://fsl.software/ — official FSL website with full license text and FAQ.

---

## Appendix A: Revision History

| Version | Date | Changes |
|---------|------|---------|
| 0.1 | 2026-04-05 | Initial draft |
| 0.2 | 2026-04-05 | Major revision addressing 13 review findings: (1) Local OCR + Element Locator as core MVP component for overlay accuracy; (2) event-driven screen capture replacing 2s polling; (3) multi-step sequences with checkpoints; (4) TTS + voice input paired in v0.2; (5) structured output via tool_use replacing raw JSON prompts; (6) user-controlled privacy replacing heuristic sensitive-screen detection; (7) 2-3x cost safety margin for early versions; (8) MVP focused on browser tasks only; (9) correction hotkey and handler; (10) session persistence and resume; (11) Pro pricing raised to $25-30/month; (12) feature gating between Community and Pro; (13) accessibility positioning as first-class use case. |
| 0.3 | 2026-04-05 | Engineering feasibility fixes: (1) Replaced PyInstaller with `pip install` for MVP, added SmartScreen mitigation strategy and distribution roadmap (§7.5); (2) Replaced EasyOCR with PaddleOCR (~50-150ms vs 200-500ms), added parallel OCR+API execution, split latency metric into subtitle < 1s / overlay < 2.5s; (3) Added multi-process architecture (§2.4) to mitigate Python GIL — CPU work in separate processes, Qt event loop stays responsive; (4) Added FSL licensing strategy (§12) with non-compete clause, CLA guidance, and tier mapping. Also: moved Tauri rewrite from v1.0 to v0.3. |
| 0.4 | 2026-04-05 | Promoted OS Accessibility API (UIA) to **primary** element locator for MVP, replacing OCR-first approach. UIA queries the browser's widget tree in < 5ms vs OCR's 50-150ms. OCR demoted to fallback (still runs in parallel as pre-cache). Added `target_role` field to tool schema for precise UIA queries. Updated data flow, pseudocode, success metrics (A11y hit rate > 85%, overlay latency < 2s on A11y path), risk table, and MVP scope. Removed UIA from v0.2 roadmap (now in MVP). |
| 0.6 | 2026-04-07 | (1) System prompt rule 12: always respond in English — fixes locale-driven Chinese responses. (2) Overlay visibility overhaul: white contrasting outline drawn underneath all overlay types (arrow/highlight/circle) so they're visible on any background; thickness increased from 3px to 4px inner + 10px white outline. (3) `.env.example` fully rewritten with all four providers, all model options with comments, all config settings. Updated §6.3 (system prompt), §7.1 (scope table), §7.2 (milestones). |
| 0.7 | 2026-04-08 | v0.1.3 + v0.1.4 changes: (1) A11y engine fixed — replaced invalid `PropertyCondition`/`PropertyId` COM API with correct `Control(RegexName=...)` uiautomation Python API; added WindowControl/TitleBarControl/PaneControl exclusion and 4× name-length ratio guard to prevent matching browser tab titles as UI elements; A11y search depth increased to 12 (fast path) and 8 (slow path) for Chrome DOM depth. (2) OCR backend switched to `Windows.Media.Ocr` as primary on Windows — eliminates PaddlePaddle 3.x PIR+OneDNN `ConvertPirAttribute2RuntimeAttribute` bug that caused every OCR inference to fail; Windows OCR is ~10ms vs 150ms with zero model downloads; PaddleOCR retained as non-Windows fallback. (3) PaddleOCR compatibility shims: `use_gpu`/`show_log` conditional inclusion, `cls` parameter try/except, 3.x dict result format support. (4) System prompt rule 3 updated: `target_text` limited to 1–5 words. Updated §3.3 (OCR locator table), §2.4 (concurrency diagram), §7.1 (scope table), §7.2 (milestones), §7.3 (tech stack). |
| 0.5 | 2026-04-06 | Post-first-test updates from Amazon + SolidWorks testing. (1) Multi-provider AI: added Gemini Flash (free tier, function calling) and Ollama (local, JSON mode) — §4.4 and §9 updated; (2) System prompt rules 10+11: generic browser references (not Edge/Chrome/Firefox by name), AI Navigator window self-awareness (minimize not close); (3) Window geometry injected into state context so model knows app window position; (4) Screen change auto-advance: mid-sequence (non-checkpoint) steps now advance automatically on screen change — previously stuck at `pass`; (5) Screen change re-query: when step sequence is complete and screen changes, AI is re-queried (debounced 5s); (6) Input queuing: input box stays enabled during processing, messages typed while thinking are queued and sent on completion; (7) §7.1 MVP scope table updated to track v0.1 / v0.1.1 / v0.2 status; (8) §7.2 milestones updated with completed items and v0.2 plan; (9) §7.4 roadmap updated; streaming flagged as v0.2 priority #1 for perceived speed. |

---

*Document end. This is a living document — update as decisions are made.*
