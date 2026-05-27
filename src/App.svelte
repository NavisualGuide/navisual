<!--
Copyright (c) 2024-2026 Jin Fu
Licensed under the Functional Source License, Version 1.1 (Apache 2.0).
See the LICENSE file in the root of this repository for complete details.
-->
<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { register, unregisterAll } from "@tauri-apps/plugin-global-shortcut";
  import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
  import { LogicalSize, LogicalPosition, PhysicalSize, PhysicalPosition } from "@tauri-apps/api/dpi";
  import { listen, emitTo } from "@tauri-apps/api/event";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { check as checkUpdate, type Update } from "@tauri-apps/plugin-updater";
  import HotkeyInput from "./HotkeyInput.svelte";

  type Rect = { x: number; y: number; width: number; height: number };
  type LocateResult = { bbox: Rect; name: string; role: string; confidence: number };
  type GuidanceStep = {
    instruction: string;
    target_text: string | null;
    target_role: string | null;
    target_nearby_text: string | null;
    overlay_type: string;
    clipboard: string | null;
    checkpoint: boolean;
  };
  type GuideResponse = {
    ok: boolean;
    session_id: string;
    steps: GuidanceStep[];
    step_index: number;
    instruction: string;
    located: LocateResult | null;
    needs_input: boolean;
    request_full_screen: boolean;
    provider: string;
    error: string | null;
    debug_screenshot_path: string | null;
    chat_thumb_b64: string | null;
    locate_trace: LocateTrace | null;
    ai_bbox: Rect | null;
  };
  type AppPhase = "idle" | "thinking" | "guiding" | "needs_input" | "consent_prompt" | "error";
  type HistoryRole = "user" | "ai" | "correction" | "system" | "error";
  type HistoryEntry = { id: number; role: HistoryRole; text: string; meta?: string; thumb?: string; thumbFading?: boolean };
  type SettingsTab = "provider" | "screen-guide" | "hotkeys" | "audio" | "developer";
  type SettingsPayload = {
    api_provider: string;
    anthropic_api_key: string;
    anthropic_model: string;
    anthropic_fast_model: string;
    gemini_api_key: string;
    gemini_model: string;
    gemini_fast_model: string;
    ollama_base_url: string;
    ollama_model: string;
    openai_api_key: string;
    openai_model: string;
    deepseek_api_key: string;
    deepseek_model: string;
    qwen_api_key: string;
    qwen_model: string;
    qwen_base_url: string;
    overlay_color: string;
    overlay_thickness: number;
    subtitle_enabled: boolean;
    auto_advance: boolean;
    tts_enabled: boolean;
    tts_voice: string;
    voice_input_enabled: boolean;
    voice_language: string;
    hotkey_next: string;
    hotkey_wrong: string;
    hotkey_pause: string;
    hotkey_icon: string;
    hotkey_talk: string;
    debug_screenshot_enabled: boolean;
    debug_show_response_info: boolean;
    debug_locate_trace_enabled: boolean;
    debug_locate_log_file_enabled: boolean;
    debug_show_ai_bbox: boolean;
    developer_mode: boolean;
  };

  // ---- Locator trace types (mirror src-tauri/src/locator/trace.rs) ----
  type A11yCandidate = {
    name: string;
    role: string;
    bbox: [number, number, number, number];
    selected: boolean;
    reject_reason: string | null;
  };
  type A11yTrace = {
    ran: boolean;
    regex_used: string;
    search_roots_count: number;
    candidates: A11yCandidate[];
    timed_out: boolean;
    elapsed_ms: number;
  };
  type OcrCandidate = {
    text: string;
    bbox: [number, number, number, number];
    confidence: number;
    strategy: string;
    score: number | null;
    selected: boolean;
    reject_reason: string | null;
  };
  type OcrTrace = {
    ran: boolean;
    line_count: number;
    word_count: number;
    sample_texts: string[];
    strategy_used: string | null;
    tier_reached: number;
    candidates: OcrCandidate[];
    elapsed_ms: number;
  };
  type FinalDecision =
    | { kind: "miss" }
    | { kind: "hit_a11y" }
    | { kind: "hit_ocr" }
    | { kind: "rejected_by_hit_test"; leaf_class: string }
    | { kind: "error"; message: string };
  type LocateTrace = {
    timestamp_ms: number;
    target_text: string;
    target_role: string | null;
    nearby_text: string | null;
    ai_bbox: { x: number; y: number; width: number; height: number } | null;
    a11y: A11yTrace;
    ocr: OcrTrace;
    final_decision: FinalDecision;
    final_bbox: { x: number; y: number; width: number; height: number } | null;
    elapsed_ms: number;
  };

  // Core state
  let task = $state("");
  let lastCompletedInstruction = $state("");  // passed to AI on Next re-query
  let phase = $state<AppPhase>("idle");

  let steps = $state<GuidanceStep[]>([]);
  let stepIndex = $state(0);
  let currentInstruction = $state("");
  let locateResult = $state<LocateResult | null>(null);
  let locateTrace = $state<LocateTrace | null>(null);
  let debugDrawerOpen = $state(false);
  // Test-user feedback (see logFeedback / submitWrong / correction).
  let wrongPickerOpen = $state(false);
  const CATEGORY_LABEL: Record<string, string> = {
    wrong_instruction: "Wrong instruction",
    wrong_spot: "Wrong spot",
    not_found: "Can't find it",
    already_done: "Already did that",
    wrong_other: "Other",
  };
  // Steering hint folded into the AI re-analysis note for each reason (the user's
  // own typed text is appended after, and is what gets logged). wrong_other has
  // no canned hint — the free text is the signal.
  const CATEGORY_HINT: Record<string, string> = {
    wrong_instruction:
      "That instruction was the wrong action for my goal. Reconsider the task and propose a different next step.",
    wrong_spot:
      "The pointer landed on the WRONG element. The target may be ambiguous (it appears more than once on screen) or you identified the wrong one. Re-examine the screenshot and return a more specific target_text, a precise target_bbox, and a target_nearby_text anchor to disambiguate.",
    not_found:
      "The pointer could not be placed — the element you described isn't visible or wasn't found. It may be off-screen (needs scrolling), hidden behind a menu, or named differently. Re-examine and either guide a scroll/expand step first or give a more findable target.",
    already_done:
      "I have ALREADY done this step. Do not repeat it — advance to the next action.",
  };
  let sessionId = $state("");
  let provider = $state("");
  // Set when the screen drifted during the 5–90s AI thinking window.
  // Surfaces a soft banner over the instruction so the user knows the
  // guidance may be referring to state that no longer exists.
  let staleResponse = $state(false);
  // Managed provider (S.1) state
  let freeRemaining = $state<number | null>(null);
  let showTrialExhausted = $state(false);

  // Phase 0.2: which app is currently shared with the AI.
  type SharedAppInfo = {
    hwnd: number;
    rect: { x: number; y: number; width: number; height: number };
    app_name: string;
    exe_name: string;
  };
  let sharedApp = $state<SharedAppInfo | null>(null);

  // Target-window picker (item 1)
  type TargetWindowInfo = { hwnd: number; title: string; exe_stem: string; display_name: string; };
  let targetPickerOpen = $state(false);
  let targetWindows = $state<TargetWindowInfo[]>([]);
  let pinnedHwnd = $state<number | null>(null);

  // Friendly names for exe stems shown in the "Shared:" chip (mirrors Rust's friendly_exe_name).
  const EXE_DISPLAY: Record<string, string> = {
    olk: "Outlook", outlook: "Outlook",
    code: "VS Code",
    winword: "Word", excel: "Excel", powerpnt: "PowerPoint", onenote: "OneNote",
    msedge: "Edge", chrome: "Chrome", firefox: "Firefox",
    slack: "Slack", teams: "Teams",
    windowsterminal: "Terminal", wt: "Terminal",
    wechat: "WeChat", notion: "Notion", obsidian: "Obsidian",
    discord: "Discord", zoom: "Zoom", notepad: "Notepad",
  };

  function exeStem(name: string): string {
    return name.replace(/\.exe$/i, "").trim() || name;
  }
  function friendlyName(exeName: string): string {
    const stem = exeStem(exeName).toLowerCase();
    return EXE_DISPLAY[stem] ?? exeStem(exeName);
  }

  // On a cold start (fresh Windows 10 install with no warm caches), WebView2
  // can finish loading App.svelte and reach onMount invocations before Rust
  // setup() finishes its cold-start I/O and calls handle.manage(AppState).
  // Tauri then rejects state-touching commands with "state not managed".
  // 8 × 150ms = 1.2s, comfortably longer than any observed cold start.
  async function invokeReady<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    for (let i = 0; i < 8; i++) {
      try {
        return await invoke<T>(cmd, args);
      } catch (e) {
        if (String(e).includes("state not managed") && i < 7) {
          await new Promise((r) => setTimeout(r, 150));
          continue;
        }
        throw e;
      }
    }
    throw new Error("unreachable");
  }

  type VoiceInfo = { id: string; name: string; };
  let availableVoices = $state<VoiceInfo[]>([]);

  async function openTargetPicker() {
    targetWindows = await invoke<TargetWindowInfo[]>("list_target_windows");
    targetPickerOpen = true;
  }

  async function selectTarget(hwnd: number | null) {
    targetPickerOpen = false;
    if (hwnd === null) {
      await invoke("unpin_target_window");
      pinnedHwnd = null;
    } else {
      await invoke("pin_target_window", { hwnd });
      pinnedHwnd = hwnd;
    }
  }

  // UI state
  let iconMode = $state(false);
  let showSettings = $state(false);
  let showAbout = $state(false);
  // First-run privacy disclosure (S5). One-shot; persisted in localStorage so
  // it never fires again on the same install.
  let showPrivacyDisclosure = $state(false);
  const PRIVACY_DISCLOSURE_KEY = "navisual-privacy-disclosed-v1";
  let appVersion = $state("…");
  let pendingUpdate = $state<Update | null>(null);
  let updateStatus = $state<"idle" | "checking" | "downloading" | "done">("idle");
  let updateProgress = $state(0);
  let settingsTab = $state<SettingsTab>("provider");
  let history = $state<HistoryEntry[]>([]);
  let historyEl: HTMLElement | null = $state(null);

  // Settings form state
  const SETTINGS_DEFAULTS: SettingsPayload = {
    api_provider: "managed",
    anthropic_api_key: "", anthropic_model: "claude-sonnet-4-6", anthropic_fast_model: "claude-haiku-4-5-20251001",
    gemini_api_key: "", gemini_model: "gemini-2.5-flash", gemini_fast_model: "gemini-2.5-flash-lite",
    ollama_base_url: "http://localhost:11434", ollama_model: "llama3.2-vision",
    openai_api_key: "", openai_model: "gpt-5.5",
    deepseek_api_key: "", deepseek_model: "deepseek-v4-flash",
    qwen_api_key: "", qwen_model: "qwen3.6-plus",
    qwen_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    overlay_color: "#FF6B35", overlay_thickness: 4,
    subtitle_enabled: true, auto_advance: false,
    tts_enabled: true, tts_voice: "", voice_input_enabled: false, voice_language: "auto",
    hotkey_next: "Ctrl+Backquote", hotkey_wrong: "Ctrl+KeyE",
    hotkey_pause: "", hotkey_icon: "", hotkey_talk: "Ctrl+KeyD",
    debug_screenshot_enabled: false,
    debug_show_response_info: false,
    debug_locate_trace_enabled: false,
    debug_locate_log_file_enabled: false,
    debug_show_ai_bbox: false,
    developer_mode: false,
  };
  let settingsForm = $state<SettingsPayload>({ ...SETTINGS_DEFAULTS });
  let settingsSaving = $state(false);
  let settingsError = $state<string | null>(null);
  let settingsSaved = $state(false);
  const MODEL_PRESETS_ANTHROPIC = ["claude-haiku-4-5-20251001","claude-sonnet-4-6","claude-opus-4-7"];
  const MODEL_PRESETS_GEMINI    = ["gemini-2.5-flash","gemini-2.5-flash-lite","gemini-3.5-flash","gemini-3.1-pro-preview"];
  const MODEL_PRESETS_OPENAI    = ["gpt-5.5","gpt-5.4-mini"];
  const MODEL_PRESETS_DEEPSEEK  = ["deepseek-v4-flash","deepseek-v4-pro"];
  const MODEL_PRESETS_QWEN      = ["qwen3.6-plus","qwen3.5-omni-plus"];

  let showKeyAnthropic = $state(false);
  let showKeyGemini = $state(false);
  let showKeyOpenAI = $state(false);
  let showKeyDeepSeek = $state(false);
  let showKeyQwen = $state(false);
  let debugShowInfo = $state(false);
  let showQuickMenu = $state(false);
  let isMuted = $state(false);
  let isOverlayCleared = $state(false);
  let isRecording = $state(false);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let speechRecognition: any = null;

  // Timer
  let elapsedMs = $state(0);
  let elapsedTimer: ReturnType<typeof setInterval> | null = null;
  let elapsedStart = 0;
  let requestToken = 0;

  const PANEL_W = 420;
  const PANEL_H = 600;
  const ICON_SIZE = 56;

  function startTimer() {
    elapsedStart = performance.now();
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = setInterval(() => {
      elapsedMs = Math.round(performance.now() - elapsedStart);
    }, 200);
  }

  function stopTimer() {
    if (elapsedTimer) { clearInterval(elapsedTimer); elapsedTimer = null; }
  }

  let _historyId = 0;
  async function addToHistory(role: HistoryRole, text: string, meta?: string): Promise<number> {
    const id = ++_historyId;
    history.push({ id, role, text, meta });
    await tick();
    if (historyEl) historyEl.scrollTop = historyEl.scrollHeight;
    return id;
  }

  // Screenshot thumbnail lightbox.
  let lightboxOpen = $state(false);
  let lightboxSrc = $state<string | null>(null);
  let lightboxLoading = $state(false);
  let _lightboxPrevSize: { w: number; h: number } | null = null;
  let _lightboxPrevPos: { x: number; y: number } | null = null;

  async function openLightbox() {
    lightboxLoading = true;
    lightboxSrc = null;
    try {
      lightboxSrc = await invoke<string | null>("get_chat_full_screenshot");
    } catch (_) {}
    lightboxLoading = false;
    if (!lightboxSrc) return;

    // Expand the panel window to comfortably display the screenshot,
    // then restore it when the lightbox closes.
    const win = getCurrentWindow();
    try {
      const outer = await win.outerSize();      // physical pixels
      const pos   = await win.outerPosition();
      _lightboxPrevSize = { w: outer.width, h: outer.height };
      _lightboxPrevPos  = { x: pos.x, y: pos.y };

      // Size + center on the monitor the panel is CURRENTLY on (not the primary),
      // so the lightbox doesn't jump to the main screen. All physical pixels.
      const mon = await currentMonitor();
      const mScale = mon?.scaleFactor ?? (window.devicePixelRatio || 1);
      const mx = mon ? mon.position.x : 0;
      const my = mon ? mon.position.y : 0;
      const mw = mon ? mon.size.width  : Math.round(window.screen.availWidth  * mScale);
      const mh = mon ? mon.size.height : Math.round(window.screen.availHeight * mScale);
      const targetW = Math.round(Math.min(mw * 0.9, 1560 * mScale));  // 1536 + margin
      const targetH = Math.round(Math.min(mh * 0.9, 800 * mScale));
      const newX = Math.round(mx + (mw - targetW) / 2);
      const newY = Math.round(my + (mh - targetH) / 2);
      await win.setSize(new PhysicalSize(targetW, targetH));
      await win.setPosition(new PhysicalPosition(newX, newY));
    } catch (_) {}

    lightboxOpen = true;
  }

  async function closeLightbox() {
    lightboxOpen = false;
    lightboxSrc = null;
    const win = getCurrentWindow();
    try {
      // Restore the exact pre-lightbox physical size + position (same monitor).
      if (_lightboxPrevSize) {
        await win.setSize(new PhysicalSize(_lightboxPrevSize.w, _lightboxPrevSize.h));
      }
      if (_lightboxPrevPos) {
        await win.setPosition(new PhysicalPosition(_lightboxPrevPos.x, _lightboxPrevPos.y));
      }
    } catch (_) {}
    _lightboxPrevSize = null;
    _lightboxPrevPos  = null;
  }

  // Attach a new thumbnail to a history entry, fading out all previous thumbnails.
  function attachThumb(entryId: number, thumbB64: string) {
    const FADE_MS = 500;
    // Mark existing visible thumbs as fading.
    const toFade = history.filter(h => h.thumb && !h.thumbFading);
    for (const e of toFade) e.thumbFading = true;
    // After the animation, erase their data.
    if (toFade.length > 0) {
      setTimeout(() => {
        for (const e of toFade) { e.thumb = undefined; e.thumbFading = false; }
      }, FADE_MS);
    }
    // Set new thumb.
    const entry = history.find(h => h.id === entryId);
    if (entry) entry.thumb = thumbB64;
  }

  // Whether the global auto-advance setting is on (loaded from config on mount).
  let autoAdvanceEnabled = $state(false);

  // Autopilot on-demand polling.
  let screenChangeDebounce = 0;
  let autopilotInterval: ReturnType<typeof setInterval> | null = null;

  function startAutopilotPolling() {
    if (autopilotInterval !== null) return;
    autopilotInterval = setInterval(async () => {
      // Bail cheaply BEFORE the invoke so we don't hammer GDI capture every
      // 500 ms during AI thinking. The capture (~50 ms each) and IPC overhead
      // were starving the SSE streaming reader and the WebView main thread,
      // making the AI feel slow and the panel laggy whenever autopilot is on.
      if (!autoAdvanceEnabled) return;
      if (phase !== "guiding") return;
      if (steps.length === 0) return;
      if (Date.now() - screenChangeDebounce < 5000) return;
      try {
        const res = await invoke<{ changed: boolean }>("check_screen_changed");
        if (!res.changed) return;
        // Re-check phase after the await — guarding against the small race where
        // a manual Cancel or new task fired while the capture was in flight.
        if (phase !== "guiding") return;
        const currentStep = steps[stepIndex];
        if (!currentStep) return;
        screenChangeDebounce = Date.now();
        addToHistory("system", "Screen changed — checking next step…");
        nextStep();
      } catch (_) {}
    }, 500);
  }

  function stopAutopilotPolling() {
    if (autopilotInterval !== null) {
      clearInterval(autopilotInterval);
      autopilotInterval = null;
    }
  }

  async function checkForUpdates(manual = false) {
    if (updateStatus === "checking" || updateStatus === "downloading") return;
    updateStatus = "checking";
    try {
      const update = await checkUpdate();
      if (update?.available) {
        pendingUpdate = update;
      } else if (manual) {
        pendingUpdate = null;
      }
    } catch (_) {
      // Silently ignore network errors on background check
    } finally {
      if (updateStatus === "checking") updateStatus = "idle";
    }
  }

  async function installUpdate() {
    if (!pendingUpdate || updateStatus === "downloading") return;
    updateStatus = "downloading";
    updateProgress = 0;
    let totalBytes = 0;
    let downloadedBytes = 0;
    try {
      await pendingUpdate.downloadAndInstall((event) => {
        if (event.event === "Started" && event.data.contentLength) {
          totalBytes = event.data.contentLength;
        } else if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength ?? 0;
          if (totalBytes > 0) updateProgress = Math.round((downloadedBytes / totalBytes) * 100);
        } else if (event.event === "Finished") {
          // "Finished" = DOWNLOAD finished. The plugin then calls install()
          // on the next line internally. Do NOT exit here — we'd kill the
          // process before NSIS gets spawned.
          updateStatus = "done";
        }
      });
      // downloadAndInstall has resolved → NSIS has been spawned and is
      // waiting for us to exit so it can replace the locked binary.
      invoke("exit_for_update").catch(() => {});
    } catch (_) {
      updateStatus = "idle";
    }
  }

  function toggleMute() {
    isMuted = !isMuted;
    settingsForm = { ...settingsForm, tts_enabled: !isMuted };
    if (isMuted) invoke("speak", { text: "" }).catch(() => {});
    invoke("save_settings", { payload: settingsForm }).catch(() => {});
    showQuickMenu = false;
  }

  function toggleVoiceInput() {
    if (!settingsForm.voice_input_enabled) {
      addToHistory("error", "Voice input is disabled — enable it in Settings → Audio");
      showQuickMenu = false;
      return;
    }
    if (isRecording) {
      stopVoiceInput();
    } else {
      startVoiceInput();
    }
  }

  function startVoiceInput() {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const SR = (window as any).SpeechRecognition ?? (window as any).webkitSpeechRecognition;
    if (!SR) {
      addToHistory("error", "Speech recognition is not supported in this environment");
      return;
    }
    speechRecognition = new SR();
    speechRecognition.continuous = false;
    speechRecognition.interimResults = false;
    speechRecognition.lang =
      settingsForm.voice_language && settingsForm.voice_language !== "auto"
        ? settingsForm.voice_language
        : navigator.language || "en-US";
    isRecording = true;

    speechRecognition.onresult = (event: any) => {
      const transcript = (event.results[0][0].transcript as string).trim();
      isRecording = false;
      if (transcript) {
        task = transcript;
        guide();
      }
    };
    speechRecognition.onerror = () => { isRecording = false; };
    speechRecognition.onend   = () => { isRecording = false; };
    speechRecognition.start();
  }

  function stopVoiceInput() {
    if (speechRecognition) { speechRecognition.stop(); speechRecognition = null; }
    isRecording = false;
  }

  async function quickToggleSubtitle() {
    settingsForm.subtitle_enabled = !settingsForm.subtitle_enabled;
    await emitTo("overlay", "overlay:theme", {
      color: settingsForm.overlay_color,
      thickness: settingsForm.overlay_thickness,
      subtitle_enabled: settingsForm.subtitle_enabled,
      show_ai_bbox: settingsForm.debug_show_ai_bbox,
    });
    showQuickMenu = false;
  }

  async function quickClearScreen() {
    isOverlayCleared = true;
    await invoke("clear_overlay").catch(() => {});
    showQuickMenu = false;
  }

  async function quickShowScreen() {
    isOverlayCleared = false;
    await invoke("restore_overlay").catch(() => {});
    await emitTo("overlay", "overlay:theme", {
      color: settingsForm.overlay_color,
      thickness: settingsForm.overlay_thickness,
      subtitle_enabled: settingsForm.subtitle_enabled,
      show_ai_bbox: settingsForm.debug_show_ai_bbox,
    });
    showQuickMenu = false;
  }

  function cancelRequest() {
    requestToken++;
    stopTimer();
    invoke("clear_overlay").catch(() => {});
    invoke("speak", { text: "" }).catch(() => {});
    staleResponse = false;
    phase = "idle";
  }

  function closeWindow() {
    getCurrentWindow().close();
  }

  // data-tauri-drag-region is unreliable on WebView2; use startDragging() instead.
  async function handleHeaderMousedown(e: MouseEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    try { await getCurrentWindow().startDragging(); } catch (_) {}
  }

  // Icon drag: track movement so a stationary click still reaches onclick.
  // startDragging() is only called once the mouse moves > 4px — below that
  // threshold the OS drag never starts and the browser fires onclick normally.
  let _iconStartX = 0, _iconStartY = 0, _iconDragged = false;
  function handleIconPointerdown(e: PointerEvent) {
    if (e.button !== 0) return;
    _iconStartX = e.screenX; _iconStartY = e.screenY; _iconDragged = false;
  }
  async function handleIconPointermove(e: PointerEvent) {
    if (_iconDragged || e.buttons !== 1) return;
    if (Math.hypot(e.screenX - _iconStartX, e.screenY - _iconStartY) > 4) {
      _iconDragged = true;
      try { await getCurrentWindow().startDragging(); } catch (_) {}
    }
  }
  function handleIconClick() {
    if (!_iconDragged) expandToPanel();
  }

  async function collapseToIcon() {
    iconMode = true;
    try { await getCurrentWindow().setSize(new LogicalSize(ICON_SIZE, ICON_SIZE)); }
    catch (e) { console.error("collapseToIcon:", e); }
  }

  async function expandToPanel() {
    iconMode = false;
    try { await getCurrentWindow().setSize(new LogicalSize(PANEL_W, PANEL_H)); }
    catch (e) { console.error("expandToPanel:", e); }
  }

  async function openSettings() {
    settingsError = null;
    settingsSaved = false;
    showKeyAnthropic = false; showKeyGemini = false; showKeyOpenAI = false; showKeyDeepSeek = false; showKeyQwen = false;
    showSettings = true;
    // Load voices and settings in parallel, but wait for both before assigning
    // settingsForm. If we assign settingsForm while availableVoices is still empty,
    // the <select bind:value> finds no matching <option> for the saved tts_voice
    // and silently resets it to "" (system default). The bound value then
    // overwrites the saved choice the next time the user clicks Apply.
    try {
      const [data, voices] = await Promise.all([
        invoke<SettingsPayload>("get_settings"),
        invoke<VoiceInfo[]>("list_tts_voices").catch(() => [] as VoiceInfo[]),
      ]);
      availableVoices = voices;
      // Keep the live auto_advance state — the Pause/Resume button may have
      // changed it since the last disk save, and the button is the source of truth.
      settingsForm = { ...data, auto_advance: autoAdvanceEnabled };
      debugShowInfo = data.debug_show_response_info;
    } catch (e) {
      settingsError = String(e);
    }
  }

  // Re-register global shortcuts. Called on mount and after settings change.
  async function registerShortcuts(hk: Pick<SettingsPayload, "hotkey_next"|"hotkey_wrong"|"hotkey_pause"|"hotkey_icon"|"hotkey_talk">) {
    await unregisterAll().catch(() => {});
    function debounced(fn: () => void, ms = 350): () => void {
      let last = 0;
      return () => { const now = Date.now(); if (now - last < ms) return; last = now; fn(); };
    }
    const pairs: Array<[string, () => void]> = [
      [hk.hotkey_next,  debounced(() => { if (!actionDisabled) nextStep(); })],
      [hk.hotkey_wrong, debounced(() => { if (!actionDisabled) openWrongPicker(); })],
      [hk.hotkey_pause, debounced(() => cancelRequest())],
      [hk.hotkey_icon,  debounced(() => { if (iconMode) expandToPanel(); else collapseToIcon(); })],
      [hk.hotkey_talk,  debounced(() => { if (settingsForm.voice_input_enabled) toggleVoiceInput(); })],
    ];
    const errors: string[] = [];
    for (const [key, handler] of pairs) {
      if (!key) continue;
      try { await register(key, handler); }
      catch (e) { errors.push(`${key}: ${e}`); console.warn("shortcut failed:", key, e); }
    }
    return errors;
  }

  function resetSettings() {
    // Restore everything to defaults but preserve API keys so the user
    // doesn't lose credentials they've already entered.
    const preserved = {
      anthropic_api_key: settingsForm.anthropic_api_key,
      gemini_api_key: settingsForm.gemini_api_key,
      openai_api_key: settingsForm.openai_api_key,
      deepseek_api_key: settingsForm.deepseek_api_key,
      qwen_api_key: settingsForm.qwen_api_key,
    };
    settingsForm = { ...SETTINGS_DEFAULTS, ...preserved };
    settingsError = null;
    settingsSaved = false;
  }

  async function applySettings() {
    settingsSaving = true;
    settingsError = null;
    settingsSaved = false;
    try {
      await invoke("save_settings", { payload: settingsForm });
      provider = settingsForm.api_provider;
      autoAdvanceEnabled = settingsForm.auto_advance;
      if (autoAdvanceEnabled) startAutopilotPolling(); else stopAutopilotPolling();
      isMuted = !settingsForm.tts_enabled;
      debugShowInfo = settingsForm.debug_show_response_info;
      await emitTo("overlay", "overlay:theme", {
        color: settingsForm.overlay_color,
        thickness: settingsForm.overlay_thickness,
        subtitle_enabled: settingsForm.subtitle_enabled,
        show_ai_bbox: settingsForm.debug_show_ai_bbox,
      });
      const hkErrors = await registerShortcuts(settingsForm);
      if (hkErrors.length) {
        settingsError = `Saved, but hotkey registration failed: ${hkErrors.join("; ")}`;
      } else {
        settingsSaved = true;
        setTimeout(() => { settingsSaved = false; }, 2000);
      }
      if (activeModel && activeModel !== lastAppliedModel) {
        addToHistory("system", `Switched to ${activeModel}`);
        lastAppliedModel = activeModel;
      }
    } catch (e) {
      settingsError = String(e);
    } finally {
      settingsSaving = false;
    }
  }

  async function newSession() {
    cancelRequest();
    // Reset Rust-side session state including target_hwnd so the next Guide me
    // call re-discovers the foreground window instead of reusing a stale target.
    await invoke("new_session").catch(() => {});
    task = "";
    steps = [];
    stepIndex = 0;
    currentInstruction = "";
    locateResult = null;
    locateTrace = null;
    sessionId = "";
    staleResponse = false;
    history = [];
    await addToHistory("system", "New session started");
  }

  function applyResponse(res: GuideResponse, idx: number, token: number) {
    if (token !== requestToken) return;
    steps = res.steps;
    stepIndex = idx;
    currentInstruction = res.instruction;
    locateResult = res.located;
    locateTrace = res.locate_trace;
    sessionId = res.session_id;
    if (res.provider) provider = res.provider;
    if (res.request_full_screen) {
      phase = "consent_prompt";
    } else {
      phase = res.needs_input ? "needs_input" : "guiding";
    }
    if (res.instruction) {
      const cleanInstruction = res.instruction;
      let meta: string | undefined;
      if (res.located) {
        meta = `${res.located.role} · ${(res.located.confidence * 100).toFixed(0)}% · ${res.located.name}`;
      } else if (steps[idx]?.target_text) {
        meta = `not located · "${steps[idx].target_text}"`;
      }
      addToHistory("ai", cleanInstruction, meta);
      if (!isMuted) invoke("speak", { text: cleanInstruction, lang: settingsForm.voice_language }).catch(() => {});
    }
    if (res.debug_screenshot_path) {
      addToHistory("system", `📷 ${res.debug_screenshot_path}`);
    }
  }

  function allowFullScreen() {
    guide_impl(true);
  }

  function denyFullScreen() {
    task = "Permission to capture full screen was denied. Please ask me to manually bring the required application into focus.";
    correction();
  }

  async function guide() {
    guide_impl(false);
  }

  async function guide_impl(fullScreen: boolean) {
    if (!task.trim() && !fullScreen) return;
    const taskText = fullScreen ? "[User granted permission to capture full desktop for the next step]" : task.trim();
    task = "";
    // Keep session context when in the middle of a task; start fresh from idle/error.
    const isReply = phase === "guiding" || phase === "needs_input" || phase === "consent_prompt";
    const prevPhase = phase;
    const userEntryId = await addToHistory("user", taskText);
    currentInstruction = "";
    staleResponse = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("guide", { task: taskText, isReply, fullScreen });
      stopTimer();
      if (token !== requestToken) return;
      if (res.chat_thumb_b64) attachThumb(userEntryId, res.chat_thumb_b64);
      if (!res.ok) {
        phase = prevPhase;
        addToHistory("system", "⚠️ " + (res.error ?? "guide failed"));
        if (!fullScreen && taskText !== "") task = taskText;
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      addToHistory("system", "⚠️ " + String(e));
      if (!fullScreen && taskText !== "") task = taskText;
    }
  }

  async function nextStep() {
    // Don't allow next while an AI call is in flight — the hotkey can fire
    // even when the Next button is disabled (Svelte derived state edge case).
    if (phase === "thinking") return;
    // Pressing Next means the current step worked → implicit success signal.
    if (phase === "guiding") logFeedback("worked", "");
    const nextIdx = stepIndex + 1;
    const prevPhase = phase;
    if (nextIdx >= steps.length) {
      // Re-query AI — tell it what was just completed so it doesn't repeat.
      const completed = currentInstruction || lastCompletedInstruction;
      lastCompletedInstruction = completed;
      currentInstruction = "";
      phase = "thinking";
      startTimer();
      const token = ++requestToken;
      // Create a history entry so the screenshot thumbnail has somewhere to live.
      const reQueryId = await addToHistory("system",
        completed ? `✓ Completed — re-analysing…` : "Re-analysing…");
      try {
        const res = await invoke<GuideResponse>("guide", {
          task: completed ? `[User completed: "${completed}"]` : "",
          isReply: false,
        });
        stopTimer();
        if (token !== requestToken) return;
        if (res.chat_thumb_b64) attachThumb(reQueryId, res.chat_thumb_b64);
        if (!res.ok) {
          phase = prevPhase;
          addToHistory("system", "⚠️ " + (res.error ?? "re-query failed"));
          return;
        }
        applyResponse(res, 0, token);
      } catch (e) {
        stopTimer();
        if (token !== requestToken) return;
        phase = prevPhase;
        addToHistory("system", "⚠️ " + String(e));
      }
      return;
    }

    lastCompletedInstruction = currentInstruction;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("next_step", { stepIndex: nextIdx });
      stopTimer();
      if (token !== requestToken) return;
      applyResponse(res, nextIdx, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      addToHistory("system", "⚠️ " + String(e));
    }
  }

  async function correction(category?: string) {
    const rawNote = task.trim();
    if (rawNote) task = "";
    // Fold a steering hint for the reason into the note the AI sees, then the
    // user's own text. (The logged note is the user's raw text only.)
    const hint = category ? (CATEGORY_HINT[category] ?? "") : "";
    const note = [hint, rawNote].filter(Boolean).join(" ").trim();
    const label = (category && CATEGORY_LABEL[category]) || "Wrong";
    const prevPhase = phase;
    const corrEntryId = await addToHistory("correction", rawNote ? `${label} — ${rawNote}` : `${label} — re-analysing…`);
    currentInstruction = "";
    staleResponse = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction", { note: note || null });
      stopTimer();
      if (token !== requestToken) return;
      if (res.chat_thumb_b64) attachThumb(corrEntryId, res.chat_thumb_b64);
      if (!res.ok) {
        phase = prevPhase;
        addToHistory("system", "⚠️ " + (res.error ?? "correction failed"));
        if (rawNote !== "") task = rawNote;
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      addToHistory("system", "⚠️ " + String(e));
      if (rawNote !== "") task = rawNote;
    }
  }

  // Best-effort test-user feedback → Supabase (see submit_feedback in lib.rs).
  // "worked" on Next; a reason category on Wrong. Failures are ignored.
  async function logFeedback(kind: string, note: string) {
    try {
      await invoke("submit_feedback", {
        payload: {
          kind,
          note: note || null,
          app_version: appVersion,
          provider: settingsForm.api_provider,
          model: activeModel,
          instruction: currentInstruction || null,
          target_text: steps[stepIndex]?.target_text ?? null,
          located: !!locateResult,
          locate_role: locateResult?.role ?? null,
          locate_conf: locateResult?.confidence ?? null,
          app_window: sharedApp ? (friendlyName(sharedApp.exe_name) || sharedApp.app_name) : null,
          session_id: sessionId || null,
        },
      });
    } catch (_) {
      /* offline / not signed in / not configured — feedback is best-effort */
    }
  }

  // Wrong button → log the reason, then re-analyse with that reason as a hint.
  async function submitWrong(category: string) {
    wrongPickerOpen = false;
    const note = task.trim();
    logFeedback(category, note);
    await correction(category);
  }

  function openWrongPicker() {
    if (phase === "guiding") wrongPickerOpen = true;
    else correction();
  }

  // About → Send feedback: open the user's mail client with version + provider
  // prefilled so long-form reports arrive with context.
  function openFeedbackEmail() {
    const subject = `Navisual feedback (v${appVersion})`;
    const body = [
      "What went wrong / what would you like to see?",
      "",
      "",
      "—",
      `App version: v${appVersion}`,
      `Provider: ${settingsForm.api_provider}`,
      `Model: ${activeModel}`,
    ].join("\n");
    openUrl(
      `mailto:feedback@navisualguide.com?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`,
    );
  }

  // Textarea submit: while the Wrong picker is open, a typed message is itself a
  // "wrong" report (logged as wrong_other) rather than a normal follow-up.
  function submitTask() {
    if (wrongPickerOpen && task.trim()) submitWrong("wrong_other");
    else guide();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !isThinking && task.trim()) {
      e.preventDefault();
      submitTask();
    }
  }

  // "paused" = auto-advance is on but we're currently idle with an active session.
  let isPaused = $derived(autoAdvanceEnabled && phase === "idle" && steps.length > 0);

  let statusLabel = $derived(
    isPaused              ? `paused · step ${stepIndex + 1}/${steps.length}`
    : phase === "idle"    ? "idle"
    : phase === "thinking"  ? `thinking`
    : phase === "guiding"   ? `step ${stepIndex + 1}/${steps.length}`
    : phase === "needs_input" ? "needs input"
    : phase === "consent_prompt" ? "needs permission"
    : "error"
  );

  // Next/Wrong enabled whenever there's a live session (guiding, needs_input, or idle with steps).
  let actionDisabled = $derived(phase === "thinking" || phase === "error" || (phase === "idle" && steps.length === 0));
  let isThinking = $derived(phase === "thinking");
  let activeModel = $derived(
    settingsForm.api_provider === "anthropic" ? settingsForm.anthropic_model
    : settingsForm.api_provider === "gemini" ? settingsForm.gemini_model
    : settingsForm.api_provider === "ollama" ? settingsForm.ollama_model
    : settingsForm.api_provider === "deepseek" ? settingsForm.deepseek_model
    : settingsForm.api_provider === "qwen" ? settingsForm.qwen_model
    : settingsForm.api_provider === "managed" ? "managed"
    : settingsForm.openai_model
  );
  let headerLabel = $derived(activeModel || provider);
  let lastAppliedModel = $state<string>("");

  onMount(async () => {
    getVersion().then(v => { appVersion = v; }).catch(() => {});
    setTimeout(() => checkForUpdates(), 5000);

    // S5 — first-run privacy disclosure. Shown once per install; the user's
    // acknowledgement is persisted in localStorage (lives in WebView2 user
    // data, removed by uninstall).
    try {
      if (!localStorage.getItem(PRIVACY_DISCLOSURE_KEY)) {
        showPrivacyDisclosure = true;
      }
    } catch (_) {}

    // Position bottom-right then show — panel starts hidden (visible:false in
    // tauri.conf.json) so the user never sees a blank frame at 0,0 while
    // WebView2 initialises. We show only once the UI is fully painted.
    // Load initial config so hotkeys, autoAdvance, and provider are correct from startup.
    let initHotkeys: Pick<SettingsPayload, "hotkey_next"|"hotkey_wrong"|"hotkey_pause"|"hotkey_icon"|"hotkey_talk"> = {
      hotkey_next: SETTINGS_DEFAULTS.hotkey_next,
      hotkey_wrong: SETTINGS_DEFAULTS.hotkey_wrong,
      hotkey_pause: SETTINGS_DEFAULTS.hotkey_pause,
      hotkey_icon: SETTINGS_DEFAULTS.hotkey_icon,
      hotkey_talk: SETTINGS_DEFAULTS.hotkey_talk,
    };
    try {
      const init = await invokeReady<SettingsPayload>("get_settings");
      // Autopilot always starts OFF — last-session state is intentionally not
      // restored. Surprise-autopilot on launch is jarring and burns API credits
      // before the user has a chance to opt in.
      autoAdvanceEnabled = false;
      isMuted = !init.tts_enabled;
      if (init.api_provider) provider = init.api_provider;
      settingsForm = { ...SETTINGS_DEFAULTS, ...init, auto_advance: false };
      initHotkeys = init;
    } catch (_) {}

    try {
      const sw = window.screen.availWidth;
      const sh = window.screen.availHeight;
      const margin = 24;
      await getCurrentWindow().setPosition(
        new LogicalPosition(sw - PANEL_W - margin, sh - PANEL_H - margin)
      );
    } catch (_) {}
    try { await getCurrentWindow().show(); } catch (_) {}

    // Sync the overlay theme from saved settings so the show_ai_bbox toggle
    // is active from the first guide call without requiring the user to open
    // Settings → Apply every session.
    emitTo("overlay", "overlay:theme", {
      color: settingsForm.overlay_color,
      thickness: settingsForm.overlay_thickness,
      subtitle_enabled: settingsForm.subtitle_enabled,
      show_ai_bbox: settingsForm.debug_show_ai_bbox,
    }).catch(() => {});

    listen<{ delta: string }>("stream_chunk", (event) => {
      if (phase === "thinking" || phase === "guiding") {
        currentInstruction += event.payload.delta;
      }
    });

    // Phase 0.2: keep the "Shared: <App>" header chip in sync with whatever
    // window the backend is capturing.
    listen<SharedAppInfo>("app_changed", (event) => {
      sharedApp = event.payload;
    });
    try {
      const initial = await invokeReady<SharedAppInfo | null>("get_shared_app_info");
      if (initial) sharedApp = initial;
    } catch (_) {}

    // E.3 — Autopilot: on-demand screen-change polling.
    // Functions are defined at module level; start now if already enabled.
    if (autoAdvanceEnabled) startAutopilotPolling();

    await registerShortcuts(initHotkeys);

    // (Removed: hardcoded Ctrl+A push-to-talk global shortcut. It hijacked the
    // OS-wide "select all" combo so users couldn't Ctrl+A in Word or any other
    // app while Navisual was running. Voice input remains available via the
    // mic button in the action row.)

    // S.1 — Managed provider: anonymous sign-in on first launch.
    if (settingsForm.api_provider === "managed") {
      try {
        await invokeReady("sign_in_anon");
      } catch (e) {
        addToHistory("system", "⚠️ Managed sign-in failed: " + String(e));
      }
      try {
        const bal = await invokeReady<{ tier: string; free_remaining: number }>("get_balance");
        freeRemaining = bal.free_remaining;
      } catch (_) {}
    }

    listen<number>("balance_update", (event) => {
      freeRemaining = event.payload;
      if (freeRemaining <= 0) showTrialExhausted = true;
    });

    listen("trial_exhausted", () => {
      freeRemaining = 0;
      showTrialExhausted = true;
    });

    // Backend detected the screen drifted enough during AI thinking
    // (Hamming distance ≥ STALE_RESPONSE_THRESHOLD between pre-call and
    // post-response captures) that the rendered guidance may not match
    // what's on screen any more.
    listen("ai_response_stale", () => {
      staleResponse = true;
    });

    lastAppliedModel = activeModel;
    await addToHistory("system", `Navisual ready — using ${activeModel}`);
  });

  onDestroy(async () => {
    stopAutopilotPolling();
    await unregisterAll().catch(() => {});
  });
