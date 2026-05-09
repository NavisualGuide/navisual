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
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { LogicalSize, LogicalPosition } from "@tauri-apps/api/dpi";
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
    grid_cell: string | null;
    debug_screenshot_path: string | null;
    locate_trace: LocateTrace | null;
  };
  type AppPhase = "idle" | "thinking" | "guiding" | "needs_input" | "consent_prompt" | "error";
  type HistoryRole = "user" | "ai" | "correction" | "system" | "error";
  type HistoryEntry = { id: number; role: HistoryRole; text: string; meta?: string };
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
    overlay_color: string;
    overlay_thickness: number;
    subtitle_enabled: boolean;
    auto_advance: boolean;
    tts_enabled: boolean;
    voice_input_enabled: boolean;
    voice_language: string;
    hotkey_next: string;
    hotkey_wrong: string;
    hotkey_pause: string;
    hotkey_icon: string;
    grid_test_enabled: boolean;
    debug_screenshot_enabled: boolean;
    debug_show_response_info: boolean;
    debug_locate_trace_enabled: boolean;
    debug_locate_log_file_enabled: boolean;
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
    grid_cell: [number, number] | null;
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
  let sessionId = $state("");
  let provider = $state("");
  let gridCell = $state<string | null>(null);

  // Strip AI self-references to grid_cell from instruction text (debug field, not for users).
  function stripGridRef(text: string): string {
    return text.replace(/\s*\(grid cell [A-I]\d{1,2}\)\.?/gi, '').trimEnd();
  }

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

  // UI state
  let iconMode = $state(false);
  let showSettings = $state(false);
  let showAbout = $state(false);
  let appVersion = $state("…");
  let pendingUpdate = $state<Update | null>(null);
  let updateStatus = $state<"idle" | "checking" | "downloading" | "done">("idle");
  let updateProgress = $state(0);
  let settingsTab = $state<SettingsTab>("provider");
  let history = $state<HistoryEntry[]>([]);
  let historyEl: HTMLElement | null = $state(null);

  // Settings form state
  const SETTINGS_DEFAULTS: SettingsPayload = {
    api_provider: "anthropic",
    anthropic_api_key: "", anthropic_model: "claude-sonnet-4-6", anthropic_fast_model: "claude-haiku-4-5-20251001",
    gemini_api_key: "", gemini_model: "gemini-2.5-flash", gemini_fast_model: "gemini-2.5-flash-lite",
    ollama_base_url: "http://localhost:11434", ollama_model: "llama3.2-vision",
    openai_api_key: "", openai_model: "gpt-4o",
    overlay_color: "#FF6B35", overlay_thickness: 4,
    subtitle_enabled: true, auto_advance: false,
    tts_enabled: true, voice_input_enabled: false, voice_language: "en-US",
    hotkey_next: "Ctrl+Backquote", hotkey_wrong: "Ctrl+KeyE",
    hotkey_pause: "Ctrl+KeyS", hotkey_icon: "Ctrl+KeyQ",
    grid_test_enabled: false,
    debug_screenshot_enabled: false,
    debug_show_response_info: false,
    debug_locate_trace_enabled: false,
    debug_locate_log_file_enabled: false,
  };
  let settingsForm = $state<SettingsPayload>({ ...SETTINGS_DEFAULTS });
  let settingsSaving = $state(false);
  let settingsError = $state<string | null>(null);
  let settingsSaved = $state(false);
  let showKeyAnthropic = $state(false);
  let showKeyGemini = $state(false);
  let showKeyOpenAI = $state(false);
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
  async function addToHistory(role: HistoryRole, text: string, meta?: string) {
    history.push({ id: ++_historyId, role, text, meta });
    await tick();
    if (historyEl) historyEl.scrollTop = historyEl.scrollHeight;
  }

  // Whether the global auto-advance setting is on (loaded from config on mount).
  let autoAdvanceEnabled = $state(false);

  // Autopilot on-demand polling.
  let screenChangeDebounce = 0;
  let autopilotInterval: ReturnType<typeof setInterval> | null = null;

  function startAutopilotPolling() {
    if (autopilotInterval !== null) return;
    autopilotInterval = setInterval(async () => {
      if (!autoAdvanceEnabled) return;
      try {
        const res = await invoke<{ changed: boolean }>("check_screen_changed");
        if (!res.changed) return;
        if (phase !== "guiding") return;
        const now = Date.now();
        if (now - screenChangeDebounce < 5000) return;
        const currentStep = steps[stepIndex];
        if (!currentStep) return;
        screenChangeDebounce = now;
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
          updateStatus = "done";
        }
      });
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
    speechRecognition.lang = settingsForm.voice_language || "en-US";
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
    });
    showQuickMenu = false;
  }

  async function wrongAndClose() {
    showQuickMenu = false;
    await correction();
  }

  function cancelRequest() {
    requestToken++;
    stopTimer();
    invoke("clear_overlay").catch(() => {});
    invoke("speak", { text: "" }).catch(() => {});
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
    showKeyAnthropic = false; showKeyGemini = false; showKeyOpenAI = false;
    showSettings = true;
    try {
      const data = await invoke<SettingsPayload>("get_settings");
      // Keep the live auto_advance state — the Pause/Resume button may have
      // changed it since the last disk save, and the button is the source of truth.
      settingsForm = { ...data, auto_advance: autoAdvanceEnabled };
      debugShowInfo = data.debug_show_response_info;
    } catch (e) {
      settingsError = String(e);
    }
  }

  // Re-register global shortcuts. Called on mount and after settings change.
  async function registerShortcuts(hk: Pick<SettingsPayload, "hotkey_next"|"hotkey_wrong"|"hotkey_pause"|"hotkey_icon">) {
    await unregisterAll().catch(() => {});
    function debounced(fn: () => void, ms = 350): () => void {
      let last = 0;
      return () => { const now = Date.now(); if (now - last < ms) return; last = now; fn(); };
    }
    const pairs: Array<[string, () => void]> = [
      [hk.hotkey_next,  debounced(() => { if (!actionDisabled) nextStep(); })],
      [hk.hotkey_wrong, debounced(() => { if (!actionDisabled) correction(); })],
      [hk.hotkey_pause, debounced(() => cancelRequest())],
      [hk.hotkey_icon,  debounced(() => { if (iconMode) expandToPanel(); else collapseToIcon(); })],
    ];
    const errors: string[] = [];
    for (const [key, handler] of pairs) {
      try { await register(key, handler); }
      catch (e) { errors.push(`${key}: ${e}`); console.warn("shortcut failed:", key, e); }
    }
    return errors;
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
      const hkErrors = await registerShortcuts(settingsForm);
      if (hkErrors.length) {
        settingsError = `Saved, but hotkey registration failed: ${hkErrors.join("; ")}`;
      } else {
        settingsSaved = true;
        setTimeout(() => { settingsSaved = false; }, 2000);
      }
    } catch (e) {
      settingsError = String(e);
    } finally {
      settingsSaving = false;
    }
  }

  async function newSession() {
    cancelRequest();
    task = "";
    steps = [];
    stepIndex = 0;
    currentInstruction = "";
    locateResult = null;
    locateTrace = null;
    sessionId = "";
    history = [];
    await addToHistory("system", "New session started");
  }

  function applyResponse(res: GuideResponse, idx: number, token: number) {
    if (token !== requestToken) return;
    steps = res.steps;
    stepIndex = idx;
    currentInstruction = stripGridRef(res.instruction);
    locateResult = res.located;
    locateTrace = res.locate_trace;
    sessionId = res.session_id;
    gridCell = res.grid_cell ?? null;
    if (res.provider) provider = res.provider;
    if (res.request_full_screen) {
      phase = "consent_prompt";
    } else {
      phase = res.needs_input ? "needs_input" : "guiding";
    }
    if (res.instruction) {
      const cleanInstruction = stripGridRef(res.instruction);
      let meta: string | undefined;
      if (res.located) {
        meta = `${res.located.role} · ${(res.located.confidence * 100).toFixed(0)}% · ${res.located.name}`;
      } else if (steps[idx]?.target_text) {
        meta = `not located · "${steps[idx].target_text}"`;
      }
      addToHistory("ai", cleanInstruction, meta);
      if (!isMuted) invoke("speak", { text: cleanInstruction }).catch(() => {});
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
    await addToHistory("user", taskText);
    currentInstruction = "";
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("guide", { task: taskText, isReply, fullScreen });
      stopTimer();
      if (token !== requestToken) return;
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
      try {
        const res = await invoke<GuideResponse>("guide", {
          task: completed ? `[User completed: "${completed}"]` : "",
          isReply: false,
        });
        stopTimer();
        if (token !== requestToken) return;
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

  async function correction() {
    const note = task.trim();
    if (note) task = "";
    const prevPhase = phase;
    addToHistory("correction", note ? `Wrong — ${note}` : "Marked wrong — re-analysing…");
    currentInstruction = "";
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction", { note: note || null });
      stopTimer();
      if (token !== requestToken) return;
      if (!res.ok) {
        phase = prevPhase;
        addToHistory("system", "⚠️ " + (res.error ?? "correction failed"));
        if (note !== "") task = note;
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      addToHistory("system", "⚠️ " + String(e));
      if (note !== "") task = note;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !isThinking && task.trim()) {
      e.preventDefault();
      guide();
    }
  }

  // "paused" = auto-advance is on but we're currently idle with an active session.
  let isPaused = $derived(autoAdvanceEnabled && phase === "idle" && steps.length > 0);

  let statusLabel = $derived(
    isPaused              ? `paused · step ${stepIndex + 1}/${steps.length}`
    : phase === "idle"    ? "idle"
    : phase === "thinking"  ? `thinking · ${(elapsedMs / 1000).toFixed(1)}s`
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
    : settingsForm.api_provider === "managed" ? "managed"
    : settingsForm.openai_model
  );
  let headerLabel = $derived(activeModel || provider);

  onMount(async () => {
    getVersion().then(v => { appVersion = v; }).catch(() => {});
    setTimeout(() => checkForUpdates(), 5000);

    // Position bottom-right then show — panel starts hidden (visible:false in
    // tauri.conf.json) so the user never sees a blank frame at 0,0 while
    // WebView2 initialises. We show only once the UI is fully painted.
    // Load initial config so hotkeys, autoAdvance, and provider are correct from startup.
    let initHotkeys: Pick<SettingsPayload, "hotkey_next"|"hotkey_wrong"|"hotkey_pause"|"hotkey_icon"> = {
      hotkey_next: SETTINGS_DEFAULTS.hotkey_next,
      hotkey_wrong: SETTINGS_DEFAULTS.hotkey_wrong,
      hotkey_pause: SETTINGS_DEFAULTS.hotkey_pause,
      hotkey_icon: SETTINGS_DEFAULTS.hotkey_icon,
    };
    try {
      const init = await invoke<SettingsPayload>("get_settings");
      autoAdvanceEnabled = init.auto_advance;
      isMuted = !init.tts_enabled;
      if (init.api_provider) provider = init.api_provider;
      settingsForm = { ...SETTINGS_DEFAULTS, ...init, auto_advance: init.auto_advance };
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
      const initial = await invoke<SharedAppInfo | null>("get_shared_app_info");
      if (initial) sharedApp = initial;
    } catch (_) {}

    // E.3 — Autopilot: on-demand screen-change polling.
    // Functions are defined at module level; start now if already enabled.
    if (autoAdvanceEnabled) startAutopilotPolling();

    await registerShortcuts(initHotkeys);

    // Ctrl+A — push-to-talk voice input (E.7)
    try {
      await register("Ctrl+KeyA", () => { toggleVoiceInput(); });
    } catch (_) {}

    // S.1 — Managed provider: anonymous sign-in on first launch.
    if (settingsForm.api_provider === "managed") {
      try {
        await invoke("sign_in_anon");
      } catch (e) {
        addToHistory("system", "⚠️ Managed sign-in failed: " + String(e));
      }
      try {
        const bal = await invoke<{ tier: string; free_remaining: number }>("get_balance");
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

    await addToHistory("system", "Navisual ready");
  });

  onDestroy(async () => {
    stopAutopilotPolling();
    await unregisterAll().catch(() => {});
  });
</script>

{#if iconMode}
  <!-- Icon mode: 56×56 orange dot — mousedown starts drag; click expands -->
  <button
    class="icon-btn"
    onclick={handleIconClick}
    onpointerdown={handleIconPointerdown}
    onpointermove={handleIconPointermove}
    title="Expand Navisual (Alt+Q)"
  >
    <span class="icon-glow"></span>
  </button>
{:else}
  <main>
    <!-- Title bar: onmousedown → startDragging() (more reliable than data-tauri-drag-region on WebView2) -->
    <div class="titlebar" role="toolbar" tabindex="-1" onmousedown={handleHeaderMousedown}>
      <span class="header-dot"></span>
      <span class="header-title">Navisual</span>
      {#if headerLabel}
        <span class="header-provider">{headerLabel}</span>
      {/if}
      {#if sharedApp}
        <span class="header-shared" title="Window currently shared with the AI">
          <span class="header-shared-dot"></span>
          {sharedApp.app_name}
        </span>
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
        <button class="hdr-btn" onclick={() => (showAbout = true)} title="About Navisual">ⓘ</button>
        <button class="hdr-btn" onclick={openSettings} title="Settings">⚙</button>
        <button class="hdr-btn" onclick={collapseToIcon} title="Collapse to icon (Alt+Q)">⊟</button>
        <button class="hdr-btn hdr-btn-close" onclick={closeWindow} title="Quit">✕</button>
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
          {#if settingsForm.grid_test_enabled && gridCell}
            <span class="badge badge-grid" title="AI-identified grid cell">⊞ {gridCell}</span>
          {/if}
          {#if locateResult}
            <span class="badge badge-{locateResult.role === 'Ocr' ? 'warn' : 'ok'}">
              {locateResult.role}
            </span>
            <span class="conf">{(locateResult.confidence * 100).toFixed(0)}%</span>
          {:else if steps[stepIndex]?.target_text}
            <span class="badge badge-miss">not located</span>
          {/if}
        </div>
        <p class="latest-text">{stripGridRef(currentInstruction)}</p>

        <!-- D6: subtle miss note — only when a target was expected but not found -->
        {#if !locateResult && steps[stepIndex]?.target_text && phase === "guiding"}
          <p class="miss-note">⊘ Pointer unavailable — follow the instruction above</p>
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
                {#if locateTrace.grid_cell}
                  <div class="debug-row">
                    <span class="debug-key">grid_cell</span>
                    <span class="debug-val">col {locateTrace.grid_cell[0] + 1}, row {String.fromCharCode(65 + locateTrace.grid_cell[1])}</span>
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
          <button class="btn-primary btn-full" onclick={() => guide()} disabled={!task.trim()}>
            {phase === "needs_input" ? "↩ Send answer" : phase === "guiding" ? "↩ Follow up" : "Guide me"}
          </button>
        {/if}
      {/if}
    </section>

    <!-- Quick-action menu (opened by ··· button) -->
    {#if showQuickMenu}
      <div class="quick-menu">
        <button class="qm-btn qm-wrong" onclick={wrongAndClose} disabled={actionDisabled} title="Alt+E">
          ✗ Wrong
        </button>
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
      <button class="btn-action btn-next" onclick={nextStep} disabled={actionDisabled} title="Next step (Alt+`)">
        → Next
      </button>
      <button class="btn-action {autoAdvanceEnabled ? 'btn-pause' : 'btn-resume'}"
        disabled={!autoAdvanceEnabled && steps.length === 0}
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
        title={settingsForm.voice_input_enabled ? (isRecording ? "Stop recording (Ctrl+A)" : "Voice input (Ctrl+A)") : "Enable voice input in Settings → Audio"}>
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
        <span>{settingsForm.hotkey_next} Next</span>
        <span>{settingsForm.hotkey_wrong} Wrong</span>
        <span>{settingsForm.hotkey_pause} Pause</span>
        <span>{settingsForm.hotkey_icon} Icon</span>
      </div>
    </footer>
  </main>

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
          <button class="tab-btn {settingsTab === 'developer' ? 'tab-active' : ''}" onclick={() => (settingsTab = "developer")}>Developer</button>
        </div>

        <div class="modal-body">
          {#if settingsTab === "provider"}
            <!-- Provider radio group -->
            <div class="setting-group">
              <p class="setting-label">Provider</p>
              <div class="provider-radios">
                {#each (["managed","anthropic","gemini","ollama","openai"] as const) as p}
                  <label class="radio-opt" class:radio-active={settingsForm.api_provider === p}>
                    <input type="radio" name="provider" value={p} bind:group={settingsForm.api_provider} />
                    {p === "managed" ? "Managed (free)" : p.charAt(0).toUpperCase() + p.slice(1)}
                  </label>
                {/each}
              </div>
            </div>

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
                <select id="anthropic-model" class="setting-select" bind:value={settingsForm.anthropic_model}>
                  <option value="claude-haiku-4-5-20251001">claude-haiku-4-5</option>
                  <option value="claude-sonnet-4-6">claude-sonnet-4-6</option>
                  <option value="claude-opus-4-7">claude-opus-4-7</option>
                </select>
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
                <select id="gemini-model" class="setting-select" bind:value={settingsForm.gemini_model}>
                  <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview</option>
                  <option value="gemini-3.1-flash-lite-preview">gemini-3.1-flash-lite-preview</option>
                  <option value="gemini-3-flash-preview">gemini-3-flash-preview</option>
                  <option value="gemini-2.5-flash">gemini-2.5-flash</option>
                  <option value="gemini-2.5-flash-lite">gemini-2.5-flash-lite</option>
                </select>
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
                <select id="openai-model" class="setting-select" bind:value={settingsForm.openai_model}>
                  <option value="gpt-4o">gpt-4o</option>
                  <option value="gpt-4o-mini">gpt-4o-mini</option>
                  <option value="o1">o1</option>
                  <option value="o3">o3</option>
                </select>
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

          {:else if settingsTab === "developer"}
            <!-- Developer tab -->
            <div class="setting-group">
              <p class="setting-label">Debug screenshots</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_screenshot_enabled} />
                <span>Save a copy of every screenshot sent to the AI</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Saved to %APPDATA%\com.navisual.app\debug\</p>
              <button class="btn-ghost" style="margin-top:8px;font-size:12px;padding:5px 10px"
                onclick={() => invoke("open_debug_folder").catch(() => {})}>
                📂 Open screenshot folder
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
              <p class="setting-label">Grid test</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.grid_test_enabled} />
                <span>Draw 16×9 grid on screenshots and show AI cell label in response</span>
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
              <p class="setting-label">Voice input</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.voice_input_enabled} />
                <span>Enable 🎤 push-to-talk (Ctrl+A)</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Uses the browser's built-in speech recognition — requires internet and microphone permission.</p>
            </div>
            <div class="setting-group">
              <label class="setting-label" for="voice-lang">Recognition language</label>
              <select id="voice-lang" class="setting-input setting-select"
                bind:value={settingsForm.voice_language}
                disabled={!settingsForm.voice_input_enabled}>
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
            </div>
          {/if}
        </div>

        <div class="modal-footer">
          {#if settingsError}
            <span class="settings-error">{settingsError}</span>
          {:else if settingsSaved}
            <span class="settings-ok">✓ Saved — no restart required</span>
          {:else}
            <span class="settings-note">All settings apply on Save</span>
          {/if}
          <button class="btn-ghost" onclick={() => (showSettings = false)}>Cancel</button>
          <button class="btn-primary" onclick={applySettings} disabled={settingsSaving}>
            {settingsSaving ? "Saving…" : "Apply"}
          </button>
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
          <div class="about-links">
            <button class="about-link" onclick={() => openUrl("https://navisualguide.com")}>navisualguide.com</button>
            <button class="about-link" onclick={() => openUrl("https://github.com/NavisualGuide/navisual")}>GitHub</button>
            <button class="about-link" onclick={() => openUrl("mailto:feedback@navisualguide.com")}>Send feedback</button>
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
    width: 56px;
    height: 56px;
    border-radius: 50%;
    background: var(--accent-500);
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: 0 0 20px rgba(255, 107, 53, 0.55);
    transition: box-shadow 160ms ease-out, transform 160ms ease-out;
  }
  .icon-btn:hover {
    box-shadow: 0 0 28px rgba(255, 107, 53, 0.75);
    transform: scale(1.06);
  }
  .icon-glow {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.85);
  }

  /* ── Panel ──────────────────────────────────────── */

  main {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 14px;
    height: calc(100vh - 16px);
    margin: 8px;
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
    cursor: default;
  }
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

  .qm-wrong {
    background: rgba(239, 68, 68, 0.12);
    color: var(--danger);
    border-color: rgba(239, 68, 68, 0.22);
  }
  .qm-wrong:not(:disabled):hover { background: rgba(239, 68, 68, 0.25); }

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
  .badge-grid { background: rgba(99, 102, 241, 0.18); color: #a5b4fc; font-family: "JetBrains Mono", ui-monospace, monospace; }
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
    width: 320px;
    max-height: 80vh;
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
    padding: 16px;
    flex: 1;
    overflow-y: auto;
  }

  .stub-hint {
    font-size: 12px;
    color: var(--text-tertiary);
    margin: 0;
  }

  .modal-footer {
    padding: 10px 14px;
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    flex-shrink: 0;
  }

  /* ── Settings form elements ──────────────────────── */

  .setting-group {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 14px;
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
