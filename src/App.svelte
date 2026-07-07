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
  import { prettyHotkey } from "./lib/hotkey";

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
    provider: string;
    model: string | null;
    input_tokens: number | null;
    output_tokens: number | null;
    error: string | null;
    debug_screenshot_path: string | null;
    chat_thumb_b64: string | null;
    locate_trace: LocateTrace | null;
    ai_bbox: Rect | null;
    suggested_tasks: string[];
  };
  type AppPhase = "idle" | "thinking" | "guiding" | "needs_input" | "error";
  type HistoryRole = "user" | "ai" | "correction" | "system" | "error";
  type HistoryEntry = { id: number; role: HistoryRole; text: string; meta?: string; thumb?: string; thumbFading?: boolean };
  type SettingsTab = "provider" | "screen-guide" | "hotkeys" | "audio" | "developer" | "billing" | "account";
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
    custom_api_key: string;
    custom_model: string;
    custom_base_url: string;
    managed_tier: string;
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
    debug_prompt_log_file_enabled: boolean;
    task_suggestions: boolean;
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
  type BboxProbe = {
    attempted: boolean;
    resolved_role: string | null;
    resolved_name: string | null;
    accepted: boolean;
    detail: string;
  };
  type A11yTrace = {
    ran: boolean;
    regex_used: string;
    search_roots_count: number;
    candidates: A11yCandidate[];
    timed_out: boolean;
    retried: boolean;
    framework: string | null;
    cached: boolean;
    element_count: number | null;
    bbox_probe: BboxProbe | null;
    elapsed_ms: number;
  };
  type Corroboration = {
    uia_control_type: string | null;
    uia_interactive: boolean;
    isolation: number;
    isolation_line_len: number;
    isolation_ok: boolean;
    near_anchor: boolean;
    near_ai_bbox: boolean;
    accepted: boolean;
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
    corroboration: Corroboration | null;
    elapsed_ms: number;
  };
  // Pass 3 — icon template matching (mirrors TemplateTrace in trace.rs).
  type TemplateTrace = {
    templates_tried: number;
    best_icon: string | null;
    best_score: number;
    best_scale: number;
    best_pos: [number, number] | null;
    scale_prior: number;
    accepted: boolean;
  };
  // Pass 0.5 — Structured-Context selection (mirrors SelectionTrace in trace.rs).
  type SelectionTrace = {
    id: number;
    snapshot_len: number;
    snapshot_name: string | null;
    verified: boolean;
    detail: string;
  };
  type FinalDecision =
    | { kind: "miss" }
    | { kind: "hit_a11y" }
    | { kind: "hit_ocr" }
    | { kind: "hit_template" }
    | { kind: "hit_adapter" }
    | { kind: "hit_selection" }
    | { kind: "rejected_by_hit_test"; leaf_class: string }
    | { kind: "rejected_uncorroborated"; detail: string }
    | { kind: "error"; message: string };
  type LocateTrace = {
    timestamp_ms: number;
    target_text: string;
    target_role: string | null;
    nearby_text: string | null;
    ai_bbox: { x: number; y: number; width: number; height: number } | null;
    selection: SelectionTrace | null;
    a11y: A11yTrace;
    ocr: OcrTrace;
    template: TemplateTrace | null;
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
  // Backend hid the pointer because the target window is occluded (not a locate miss).
  let pointerOccluded = $state(false);
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
  // The model that actually handled the last AI response. For managed this is the
  // concrete model OpenRouter routed to (the relay sends the `openrouter/free` router);
  // shown in the debug drawer and logged with feedback. Empty until the first response.
  let routedModel = $state("");
  // Set when the screen drifted during the 5–90s AI thinking window.
  // Surfaces a soft banner over the instruction so the user knows the
  // guidance may be referring to state that no longer exists.
  let staleResponse = $state(false);
  // Managed provider (S.1 / S.2) state
  let freeRemaining = $state<number | null>(null);
  let coinBalance = $state<number | null>(null);       // µ$ (divide by 5_000 for coins; 1 coin = $0.005)
  let managedTier = $state<"free" | "paid">("free");
  let showTrialExhausted = $state(false);
  let oauthPending = $state(false);     // true while waiting for Google OAuth callback
  let checkoutPending = $state(false);  // true while waiting for user to pay in browser
  let buyAmount = $state<number | "custom">(20);  // USD top-up; "custom" reveals a field
  let customAmount = $state(20);                   // USD entered when buyAmount === "custom"
  let effectiveAmount = $derived(buyAmount === "custom" ? customAmount : buyAmount);
  let amountValid = $derived(effectiveAmount >= 5 && effectiveAmount <= 500);

  // Account management (S.2.1) state
  type AccountInfo = { email: string | null; is_anonymous: boolean; providers: string[] };
  let accountInfo = $state<AccountInfo | null>(null);
  type AccountView = "signin" | "signup" | "verify_signup" | "forgot" | "verify_reset" | "account";
  let accountView = $state<AccountView>("signin");
  let acctEmail = $state("");
  let acctPassword = $state("");
  let acctCode = $state("");          // 6-digit OTP
  let acctNewPassword = $state("");
  let acctBusy = $state(false);
  let acctError = $state("");
  let acctNotice = $state("");
  let showChangePw = $state(false);
  let showDeleteConfirm = $state(false);
  // Signed in with a real (non-anonymous) email account?
  let acctSignedIn = $derived(!!accountInfo && !accountInfo.is_anonymous && !!accountInfo.email);
  // How they authenticated — drives password UI. A Google (OAuth) account's
  // password is managed by Google, so we don't offer "Change password" for it.
  let acctIsGoogle = $derived(!!accountInfo && accountInfo.providers.includes("google"));
  let acctHasPassword = $derived(!!accountInfo && accountInfo.providers.includes("email"));
  // Show the change-password control unless it's an OAuth-only account.
  let acctShowChangePw = $derived(acctHasPassword || !acctIsGoogle);

  // Phase 0.2: which app is currently shared with the AI.
  type SharedAppInfo = {
    hwnd: number;
    rect: { x: number; y: number; width: number; height: number };
    app_name: string;
    exe_name: string;
  };
  let sharedApp = $state<SharedAppInfo | null>(null);

  // ---- Workstream P (v0.7): prefilled task suggestions ----
  // The task box is prefilled with a plausible task, rendered SELECTED so one
  // keystroke replaces it; a small ▾ toggle reveals the other guesses in a
  // popover when there is more than one. Display-only — nothing is ever
  // auto-submitted.
  let taskSuggestions = $state<string[]>([]); // current guess list (≤3)
  let prefillActive = $state(false); // task box holds an untouched prefill
  let showSuggestAlts = $state(false); // the ▾ popover of alternates is open
  let taskInputEl: HTMLTextAreaElement | undefined = $state(undefined);
  // The suggestion currently sitting in the box never repeats in its own
  // dropdown — only the OTHER guesses are worth surfacing there.
  let suggestAlternatives = $derived(taskSuggestions.filter((s) => s !== task));

  /// Prefill the box with `suggestions[0]` + list the rest. Never clobbers
  /// user-typed text (only an empty box or an untouched previous prefill is
  /// replaced) and never runs while the AI needs an answer (the box is a reply).
  function applyPrefill(suggestions: string[]) {
    if (!settingsForm.task_suggestions || suggestions.length === 0) return;
    if (phase === "needs_input" || phase === "thinking") return;
    if (task.trim() && !prefillActive) return;
    taskSuggestions = suggestions.slice(0, 3);
    task = taskSuggestions[0];
    prefillActive = true;
    // Select so the first keystroke replaces the guess. Only when our own window
    // already has focus — select() implies focus, and stealing OS focus from the
    // target app mid-session would corrupt the next capture. When the panel is
    // background, the select-on-focus handler on the textarea covers it instead.
    tick().then(() => {
      if (document.hasFocus() && taskInputEl) {
        taskInputEl.focus();
        taskInputEl.select();
      }
    });
  }

  function clearPrefill() {
    prefillActive = false;
    taskSuggestions = [];
    showSuggestAlts = false;
  }

  function selectSuggestion(s: string) {
    task = s;
    prefillActive = true; // still a prefill — typing replaces, submit sends
    showSuggestAlts = false;
    tick().then(() => {
      taskInputEl?.focus();
      taskInputEl?.select();
    });
  }

  /// Cold-start prefill (P.1) — purely local, no AI call: pack starter tasks for
  /// the focused app (if its nav-pack curates any) ahead of a generic
  /// "Show me around {app}". Runs only while idle with an untouched box.
  async function coldStartPrefill() {
    if (!settingsForm.task_suggestions) return;
    if (phase !== "idle" && phase !== "error") return;
    if (task.trim() && !prefillActive) return;
    let starters: string[] = [];
    if (sharedApp) {
      try {
        starters = await invoke<string[]>("get_pack_starters", { hwnd: sharedApp.hwnd });
      } catch (_) {}
    }
    const appName = sharedApp ? (friendlyName(sharedApp.exe_name) || sharedApp.app_name) : "";
    const generic = appName ? `Show me around ${appName}` : "Explore this app";
    const list = [...starters];
    if (!list.some((s) => s.toLowerCase() === generic.toLowerCase())) list.push(generic);
    applyPrefill(list);
  }

  // Target-window picker (item 1)
  type TargetWindowInfo = { hwnd: number; title: string; exe_stem: string; display_name: string; };
  let targetPickerOpen = $state(false);
  let targetWindows = $state<TargetWindowInfo[]>([]);
  let pinnedHwnd = $state<number | null>(null);
  // User chose a full-screen capture target in the picker (backend full_screen_mode).
  // Mutually exclusive with pinnedHwnd; the user-initiated replacement for the
  // old AI-requested full-screen consent flow.
  let fullScreenTarget = $state(false);
  // Connected monitors. With 2+ the picker offers individual screens (a stitched
  // all-screens capture is downscaled past usefulness); with 1 it's "Entire desktop".
  type MonitorInfo = { index: number; primary: boolean; x: number; y: number; width: number; height: number; };
  let monitors = $state<MonitorInfo[]>([]);
  // Which screen the full-screen target is pinned to (null = whole desktop, the
  // single-monitor case). Drives the picker checkmark and the header chip label.
  let fullScreenMonitorIndex = $state<number | null>(null);

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
    dismissTargetHint(); // they found the picker — the coach mark is no longer needed
    [targetWindows, monitors] = await Promise.all([
      invoke<TargetWindowInfo[]>("list_target_windows"),
      invoke<MonitorInfo[]>("list_monitors"),
    ]);
    targetPickerOpen = true;
  }

  // One-time coach mark on the target-app chip — testers didn't realise the
  // chip is clickable (it reads as a status badge). Shown once ever (flag is
  // written the moment it appears), then fades on its own — no acknowledgement
  // needed. Clicking the bubble opens the picker it's describing.
  const TARGET_HINT_KEY = "navisual-target-chip-hint-v1";
  let showTargetHint = $state(false);
  function maybeShowTargetHint() {
    if (showTargetHint) return;
    if (localStorage.getItem(TARGET_HINT_KEY)) return;
    localStorage.setItem(TARGET_HINT_KEY, "1");
    showTargetHint = true;
    setTimeout(() => (showTargetHint = false), 12_000);
  }
  function dismissTargetHint() {
    showTargetHint = false;
  }

  async function selectTarget(hwnd: number | null) {
    targetPickerOpen = false;
    fullScreenTarget = false;
    if (hwnd === null) {
      await invoke("unpin_target_window");
      pinnedHwnd = null;
    } else {
      await invoke("pin_target_window", { hwnd });
      pinnedHwnd = hwnd;
    }
  }

  // Full-screen capture target — the user-initiated full-screen target. `monitorIndex`
  // pins a single screen (multi-monitor); `null` shares the whole desktop (single
  // monitor). Sticky like a pin; survives new tasks until the user picks a window or
  // Auto-detect again.
  async function selectDesktop(monitorIndex: number | null) {
    targetPickerOpen = false;
    await invoke("pin_full_screen_target", { monitorIndex });
    pinnedHwnd = null;
    fullScreenTarget = true;
    fullScreenMonitorIndex = monitorIndex;
  }

  // UI state
  let iconMode = $state(false);
  let showSettings = $state(false);
  let showAbout = $state(false);
  // Info (About) dialog tab — "about" (version/links/update) or "usage" (token usage).
  let aboutTab = $state<"about" | "usage">("about");
  // First-run privacy disclosure (S5). One-shot; persisted in localStorage so
  // it never fires again on the same install.
  let showPrivacyDisclosure = $state(false);
  const PRIVACY_DISCLOSURE_KEY = "navisual-privacy-disclosed-v1";
  let appVersion = $state("…");
  let pendingUpdate = $state<Update | null>(null);
  let updateStatus = $state<"idle" | "checking" | "downloading" | "done">("idle");
  let updateProgress = $state(0);
  let settingsTab = $state<SettingsTab>("provider");

  // Info (About) dialog → Usage tab
  type UsageRow = {
    provider: string; model: string;
    daily_in: number; daily_out: number; monthly_in: number; monthly_out: number;
    daily_cost: number | null; monthly_cost: number | null; free: boolean;
  };
  let usageRows = $state<UsageRow[]>([]);
  let usageManagedRemaining = $state<number | null>(null);
  let usagePeriod = $state<"today" | "month">("today");
  let usageLoaded = $state(false);
  // BYOK / local token usage only. Managed rows are billed as coins, not tokens —
  // they're shown in the separate "Navisual account" section, never in this token
  // table (filtered by provider name, so real token counts on managed are harmless).
  let usageView = $derived(
    usageRows
      .filter((r) => r.provider !== "managed")
      .map((r) => ({
        provider: r.provider,
        model: r.model,
        tokens: usagePeriod === "today" ? r.daily_in + r.daily_out : r.monthly_in + r.monthly_out,
        cost: usagePeriod === "today" ? r.daily_cost : r.monthly_cost,
        free: r.free,
      })),
  );
  let usageTotalCost = $derived(usageView.reduce((s, r) => s + (r.cost ?? 0), 0));
  let usageHasEstimate = $derived(usageView.some((r) => r.cost != null && !r.free));

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
    custom_api_key: "", custom_model: "", custom_base_url: "",
    managed_tier: "regular",
    overlay_color: "#FF6B35", overlay_thickness: 4,
    subtitle_enabled: true, auto_advance: false,
    tts_enabled: true, tts_voice: "", voice_input_enabled: false, voice_language: "auto",
    hotkey_next: "Ctrl+Backquote", hotkey_wrong: "Ctrl+KeyE",
    hotkey_pause: "", hotkey_icon: "", hotkey_talk: "Ctrl+KeyD",
    debug_screenshot_enabled: false,
    debug_show_response_info: false,
    debug_locate_trace_enabled: false,
    debug_locate_log_file_enabled: false,
    debug_prompt_log_file_enabled: false,
    task_suggestions: true,
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
  // Qwen DashScope OpenAI-compatible endpoints by region. Picking a region in the
  // Settings "Endpoint" dropdown auto-fills qwen_base_url; "Custom" reveals a free
  // field for local servers (LM Studio / llama.cpp) and workspace URLs (e.g. HK ws-xxx…).
  const QWEN_ENDPOINTS = {
    intl: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
    beijing: "https://dashscope.aliyuncs.com/compatible-mode/v1",
  };
  let qwenEndpointChoice = $derived(
    settingsForm.qwen_base_url === QWEN_ENDPOINTS.intl ? "intl" : "beijing"
  );
  // Qwen now offers only the two cloud regions (custom/local moved to its own
  // "Custom" provider). Pin a stale or non-preset qwen_base_url back to a region
  // the moment Qwen is active, so the dropdown and the saved value never disagree.
  $effect(() => {
    if (
      settingsForm.api_provider === "qwen" &&
      settingsForm.qwen_base_url !== QWEN_ENDPOINTS.intl &&
      settingsForm.qwen_base_url !== QWEN_ENDPOINTS.beijing
    ) {
      settingsForm.qwen_base_url = QWEN_ENDPOINTS.beijing;
    }
  });

  let showKeyAnthropic = $state(false);
  let showKeyGemini = $state(false);
  let showKeyOpenAI = $state(false);
  let showKeyDeepSeek = $state(false);
  let showKeyQwen = $state(false);
  let showKeyCustom = $state(false);
  let debugShowInfo = $state(false);

  let customAnthropic = $state(false);
  let customGemini = $state(false);
  let customOpenAI = $state(false);
  let customDeepSeek = $state(false);
  let customQwen = $state(false);
  let customOllama = $state(false);

  function syncCustomModelFlags() {
    customAnthropic = !MODEL_PRESETS_ANTHROPIC.includes(settingsForm.anthropic_model);
    customGemini = !MODEL_PRESETS_GEMINI.includes(settingsForm.gemini_model);
    customOpenAI = !MODEL_PRESETS_OPENAI.includes(settingsForm.openai_model);
    customDeepSeek = !MODEL_PRESETS_DEEPSEEK.includes(settingsForm.deepseek_model);
    customQwen = !MODEL_PRESETS_QWEN.includes(settingsForm.qwen_model);
    customOllama = !ollamaModels.includes(settingsForm.ollama_model);
  }
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

  // Ollama: live list of models installed on the server (GET /api/tags via the
  // backend, so the LAN/http server isn't blocked by WebView fetch rules).
  let ollamaModels = $state<string[]>([]);
  let ollamaModelsMsg = $state<string>("");

  async function refreshOllamaModels() {
    ollamaModelsMsg = "Loading…";
    try {
      const baseUrl = settingsForm.ollama_base_url?.trim() || "http://localhost:11434";
      const models = await invoke<string[]>("list_ollama_models", { baseUrl });
      ollamaModels = models;
      ollamaModelsMsg = models.length ? "" : "No models found — pull one with `ollama pull`.";
      customOllama = !ollamaModels.includes(settingsForm.ollama_model);
    } catch {
      ollamaModels = [];
      ollamaModelsMsg = "Couldn't reach the Ollama server — type the model name below.";
      customOllama = true;
    }
  }

  function fmtTok(n: number): string {
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
    if (n >= 1_000) return (n / 1_000).toFixed(1) + "k";
    return String(n);
  }
  function fmtCost(c: number | null, free: boolean): string {
    if (free) return "free";
    if (c == null) return "—";
    return "~$" + c.toFixed(c < 1 ? 3 : 2);
  }
  async function loadUsage() {
    try {
      const res = await invoke<{ rows: UsageRow[]; managed_free_remaining: number | null }>(
        "get_usage",
      );
      usageRows = res.rows;
      usageManagedRemaining = res.managed_free_remaining;
    } catch {
      usageRows = [];
      usageManagedRemaining = null;
    }
    usageLoaded = true;
  }
  async function resetUsage() {
    await invoke("reset_usage").catch(() => {});
    loadUsage();
  }
  function openAbout(tab: "about" | "usage" = "about") {
    aboutTab = tab;
    if (tab === "usage") loadUsage();
    showAbout = true;
  }

  // The panel is always-on-top (instructions stay visible over the target app).
  // The checkout/OAuth flow temporarily drops this so the browser isn't buried,
  // then restores it on return.
  async function setPanelOnTop(onTop: boolean) {
    try { await getCurrentWindow().setAlwaysOnTop(onTop); } catch (_) {}
  }

  // Buy coins. Tries to create a Stripe Checkout session directly; if the
  // backend says oauth_required (anonymous session), it runs Google OAuth
  // first and retries. Signed-in users skip OAuth entirely — no need to track
  // is_anonymous on the client. Opens Stripe Checkout in the system browser.
  async function buyCoins(amountUsd = 20) {
    if (oauthPending || checkoutPending) return;
    // The checkout/OAuth pages open in the system browser. The panel is
    // alwaysOnTop, so it would sit OVER the browser even when the browser has
    // focus — and the open Settings modal covers the draggable titlebar. So
    // close Settings and drop always-on-top here; refreshBalance() restores it
    // when the user returns (auto via the focus listener, or the manual button).
    showSettings = false;
    await setPanelOnTop(false);
    try {
      let url: string;
      try {
        url = await invoke<string>("create_checkout", { amountUsd });
      } catch (e) {
        if (String(e).includes("oauth_required")) {
          oauthPending = true;
          await invoke("start_google_oauth"); // oauth_complete listener refreshes balance
          oauthPending = false;
          url = await invoke<string>("create_checkout", { amountUsd });
        } else {
          throw e;
        }
      }
      checkoutPending = true;
      openUrl(url);
    } catch (e) {
      addToHistory("system", "⚠️ Checkout failed: " + String(e));
      await setPanelOnTop(true); // nothing opened — restore always-on-top
    } finally {
      oauthPending = false;
    }
  }

  // Re-fetch balance from the relay (after returning from Stripe Checkout).
  // Always clears the pending flags — by the time we refresh, the checkout/OAuth
  // round-trip is over (whether the user paid or cancelled), so the UI shouldn't
  // stay stuck on "Checkout open in browser…".
  async function refreshBalance() {
    try {
      const bal = await invoke<{ tier: string; free_remaining: number; coin_balance_microdollars: number }>("get_balance");
      freeRemaining = bal.free_remaining;
      coinBalance = bal.coin_balance_microdollars;
      managedTier = (bal.tier === "paid") ? "paid" : "free";
      if (managedTier === "paid") showTrialExhausted = false;
    } catch (_) {}
    oauthPending = false;
    checkoutPending = false;
    await setPanelOnTop(true); // back from the browser — restore always-on-top
  }

  // ── Account management (S.2.1) ──────────────────────────────────────────────

  // Fetch the current identity and pick the right Account view. Called when the
  // Account tab opens and on every `account_changed` event. Pass force=true from a
  // flow that has just *completed* (verify success) so it advances past the guard.
  async function loadAccountInfo(force = false) {
    try {
      accountInfo = await invoke<AccountInfo>("get_account_info");
    } catch (_) {
      accountInfo = null;
    }
    // Don't yank the user out of a multi-step flow (verify/forgot) on a passive refresh.
    if (!force && (accountView === "verify_signup" || accountView === "verify_reset" || accountView === "forgot")) return;
    accountView = acctSignedIn ? "account" : "signin";
  }

  function resetAcctFields() {
    acctPassword = "";
    acctCode = "";
    acctNewPassword = "";
    acctError = "";
    acctNotice = "";
  }

  // Add an email + password to the current anonymous account (in-place upgrade),
  // then move to the OTP-entry step.
  async function acctSignUp() {
    if (acctBusy) return;
    acctError = ""; acctNotice = "";
    if (!acctEmail.trim() || acctPassword.length < 6) {
      acctError = "Enter an email and a password of at least 6 characters.";
      return;
    }
    acctBusy = true;
    try {
      await invoke("sign_up_email", { email: acctEmail.trim(), password: acctPassword });
      acctNotice = `Enter the verification code we emailed to ${acctEmail.trim()}. Already requested one? It's valid for 1 hour.`;
      accountView = "verify_signup";
    } catch (e) {
      const msg = String(e);
      if (/sign in instead/i.test(msg)) {
        // Email already belongs to a confirmed account — route to sign-in (email stays prefilled).
        accountView = "signin";
        acctNotice = "This email already has an account. Enter your password to sign in.";
      } else {
        acctError = msg;
      }
    } finally {
      acctBusy = false;
    }
  }

  // Resend a fresh sign-up code (used by "Resend code" and the unverified-login path).
  async function acctResend() {
    if (acctBusy) return;
    acctError = ""; acctNotice = "";
    if (!acctEmail.trim()) { acctError = "Enter your email first."; return; }
    acctBusy = true;
    try {
      await invoke("resend_email_otp", { email: acctEmail.trim() });
      acctNotice = `New code sent to ${acctEmail.trim()}. Enter it below.`;
      accountView = "verify_signup";
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctVerifySignup() {
    if (acctBusy) return;
    acctError = "";
    if (acctCode.trim().length < 6) { acctError = "Enter the code from your email."; return; }
    acctBusy = true;
    try {
      await invoke("verify_email_otp", { email: acctEmail.trim(), token: acctCode.trim() });
      resetAcctFields();
      await loadAccountInfo(true);   // flow complete → leave the verify page for "account"
      await refreshBalance();
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctSignIn() {
    if (acctBusy) return;
    acctError = ""; acctNotice = "";
    if (!acctEmail.trim() || !acctPassword) { acctError = "Enter your email and password."; return; }
    acctBusy = true;
    try {
      await invoke("sign_in_email", { email: acctEmail.trim(), password: acctPassword });
      resetAcctFields();
      await loadAccountInfo();   // → "account"
      await refreshBalance();
    } catch (e) {
      const msg = String(e);
      if (/EMAIL_NOT_CONFIRMED/i.test(msg)) {
        // Account exists but its email was never verified → finish verification.
        accountView = "verify_signup";
        try {
          await invoke("resend_email_otp", { email: acctEmail.trim() });
          acctNotice = `Your email isn't verified yet — we sent a new code to ${acctEmail.trim()}. Enter it below.`;
        } catch {
          acctNotice = `Your email isn't verified yet. Enter the code we emailed to ${acctEmail.trim()}, or tap Resend code.`;
        }
      } else {
        acctError = msg;
      }
    } finally {
      acctBusy = false;
    }
  }

  async function acctSignOut() {
    if (acctBusy) return;
    acctBusy = true; acctError = "";
    try {
      await invoke("sign_out");  // backend re-signs anonymously (free quota is per-device)
      accountInfo = null;
      acctEmail = "";
      resetAcctFields();
      accountView = "signin";
      showChangePw = false;
      showDeleteConfirm = false;
      await loadAccountInfo();
      await refreshBalance();
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctForgot() {
    if (acctBusy) return;
    acctError = ""; acctNotice = "";
    if (!acctEmail.trim()) { acctError = "Enter your account email."; return; }
    acctBusy = true;
    try {
      await invoke("request_password_reset", { email: acctEmail.trim() });
      acctNotice = `We sent a reset code to ${acctEmail.trim()}. Enter it with your new password.`;
      accountView = "verify_reset";
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctVerifyReset() {
    if (acctBusy) return;
    acctError = "";
    if (acctCode.trim().length < 6 || acctNewPassword.length < 6) {
      acctError = "Enter the code from your email and a new password (min 6 characters).";
      return;
    }
    acctBusy = true;
    try {
      await invoke("verify_password_reset", {
        email: acctEmail.trim(),
        token: acctCode.trim(),
        newPassword: acctNewPassword,
      });
      resetAcctFields();
      await loadAccountInfo(true);   // reset complete → leave the verify page for "account"
      await refreshBalance();
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctChangePassword() {
    if (acctBusy) return;
    acctError = ""; acctNotice = "";
    if (acctNewPassword.length < 6) { acctError = "New password must be at least 6 characters."; return; }
    acctBusy = true;
    try {
      await invoke("change_password", { newPassword: acctNewPassword });
      acctNewPassword = "";
      showChangePw = false;
      acctNotice = "Password changed.";
    } catch (e) {
      acctError = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctDeleteAccount() {
    if (acctBusy) return;
    acctBusy = true; acctError = "";
    try {
      await invoke("delete_account");  // backend re-signs anonymously (free quota is per-device)
      accountInfo = null;
      acctEmail = "";
      resetAcctFields();
      showDeleteConfirm = false;
      showChangePw = false;
      accountView = "signin";
      await loadAccountInfo();
      await refreshBalance();
    } catch (e) {
      acctError = String(e);
      showDeleteConfirm = false;
    } finally {
      acctBusy = false;
    }
  }

  async function openSettings() {
    settingsError = null;
    settingsSaved = false;
    showKeyAnthropic = false; showKeyGemini = false; showKeyOpenAI = false; showKeyDeepSeek = false; showKeyQwen = false; showKeyCustom = false;
    ollamaModels = []; ollamaModelsMsg = "";
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
      syncCustomModelFlags();
      debugShowInfo = data.debug_show_response_info;
      if (data.api_provider === "ollama") refreshOllamaModels();
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
    syncCustomModelFlags();
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
      if (providerLabel && providerLabel !== lastAppliedModel) {
        addToHistory("system", `AI provider switched to ${providerLabel}`);
        lastAppliedModel = providerLabel;
      }
    } catch (e) {
      settingsError = String(e);
    } finally {
      settingsSaving = false;
    }
  }

  async function applySettingsAndClose() {
    await applySettings();
    if (!settingsError) showSettings = false;
  }

  async function newSession() {
    isOverlayCleared = false;
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
    await addToHistory(
      "system",
      "New session started — guidance follows the app you click into next. To lock one app, click its name in the title bar.",
    );
    // Workstream P: fresh session, fresh cold-start prefill.
    clearPrefill();
    coldStartPrefill();
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
    if (res.model) routedModel = res.model;
    phase = res.needs_input ? "needs_input" : "guiding";
    if (res.instruction) {
      const cleanInstruction = res.instruction;
      let meta: string | undefined;
      if (res.located) {
        meta = `${res.located.role} · ${(res.located.confidence * 100).toFixed(0)}% · ${res.located.name}`;
      } else if (steps[idx]?.target_text) {
        meta = `not located · "${steps[idx].target_text}"`;
      }
      if (res.model) meta = meta ? `${meta} · ${res.model}` : res.model;
      if (res.input_tokens != null && res.output_tokens != null) {
        const k = (n: number) => (n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));
        const tok = `${k(res.input_tokens)} in · ${k(res.output_tokens)} out`;
        meta = meta ? `${meta} · ${tok}` : tok;
      }
      addToHistory("ai", cleanInstruction, meta);
      if (!isMuted) invoke("speak", { text: cleanInstruction, lang: settingsForm.voice_language }).catch(() => {});
    }
    if (res.debug_screenshot_path) {
      addToHistory("system", `📷 ${res.debug_screenshot_path}`);
    }
    // Workstream P: the AI offered next-task guesses (task complete / nothing in
    // progress). applyPrefill enforces the guards (toggle, needs_input, typed text).
    if (res.suggested_tasks?.length) {
      applyPrefill(res.suggested_tasks);
    }
  }

  async function guide() {
    if (!task.trim()) return;
    isOverlayCleared = false;
    const taskText = task.trim();
    task = "";
    clearPrefill();
    // Keep session context when in the middle of a task; start fresh from idle/error.
    const isReply = phase === "guiding" || phase === "needs_input";
    const prevPhase = phase;
    const userEntryId = await addToHistory("user", taskText);
    currentInstruction = "";
    staleResponse = false;
    pointerOccluded = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("guide", { task: taskText, isReply });
      stopTimer();
      if (token !== requestToken) return;
      if (res.chat_thumb_b64) attachThumb(userEntryId, res.chat_thumb_b64);
      if (!res.ok) {
        phase = prevPhase;
        addToHistory("system", "⚠️ " + (res.error ?? "guide failed"));
        if (taskText !== "") task = taskText;
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      addToHistory("system", "⚠️ " + String(e));
      if (taskText !== "") task = taskText;
    }
  }

  async function nextStep() {
    // Don't allow next while an AI call is in flight — the hotkey can fire
    // even when the Next button is disabled (Svelte derived state edge case).
    if (phase === "thinking") return;
    isOverlayCleared = false;
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
    isOverlayCleared = false;
    const rawNote = task.trim();
    if (rawNote) task = "";
    // Fold a steering hint for the reason into the note the AI sees, then the
    // user's own text. (The logged note is the user's raw text only.)
    const hint = category ? (CATEGORY_HINT[category] ?? "") : "";
    const note = [hint, rawNote].filter(Boolean).join(" ").trim();
    // Wrong spot: tell the locator where NOT to point again — the bbox the
    // rejected pointer occupied. The retry then surfaces the second-best match
    // instead of deterministically repeating the same pick.
    const avoidBbox = category === "wrong_spot" && locateResult ? locateResult.bbox : null;
    const label = (category && CATEGORY_LABEL[category]) || "Wrong";
    const prevPhase = phase;
    const corrEntryId = await addToHistory("correction", rawNote ? `${label} — ${rawNote}` : `${label} — re-analysing…`);
    currentInstruction = "";
    staleResponse = false;
    pointerOccluded = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction", { note: note || null, avoidBbox });
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
          model: routedModel || activeModel,
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
    if (phase === "guiding") {
      // Workstream P: while the picker is open the task box is a free-text
      // "wrong" note — an untouched prefill must not become one accidentally.
      if (prefillActive) {
        task = "";
        clearPrefill();
      }
      wrongPickerOpen = true;
    } else correction();
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
    // Workstream P: Esc dismisses the prefill entirely (text + dropdown) —
    // back to the empty box the user had before.
    if (e.key === "Escape" && prefillActive) {
      task = "";
      clearPrefill();
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
    : settingsForm.api_provider === "custom" ? (settingsForm.custom_model || "custom")
    : settingsForm.api_provider === "managed" ? "managed"
    : settingsForm.openai_model
  );
  let headerLabel = $derived(activeModel || provider);
  // Human-readable "supplier — detail" for the chat status messages. For managed
  // it names the selected quality tier (Speed/Regular/Smart) so the user can see
  // which managed model is active; for BYOK it's the provider + model.
  const TIER_LABELS: Record<string, string> = { speed: "Speed", regular: "Regular", smart: "Smart" };
  const TIER_COINS: Record<string, number> = { speed: 6, regular: 12, smart: 18 };
  let providerLabel = $derived(
    settingsForm.api_provider === "managed" ? `Managed (${TIER_LABELS[settingsForm.managed_tier] ?? "Regular"})`
    : settingsForm.api_provider === "anthropic" ? `Anthropic · ${settingsForm.anthropic_model}`
    : settingsForm.api_provider === "gemini" ? `Google Gemini · ${settingsForm.gemini_model}`
    : settingsForm.api_provider === "openai" ? `OpenAI · ${settingsForm.openai_model}`
    : settingsForm.api_provider === "deepseek" ? `DeepSeek · ${settingsForm.deepseek_model}`
    : settingsForm.api_provider === "qwen" ? `Qwen · ${settingsForm.qwen_model}`
    : settingsForm.api_provider === "ollama" ? `Ollama · ${settingsForm.ollama_model}`
    : settingsForm.api_provider === "custom" ? `Custom · ${settingsForm.custom_model || "model"}`
    : activeModel
  );
  let lastAppliedModel = $state<string>("");

  // Friendly provider names for the Usage tab's "your own key" note.
  const PROVIDER_NAMES: Record<string, string> = {
    managed: "Navisual", anthropic: "Anthropic", gemini: "Google Gemini",
    openai: "OpenAI", deepseek: "DeepSeek", qwen: "Qwen", ollama: "Ollama",
    custom: "custom endpoint",
  };
  // BYOK = a provider billed on the user's own account (not managed, not local Ollama).
  let isByok = $derived(!["managed", "ollama"].includes(settingsForm.api_provider));

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
      syncCustomModelFlags();
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
      const prevExe = sharedApp?.exe_name;
      sharedApp = event.payload;
      maybeShowTargetHint();
      // Workstream P: a different app means stale guesses — refresh the cold-start
      // prefill (no-op unless idle with an untouched box; clearPrefill first so an
      // old app's prefill can't survive the switch).
      if (event.payload.exe_name !== prevExe) {
        if (prefillActive) {
          task = "";
          clearPrefill();
        }
        coldStartPrefill();
      }
    });
    try {
      const initial = await invokeReady<SharedAppInfo | null>("get_shared_app_info");
      if (initial) {
        sharedApp = initial;
        maybeShowTargetHint();
      }
    } catch (_) {}
    // Workstream P: first cold-start prefill once the shared-app info settled.
    coldStartPrefill();

    // E.3 — Autopilot: on-demand screen-change polling.
    // Functions are defined at module level; start now if already enabled.
    if (autoAdvanceEnabled) startAutopilotPolling();

    await registerShortcuts(initHotkeys);

    // (Removed: hardcoded Ctrl+A push-to-talk global shortcut. It hijacked the
    // OS-wide "select all" combo so users couldn't Ctrl+A in Word or any other
    // app while Navisual was running. Voice input remains available via the
    // mic button in the action row.)

    // S.1/S.2 — Managed provider: anonymous sign-in on first launch.
    if (settingsForm.api_provider === "managed") {
      try {
        await invokeReady("sign_in_anon");
      } catch (e) {
        addToHistory("system", "⚠️ Managed sign-in failed: " + String(e));
      }
      try {
        const bal = await invokeReady<{ tier: string; free_remaining: number; coin_balance_microdollars: number }>("get_balance");
        freeRemaining = bal.free_remaining;
        coinBalance = bal.coin_balance_microdollars;
        managedTier = (bal.tier === "paid") ? "paid" : "free";
      } catch (_) {}
    }

    listen<number>("balance_update", (event) => {
      freeRemaining = event.payload;
      if (freeRemaining <= 0 && managedTier === "free") showTrialExhausted = true;
    });

    // Paid tier — coins debited server-side after each request; relay returns
    // the new µ$ balance in X-Coin-Balance and the backend forwards it here.
    listen<number>("coin_balance_update", (event) => {
      coinBalance = event.payload;
      managedTier = "paid";
      if (coinBalance <= 0) showTrialExhausted = true;
    });

    // When the panel regains focus after a checkout, pull the fresh balance and
    // clear the pending state — covers both "paid" and "cancelled the page".
    getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused && checkoutPending) refreshBalance();
    });

    listen("trial_exhausted", () => {
      freeRemaining = 0;
      showTrialExhausted = true;
    });

    listen("oauth_complete", async () => {
      oauthPending = false;
      // Refresh balance — tier is now paid if the user had pre-existing coins.
      try {
        const bal = await invoke<{ tier: string; free_remaining: number; coin_balance_microdollars: number }>("get_balance");
        freeRemaining = bal.free_remaining;
        coinBalance = bal.coin_balance_microdollars;
        managedTier = (bal.tier === "paid") ? "paid" : "free";
      } catch (_) {}
    });

    // Emitted after any account change (sign in/up/out, delete) so the Account
    // tab reflects the new identity if it's open.
    listen("account_changed", () => {
      if (showSettings && settingsTab === "account") loadAccountInfo();
    });

    // Backend detected the screen drifted enough during AI thinking
    // (Hamming distance ≥ STALE_RESPONSE_THRESHOLD between pre-call and
    // post-response captures) that the rendered guidance may not match
    // what's on screen any more.
    listen("ai_response_stale", () => {
      staleResponse = true;
    });

    // Backend located the target but hid the pointer because the target window is
    // covered by another app. Offer a re-analyse (after the user brings it forward).
    listen("pointer_occluded", () => {
      pointerOccluded = true;
    });
    // The tracker auto-redrew the pointer once the target became visible again.
    listen("pointer_restored", () => {
      pointerOccluded = false;
    });

    lastAppliedModel = providerLabel;
    await addToHistory("system", `Navisual ready — using ${providerLabel}`);
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
    <div class="titlebar" role="toolbar" tabindex="-1" onmousedown={handleHeaderMousedown}>
      <span class="header-dot"></span>
      <span class="header-title">Navisual</span>
      <button
        class="header-shared"
        class:header-shared-pinned={pinnedHwnd !== null || fullScreenTarget}
        title={fullScreenTarget ? "Sharing your screen — click to switch target" : pinnedHwnd !== null ? "Target app pinned — click to switch or unpin" : "Target app — click to switch or pin"}
        onmousedown={(e) => e.stopPropagation()}
        onclick={openTargetPicker}
      >
        <span class="header-shared-dot"></span>
        {#if fullScreenTarget}
          🖥️ {fullScreenMonitorIndex !== null ? `Screen ${fullScreenMonitorIndex + 1}` : "Entire desktop"}
        {:else if sharedApp}
          {friendlyName(sharedApp.exe_name) || sharedApp.app_name}
          {#if pinnedHwnd !== null}<span class="header-shared-pin">📌</span>{/if}
        {:else}
          Auto-detect
        {/if}
        <span class="header-shared-caret">▾</span>
      </button>
      {#if settingsForm.api_provider === "managed" && managedTier === "paid" && coinBalance !== null}
        <button class="header-balance" class:header-balance-low={coinBalance < 200_000} onclick={() => openAbout("usage")} title="View coin balance">{Math.floor(coinBalance / 5_000)} 🪙</button>
      {:else if settingsForm.api_provider === "managed" && freeRemaining !== null}
        <button class="header-balance" class:header-balance-low={freeRemaining <= 5} onclick={() => openAbout("usage")} title="View usage">{freeRemaining} left</button>
      {/if}
      {#if pendingUpdate}
        <button class="header-update" onclick={() => openAbout("about")} title="Update available">
          ↑ {pendingUpdate.version}
        </button>
      {/if}
      <div class="header-actions">
        <button class="hdr-btn" onclick={() => openAbout("about")} title="About Navisual" aria-label="About Navisual">
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

        <!-- Pointer hidden because the target window is covered by another app -->
        {#if pointerOccluded && phase === "guiding"}
          <div class="stale-banner" role="status">
            <span class="stale-icon">⊘</span>
            <span class="stale-text">Target window isn't visible — bring it to the front to see the pointer.</span>
            <button class="stale-action" onclick={() => { pointerOccluded = false; correction(); }} title="Re-analyse the current screen">↻ Re-analyse</button>
            <button class="stale-dismiss" onclick={() => (pointerOccluded = false)} title="Dismiss">✕</button>
          </div>
        {/if}

        <!-- D6: subtle miss note — only when a target was expected but genuinely not found -->
        {#if !locateResult && !pointerOccluded && steps[stepIndex]?.target_text && phase === "guiding"}
          <p class="miss-note">⊘ Pointer unavailable — follow the instruction above</p>
        {/if}

        <!-- Feedback: mark this step wrong (promoted from the ··· quick-menu) -->
        {#if phase === "guiding"}
          <div class="wrong-footer">
            {#if !wrongPickerOpen}
              <button class="wrong-btn" onclick={openWrongPicker} title="This guidance is wrong (Ctrl+E)">✗ This is wrong</button>
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

                <!-- Selection section (Pass 0.5 — Structured-Context, v0.7 S.3) -->
                {#if locateTrace.selection}
                  {@const s = locateTrace.selection}
                  <div class="debug-section">
                    <div class="debug-section-head">
                      Selection · id {s.id} of {s.snapshot_len} element{s.snapshot_len === 1 ? "" : "s"}
                    </div>
                    <div class="debug-cand {s.verified ? 'cand-selected' : 'cand-rejected'}">
                      <span class="cand-mark">{s.verified ? "✔" : "⊘"}</span>
                      <span class="cand-text">{s.snapshot_name ? `"${s.snapshot_name}"` : "id not in snapshot"}</span>
                      <span class="cand-reason">— {s.detail}</span>
                    </div>
                  </div>
                {/if}

                <!-- A11y section -->
                <div class="debug-section">
                  <div class="debug-section-head">
                    A11y · {locateTrace.a11y.candidates.length} candidate{locateTrace.a11y.candidates.length === 1 ? "" : "s"}
                    {#if locateTrace.a11y.framework} · {locateTrace.a11y.framework.toLowerCase()}{/if}
                    {#if locateTrace.a11y.cached} · cached{/if}
                    {#if locateTrace.a11y.element_count !== null} · {locateTrace.a11y.element_count} elems{/if}
                    {#if locateTrace.a11y.timed_out} · timed out{/if}
                    {#if locateTrace.a11y.retried} · retried{/if}
                    · {locateTrace.a11y.elapsed_ms} ms
                  </div>
                  {#if locateTrace.a11y.regex_used}
                    <div class="debug-mono">{locateTrace.a11y.regex_used}</div>
                  {/if}
                  {#if locateTrace.a11y.bbox_probe}
                    {@const p = locateTrace.a11y.bbox_probe}
                    <div class="debug-cand {p.accepted ? 'cand-selected' : 'cand-rejected'}">
                      <span class="cand-mark">{p.accepted ? "✔" : "·"}</span>
                      <span class="cand-text">bbox probe{p.resolved_name ? ` → "${p.resolved_name}"` : ""}</span>
                      {#if p.resolved_role}<span class="cand-meta">{p.resolved_role}</span>{/if}
                      <span class="cand-reason">— {p.detail}</span>
                    </div>
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
                    {#if locateTrace.ocr.corroboration}
                      {@const co = locateTrace.ocr.corroboration}
                      <div class="debug-cand {co.accepted ? 'cand-selected' : 'cand-rejected'}">
                        <span class="cand-mark">{co.accepted ? "✔" : "⊘"}</span>
                        <span class="cand-text">corroboration {co.accepted ? "accepted" : "rejected"}</span>
                        <span class="cand-meta">uia={co.uia_control_type ?? "—"}{co.uia_interactive ? "✓" : ""} · iso={co.isolation.toFixed(2)}/{co.isolation_line_len}{co.isolation_ok ? "✓" : ""} · anchor={co.near_anchor ? "✓" : "✗"} · bbox={co.near_ai_bbox ? "✓" : "✗"}</span>
                      </div>
                    {/if}
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

                <!-- Template section (Pass 3 — nav-pack icon matching) -->
                {#if locateTrace.template}
                  {@const t = locateTrace.template}
                  <div class="debug-section">
                    <div class="debug-section-head">
                      Template · {t.templates_tried} icon{t.templates_tried === 1 ? "" : "s"}
                      {#if t.scale_prior !== 1} · dpi prior {t.scale_prior.toFixed(2)}×{/if}
                    </div>
                    <div class="debug-cand {t.accepted ? 'cand-selected' : 'cand-rejected'}">
                      <span class="cand-mark">{t.accepted ? "✔" : "⊘"}</span>
                      <span class="cand-text">{t.best_icon ? `"${t.best_icon}"` : "no icon decoded"}</span>
                      <span class="cand-meta">{(t.best_score * 100).toFixed(0)}% · scale {t.best_scale.toFixed(2)}×{t.best_pos ? ` · @(${t.best_pos[0]},${t.best_pos[1]})` : ""}</span>
                      {#if !t.accepted}<span class="cand-reason">— below 90% threshold</span>{/if}
                    </div>
                  </div>
                {/if}

                <!-- Corroboration rejection detail -->
                {#if locateTrace.final_decision.kind === "rejected_uncorroborated"}
                  <div class="debug-section">
                    <div class="debug-section-head" style="color: #f59e0b">⊘ Uncorroborated — no pointer (content text?)</div>
                    <div class="debug-row">
                      <span class="debug-key">detail</span>
                      <span class="debug-val">{locateTrace.final_decision.detail}</span>
                    </div>
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
      {#if phase === "needs_input"}
        <div class="input-hint">💬 AI needs your input — type your answer below</div>
      {:else if phase === "guiding"}
        <div class="input-hint">Type a follow-up or correction · ＋ for a new task</div>
      {/if}
      <div class="task-input-wrap">
        <textarea
          bind:value={task}
          bind:this={taskInputEl}
          onkeydown={handleKeydown}
          oninput={() => {
            // Real typing replaces the prefill (the selection is typed over) and
            // protects the user's text from any further prefill.
            if (prefillActive) clearPrefill();
          }}
          onfocus={() => {
            // Select-on-focus: applyPrefill skips select() while the panel is a
            // background window (it would steal focus from the target app), so the
            // "one keystroke replaces" behaviour arms when the user clicks in.
            if (prefillActive) taskInputEl?.select();
          }}
          placeholder={phase === "needs_input" ? "Type your answer…" : "What do you need help with?"}
          rows={2}
        ></textarea>
        {#if prefillActive && suggestAlternatives.length > 0}
          <button
            type="button"
            class="suggest-toggle"
            class:suggest-toggle-open={showSuggestAlts}
            onclick={() => (showSuggestAlts = !showSuggestAlts)}
            title="Other suggested tasks"
            aria-label="Show other suggested tasks"
            aria-expanded={showSuggestAlts}
          >▾</button>
          {#if showSuggestAlts}
            <div class="suggest-menu" role="listbox" aria-label="Other suggested tasks">
              {#each suggestAlternatives as s (s)}
                <button
                  class="suggest-item"
                  role="option"
                  aria-selected="false"
                  onclick={() => selectSuggestion(s)}
                >{s}</button>
              {/each}
            </div>
          {/if}
        {/if}
      </div>
      {#if isThinking}
        <button class="btn-ghost btn-full" onclick={cancelRequest}>⏹ Cancel ({(elapsedMs / 1000).toFixed(1)}s)</button>
      {:else}
        <button class="btn-primary btn-full" onclick={submitTask} disabled={!task.trim()}>
          {phase === "needs_input" ? "↩ Send answer" : phase === "guiding" ? "↩ Follow up" : "Guide me"}
        </button>
      {/if}
    </section>

    <!-- Quick-action menu (opened by ··· button) -->
    {#if showQuickMenu}
      <div class="quick-menu">
        <button class="qm-btn" onclick={() => { showQuickMenu = false; openTargetPicker(); }} title="Choose which app Navisual assists with">
          🎯 Switch app
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
        {#each [
          { accel: settingsForm.hotkey_next,  label: "Next" },
          { accel: settingsForm.hotkey_wrong, label: "Wrong" },
          { accel: settingsForm.hotkey_pause, label: "Pause" },
          { accel: settingsForm.hotkey_icon,  label: "Icon" },
        ] as hk (hk.label)}
          <span class="hk-item" class:hk-unset={!hk.accel}>
            <span class="hk-label">{hk.label}</span>
            {#if hk.accel}
              <kbd class="hk-key">{prettyHotkey(hk.accel)}</kbd>
            {:else}
              <span class="hk-none">not set</span>
            {/if}
          </span>
        {/each}
      </div>
    </footer>
  </main>

  <!-- Target-window picker dropdown (item 1) — fixed so it escapes main's overflow:hidden -->
  {#if targetPickerOpen}
    <div class="target-picker-backdrop" role="presentation" onclick={() => (targetPickerOpen = false)}></div>
    <div class="target-picker" role="listbox" aria-label="Choose target app">
      <button class="target-pick-item" class:target-pick-selected={pinnedHwnd === null && !fullScreenTarget} onclick={() => selectTarget(null)}>
        <span class="target-pick-check">{pinnedHwnd === null && !fullScreenTarget ? "✓" : ""}</span>
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
      {#if monitors.length > 1}
        {#each monitors as m (m.index)}
          <button class="target-pick-item" class:target-pick-selected={fullScreenTarget && fullScreenMonitorIndex === m.index} onclick={() => selectDesktop(m.index)}>
            <span class="target-pick-check">{fullScreenTarget && fullScreenMonitorIndex === m.index ? "✓" : ""}</span>
            <span class="target-pick-name">🖥️ Screen {m.index + 1}{m.primary ? " (primary)" : ""}</span>
            <span class="target-pick-sub">{m.width}×{m.height} — this screen only</span>
          </button>
        {/each}
      {:else}
        <button class="target-pick-item" class:target-pick-selected={fullScreenTarget} onclick={() => selectDesktop(null)}>
          <span class="target-pick-check">{fullScreenTarget ? "✓" : ""}</span>
          <span class="target-pick-name">🖥️ Entire desktop</span>
          <span class="target-pick-sub">share the whole screen — all windows</span>
        </button>
      {/if}
    </div>
  {/if}

  <!-- One-time coach mark pointing at the target-app chip; clicking it opens
       the picker it describes, and it fades on its own after a few seconds. -->
  {#if showTargetHint && sharedApp && !showPrivacyDisclosure && !targetPickerOpen}
    <button class="target-hint" onclick={openTargetPicker}>
      <span class="target-hint-arrow"></span>
      Click here to select the app you want me to assist with.
    </button>
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
            <li>On the free tier, a one-way hash of a device identifier counts your 50 free requests per machine — it can't identify you and isn't used on paid or your-own-key providers.</li>
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

          {#if oauthPending}
            <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
              Signing in with Google in your browser…
            </p>
          {:else if checkoutPending}
            <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
              Checkout opened in your browser. Come back once you've paid — your balance will update automatically.
            </p>
            <button class="btn-primary btn-full" onclick={refreshBalance}>Refresh balance</button>
          {:else}
            <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 16px;">
              Top up with coins to continue on the Navisual managed relay.
            </p>
            <button class="btn-primary btn-full" style="margin-bottom: 6px;" onclick={() => buyCoins(20)}>Buy coins ($20)</button>
            <p class="legal-agree" style="margin-bottom: 14px;">
              By buying coins you agree to our
              <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/terms.html")}>Terms</button>
              and
              <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>Privacy Policy</button>.
            </p>
            <p style="font-size: 0.85em; color: var(--text-secondary); margin-bottom: 16px;">
              Or keep going free with your own key:
              Settings → Provider → Gemini (Google AI Studio) or Ollama (local).
            </p>
          {/if}

          <button class="btn-ghost btn-full" onclick={() => { showTrialExhausted = false; oauthPending = false; checkoutPending = false; }}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- Settings modal (E.6) -->
  <!-- Click-outside does NOT dismiss: Settings is a form with unsaved state, so a
       stray click must not silently discard edits. Close only via Cancel / X
       (discard) or Apply / OK (save). Esc also cancels. -->
  {#if showSettings}
    <div
      class="modal-backdrop"
      role="presentation"
      onkeydown={(e) => { if (e.key === "Escape") showSettings = false; }}
    >
      <div
        class="modal"
        role="dialog"
        tabindex="-1"
        aria-modal="true"
        aria-label="Settings"
      >
        <div class="modal-header">
          <span class="modal-title">Settings</span>
          <button class="hdr-btn hdr-btn-close" onclick={() => (showSettings = false)}>✕</button>
        </div>
        <div class="modal-tabs">
          <button class="tab-btn {settingsTab === 'provider' ? 'tab-active' : ''}" onclick={() => (settingsTab = "provider")}>Provider</button>
          <button class="tab-btn {settingsTab === 'billing' ? 'tab-active' : ''}" onclick={() => { settingsTab = "billing"; refreshBalance(); }}>Billing</button>
          <button class="tab-btn {settingsTab === 'account' ? 'tab-active' : ''}" onclick={() => { settingsTab = "account"; loadAccountInfo(); }}>Account</button>
          <button class="tab-btn {settingsTab === 'screen-guide' ? 'tab-active' : ''}" onclick={() => (settingsTab = "screen-guide")}>Screen Guide</button>
          <button class="tab-btn {settingsTab === 'hotkeys' ? 'tab-active' : ''}" onclick={() => (settingsTab = "hotkeys")}>Hotkeys</button>
          <button class="tab-btn {settingsTab === 'audio' ? 'tab-active' : ''}" onclick={() => (settingsTab = "audio")}>Audio</button>
          {#if settingsForm.developer_mode}
            <button class="tab-btn {settingsTab === 'developer' ? 'tab-active' : ''}" onclick={() => (settingsTab = "developer")}>Developer</button>
          {/if}
        </div>

        <div class="modal-body">
          {#if settingsTab === "billing"}
            <div class="setting-group">
              <span class="setting-label">Account</span>
              <p class="setting-hint">{managedTier === "paid" ? "Paid (coins)" : "Free trial"}</p>
            </div>
            {#if coinBalance !== null && coinBalance > 0}
              <div class="setting-group">
                <span class="setting-label">Coin balance</span>
                <p class="setting-hint">{Math.floor(coinBalance / 5_000)} coins</p>
              </div>
            {/if}
            <div class="setting-group">
              <span class="setting-label">Free requests</span>
              <p class="setting-hint">{freeRemaining ?? "—"} remaining of 50</p>
            </div>
            <p class="setting-hint">Change your <strong>quality tier</strong> (which model answers, and its coin cost) on the <strong>Provider</strong> tab.</p>

            <!-- Amount picker -->
            <div class="setting-group" style="margin-top: 14px;">
              <label class="setting-label" for="amount-select">Top-up amount</label>
              <select id="amount-select" class="setting-select" bind:value={buyAmount}>
                <option value={5}>$5 · 1,000 coins</option>
                <option value={10}>$10 · 2,000 coins</option>
                <option value={20}>$20 · 4,000 coins</option>
                <option value={50}>$50 · 10,000 coins</option>
                <option value="custom">Custom…</option>
              </select>
              {#if buyAmount === "custom"}
                <input
                  class="setting-input" type="number" min="5" max="500" step="1"
                  bind:value={customAmount} placeholder="Enter $5–$500" style="margin-top: 8px;" />
                <p class="setting-hint">
                  {amountValid
                    ? `${(customAmount * 200).toLocaleString()} coins`
                    : "Amount must be $5–$500"}
                </p>
              {/if}
            </div>

            <div class="setting-group" style="margin-top: 12px;">
              <button class="btn-primary" onclick={() => buyCoins(effectiveAmount)} disabled={oauthPending || checkoutPending || !amountValid}>
                {oauthPending ? "Signing in…" : checkoutPending ? "Checkout open in browser…" : `Buy coins ($${effectiveAmount})`}
              </button>
              {#if checkoutPending}
                <button class="btn-ghost" style="margin-top: 8px;" onclick={refreshBalance}>Refresh balance</button>
              {/if}
            </div>
            <p class="setting-hint legal-agree">
              By buying coins you agree to our
              <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/terms.html")}>Terms</button>
              and
              <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>Privacy Policy</button>.
            </p>
            <p class="setting-hint" style="margin-top: 8px;">
              Coins power the Managed provider's paid tiers. Checkout opens in your default
              browser; if you're not signed in yet, Google sign-in runs first. Your balance
              updates automatically when you return.
              {#if settingsForm.api_provider !== "managed"}
                <br /><br />Note: you're currently on the <strong>{settingsForm.api_provider}</strong>
                provider. Switch to <strong>Managed</strong> on the Provider tab to spend coins.
              {/if}
            </p>
          {:else if settingsTab === "account"}
            <!-- Account management (S.2.1): sign in/up/out, forgot/change password, delete -->
            {#if acctError}<p class="setting-hint acct-error">⚠️ {acctError}</p>{/if}
            {#if acctNotice}<p class="setting-hint acct-notice">{acctNotice}</p>{/if}

            {#if accountView === "account"}
              <div class="setting-group">
                <span class="setting-label">Signed in as</span>
                <p class="setting-hint"><strong>{accountInfo?.email}</strong></p>
              </div>
              {#if coinBalance !== null && coinBalance > 0}
                <p class="setting-hint">{Math.floor(coinBalance / 5_000)} coins · your balance and purchases stay with this account.</p>
              {/if}

              {#if acctShowChangePw}
                {#if !showChangePw}
                  <div class="setting-group" style="margin-top: 12px;">
                    <button class="btn-ghost" onclick={() => { showChangePw = true; acctError = ""; acctNotice = ""; }}>Change password</button>
                  </div>
                {:else}
                  <div class="setting-group" style="margin-top: 12px;">
                    <label class="setting-label" for="acct-newpw">New password</label>
                    <input id="acct-newpw" class="setting-input" type="password" bind:value={acctNewPassword} placeholder="At least 6 characters" />
                    <div style="display:flex; gap:8px; margin-top:8px;">
                      <button class="btn-primary" onclick={acctChangePassword} disabled={acctBusy}>{acctBusy ? "Saving…" : "Save password"}</button>
                      <button class="btn-ghost" onclick={() => { showChangePw = false; acctNewPassword = ""; }}>Cancel</button>
                    </div>
                  </div>
                {/if}
              {:else if acctIsGoogle}
                <p class="setting-hint" style="margin-top: 12px;">
                  Signed in with Google — your password is managed by Google, not Navisual. Change it at
                  <button class="legal-link" onclick={() => openUrl("https://myaccount.google.com/security")}>myaccount.google.com</button>.
                </p>
              {/if}

              <div class="setting-group" style="margin-top: 12px;">
                <button class="btn-ghost" onclick={acctSignOut} disabled={acctBusy}>Sign out</button>
              </div>

              <hr class="acct-sep" />
              {#if !showDeleteConfirm}
                <button class="legal-link acct-danger" onclick={() => { showDeleteConfirm = true; acctError = ""; }}>Delete account</button>
              {:else}
                <div class="setting-group">
                  <p class="setting-hint acct-error">This permanently deletes your account. Coins are <strong>not</strong> refunded and cannot be recovered.</p>
                  <div style="display:flex; gap:8px; margin-top:8px;">
                    <button class="btn-danger" onclick={acctDeleteAccount} disabled={acctBusy}>{acctBusy ? "Deleting…" : "Delete permanently"}</button>
                    <button class="btn-ghost" onclick={() => (showDeleteConfirm = false)}>Cancel</button>
                  </div>
                </div>
              {/if}

            {:else if accountView === "signin"}
              <p class="setting-hint">Sign in to keep your coins and purchases across devices.</p>
              <div class="setting-group">
                <label class="setting-label" for="acct-email">Email</label>
                <input id="acct-email" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
              </div>
              <div class="setting-group">
                <label class="setting-label" for="acct-pw">Password</label>
                <input id="acct-pw" class="setting-input" type="password" autocomplete="current-password" bind:value={acctPassword} placeholder="Your password" />
              </div>
              <div class="setting-group" style="margin-top: 10px;">
                <button class="btn-primary" onclick={acctSignIn} disabled={acctBusy}>{acctBusy ? "Signing in…" : "Sign in"}</button>
              </div>
              <div class="acct-links">
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "signup"; }}>Create account</button>
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "forgot"; }}>Forgot password?</button>
              </div>
              <p class="setting-hint" style="margin-top: 8px;">
                Signed up but never verified?
                <button class="legal-link" onclick={acctResend} disabled={acctBusy}>Resend verification code</button>
              </p>
              <hr class="acct-sep" />
              <button class="btn-ghost" onclick={async () => { if (oauthPending) return; oauthPending = true; acctError = ""; try { await invoke("start_google_oauth"); await loadAccountInfo(); await refreshBalance(); } catch (e) { acctError = String(e); } finally { oauthPending = false; } }} disabled={oauthPending}>
                {oauthPending ? "Signing in…" : "Continue with Google"}
              </button>

            {:else if accountView === "signup"}
              <p class="setting-hint">Create an account — your current free requests and any coins carry over.</p>
              <div class="setting-group">
                <label class="setting-label" for="acct-email-up">Email</label>
                <input id="acct-email-up" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
              </div>
              <div class="setting-group">
                <label class="setting-label" for="acct-pw-up">Password</label>
                <input id="acct-pw-up" class="setting-input" type="password" autocomplete="new-password" bind:value={acctPassword} placeholder="At least 6 characters" />
              </div>
              <div class="setting-group" style="margin-top: 10px;">
                <button class="btn-primary" onclick={acctSignUp} disabled={acctBusy}>{acctBusy ? "Sending code…" : "Create account"}</button>
              </div>
              <div class="acct-links">
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "signin"; }}>Already have an account? Sign in</button>
              </div>

            {:else if accountView === "verify_signup"}
              <div class="setting-group">
                <label class="setting-label" for="acct-code">Verification code</label>
                <input id="acct-code" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder="Code from email" />
              </div>
              <div class="setting-group" style="margin-top: 10px;">
                <button class="btn-primary" onclick={acctVerifySignup} disabled={acctBusy}>{acctBusy ? "Verifying…" : "Verify & finish"}</button>
              </div>
              <div class="acct-links">
                <button class="legal-link" onclick={acctResend} disabled={acctBusy}>Resend code</button>
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "signin"; }}>Cancel</button>
              </div>

            {:else if accountView === "forgot"}
              <p class="setting-hint">Enter your account email and we'll send a reset code.</p>
              <div class="setting-group">
                <label class="setting-label" for="acct-email-fp">Email</label>
                <input id="acct-email-fp" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
              </div>
              <div class="setting-group" style="margin-top: 10px;">
                <button class="btn-primary" onclick={acctForgot} disabled={acctBusy}>{acctBusy ? "Sending…" : "Send reset code"}</button>
              </div>
              <div class="acct-links">
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "signin"; }}>Back to sign in</button>
              </div>

            {:else if accountView === "verify_reset"}
              <div class="setting-group">
                <label class="setting-label" for="acct-code-r">Reset code</label>
                <input id="acct-code-r" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder="Code from email" />
              </div>
              <div class="setting-group">
                <label class="setting-label" for="acct-newpw-r">New password</label>
                <input id="acct-newpw-r" class="setting-input" type="password" autocomplete="new-password" bind:value={acctNewPassword} placeholder="At least 6 characters" />
              </div>
              <div class="setting-group" style="margin-top: 10px;">
                <button class="btn-primary" onclick={acctVerifyReset} disabled={acctBusy}>{acctBusy ? "Saving…" : "Set new password"}</button>
              </div>
              <div class="acct-links">
                <button class="legal-link" onclick={() => { resetAcctFields(); accountView = "signin"; }}>Cancel</button>
              </div>
            {/if}

          {:else if settingsTab === "provider"}
            <!-- Provider selector — grouped so it scales as providers + paid tiers grow -->
            <div class="setting-group">
              <label class="setting-label" for="provider-select">Provider</label>
              <select id="provider-select" class="setting-select"
                bind:value={settingsForm.api_provider}
                onchange={() => { if (settingsForm.api_provider === "ollama" && ollamaModels.length === 0) refreshOllamaModels(); }}>
                <optgroup label="Navisual (hosted)">
                  <option value="managed">Managed — free + paid</option>
                </optgroup>
                <optgroup label="Bring your own key">
                  <option value="anthropic">Anthropic</option>
                  <option value="gemini">Google Gemini</option>
                  <option value="openai">OpenAI</option>
                  <option value="deepseek">DeepSeek</option>
                  <option value="qwen">Qwen (DashScope)</option>
                </optgroup>
                <optgroup label="Local &amp; custom">
                  <option value="ollama">Ollama</option>
                  <option value="custom">Custom (OpenAI-compatible)</option>
                </optgroup>
              </select>
            </div>

            <!-- Per-provider contextual hint -->
            <p class="setting-hint provider-hint">
              {#if settingsForm.api_provider === "managed"}
                Free · 50 requests included. Powered by OpenRouter's free model router via the Navisual relay. May be slower than BYOK providers — ideal for getting started.
              {:else if settingsForm.api_provider === "gemini"}
                Recommended for most users outside mainland China. Free API key available at aistudio.google.com.
              {:else if settingsForm.api_provider === "anthropic"}
                Pay per use · highest quality. API key at console.anthropic.com.
              {:else if settingsForm.api_provider === "openai"}
                Pay per use. API key at platform.openai.com.
              {:else if settingsForm.api_provider === "deepseek"}
                ⚠ Text-only — DeepSeek cannot see your screen (its API rejects images). Guidance is inferred from your description, so it may be wrong on unfamiliar or custom apps. For mainland China <em>with</em> screen analysis, use Qwen instead.
              {:else if settingsForm.api_provider === "qwen"}
                Qwen (DashScope) — pick your region below and the endpoint fills in automatically. Supports image analysis, and is the recommended cloud option for mainland China where US AI services are geoblocked.
              {:else if settingsForm.api_provider === "ollama"}
                Free · runs locally · no data leaves your machine. Requires Ollama installed with a vision model (e.g. llama3.2-vision).
              {:else if settingsForm.api_provider === "custom"}
                Any OpenAI-compatible <code>/v1</code> endpoint — a local server (LM Studio, llama.cpp, vLLM) to run fully offline, a DashScope workspace URL, or another cloud. Use a <em>vision</em> model so it can see the screen; the API key is optional for local servers.
              {/if}
            </p>

            {#if settingsForm.api_provider === "managed"}
              {#if managedTier === "paid"}
                <!-- Quality tier — which managed model answers (and its coin cost). Persisted via
                     Apply/OK. Paid only: the relay routes free users to the free model chain and
                     ignores this tier, so showing Speed/Regular/Smart to them is misleading. -->
                <div class="setting-group">
                  <label class="setting-label" for="tier-select">Quality tier</label>
                  <select id="tier-select" class="setting-select" bind:value={settingsForm.managed_tier}>
                    <option value="speed">Speed — fastest · 6 coins/request</option>
                    <option value="regular">Regular — balanced · 12 coins/request</option>
                    <option value="smart">Smart — best grounding · 18 coins/request</option>
                  </select>
                  <p class="setting-hint">
                    {#if settingsForm.managed_tier === "speed"}
                      GPT-5.4-mini, falls back to Gemini 3 Flash. Cheapest; good for simple, text-heavy UIs.
                    {:else if settingsForm.managed_tier === "smart"}
                      Gemini 3 Pro, falls back to GPT-5.4. Best at pointing precisely on dense/visual UIs.
                    {:else}
                      Gemini 3.5 Flash, falls back to GPT-5.4-mini. The best all-round default.
                    {/if}
                    Coins are bought on the Billing tab.
                  </p>
                </div>
              {:else}
                <div class="setting-group">
                  <span class="setting-label">Quality tier</span>
                  <p class="setting-hint">
                    You're on the <strong>free tier</strong> — requests use the free model.
                    Buy coins on the <strong>Billing</strong> tab to choose Speed / Regular / Smart.
                  </p>
                </div>
              {/if}
            {/if}

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
                  value={customAnthropic ? "__custom__" : settingsForm.anthropic_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customAnthropic = false; settingsForm.anthropic_model = v; } else { customAnthropic = true; settingsForm.anthropic_model = ""; } }}>
                  <option value="claude-haiku-4-5-20251001">claude-haiku-4-5 (fast)</option>
                  <option value="claude-sonnet-4-6">claude-sonnet-4-6 (recommended)</option>
                  <option value="claude-opus-4-7">claude-opus-4-7 (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customAnthropic}
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
                  value={customGemini ? "__custom__" : settingsForm.gemini_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customGemini = false; settingsForm.gemini_model = v; } else { customGemini = true; settingsForm.gemini_model = ""; } }}>
                  <option value="gemini-2.5-flash">gemini-2.5-flash (recommended)</option>
                  <option value="gemini-2.5-flash-lite">gemini-2.5-flash-lite (fast)</option>
                  <option value="gemini-3.5-flash">gemini-3.5-flash</option>
                  <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customGemini}
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
                <select id="ollama-model" class="setting-select"
                  value={customOllama ? "__custom__" : settingsForm.ollama_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customOllama = false; settingsForm.ollama_model = v; } else { customOllama = true; settingsForm.ollama_model = ""; } }}>
                  {#each ollamaModels as m}
                    <option value={m}>{m}</option>
                  {/each}
                  <option value="__custom__">Custom / not listed…</option>
                </select>
                {#if customOllama}
                  <input class="setting-input" type="text" bind:value={settingsForm.ollama_model}
                    placeholder="e.g. gemma4:e4b" spellcheck="false" style="margin-top:6px" />
                {/if}
                <div style="display:flex; align-items:center; gap:8px; margin-top:6px">
                  <button class="key-toggle" type="button" onclick={refreshOllamaModels}>↻ Refresh</button>
                  <span class="setting-hint" style="margin:0">
                    {ollamaModelsMsg || `${ollamaModels.length} model${ollamaModels.length === 1 ? "" : "s"} on the server · must be vision-capable`}
                  </span>
                </div>
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
                  value={customOpenAI ? "__custom__" : settingsForm.openai_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customOpenAI = false; settingsForm.openai_model = v; } else { customOpenAI = true; settingsForm.openai_model = ""; } }}>
                  <option value="gpt-5.5">gpt-5.5 (recommended)</option>
                  <option value="gpt-5.4-mini">gpt-5.4-mini (fast)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customOpenAI}
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
                  value={customDeepSeek ? "__custom__" : settingsForm.deepseek_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customDeepSeek = false; settingsForm.deepseek_model = v; } else { customDeepSeek = true; settingsForm.deepseek_model = ""; } }}>
                  <option value="deepseek-v4-flash">deepseek-v4-flash (recommended)</option>
                  <option value="deepseek-v4-pro">deepseek-v4-pro (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customDeepSeek}
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
                  value={customQwen ? "__custom__" : settingsForm.qwen_model}
                  onchange={(e) => { const v = e.currentTarget.value; if (v !== "__custom__") { customQwen = false; settingsForm.qwen_model = v; } else { customQwen = true; settingsForm.qwen_model = ""; } }}>
                  <option value="qwen3.6-plus">qwen3.6-plus (recommended)</option>
                  <option value="qwen3.5-omni-plus">qwen3.5-omni-plus (multimodal)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customQwen}
                  <input class="setting-input" type="text" bind:value={settingsForm.qwen_model}
                    placeholder="e.g. qwen3.6-plus" spellcheck="false" style="margin-top:6px" />
                {/if}
              </div>
              <div class="setting-group">
                <label class="setting-label" for="qwen-endpoint">Region</label>
                <select id="qwen-endpoint" class="setting-select"
                  value={qwenEndpointChoice}
                  onchange={(e) => {
                    settingsForm.qwen_base_url = e.currentTarget.value === "intl" ? QWEN_ENDPOINTS.intl : QWEN_ENDPOINTS.beijing;
                  }}>
                  <option value="intl">International — Singapore</option>
                  <option value="beijing">China — Beijing</option>
                </select>
                <p class="setting-hint">DashScope endpoint, filled in automatically. For a local server, a DashScope workspace URL, or another cloud, use the <strong>Custom (OpenAI-compatible)</strong> provider instead.</p>
              </div>
            {:else if settingsForm.api_provider === "custom"}
              <div class="setting-group">
                <label class="setting-label" for="custom-url">Base URL</label>
                <input id="custom-url" class="setting-input" type="text"
                  bind:value={settingsForm.custom_base_url}
                  placeholder="http://localhost:1234/v1" spellcheck="false" />
                <p class="setting-hint">
                  OpenAI-compatible <code>/v1</code> endpoint — Navisual appends <code>/chat/completions</code>.<br />
                  LM Studio <code>http://localhost:1234/v1</code> · llama.cpp / llamafile <code>http://localhost:8080/v1</code> (use the host's LAN IP from another machine). Also accepts a DashScope workspace URL (<code>ws-xxx.&lt;region&gt;.maas.aliyuncs.com/compatible-mode/v1</code>) or any other OpenAI-compatible cloud.
                </p>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="custom-model">Model</label>
                <input id="custom-model" class="setting-input" type="text"
                  bind:value={settingsForm.custom_model}
                  placeholder="e.g. qwen2.5-vl-7b-instruct" spellcheck="false" />
                <p class="setting-hint">Use a <em>vision</em> model so it can see the screen.</p>
              </div>
              <div class="setting-group">
                <label class="setting-label" for="custom-key">API Key <span style="opacity:.55">· optional for local servers</span></label>
                <div class="key-row">
                  {#if showKeyCustom}
                    <input id="custom-key" class="setting-input" type="text"
                      bind:value={settingsForm.custom_api_key}
                      placeholder="sk-… or leave blank" spellcheck="false" />
                  {:else}
                    <input id="custom-key" class="setting-input" type="password"
                      bind:value={settingsForm.custom_api_key}
                      placeholder="sk-… or leave blank" spellcheck="false" />
                  {/if}
                  <button class="key-toggle" onclick={() => { showKeyCustom = !showKeyCustom; }}>
                    {showKeyCustom ? "Hide" : "Show"}
                  </button>
                </div>
              </div>
            {/if}

          {:else if settingsTab === "screen-guide"}
            <div class="setting-group">
              <p class="setting-label">Task suggestions</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.task_suggestions} />
                <span>Prefill the task box with suggested next tasks — you can always type over them</span>
              </label>
            </div>
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
                <span>Save AI screenshots, OCR inputs, and the exact prompt text sent to the AI to the debug folder</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Saved to %APPDATA%\com.navisual.app\debug\ — one prompt_&lt;timestamp&gt;.txt per request, alongside its screenshot. See "Prompt log" below for a single running history instead.</p>
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
              <p class="setting-label">Prompt log</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_prompt_log_file_enabled} />
                <span>Append every prompt sent to the AI to prompt_log.jsonl</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Log file: %APPDATA%\com.navisual.app\prompt_log.jsonl — a single running history covering every request (task, follow-up, re-query, and ✗ Wrong corrections). The system prompt is static (src-tauri/src/ai/prompts.rs) and never logged, only the per-request dynamic text.</p>
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
              <p class="stub-hint" style="margin-top:4px">Auto speaks each reply in its own language (using an installed voice for it). A picked voice is used for replies in its language; replies in other languages still auto-pick a matching voice.</p>
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
            <button class="btn-ghost" onclick={applySettings} disabled={settingsSaving}>
              {settingsSaving ? "Saving…" : "Apply"}
            </button>
            <button class="btn-primary" onclick={applySettingsAndClose} disabled={settingsSaving}>OK</button>
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
          <span class="modal-title">Navisual</span>
          <button class="hdr-btn hdr-btn-close" onclick={() => (showAbout = false)}>✕</button>
        </div>
        <div class="modal-tabs">
          <button class="tab-btn {aboutTab === 'about' ? 'tab-active' : ''}" onclick={() => (aboutTab = "about")}>About</button>
          <button class="tab-btn {aboutTab === 'usage' ? 'tab-active' : ''}" onclick={() => { aboutTab = "usage"; loadUsage(); }}>Usage</button>
        </div>
        {#if aboutTab === "about"}
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
          <p class="about-license">The bundled Blender Nav-Pack references Blender's own icon designs (© Blender Foundation) for on-screen guidance only. Blender is a registered trademark of the Blender Foundation. Navisual is not affiliated with or endorsed by the Blender Foundation.</p>
        </div>
        {:else}
        <!-- Usage tab — Navisual account (coins/free) kept separate from your-own-key token usage -->
        <div class="modal-body">
          <!-- Section 1 — Navisual managed account (coins or free requests) -->
          <div class="setting-group">
            <p class="setting-label" style="margin:0 0 8px">Navisual account</p>
            {#if managedTier === "paid" && coinBalance != null}
              <p class="setting-hint">🪙 {Math.floor(coinBalance / 5_000).toLocaleString()} coins left · {TIER_LABELS[settingsForm.managed_tier] ?? "Regular"} tier · {TIER_COINS[settingsForm.managed_tier] ?? 12} coins/request</p>
            {:else if usageManagedRemaining != null}
              <p class="setting-hint">Free tier — {usageManagedRemaining} / 50 requests left</p>
            {:else}
              <p class="setting-hint">Free tier</p>
            {/if}
          </div>

          <!-- Section 2 — Your own API keys. Detailed token table is developer-only;
               regular BYOK users get one honest line pointing to their provider. -->
          {#if settingsForm.developer_mode}
            <div class="setting-group">
              <div style="display:flex; align-items:center; justify-content:space-between; gap:8px; margin-bottom:10px">
                <p class="setting-label" style="margin:0">Your own keys — token usage</p>
                <div style="display:flex; gap:6px">
                  <button class="tab-btn {usagePeriod === 'today' ? 'tab-active' : ''}" type="button" onclick={() => (usagePeriod = "today")}>Today</button>
                  <button class="tab-btn {usagePeriod === 'month' ? 'tab-active' : ''}" type="button" onclick={() => (usagePeriod = "month")}>This month</button>
                </div>
              </div>

              {#if !usageLoaded}
                <p class="setting-hint">Loading…</p>
              {:else if usageView.length === 0}
                <p class="setting-hint">No bring-your-own-key usage recorded yet.</p>
              {:else}
                <div style="display:flex; flex-direction:column; gap:5px">
                  {#each usageView as r}
                    <div style="display:flex; align-items:baseline; gap:10px; font-size:13px">
                      <span style="flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:var(--text-primary)">{r.model || PROVIDER_NAMES[r.provider] || r.provider}</span>
                      <span style="white-space:nowrap; min-width:84px; text-align:right; color:var(--text-secondary)">{fmtTok(r.tokens)} tok</span>
                      <span style="white-space:nowrap; min-width:62px; text-align:right; color:{r.free ? 'var(--text-secondary)' : 'var(--text-primary)'}">{fmtCost(r.cost, r.free)}</span>
                    </div>
                  {/each}
                  {#if usageHasEstimate}
                    <div style="display:flex; justify-content:space-between; gap:10px; font-size:13px; font-weight:600; border-top:1px solid var(--border); margin-top:4px; padding-top:7px">
                      <span>Estimated total</span>
                      <span style="min-width:62px; text-align:right">~${usageTotalCost.toFixed(2)}</span>
                    </div>
                  {/if}
                </div>
                {#if usageHasEstimate}
                  <p class="setting-hint" style="margin-top:8px">Estimates only — based on each provider's published list pricing, which is set by the provider and subject to change. Check your provider's dashboard for actual charges.</p>
                {/if}
              {/if}

              <div style="margin-top:14px">
                <button class="btn-ghost" type="button" onclick={resetUsage}>↻ Reset usage</button>
              </div>
            </div>
          {:else if isByok}
            <div class="setting-group">
              <p class="setting-label" style="margin:0 0 8px">Your own key</p>
              <p class="setting-hint">Requests run on your own {PROVIDER_NAMES[settingsForm.api_provider] ?? "provider"} account — usage and charges are billed by your provider, not Navisual. Check your provider's dashboard for token counts and costs.</p>
            </div>
          {:else if settingsForm.api_provider === "ollama"}
            <div class="setting-group">
              <p class="setting-label" style="margin:0 0 8px">Local model</p>
              <p class="setting-hint">Running locally with Ollama — nothing is billed and no usage leaves your machine.</p>
            </div>
          {/if}
        </div>
        {/if}
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
    /* Above .modal-backdrop (z-index 100) so the window stays draggable and the
       titlebar controls (pin, collapse, close) stay clickable even with a modal
       open. Opaque bg + z-index keeps it bright/live while the body dims. */
    position: relative;
    z-index: 200;
    background: var(--surface-1);
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
    border: none;
    border-radius: 4px;
    padding: 1px 5px;
  }
  .header-balance:hover {
    background: rgba(255, 107, 53, 0.22);
  }
  .header-balance-low {
    color: #ff4040;
    background: rgba(255, 64, 64, 0.15);
  }
  .header-balance-low:hover {
    background: rgba(255, 64, 64, 0.25);
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
  .header-shared-caret { font-size: 11px; opacity: 0.9; flex-shrink: 0; }
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
  /* One-time coach mark anchored under the target-app chip (same spot the
     picker opens at). It's a button: clicking it opens the picker. */
  .target-hint {
    position: fixed;
    top: 38px;
    left: 8px;
    max-width: 250px;
    text-align: left;
    background: var(--surface-2);
    border: 1px solid rgba(255, 107, 53, 0.45);
    border-radius: 8px;
    padding: 9px 11px;
    font-size: 11.5px;
    font-weight: 400;
    line-height: 1.45;
    color: var(--text-secondary);
    z-index: 997;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    cursor: pointer;
  }
  .target-hint:hover {
    color: var(--text-primary);
    border-color: rgba(255, 107, 53, 0.7);
  }
  .target-hint-arrow {
    position: absolute;
    top: -5px;
    left: 96px;
    width: 8px;
    height: 8px;
    transform: rotate(45deg);
    background: var(--surface-2);
    border-left: 1px solid rgba(255, 107, 53, 0.45);
    border-top: 1px solid rgba(255, 107, 53, 0.45);
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

  /* Point-of-purchase legal agreement line + inline links */
  .legal-agree {
    font-size: 11.5px;
    color: var(--text-tertiary);
    line-height: 1.5;
    text-align: center;
    margin-top: 8px;
  }
  .legal-link {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--text-secondary);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .legal-link:hover { color: var(--accent-500); }

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

  /* Workstream P — the task box holds exactly one prefill; a small ▾ toggle
     (only rendered when there's something else to show) reveals the other
     guesses in a floating popover instead of listing all of them inline. */
  .task-input-wrap {
    position: relative;
    width: 100%;
  }
  .suggest-toggle {
    position: absolute;
    top: 6px;
    right: 6px;
    width: 26px;
    height: 26px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border-radius: 6px;
    border: none;
    background: transparent;
    color: var(--text-tertiary);
    font-size: 17px;
    line-height: 1;
    cursor: pointer;
    transition: color 120ms ease-out, background 120ms ease-out, transform 120ms ease-out;
  }
  .suggest-toggle:hover {
    color: var(--text-primary);
    background: var(--surface-3);
  }
  .suggest-toggle-open {
    color: var(--accent-500);
    transform: rotate(180deg);
  }
  .suggest-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
  }
  .suggest-item {
    text-align: left;
    font-family: inherit;
    font-size: 12px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .suggest-item:hover {
    color: var(--text-primary);
    background: var(--surface-3);
    border-color: var(--border);
  }

  textarea {
    width: 100%;
    box-sizing: border-box;
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
    gap: 16px;
    flex-wrap: wrap;
    align-items: center;
  }
  .hk-item {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }
  .hk-key {
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 10px;
    line-height: 1;
    color: var(--text-secondary);
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-bottom-width: 2px;
    border-radius: 4px;
    padding: 2px 5px;
  }
  .hk-none {
    font-size: 10px;
    color: var(--text-tertiary);
    font-style: italic;
    opacity: 0.7;
  }
  .hk-label {
    font-size: 10px;
    color: var(--text-secondary);
    font-weight: 500;
  }
  .hk-item.hk-unset .hk-label {
    color: var(--text-tertiary);
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

  .btn-danger {
    background: #b91c1c;
    color: #fff;
    border-color: transparent;
  }
  .btn-danger:hover:not(:disabled) { background: #dc2626; }
  .btn-danger:disabled { opacity: 0.4; cursor: not-allowed; }

  /* ── Account tab ─────────────────────────────────── */
  .acct-error { color: #f87171; }
  .acct-notice { color: var(--accent-400); }
  .acct-sep {
    border: none;
    border-top: 1px solid var(--border);
    margin: 14px 0;
  }
  .acct-links {
    display: flex;
    gap: 14px;
    flex-wrap: wrap;
    margin-top: 10px;
  }
  .acct-danger { color: #f87171; }
  .acct-danger:hover { color: #fca5a5; }

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
    /* Leave the titlebar (~44px) uncovered so the window stays draggable and the
       titlebar controls stay clickable while a modal is open. */
    padding-top: 44px;
    box-sizing: border-box;
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
    max-width: 360px;
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