</script>

{#if iconMode}
  <!-- Icon mode: goldfish icon — mousedown starts drag; click expands -->
  <button
    class="icon-btn"
    onclick={handleIconClick}
    onpointerdown={handleIconPointerdown}
    onpointermove={handleIconPointermove}
    title="Expand Navisual (Ctrl+Q)"
  >
    <img src="/goldfish.svg" class="icon-fish" alt="Navisual" draggable="false" />
  </button>
{:else}
  <main>
    <!-- Title bar: onmousedown → startDragging() (more reliable than data-tauri-drag-region on WebView2) -->
    <div class="titlebar" role="toolbar" tabindex="-1" data-tauri-drag-region onmousedown={handleHeaderMousedown}>
      <span class="header-dot"></span>
      <span class="header-title">Navisual</span>
      {#if sharedApp}
        <button
          class="header-shared"
          class:header-shared-pinned={pinnedHwnd !== null}
          title={pinnedHwnd !== null ? "Target pinned — click to change" : "Click to choose target app"}
          onmousedown={(e) => e.stopPropagation()}
          onclick={openTargetPicker}
        >
          <span class="header-shared-dot"></span>
          {friendlyName(sharedApp.exe_name) || sharedApp.app_name}
          {#if pinnedHwnd !== null}<span class="header-shared-pin">📌</span>{/if}
        </button>
      {/if}
      {#if settingsForm.api_provider === "managed" && freeRemaining !== null}
        <span class="header-balance" class:header-balance-low={freeRemaining <= 5}>{freeRemaining} left</span>
      {/if}
      {#if pendingUpdate}
        <button class="header-update" onclick={() => (showAbout = true)} title="Update available">
          ↑ {pendingUpdate.version}
        </button>
      {/if}
      <div class="header-actions">
        <button class="hdr-btn" onclick={() => (showAbout = true)} title="About Navisual" aria-label="About Navisual">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
        </button>
        <button class="hdr-btn" onclick={openSettings} title="Settings" aria-label="Settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
        </button>
        <button class="hdr-btn" onclick={collapseToIcon} title="Collapse to floating icon" aria-label="Collapse to floating icon">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="5" width="18" height="14" rx="2"/><rect x="12.5" y="12" width="7" height="5.5" rx="1" fill="currentColor" stroke="none"/></svg>
        </button>
        <button class="hdr-btn hdr-btn-close" onclick={closeWindow} title="Quit" aria-label="Quit">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
        </button>
      </div>
    </div>

    <!-- Latest instruction (visible when guiding) -->
    {#if currentInstruction && (phase === "guiding" || phase === "needs_input" || (isThinking && currentInstruction))}
      <section class="latest-box">
        <div class="latest-header">
          <span class="step-counter">Step {stepIndex + 1} of {steps.length}</span>
          {#if steps[stepIndex]?.clipboard}
            <span class="badge badge-clip" title="Text copied to clipboard">📋 copied</span>
          {/if}
        </div>
        {#if staleResponse && phase !== "thinking"}
          <div class="stale-banner" role="status">
            <span class="stale-icon">⚠</span>
            <span class="stale-text">Screen changed while I was thinking — this guidance may be out of date.</span>
            <button class="stale-action" onclick={() => { staleResponse = false; correction(); }} title="Re-analyse the current screen">↻ Re-analyse</button>
            <button class="stale-dismiss" onclick={() => (staleResponse = false)} title="Dismiss">✕</button>
          </div>
        {/if}
        <p class="latest-text">{currentInstruction}</p>

        <!-- D6: subtle miss note — only when a target was expected but not found -->
        {#if !locateResult && steps[stepIndex]?.target_text && phase === "guiding"}
          <p class="miss-note">⊘ Pointer unavailable — follow the instruction above</p>
        {/if}

        <!-- Feedback: mark this step wrong (promoted from the ··· quick-menu) -->
        {#if phase === "guiding"}
          <div class="wrong-footer">
            {#if !wrongPickerOpen}
              <button class="wrong-btn" onclick={() => (wrongPickerOpen = true)} title="This guidance is wrong (Ctrl+E)">✗ This is wrong</button>
            {:else}
              <div class="reason-row">
                <span class="reason-prompt">What went wrong?</span>
                <button class="reason-cancel" onclick={() => (wrongPickerOpen = false)} title="Cancel" aria-label="Cancel">✕</button>
              </div>
              <div class="reason-chips">
                <button class="reason-chip" onclick={() => submitWrong("wrong_instruction")}>Wrong instruction</button>
                <button class="reason-chip" onclick={() => submitWrong("wrong_spot")}>Wrong spot</button>
                <button class="reason-chip" onclick={() => submitWrong("not_found")}>Can't find it</button>
                <button class="reason-chip" onclick={() => submitWrong("already_done")}>Already did that</button>
              </div>
              <p class="feedback-hint">Not one of these? Type what's wrong below, then ↩ Follow up.</p>
              <p class="feedback-note">Shared with the Navisual team to improve guidance — never your screen or request text.</p>
            {/if}
          </div>
        {/if}

        <!-- Phase 0.1: locate-trace debug drawer -->
        {#if settingsForm.debug_locate_trace_enabled && locateTrace}
          <div class="debug-drawer">
            <button class="debug-toggle" onclick={() => (debugDrawerOpen = !debugDrawerOpen)}>
              {debugDrawerOpen ? "▾" : "▸"} Debug · {locateTrace.final_decision.kind} · {locateTrace.elapsed_ms} ms
            </button>
            {#if debugDrawerOpen}
              <div class="debug-body">
                <div class="debug-row">
                  <span class="debug-key">located</span>
                  {#if locateResult}
                    <span class="debug-val">
                      <span class="badge badge-{locateResult.role === 'Ocr' ? 'warn' : 'ok'}">{locateResult.role}</span>
                      <span class="conf">{(locateResult.confidence * 100).toFixed(0)}%</span>
                      · "{locateResult.name}"
                    </span>
                  {:else}
                    <span class="debug-val">not located</span>
                  {/if}
                </div>
                <div class="debug-row">
                  <span class="debug-key">target</span>
                  <span class="debug-val">"{locateTrace.target_text}"</span>
                </div>
                {#if locateTrace.target_role}
                  <div class="debug-row">
                    <span class="debug-key">role</span>
                    <span class="debug-val">{locateTrace.target_role}</span>
                  </div>
                {/if}
                {#if locateTrace.nearby_text}
                  <div class="debug-row">
                    <span class="debug-key">nearby</span>
                    <span class="debug-val">"{locateTrace.nearby_text}"</span>
                  </div>
                {/if}
                {#if locateTrace.ai_bbox}
                  <div class="debug-row">
                    <span class="debug-key">ai_bbox</span>
                    <span class="debug-val">{locateTrace.ai_bbox.x}, {locateTrace.ai_bbox.y} · {locateTrace.ai_bbox.width}×{locateTrace.ai_bbox.height}</span>
                  </div>
                {/if}

                <!-- A11y section -->
                <div class="debug-section">
                  <div class="debug-section-head">
                    A11y · {locateTrace.a11y.candidates.length} candidate{locateTrace.a11y.candidates.length === 1 ? "" : "s"}
                    {#if locateTrace.a11y.timed_out} · timed out{/if}
                    · {locateTrace.a11y.elapsed_ms} ms
                  </div>
                  {#if locateTrace.a11y.regex_used}
                    <div class="debug-mono">{locateTrace.a11y.regex_used}</div>
                  {/if}
                  {#each locateTrace.a11y.candidates as c}
                    <div class="debug-cand {c.selected ? 'cand-selected' : 'cand-rejected'}">
                      <span class="cand-mark">{c.selected ? "✔" : "·"}</span>
                      <span class="cand-text">"{c.name}"</span>
                      <span class="cand-meta">{c.role}</span>
                      {#if c.reject_reason}<span class="cand-reason">— {c.reject_reason}</span>{/if}
                    </div>
                  {/each}
                </div>

                <!-- OCR section -->
                {#if locateTrace.ocr.ran}
                  <div class="debug-section">
                    <div class="debug-section-head">
                      OCR · {locateTrace.ocr.line_count} line{locateTrace.ocr.line_count === 1 ? "" : "s"}, {locateTrace.ocr.word_count} word{locateTrace.ocr.word_count === 1 ? "" : "s"}
                      {#if locateTrace.ocr.strategy_used} · {locateTrace.ocr.strategy_used}{/if}
                      · {locateTrace.ocr.elapsed_ms} ms
                    </div>
                    {#each locateTrace.ocr.candidates as c}
                      <div class="debug-cand {c.selected ? 'cand-selected' : 'cand-rejected'}">
                        <span class="cand-mark">{c.selected ? "✔" : "·"}</span>
                        <span class="cand-text">"{c.text}"</span>
                        <span class="cand-meta">{c.strategy}{c.score !== null ? ` ${(c.score * 100).toFixed(0)}%` : ""}</span>
                        {#if c.reject_reason}<span class="cand-reason">— {c.reject_reason}</span>{/if}
                      </div>
                    {/each}
                    {#if locateTrace.ocr.sample_texts.length > 0}
                      <details class="debug-samples">
                        <summary>OCR sample ({locateTrace.ocr.sample_texts.length} of first 30)</summary>
                        <ul>
                          {#each locateTrace.ocr.sample_texts as s}
                            <li>"{s}"</li>
                          {/each}
                        </ul>
                      </details>
                    {/if}
                  </div>
                {/if}

                <!-- C5 hit-test rejection detail -->
                {#if locateTrace.final_decision.kind === "rejected_by_hit_test"}
                  <div class="debug-section">
                    <div class="debug-section-head" style="color: #f59e0b">⊘ C5 hit-test rejected</div>
                    <div class="debug-row">
                      <span class="debug-key">leaf class</span>
                      <span class="debug-val">{locateTrace.final_decision.leaf_class}</span>
                    </div>
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    <!-- History -->
    <div class="history" bind:this={historyEl}>
      {#each history as entry (entry.id)}
        <div class="h-entry h-{entry.role}">
          <span class="h-label">
            {entry.role === "user" ? "You"
            : entry.role === "ai" ? "AI"
            : entry.role === "correction" ? "Wrong"
            : entry.role === "error" ? "Error"
            : "·"}
          </span>
          <div class="h-body">
            <span class="h-text">{entry.text}</span>
            {#if entry.meta && debugShowInfo}
              <span class="h-meta">{entry.meta}</span>
            {/if}
          </div>
          {#if entry.thumb}
            <button
              class="h-thumb-btn"
              class:h-thumb-fading={entry.thumbFading}
              onclick={openLightbox}
              title="Click to view full screenshot"
            >
              <img class="h-thumb" src="data:image/jpeg;base64,{entry.thumb}" alt="screenshot" />
            </button>
          {/if}
        </div>
      {/each}
      {#if isThinking}
        <div class="h-entry h-system h-thinking">
          <span class="h-label">·</span>
          <span class="h-text thinking-dots">Thinking…</span>
        </div>
      {/if}
    </div>

    <!-- Task input — always enabled; Enter submits, isReply detected from phase -->
    <section class="task-section">
      {#if phase === "consent_prompt"}
        <div class="consent-box" style="padding: 8px; font-size: 0.9em; line-height: 1.4; color: var(--fg);">
          <p style="margin: 0 0 8px 0;">🛡️ <strong>Permission Request</strong><br/>
            The AI needs to look outside your current window to find what you're looking for. Allow Navisual to capture your entire screen for this next step?</p>
          <div style="display: flex; gap: 8px;">
            <button class="btn-primary" style="flex: 1" onclick={allowFullScreen}>Allow Once</button>
            <button class="btn-ghost" style="flex: 1" onclick={denyFullScreen}>Deny</button>
          </div>
        </div>
      {:else}
        {#if phase === "needs_input"}
          <div class="input-hint">💬 AI needs your input — type your answer below</div>
        {:else if phase === "guiding"}
          <div class="input-hint">Type a follow-up or correction · ＋ for a new task</div>
        {/if}
        <textarea
          bind:value={task}
          onkeydown={handleKeydown}
          placeholder={phase === "needs_input" ? "Type your answer…" : "What do you need help with?"}
          rows={2}
        ></textarea>
        {#if isThinking}
          <button class="btn-ghost btn-full" onclick={cancelRequest}>⏹ Cancel ({(elapsedMs / 1000).toFixed(1)}s)</button>
        {:else}
          <button class="btn-primary btn-full" onclick={submitTask} disabled={!task.trim()}>
            {phase === "needs_input" ? "↩ Send answer" : phase === "guiding" ? "↩ Follow up" : "Guide me"}
          </button>
        {/if}
      {/if}
    </section>

    <!-- Quick-action menu (opened by ··· button) -->
    {#if showQuickMenu}
      <div class="quick-menu">
        <button class="qm-btn" class:qm-active={isMuted} onclick={toggleMute}>
          {isMuted ? "🔇 Unmute" : "🔊 Mute"}
        </button>
        <button class="qm-btn" class:qm-active={settingsForm.subtitle_enabled} onclick={quickToggleSubtitle}>
          💬 {settingsForm.subtitle_enabled ? "Caption: on" : "Caption: off"}
        </button>
        {#if isOverlayCleared}
          <button class="qm-btn" onclick={quickShowScreen}>
            👁 Show
          </button>
        {:else}
          <button class="qm-btn" onclick={quickClearScreen}>
            ✕ Clear
          </button>
        {/if}
      </div>
    {/if}

    <!-- Action row: Next · Autopilot · New Task · 🎤 · ··· -->
    <div class="action-row">
      <button class="btn-action btn-next" onclick={nextStep} disabled={actionDisabled} title="Next step (Ctrl+`)">
        → Next
      </button>
      <button class="btn-action {autoAdvanceEnabled ? 'btn-pause' : 'btn-resume'}"
        onclick={() => {
          autoAdvanceEnabled = !autoAdvanceEnabled;
          settingsForm = { ...settingsForm, auto_advance: autoAdvanceEnabled };
          invoke("save_settings", { payload: settingsForm }).catch(() => {});
          if (autoAdvanceEnabled) startAutopilotPolling(); else stopAutopilotPolling();
        }}
        title={autoAdvanceEnabled ? "Autopilot on — click to turn off" : "Autopilot off — click to turn on"}>
        {autoAdvanceEnabled ? "⏸ Autopilot" : "✈ Autopilot"}
      </button>
      <button class="btn-action btn-new" onclick={newSession} title="Clear session and start fresh">
        ＋ New task
      </button>
      <button class="btn-action btn-mic" class:btn-mic-active={isRecording}
        onclick={toggleVoiceInput}
        disabled={!settingsForm.voice_input_enabled}
        title={settingsForm.voice_input_enabled ? (isRecording ? `Stop recording (${settingsForm.hotkey_talk})` : `Voice input (${settingsForm.hotkey_talk})`) : "Enable voice input in Settings → Audio"}>
        🎤
      </button>
      <button class="btn-action btn-more" class:btn-more-open={showQuickMenu}
        onclick={() => { showQuickMenu = !showQuickMenu; }}
        title="More actions">
        ···
      </button>
    </div>

    <!-- Status + shortcut legend -->
    <footer>
      <div class="status-row">
        <span class="status-dot status-{phase}"></span>
        <span class="status-label">{statusLabel}</span>
        {#if sessionId}
          <span class="session-id">{sessionId.slice(0, 8)}</span>
        {/if}
      </div>
      <div class="shortcut-legend">
        <span>{settingsForm.hotkey_next} <span class="hk-label">Next</span></span>
        <span>{settingsForm.hotkey_wrong} <span class="hk-label">Wrong</span></span>
        <span>{settingsForm.hotkey_pause} <span class="hk-label">Pause</span></span>
        <span>{settingsForm.hotkey_icon} <span class="hk-label">Icon</span></span>
      </div>
    </footer>
  </main>

  <!-- Target-window picker dropdown (item 1) — fixed so it escapes main's overflow:hidden -->
  {#if targetPickerOpen}
    <div class="target-picker-backdrop" role="presentation" onclick={() => (targetPickerOpen = false)}></div>
    <div class="target-picker" role="listbox" aria-label="Choose target app">
      <button class="target-pick-item" class:target-pick-selected={pinnedHwnd === null} onclick={() => selectTarget(null)}>
        <span class="target-pick-check">{pinnedHwnd === null ? "✓" : ""}</span>
        <span class="target-pick-name">Auto-detect</span>
        <span class="target-pick-sub">follow the foreground window</span>
      </button>
      {#each targetWindows as w (w.hwnd)}
        <button class="target-pick-item" class:target-pick-selected={pinnedHwnd === w.hwnd} onclick={() => selectTarget(w.hwnd)}>
          <span class="target-pick-check">{pinnedHwnd === w.hwnd ? "✓" : ""}</span>
          <span class="target-pick-name">{w.display_name}</span>
          {#if w.title && w.title !== w.display_name}
            <span class="target-pick-sub">{w.title.length > 40 ? w.title.slice(0, 38) + "…" : w.title}</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}

  <!-- Screenshot lightbox — panel window is temporarily expanded to fit -->
  {#if lightboxOpen}
    <div class="lightbox-backdrop" role="presentation" onclick={closeLightbox}>
      {#if lightboxLoading}
        <span class="lightbox-loading">Loading…</span>
      {:else if lightboxSrc}
        <img
          class="lightbox-img"
          src="data:image/jpeg;base64,{lightboxSrc}"
          alt="Full screenshot"
        />
        <span class="lightbox-hint">Click anywhere to close</span>
      {/if}
    </div>
  {/if}

  <!-- First-run privacy disclosure (S5) — one-shot, persisted in localStorage. -->
  {#if showPrivacyDisclosure}
    <div class="modal-backdrop" role="presentation">
      <div
        class="modal"
        role="dialog"
        tabindex="-1"
        aria-modal="true"
        aria-label="Privacy notice"
        style="max-width: 360px;"
      >
        <div class="modal-header">
          <span class="modal-title">Before your first task</span>
        </div>
        <div class="modal-body" style="padding: 18px 20px; line-height: 1.5;">
          <p style="margin: 0 0 10px 0;">
            Navisual captures your active window and sends it to the AI provider you've selected.
          </p>
          <ul style="margin: 0 0 14px 0; padding-left: 18px; color: var(--text-secondary); font-size: 0.92em;">
            <li>Screenshots are held in memory only — never written to disk by Navisual.</li>
            <li>Only the active window is captured by default; full-screen needs your permission each time.</li>
            <li>Your selected provider may log requests per their own terms.</li>
            <li>Voice input (optional) sends audio to Microsoft's online speech service via the WebView2 Web Speech API.</li>
            <li>For zero data sharing, use the Ollama provider — it runs locally.</li>
          </ul>
          <p style="margin: 0 0 14px 0; font-size: 0.85em; color: var(--text-tertiary);">
            Use the Pause hotkey (configurable in Settings → Hotkeys) to stop all capture instantly.
          </p>
          <button
            class="btn-primary btn-full"
            onclick={() => {
              try { localStorage.setItem(PRIVACY_DISCLOSURE_KEY, "1"); } catch (_) {}
              showPrivacyDisclosure = false;
            }}
          >
            I understand — continue
          </button>
        </div>
      </div>
    </div>
  {/if}

  <!-- Trial exhausted modal (S.1) -->
  {#if showTrialExhausted}
    <div
      class="modal-backdrop"
      role="presentation"
      onclick={() => (showTrialExhausted = false)}
      onkeydown={(e) => { if (e.key === "Escape") showTrialExhausted = false; }}
    >
      <div
        class="modal"
        role="dialog"
        tabindex="-1"
        aria-modal="true"
        aria-label="Free trial exhausted"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
        style="max-width: 320px;"
      >
        <div class="modal-header">
          <span class="modal-title">Free trial used</span>
          <button class="hdr-btn hdr-btn-close" onclick={() => (showTrialExhausted = false)}>✕</button>
        </div>
        <div class="modal-body" style="padding: 20px; text-align: center; line-height: 1.6;">
          <p style="font-size: 2em; margin-bottom: 12px;">🎯</p>
          <p style="margin-bottom: 8px; font-weight: 600;">Your 50 free requests have been used.</p>
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            Coin purchases are coming soon — stay tuned.
          </p>
          <button class="btn-primary btn-full" onclick={() => (showTrialExhausted = false)}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- Settings modal (E.6) -->
  {#if showSettings}
    <div
      class="modal-backdrop"
      role="presentation"
      onclick={() => (showSettings = false)}
      onkeydown={(e) => { if (e.key === "Escape") showSettings = false; }}
    >
      <div
        class="modal"
        role="dialog"
        tabindex="-1"
        aria-modal="true"
        aria-label="Settings"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <div class="modal-header">
          <span class="modal-title">Settings</span>
          <button class="hdr-btn hdr-btn-close" onclick={() => (showSettings = false)}>✕</button>
        </div>
        <div class="modal-tabs">
          <button class="tab-btn {settingsTab === 'provider' ? 'tab-active' : ''}" onclick={() => (settingsTab = "provider")}>Provider</button>
          <button class="tab-btn {settingsTab === 'screen-guide' ? 'tab-active' : ''}" onclick={() => (settingsTab = "screen-guide")}>Screen Guide</button>
          <button class="tab-btn {settingsTab === 'hotkeys' ? 'tab-active' : ''}" onclick={() => (settingsTab = "hotkeys")}>Hotkeys</button>
          <button class="tab-btn {settingsTab === 'audio' ? 'tab-active' : ''}" onclick={() => (settingsTab = "audio")}>Audio</button>
          {#if settingsForm.developer_mode}
            <button class="tab-btn {settingsTab === 'developer' ? 'tab-active' : ''}" onclick={() => (settingsTab = "developer")}>Developer</button>
          {/if}
        </div>

        <div class="modal-body">
          {#if settingsTab === "provider"}
            <!-- Provider radio group -->
            <div class="setting-group">
              <p class="setting-label">Provider</p>
              <div class="provider-radios">
                {#each (["managed","anthropic","gemini","ollama","openai","deepseek","qwen"] as const) as p}
                  <label class="radio-opt" class:radio-active={settingsForm.api_provider === p}>
                    <input type="radio" name="provider" value={p} bind:group={settingsForm.api_provider} />
                    {p === "managed" ? "Managed (free)" : p.charAt(0).toUpperCase() + p.slice(1)}
                  </label>
                {/each}
              </div>
            </div>

            <!-- Per-provider contextual hint -->
            <p class="setting-hint provider-hint">
              {#if settingsForm.api_provider === "managed"}
                Free · 50 requests included. Powered by NVIDIA Nemotron via the Navisual relay. May be slower than BYOK providers — ideal for getting started.
              {:else if settingsForm.api_provider === "gemini"}
                Recommended for most users outside mainland China. Free API key available at aistudio.google.com.
              {:else if settingsForm.api_provider === "anthropic"}
                Pay per use · highest quality. API key at console.anthropic.com.
              {:else if settingsForm.api_provider === "openai"}
                Pay per use. API key at platform.openai.com.
              {:else if settingsForm.api_provider === "deepseek"}
                ⚠ Text-only — DeepSeek cannot see your screen (its API rejects images). Guidance is inferred from your description, so it may be wrong on unfamiliar or custom apps. For mainland China <em>with</em> screen analysis, use Qwen instead.
              {:else if settingsForm.api_provider === "qwen"}
                Recommended for mainland China users — US AI services (Gemini, Anthropic, OpenAI) are geoblocked there. Qwen supports image analysis.
              {:else if settingsForm.api_provider === "ollama"}
                Free · runs locally · no data leaves your machine. Requires Ollama installed with a vision model (e.g. llama3.2-vision).
              {/if}
            </p>

            {#if settingsForm.api_provider === "anthropic"}
              <div class="setting-group">
                <label class="setting-label" for="anthropic-key">API Key</label>
                <div class="key-row">
                  {#if showKeyAnthropic}
                    <input id="anthropic-key" class="setting-input" type="text"
                      bind:value={settingsForm.anthropic_api_key}
                      placeholder="sk-ant-…" spellcheck="false" />
                  {:else}
                    <input id="anthropic-key" class="setting-input" type="password"
                      bind:value={settingsForm.anthropic_api_key}
                      placeholder="sk-ant-…" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyAnthropic = !showKeyAnthropic; }}>
                    {showKeyAnthropic ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="anthropic-model">Model</label>
                <select id="anthropic-model" class="setting-select"
                  value={MODEL_PRESETS_ANTHROPIC.includes(settingsForm.anthropic_model) ? settingsForm.anthropic_model : "__custom__"}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") settingsForm.anthropic_model = v; else settingsForm.anthropic_model = ""; }}>
                  <option value="claude-haiku-4-5-20251001">claude-haiku-4-5 (fast)</option>
                  <option value="claude-sonnet-4-6">claude-sonnet-4-6 (recommended)</option>
                  <option value="claude-opus-4-7">claude-opus-4-7 (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if !MODEL_PRESETS_ANTHROPIC.includes(settingsForm.anthropic_model)}
                  <input class="setting-input" type="text" bind:value={settingsForm.anthropic_model}
                    placeholder="e.g. claude-sonnet-4-6" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>

            {:else if settingsForm.api_provider === "gemini"}
              <div class="setting-group">
                <label class="setting-label" for="gemini-key">API Key</label>
                <div class="key-row">
                  {#if showKeyGemini}
                    <input id="gemini-key" class="setting-input" type="text"
                      bind:value={settingsForm.gemini_api_key}
                      placeholder="AIza…" spellcheck="false" />
                  {:else}
                    <input id="gemini-key" class="setting-input" type="password"
                      bind:value={settingsForm.gemini_api_key}
                      placeholder="AIza…" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyGemini = !showKeyGemini; }}>
                    {showKeyGemini ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="gemini-model">Model</label>
                <select id="gemini-model" class="setting-select"
                  value={MODEL_PRESETS_GEMINI.includes(settingsForm.gemini_model) ? settingsForm.gemini_model : "__custom__"}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") settingsForm.gemini_model = v; else settingsForm.gemini_model = ""; }}>
                  <option value="gemini-2.5-flash">gemini-2.5-flash (recommended)</option>
                  <option value="gemini-2.5-flash-lite">gemini-2.5-flash-lite (fast)</option>
                  <option value="gemini-3.5-flash">gemini-3.5-flash</option>
                  <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if !MODEL_PRESETS_GEMINI.includes(settingsForm.gemini_model)}
                  <input class="setting-input" type="text" bind:value={settingsForm.gemini_model}
                    placeholder="e.g. gemini-2.5-pro" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>

            {:else if settingsForm.api_provider === "ollama"}
              <div class="setting-group">
                <label class="setting-label" for="ollama-url">Base URL</label>
                <input id="ollama-url" class="setting-input" type="text"
                  bind:value={settingsForm.ollama_base_url}
                  placeholder="http://localhost:11434" />
              </div>
              <div class="setting-group">
                <label class="setting-label" for="ollama-model">Model</label>
                <input id="ollama-model" class="setting-input" type="text" list="ollama-models"
                  bind:value={settingsForm.ollama_model} placeholder="llama3.2-vision" />
                <datalist id="ollama-models">
                  <option value="llama3.2-vision"></option>
                  <option value="llava"></option>
                  <option value="moondream"></option>
                </datalist>
              </div>

            {:else if settingsForm.api_provider === "openai"}
              <div class="setting-group">
                <label class="setting-label" for="openai-key">API Key</label>
                <div class="key-row">
                  {#if showKeyOpenAI}
                    <input id="openai-key" class="setting-input" type="text"
                      bind:value={settingsForm.openai_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {:else}
                    <input id="openai-key" class="setting-input" type="password"
                      bind:value={settingsForm.openai_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyOpenAI = !showKeyOpenAI; }}>
                    {showKeyOpenAI ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="openai-model">Model</label>
                <select id="openai-model" class="setting-select"
                  value={MODEL_PRESETS_OPENAI.includes(settingsForm.openai_model) ? settingsForm.openai_model : "__custom__"}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") settingsForm.openai_model = v; else settingsForm.openai_model = ""; }}>
                  <option value="gpt-5.5">gpt-5.5 (recommended)</option>
                  <option value="gpt-5.4-mini">gpt-5.4-mini (fast)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if !MODEL_PRESETS_OPENAI.includes(settingsForm.openai_model)}
                  <input class="setting-input" type="text" bind:value={settingsForm.openai_model}
                    placeholder="e.g. gpt-4o" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>

            {:else if settingsForm.api_provider === "deepseek"}
              <div class="setting-group">
                <label class="setting-label" for="deepseek-key">API Key</label>
                <div class="key-row">
                  {#if showKeyDeepSeek}
                    <input id="deepseek-key" class="setting-input" type="text"
                      bind:value={settingsForm.deepseek_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {:else}
                    <input id="deepseek-key" class="setting-input" type="password"
                      bind:value={settingsForm.deepseek_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyDeepSeek = !showKeyDeepSeek; }}>
                    {showKeyDeepSeek ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="deepseek-model">Model</label>
                <select id="deepseek-model" class="setting-select"
                  value={MODEL_PRESETS_DEEPSEEK.includes(settingsForm.deepseek_model) ? settingsForm.deepseek_model : "__custom__"}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") settingsForm.deepseek_model = v; else settingsForm.deepseek_model = ""; }}>
                  <option value="deepseek-v4-flash">deepseek-v4-flash (recommended)</option>
                  <option value="deepseek-v4-pro">deepseek-v4-pro (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if !MODEL_PRESETS_DEEPSEEK.includes(settingsForm.deepseek_model)}
                  <input class="setting-input" type="text" bind:value={settingsForm.deepseek_model}
                    placeholder="e.g. deepseek-v4-flash" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>

            {:else if settingsForm.api_provider === "qwen"}
              <div class="setting-group">
                <label class="setting-label" for="qwen-key">API Key</label>
                <div class="key-row">
                  {#if showKeyQwen}
                    <input id="qwen-key" class="setting-input" type="text"
                      bind:value={settingsForm.qwen_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {:else}
                    <input id="qwen-key" class="setting-input" type="password"
                      bind:value={settingsForm.qwen_api_key}
                      placeholder="sk-…" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyQwen = !showKeyQwen; }}>
                    {showKeyQwen ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="qwen-model">Model</label>
                <select id="qwen-model" class="setting-select"
                  value={MODEL_PRESETS_QWEN.includes(settingsForm.qwen_model) ? settingsForm.qwen_model : "__custom__"}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") settingsForm.qwen_model = v; else settingsForm.qwen_model = ""; }}>
                  <option value="qwen3.6-plus">qwen3.6-plus (recommended)</option>
                  <option value="qwen3.5-omni-plus">qwen3.5-omni-plus (multimodal)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if !MODEL_PRESETS_QWEN.includes(settingsForm.qwen_model)}
                  <input class="setting-input" type="text" bind:value={settingsForm.qwen_model}
                    placeholder="e.g. qwen3.6-plus" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>
              <div class="setting-group">
                <label class="setting-label" for="qwen-url">Base URL</label>
                <input id="qwen-url" class="setting-input" type="text"
                  bind:value={settingsForm.qwen_base_url}
                  placeholder="https://dashscope.aliyuncs.com/compatible-mode/v1" />
                <p class="setting-hint">Leave blank to use the default DashScope endpoint (mainland China). Only change if using a custom workspace URL.</p>
              </div>
            {/if}

          {:else if settingsTab === "screen-guide"}
            <div class="setting-group">
              <label class="setting-label" for="overlay-color">Accent color</label>
              <div class="color-row">
                <input id="overlay-color" class="color-picker" type="color" bind:value={settingsForm.overlay_color} />
                <span class="color-hex">{settingsForm.overlay_color}</span>
                <button class="key-toggle" onclick={() => (settingsForm.overlay_color = "#FF6B35")}>Reset</button>
              </div>
            </div>
            <div class="setting-group">
              <label class="setting-label" for="overlay-thickness">Border thickness — {settingsForm.overlay_thickness} px</label>
              <input id="overlay-thickness" class="setting-range" type="range" min="1" max="10"
                bind:value={settingsForm.overlay_thickness} />
            </div>
            <div class="setting-group">
              <p class="setting-label">Live caption</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.subtitle_enabled} />
                <span>Show instruction text at bottom of screen</span>
              </label>
            </div>
            <div class="setting-group">
              <p class="setting-label">Autopilot</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.auto_advance} />
                <span>Automatically move to the next step when the screen changes</span>
              </label>
            </div>

          {:else if settingsTab === "hotkeys"}
            <p class="stub-hint" style="margin-bottom:10px">Click a field then press your shortcut combo. Re-registered immediately on Save — no restart needed.</p>
            <div class="setting-group">
              <label class="setting-label">Next step</label>
              <HotkeyInput bind:value={settingsForm.hotkey_next} />
            </div>
            <div class="setting-group">
              <label class="setting-label">Mark wrong</label>
              <HotkeyInput bind:value={settingsForm.hotkey_wrong} />
            </div>
            <div class="setting-group">
              <label class="setting-label">Pause / cancel</label>
              <HotkeyInput bind:value={settingsForm.hotkey_pause} />
            </div>
            <div class="setting-group">
              <label class="setting-label">Toggle icon mode</label>
              <HotkeyInput bind:value={settingsForm.hotkey_icon} />
            </div>
            <div class="setting-group">
              <label class="setting-label">Voice input (push-to-talk)</label>
              <HotkeyInput bind:value={settingsForm.hotkey_talk} />
            </div>

          {:else if settingsTab === "developer" && settingsForm.developer_mode}
            <!-- Developer tab — gated by NAVISUAL_DEV=true -->
            <div class="setting-group">
              <p class="setting-label">Debug captures</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_screenshot_enabled} />
                <span>Save AI screenshots and OCR inputs to the debug folder</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Saved to %APPDATA%\com.navisual.app\debug\</p>
              <button class="btn-ghost" style="margin-top:8px;font-size:12px;padding:5px 10px"
                onclick={() => invoke("open_debug_folder").catch(() => {})}>
                📂 Open debug folder
              </button>
            </div>
            <div class="setting-group">
              <p class="setting-label">Response info</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_show_response_info} />
                <span>Show locate method, confidence, and element name after each AI response</span>
              </label>
            </div>
            <div class="setting-group" style="margin-top:12px;border-top:1px solid rgba(255,255,255,0.07);padding-top:12px">
              <p class="setting-label">Locate diagnostics</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_locate_trace_enabled} />
                <span>Show locate trace drawer in panel after each step</span>
              </label>
              <label class="toggle-row" style="margin-top:6px">
                <input type="checkbox" bind:checked={settingsForm.debug_locate_log_file_enabled} />
                <span>Append every locate to locate_log.jsonl</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Log file: %APPDATA%\com.navisual.app\locate_log.jsonl</p>
            </div>
            <div class="setting-group" style="margin-top:12px;border-top:1px solid rgba(255,255,255,0.07);padding-top:12px">
              <p class="setting-label">AI bounding box</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_show_ai_bbox} />
                <span>Draw the AI-returned target_bbox on the overlay (cyan dashed)</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Drawn alongside the production pointer for visual comparison. Coordinate-system per provider — Gemini normalized 0–1000, others absolute pixels.</p>
            </div>

          {:else}
            <!-- Audio tab -->
            <div class="setting-group">
              <p class="setting-label">Audio output (TTS)</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.tts_enabled} />
                <span>Enable text-to-speech for instructions</span>
              </label>
            </div>
            <div class="setting-group">
              <label class="setting-label" for="tts-voice">Preferred voice (optional)</label>
              <select id="tts-voice" class="setting-select"
                bind:value={settingsForm.tts_voice}
                disabled={!settingsForm.tts_enabled}>
                <option value="">Auto — match the language</option>
                {#if availableVoices.length === 0}
                  <option disabled value="">Loading voices…</option>
                {/if}
                {#each availableVoices as v}
                  <option value={v.id}>{v.name}</option>
                {/each}
              </select>
              <p class="stub-hint" style="margin-top:4px">Auto speaks each reply in its own language (using an installed voice for it). Pick a specific voice to force it — applied only when it matches the spoken language.</p>
            </div>
            <div class="setting-group">
              <p class="setting-label">Voice input</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.voice_input_enabled} />
                <span>Enable 🎤 push-to-talk</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Uses the WebView2 Web Speech API — audio is sent to Microsoft's online speech service; requires internet and microphone permission.</p>
            </div>
            <div class="setting-group">
              <label class="setting-label" for="voice-lang">Language</label>
              <select id="voice-lang" class="setting-input setting-select"
                bind:value={settingsForm.voice_language}
                disabled={!settingsForm.tts_enabled && !settingsForm.voice_input_enabled}>
                <option value="auto">Auto-detect</option>
                <option value="en-US">English (US)</option>
                <option value="en-GB">English (UK)</option>
                <option value="fr-FR">French</option>
                <option value="de-DE">German</option>
                <option value="es-ES">Spanish</option>
                <option value="ja-JP">Japanese</option>
                <option value="zh-CN">Chinese (Simplified)</option>
                <option value="ko-KR">Korean</option>
                <option value="pt-BR">Portuguese (Brazil)</option>
              </select>
              <p class="stub-hint" style="margin-top:4px">Sets both the TTS voice language and the voice-input language. Auto-detect speaks each reply in its own language and uses your OS language for voice input.</p>
            </div>
          {/if}
        </div>

        <div class="modal-footer">
          <div class="footer-status">
            {#if settingsError}
              <span class="settings-error">{settingsError}</span>
            {:else if settingsSaved}
              <span class="settings-ok">✓ Saved — no restart required</span>
            {:else}
              <span class="settings-note">Changes take effect when you click Apply</span>
            {/if}
          </div>
          <div class="footer-actions">
            <button class="btn-ghost btn-reset" onclick={resetSettings} title="Restore all settings to defaults (API keys are preserved)">Reset to defaults</button>
            <button class="btn-ghost" onclick={() => (showSettings = false)}>Cancel</button>
            <button class="btn-primary" onclick={applySettings} disabled={settingsSaving}>
              {settingsSaving ? "Saving…" : "Apply"}
            </button>
          </div>
        </div>
      </div>
    </div>
  {/if}

  <!-- About modal -->
  {#if showAbout}
    <div
      class="modal-backdrop"
      role="presentation"
      onclick={() => (showAbout = false)}
      onkeydown={(e) => { if (e.key === "Escape") showAbout = false; }}
    >
      <div
        class="modal about-modal"
        role="dialog"
        tabindex="-1"
        aria-modal="true"
        aria-label="About Navisual"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <div class="modal-header">
          <span class="modal-title">About</span>
          <button class="hdr-btn hdr-btn-close" onclick={() => (showAbout = false)}>✕</button>
        </div>
        <div class="about-body">
          <div class="about-logo">
            <span class="about-dot"></span>
            <span class="about-name">Navisual</span>
            <span class="about-version">v{appVersion}</span>
          </div>
          <p class="about-tagline">The AI guides, never overrides.</p>
          <p class="about-disclaimer">Navisual uses AI, which can make mistakes. Always verify each suggested action before performing it.</p>
          <div class="about-links">
            <button class="about-link" onclick={() => openUrl("https://navisualguide.com")}>navisualguide.com</button>
            <button class="about-link" onclick={() => openUrl("https://github.com/NavisualGuide/navisual")}>GitHub</button>
            <button class="about-link" onclick={openFeedbackEmail}>Send feedback</button>
          </div>

          <!-- Update section -->
          <div class="about-update">
            {#if updateStatus === "downloading"}
              <span class="update-status">Downloading… {updateProgress}%</span>
              <div class="update-progress-bar"><div class="update-progress-fill" style="width:{updateProgress}%"></div></div>
            {:else if updateStatus === "done"}
              <span class="update-status update-done">✓ Installed — please restart Navisual</span>
            {:else if pendingUpdate}
              <span class="update-status update-avail">v{pendingUpdate.version} available</span>
              <button class="btn-primary" onclick={installUpdate}>Install &amp; restart</button>
            {:else if updateStatus === "checking"}
              <span class="update-status">Checking for updates…</span>
            {:else}
              <button class="btn-ghost" onclick={() => checkForUpdates(true)}>Check for updates</button>
            {/if}
          </div>

          <p class="about-license">Licensed under FSL-1.1-Apache-2.0 — converts to Apache 2.0 two years after each release.</p>
        </div>
      </div>
    </div>
  {/if}
{/if}

<style>
  :global(:root) {
    --surface-0: #0a0a0b;
    --surface-1: #141416;
    --surface-2: #1c1c20;
    --surface-3: #26262b;
    --border: rgba(255, 255, 255, 0.08);
    --text-primary: #f5f5f7;
    --text-secondary: #a1a1aa;
    --text-tertiary: #6b6b73;
    --accent-500: #ff6b35;
    --accent-400: #ff8555;
    --accent-600: #e55520;
    --success: #22c55e;
    --danger: #ef4444;
    --warning: #f59e0b;
    --info: #0ea5e9;
    font-family: Inter, -apple-system, "Segoe UI", Roboto, sans-serif;
    color-scheme: dark;
    font-size: 13px;
  }

  :global(body) {
    margin: 0;
    background: transparent;
    color: var(--text-primary);
    -webkit-font-smoothing: antialiased;
    overflow: hidden;
  }

  /* ── Icon mode ─────────────────────────────────── */

  .icon-btn {
    width: 64px;
    height: 64px;
    border-radius: 16px;
    background: none;
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    filter: drop-shadow(0 4px 12px rgba(255, 107, 53, 0.5));
    transition: filter 160ms ease-out, transform 160ms ease-out;
  }
  .icon-btn:hover {
    filter: drop-shadow(0 6px 18px rgba(255, 107, 53, 0.75));
    transform: scale(1.08);
  }
  .icon-fish {
    width: 64px;
    height: 64px;
    border-radius: 14px;
    pointer-events: none;
    user-select: none;
  }

  /* ── Panel ──────────────────────────────────────── */

  main {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 14px;
    height: calc(100vh - 6px);
    margin: 2px 4px 4px 4px;
    min-width: 352px;
    min-height: 370px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.55);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  /* ── Header ─────────────────────────────────────── */

  .titlebar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    cursor: default;
    user-select: none;
    outline: none;
  }

  .header-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent-500);
    box-shadow: 0 0 10px rgba(255, 107, 53, 0.5);
    flex-shrink: 0;
  }

  .header-title {
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.01em;
    flex-shrink: 0;
  }

  .header-balance {
    font-size: 11px;
    color: var(--accent);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    background: rgba(255, 107, 53, 0.12);
    border-radius: 4px;
    padding: 1px 5px;
  }
  .header-balance-low {
    color: #ff4040;
    background: rgba(255, 64, 64, 0.15);
  }

  .header-provider {
    font-size: 11px;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    flex-shrink: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: var(--surface-3);
    padding: 1px 6px;
    border-radius: 4px;
  }

  /* Phase 0.2: "Shared: <App>" indicator chip. */
  .header-shared {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--accent, #ff6b35);
    background: rgba(255, 107, 53, 0.10);
    border: 1px solid rgba(255, 107, 53, 0.35);
    padding: 1px 6px;
    border-radius: 4px;
    flex-shrink: 1;
    min-width: 0;
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    font-family: inherit;
  }
  .header-shared:hover { background: rgba(255, 107, 53, 0.18); }
  .header-shared-pinned { border-style: solid; border-width: 1.5px; }
  .header-shared-pin { font-size: 9px; opacity: 0.8; }
  .header-shared-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent, #ff6b35);
    flex-shrink: 0;
    animation: shared-pulse 2.4s ease-in-out infinite;
  }
  @keyframes shared-pulse {
    0%, 100% { opacity: 0.55; }
    50% { opacity: 1.0; }
  }

  /* Target-window picker (item 1) */
  .target-picker-backdrop {
    position: fixed;
    inset: 0;
    z-index: 998;
  }
  .target-picker {
    position: fixed;
    top: 34px;
    left: 8px;
    min-width: 220px;
    max-width: 320px;
    max-height: 320px;
    overflow-y: auto;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 4px;
    z-index: 999;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.55);
  }
  .target-pick-item {
    display: grid;
    grid-template-columns: 14px 1fr;
    grid-template-rows: auto auto;
    align-items: center;
    column-gap: 6px;
    width: 100%;
    padding: 5px 8px;
    border-radius: 5px;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-size: 12px;
    font-family: inherit;
    text-align: left;
    cursor: pointer;
  }
  .target-pick-item:hover { background: var(--surface-3); }
  .target-pick-selected { color: var(--accent, #ff6b35); }
  .target-pick-check { font-size: 11px; grid-row: 1 / 3; }
  .target-pick-name { font-weight: 500; }
  .target-pick-sub {
    grid-column: 2;
    font-size: 10px;
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .header-actions {
    margin-left: auto;
    display: flex;
    gap: 2px;
    flex-shrink: 0;
  }

  .hdr-btn {
    width: 28px;
    height: 28px;
    padding: 0;
    border-radius: 6px;
    font-size: 13px;
    background: transparent;
    color: var(--text-secondary);
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out;
  }
  .hdr-btn svg { width: 17px; height: 17px; display: block; }
  .hdr-btn:hover { background: var(--surface-3); color: var(--text-primary); }
  .hdr-btn-close:hover { background: rgba(239, 68, 68, 0.2); color: var(--danger); }

  /* ── Latest instruction box ─────────────────────── */

  .latest-box {
    background: rgba(255, 107, 53, 0.06);
    border-bottom: 1px solid rgba(255, 107, 53, 0.15);
    border-left: 3px solid var(--accent-500);
    padding: 10px 12px 10px 10px;
    flex-shrink: 0;
  }

  .latest-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 5px;
  }

  .step-counter {
    font-size: 10px;
    font-weight: 600;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    flex-shrink: 0;
  }

  .latest-text {
    font-size: 14px;
    font-weight: 500;
    line-height: 1.5;
    color: var(--text-primary);
    margin: 0;
  }

  .miss-note {
    font-size: 11px;
    color: var(--text-muted, #6b7280);
    margin: 4px 0 0;
  }

  /* ── Feedback: mark-wrong footer + reason chips ───── */

  .wrong-footer {
    margin-top: 10px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .wrong-btn {
    background: rgba(239, 68, 68, 0.1);
    color: var(--danger);
    border: 1px solid rgba(239, 68, 68, 0.22);
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    padding: 4px 10px;
    cursor: pointer;
    transition: background 0.12s;
  }
  .wrong-btn:hover { background: rgba(239, 68, 68, 0.22); }

  .reason-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .reason-prompt {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-secondary);
  }
  .reason-cancel {
    background: none;
    border: none;
    color: var(--text-tertiary);
    font-size: 12px;
    cursor: pointer;
    padding: 0 2px;
    line-height: 1;
  }
  .reason-cancel:hover { color: var(--text-primary); }

  .reason-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .reason-chip {
    background: var(--surface-3, #2d2d33);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: 100px;
    font-size: 12px;
    font-weight: 500;
    padding: 5px 12px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
  }
  .reason-chip:hover {
    background: rgba(239, 68, 68, 0.15);
    color: var(--danger);
    border-color: rgba(239, 68, 68, 0.3);
  }
  .feedback-hint {
    font-size: 11px;
    color: var(--text-secondary);
    margin: 8px 0 0;
  }
  .feedback-note {
    font-size: 10px;
    color: var(--text-tertiary);
    margin: 6px 0 0;
  }

  /* Stale-response banner: screen drifted during AI thinking. */
  .stale-banner {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 0 8px;
    padding: 6px 8px;
    background: rgba(255, 184, 0, 0.10);
    border: 1px solid rgba(255, 184, 0, 0.32);
    border-radius: 6px;
    font-size: 11px;
    color: var(--text-secondary, #c8c8c8);
    line-height: 1.35;
  }
  .stale-icon { color: #ffb800; font-size: 13px; flex-shrink: 0; }
  .stale-text { flex: 1; min-width: 0; }
  .stale-action {
    background: transparent;
    border: 1px solid rgba(255, 184, 0, 0.45);
    color: #ffb800;
    border-radius: 4px;
    padding: 2px 8px;
    font-size: 11px;
    cursor: pointer;
    white-space: nowrap;
    flex-shrink: 0;
  }
  .stale-action:hover { background: rgba(255, 184, 0, 0.14); }
  .stale-dismiss {
    background: transparent;
    border: none;
    color: var(--text-muted, #6b7280);
    font-size: 12px;
    cursor: pointer;
    padding: 0 4px;
    flex-shrink: 0;
  }
  .stale-dismiss:hover { color: var(--text-primary); }

  /* ── Debug drawer (Phase 0.1) ────────────────────── */

  .debug-drawer {
    margin-top: 8px;
    border-top: 1px dashed rgba(255, 255, 255, 0.1);
    padding-top: 6px;
  }
  .debug-toggle {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font: 11px ui-monospace, monospace;
    padding: 2px 0;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }
  .debug-toggle:hover { color: var(--text-primary); }
  .debug-body {
    margin-top: 4px;
    font: 11px ui-monospace, monospace;
    color: var(--text-primary);
  }
  .debug-row {
    display: flex;
    gap: 6px;
    line-height: 1.5;
  }
  .debug-key {
    color: var(--text-muted);
    min-width: 64px;
  }
  .debug-val { color: var(--text-primary); word-break: break-all; }
  .debug-section {
    margin-top: 6px;
    padding-top: 4px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
  }
  .debug-section-head {
    color: var(--accent, #ff6b35);
    font-weight: 600;
    margin-bottom: 3px;
  }
  .debug-mono {
    color: var(--text-muted);
    font-size: 10px;
    margin-bottom: 4px;
    word-break: break-all;
  }
  .debug-cand {
    display: flex;
    gap: 4px;
    line-height: 1.45;
    flex-wrap: wrap;
  }
  .cand-selected { color: #67e480; }
  .cand-rejected { color: var(--text-muted); }
  .cand-mark { width: 10px; flex-shrink: 0; }
  .cand-text { flex-shrink: 0; }
  .cand-meta { color: var(--text-muted); }
  .cand-reason { color: var(--text-muted); font-style: italic; }
  .debug-samples {
    margin-top: 4px;
    color: var(--text-muted);
    font-size: 10px;
  }
  .debug-samples ul {
    margin: 4px 0 0 12px;
    padding: 0;
    list-style: disc;
  }
  .debug-samples li { line-height: 1.4; }

  /* ── History ─────────────────────────────────────── */

  .history {
    flex: 1;
    overflow-y: auto;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-height: 0;
  }

  .history::-webkit-scrollbar { width: 4px; }
  .history::-webkit-scrollbar-track { background: transparent; }
  .history::-webkit-scrollbar-thumb { background: var(--surface-3); border-radius: 2px; }

  .h-entry {
    display: flex;
    gap: 7px;
    align-items: flex-start;
    font-size: 13px;
    line-height: 1.5;
  }
  .h-thumb-btn {
    flex-shrink: 0;
    align-self: center;
    background: none;
    border: none;
    padding: 0;
    cursor: zoom-in;
    border-radius: 4px;
    transition: opacity 0.5s ease-out;
  }
  .h-thumb-btn:hover .h-thumb { opacity: 1; }
  .h-thumb-fading { opacity: 0; pointer-events: none; }
  .h-thumb {
    display: block;
    width: 80px;
    height: 45px;
    object-fit: cover;
    border-radius: 4px;
    border: 1px solid var(--border);
    opacity: 0.7;
    transition: opacity 0.2s;
  }
  .lightbox-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.82);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
    cursor: zoom-out;
  }
  .lightbox-img {
    max-width: 92%;
    max-height: 88vh;
    border-radius: 6px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.7);
    cursor: zoom-out;
  }
  .lightbox-loading {
    color: var(--text-secondary);
    font-size: 14px;
  }
  .lightbox-hint {
    position: absolute;
    bottom: 12px;
    left: 50%;
    transform: translateX(-50%);
    font-size: 11px;
    color: rgba(255, 255, 255, 0.45);
    pointer-events: none;
  }
  .h-label {
    font-weight: 700;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    flex-shrink: 0;
    padding-top: 1px;
    min-width: 34px;
    text-align: right;
  }

  .h-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .h-text { color: var(--text-secondary); word-break: break-word; }
  .h-meta { font-size: 11px; color: var(--text-tertiary); font-family: "JetBrains Mono", ui-monospace, monospace; }

  .h-user .h-label { color: var(--accent-400); }
  .h-user .h-text  { color: var(--text-primary); }

  .h-ai .h-label   { color: var(--info); }
  .h-ai .h-text    { color: var(--text-secondary); }

  .h-correction .h-label { color: var(--warning); }
  .h-correction .h-text  { color: var(--warning); font-style: italic; }

  .h-system .h-label { color: var(--text-tertiary); }
  .h-system .h-text  { color: var(--text-tertiary); font-style: italic; }

  .h-error .h-label { color: var(--danger); }
  .h-error .h-text  { color: var(--danger); }

  .h-thinking .h-text { color: var(--text-tertiary); }

  @keyframes thinking-fade {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }
  .thinking-dots { animation: thinking-fade 1.2s ease-in-out infinite; }

  /* ── Task input ──────────────────────────────────── */

  /* ── Badge variants ──────────────────────────────── */
  .badge-clip {
    background: rgba(14, 165, 233, 0.15);
    color: var(--info);
    border: 1px solid rgba(14, 165, 233, 0.25);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 4px;
    font-weight: 600;
    flex-shrink: 0;
  }

  .task-section {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }

  .input-hint {
    font-size: 11px;
    color: var(--text-tertiary);
    padding: 2px 0;
    font-style: italic;
  }

  textarea {
    font-family: inherit;
    font-size: 13px;
    padding: 8px 10px;
    border-radius: 7px;
    background: var(--surface-2);
    color: var(--text-primary);
    border: 1px solid var(--border);
    outline: none;
    resize: none;
    line-height: 1.5;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }
  textarea:focus { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255, 107, 53, 0.15); }
  textarea:disabled { opacity: 0.45; }

  /* ── Action row ──────────────────────────────────── */

  .action-row {
    display: flex;
    gap: 6px;
    padding: 0 12px 8px;
    flex-shrink: 0;
  }

  .btn-action {
    flex: 1;
    padding: 7px 4px;
    border-radius: 7px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    border: 1px solid transparent;
    font-family: inherit;
    transition: background 120ms ease-out, opacity 120ms ease-out;
  }
  .btn-action:disabled { opacity: 0.35; cursor: not-allowed; }

  .btn-next {
    background: rgba(34, 197, 94, 0.15);
    color: var(--success);
    border-color: rgba(34, 197, 94, 0.25);
  }
  .btn-next:not(:disabled):hover { background: rgba(34, 197, 94, 0.25); }

  .btn-more {
    flex: 0 0 32px;
    padding: 7px 0;
    background: rgba(161, 161, 170, 0.08);
    color: var(--text-secondary);
    border-color: rgba(161, 161, 170, 0.18);
    letter-spacing: 0.12em;
    font-size: 11px;
  }
  .btn-more:hover { background: rgba(161, 161, 170, 0.18); color: var(--text-primary); }

  .btn-more-open {
    background: rgba(161, 161, 170, 0.22) !important;
    color: var(--text-primary) !important;
    border-color: rgba(161, 161, 170, 0.35) !important;
  }

  .btn-mic {
    flex: 0 0 32px;
    padding: 7px 0;
    background: rgba(161, 161, 170, 0.08);
    color: var(--text-secondary);
    border-color: rgba(161, 161, 170, 0.18);
    font-size: 13px;
  }
  .btn-mic:hover:not(:disabled) { background: rgba(161, 161, 170, 0.18); color: var(--text-primary); }
  .btn-mic-active {
    background: rgba(239, 68, 68, 0.18) !important;
    border-color: rgba(239, 68, 68, 0.35) !important;
    animation: pulse 0.9s ease-in-out infinite;
  }

  .btn-pause {
    background: rgba(245, 158, 11, 0.12);
    color: var(--warning);
    border-color: rgba(245, 158, 11, 0.22);
  }
  .btn-pause:not(:disabled):hover { background: rgba(245, 158, 11, 0.22); }

  .btn-resume {
    background: rgba(34, 197, 94, 0.15);
    color: var(--success);
    border-color: rgba(34, 197, 94, 0.25);
  }
  .btn-resume:hover { background: rgba(34, 197, 94, 0.25); }

  .btn-new {
    background: rgba(161, 161, 170, 0.1);
    color: var(--text-secondary);
    border-color: rgba(161, 161, 170, 0.2);
  }
  .btn-new:hover { background: rgba(161, 161, 170, 0.2); color: var(--text-primary); }

  /* ── Quick-action menu ───────────────────────────── */

  .quick-menu {
    display: flex;
    gap: 5px;
    padding: 6px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    flex-shrink: 0;
  }

  .qm-btn {
    flex: 1;
    padding: 6px 4px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    border: 1px solid var(--border);
    background: var(--surface-3);
    color: var(--text-secondary);
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out, border-color 120ms ease-out;
    white-space: nowrap;
  }
  .qm-btn:hover:not(:disabled) { background: #2d2d33; color: var(--text-primary); }
  .qm-btn:disabled { opacity: 0.35; cursor: not-allowed; }

  .qm-active {
    background: rgba(255, 107, 53, 0.15) !important;
    border-color: rgba(255, 107, 53, 0.35) !important;
    color: var(--accent-400) !important;
  }

  /* ── Footer ──────────────────────────────────────── */

  footer {
    padding: 6px 12px 10px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--text-tertiary);
  }
  .status-dot.status-idle       { background: var(--text-tertiary); }
  .status-dot.status-thinking   { background: var(--warning); animation: pulse 1s ease-in-out infinite; }
  .status-dot.status-guiding    { background: var(--success); }
  .status-dot.status-needs_input { background: var(--accent-500); }
  .status-dot.status-error      { background: var(--danger); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }

  .status-label {
    font-size: 11px;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
  }

  .session-id {
    margin-left: auto;
    font-size: 10px;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    opacity: 0.5;
  }

  .shortcut-legend {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }
  .shortcut-legend span {
    font-size: 10px;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    white-space: nowrap;
  }
  .shortcut-legend .hk-label {
    color: var(--text-secondary);
    font-family: inherit;
    font-weight: 500;
  }

  /* ── Shared buttons ──────────────────────────────── */

  button {
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    padding: 7px 12px;
    border-radius: 7px;
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 120ms ease-out, border-color 120ms ease-out;
  }

  .btn-primary {
    background: var(--accent-500);
    color: #fff;
    border-color: transparent;
  }
  .btn-primary:hover:not(:disabled) { background: var(--accent-400); }
  .btn-primary:active { background: var(--accent-600); }
  .btn-primary:disabled { opacity: 0.4; cursor: not-allowed; }

  .btn-ghost {
    background: var(--surface-3);
    color: var(--text-primary);
    border-color: var(--border);
  }
  .btn-ghost:hover { background: #2d2d33; }

  .btn-full { width: 100%; }

  /* ── Badges ──────────────────────────────────────── */

  .badge {
    font-size: 9px;
    font-weight: 700;
    padding: 1px 6px;
    border-radius: 9999px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    flex-shrink: 0;
  }
  .badge-ok   { background: rgba(34, 197, 94, 0.15); color: var(--success); }
  .badge-warn { background: rgba(245, 158, 11, 0.15); color: var(--warning); }
  .badge-miss { background: rgba(239, 68, 68, 0.12); color: var(--danger); }
  .conf { font-size: 10px; color: var(--text-tertiary); font-family: "JetBrains Mono", ui-monospace, monospace; }

  /* ── Settings modal ──────────────────────────────── */

  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    border-radius: 14px;
  }

  .modal {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 12px;
    width: calc(100% - 32px);
    max-width: 400px;
    max-height: 92vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7);
  }

  .modal-header {
    display: flex;
    align-items: center;
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .modal-title {
    font-size: 14px;
    font-weight: 600;
    flex: 1;
  }

  .modal-tabs {
    display: flex;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .tab-btn {
    flex: 1;
    padding: 8px 4px;
    font-size: 12px;
    font-weight: 500;
    background: transparent;
    color: var(--text-tertiary);
    border: none;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    cursor: pointer;
    transition: color 120ms ease-out, border-color 120ms ease-out;
  }
  .tab-btn:hover { color: var(--text-primary); }
  .tab-active { color: var(--accent-500) !important; border-bottom-color: var(--accent-500) !important; }

  .modal-body {
    padding: 12px 14px;
    flex: 1;
    overflow-y: auto;
  }

  .stub-hint, .setting-hint {
    font-size: 12px;
    color: var(--text-tertiary);
    margin: 0;
  }

  .provider-hint {
    margin: 4px 0 10px;
    padding: 6px 8px;
    background: var(--bg-secondary);
    border-radius: 6px;
    line-height: 1.5;
  }

  .modal-footer {
    padding: 8px 14px 10px;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex-shrink: 0;
  }
  .footer-status {
    min-height: 16px;
    display: flex;
    align-items: center;
  }
  .footer-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .btn-reset { margin-right: auto; font-size: 12px; opacity: 0.75; }
  .btn-reset:hover { opacity: 1; }

  /* ── Settings form elements ──────────────────────── */

  .setting-group {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-bottom: 12px;
  }
  .setting-group:last-child { margin-bottom: 0; }

  .setting-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-tertiary);
    margin: 0;
  }

  .setting-input {
    width: 100%;
    font-family: inherit;
    font-size: 13px;
    padding: 7px 10px;
    border-radius: 7px;
    background: var(--surface-2);
    color: var(--text-primary);
    border: 1px solid var(--border);
    outline: none;
    box-sizing: border-box;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }
  .setting-input:focus { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255, 107, 53, 0.15); }
  .setting-select {
    width: 100%; font-family: inherit; font-size: 13px; padding: 7px 10px;
    border-radius: 7px; background: var(--surface-2); color: var(--text-primary);
    border: 1px solid var(--border); outline: none; box-sizing: border-box; cursor: pointer;
    transition: border-color 120ms ease-out;
    appearance: auto;
  }
  .setting-select:focus { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255,107,53,0.15); }
  .setting-select:disabled { opacity: 0.4; cursor: not-allowed; }

  .key-row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .key-row .setting-input { flex: 1; width: auto; }

  .key-toggle {
    padding: 6px 10px;
    font-size: 11px;
    font-weight: 600;
    border-radius: 6px;
    background: var(--surface-3);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    cursor: pointer;
    flex-shrink: 0;
    font-family: inherit;
    white-space: nowrap;
    transition: background 120ms ease-out;
  }
  .key-toggle:hover { background: #2d2d33; color: var(--text-primary); }

  .color-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .color-picker {
    width: 36px;
    height: 30px;
    padding: 2px;
    border-radius: 6px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    cursor: pointer;
    flex-shrink: 0;
  }

  .color-hex {
    font-size: 12px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    color: var(--text-secondary);
    flex: 1;
  }

  .setting-range {
    width: 100%;
    accent-color: var(--accent-500);
    cursor: pointer;
  }

  .provider-radios {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .radio-opt {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 5px 10px;
    border-radius: 6px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
    transition: background 120ms ease-out, border-color 120ms ease-out, color 120ms ease-out;
    user-select: none;
  }
  .radio-opt input[type="radio"] { display: none; }
  .radio-opt:hover { background: var(--surface-3); color: var(--text-primary); }
  .radio-active {
    background: rgba(255, 107, 53, 0.12) !important;
    border-color: rgba(255, 107, 53, 0.4) !important;
    color: var(--accent-400) !important;
  }

  .settings-error {
    font-size: 12px;
    color: var(--danger);
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .settings-ok {
    font-size: 12px;
    color: var(--success);
    flex: 1;
  }

  .settings-note {
    font-size: 11px;
    color: var(--text-tertiary);
    flex: 1;
    font-style: italic;
  }

  .toggle-row {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    font-size: 13px;
    color: var(--text-secondary);
    user-select: none;
  }
  .toggle-row input[type="checkbox"] {
    width: 14px;
    height: 14px;
    accent-color: var(--accent-500);
    cursor: pointer;
    flex-shrink: 0;
  }

  /* ── About modal ──────────────────────────────────── */

  .about-modal {
    max-width: 320px;
  }

  .about-body {
    padding: 24px 20px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .about-logo {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .about-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--accent-500);
    box-shadow: 0 0 6px var(--accent-500);
    flex-shrink: 0;
  }

  .about-name {
    font-size: 16px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .about-version {
    font-size: 11px;
    color: var(--text-tertiary);
    background: var(--surface-3);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .about-tagline {
    margin: 0;
    color: var(--text-secondary);
    font-style: italic;
    font-size: 12px;
  }
  .about-disclaimer {
    margin: 0;
    color: var(--text-tertiary);
    font-size: 11px;
    line-height: 1.4;
  }

  .about-links {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .about-link {
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--accent-400);
    font-size: 12px;
    padding: 4px 10px;
    cursor: pointer;
    transition: background 0.15s;
  }

  .about-link:hover {
    background: var(--surface-2);
    color: var(--accent-500);
  }

  .about-license {
    margin: 0;
    font-size: 10px;
    color: var(--text-tertiary);
    line-height: 1.5;
  }

  .about-update {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--surface-2);
    border-radius: 8px;
    border: 1px solid var(--border);
  }

  .update-status {
    font-size: 11px;
    color: var(--text-secondary);
  }

  .update-avail {
    color: var(--warning);
    font-weight: 600;
  }

  .update-done {
    color: var(--success);
  }

  .update-progress-bar {
    height: 4px;
    background: var(--surface-3);
    border-radius: 2px;
    overflow: hidden;
  }

  .update-progress-fill {
    height: 100%;
    background: var(--accent-500);
    border-radius: 2px;
    transition: width 0.2s ease;
  }

  .header-update {
    font-size: 10px;
    font-weight: 600;
    color: var(--warning);
    background: rgba(245, 158, 11, 0.12);
    border: 1px solid rgba(245, 158, 11, 0.3);
    border-radius: 4px;
    padding: 2px 6px;
    cursor: pointer;
    margin-right: 4px;
    flex-shrink: 0;
  }

  .header-update:hover {
    background: rgba(245, 158, 11, 0.2);
  }
</style>
