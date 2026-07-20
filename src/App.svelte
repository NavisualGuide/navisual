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
  import { billing, MICRO_PER_COIN } from "./lib/billing.svelte";
  import { account } from "./lib/account.svelte";
  import TrialExhaustedModal from "./TrialExhaustedModal.svelte";
  import BillingPanel from "./BillingPanel.svelte";
  import AccountPanel from "./AccountPanel.svelte";

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
    request_id: string | null;
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
    hint_shown: boolean;
    // Flow A: ranked candidate boxes when a Wrong-spot retry found 2+ distinct
    // possibilities (empty otherwise). Shown as numbered overlay boxes; never a
    // picker — the user's next real click in the app resolves it.
    candidates: Rect[];
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
    training_capture_enabled: boolean;
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
    // Flow B: a pass declared a ground-truth tie during this locate (recorded even
    // when the boxes weren't shown — that's the fire-rate instrumentation).
    ambiguity_set: { source: string; boxes: Rect[] } | null;
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
  // How many steps have started streaming in the in-flight response (backend
  // stream_chunk.steps_seen). >1 while thinking → show "Step 1 of ~N" live.
  let streamStepsSeen = $state(0);
  let locateResult = $state<LocateResult | null>(null);
  // Backend hid the pointer because the target window is occluded (not a locate miss).
  let pointerOccluded = $state(false);
  let locateTrace = $state<LocateTrace | null>(null);
  let debugDrawerOpen = $state(false);
  // Test-user feedback (see logFeedback / submitWrong / correction).
  let wrongPickerOpen = $state(false);
  // B5 "wrong spot" memory: every pointer bbox the user rejected for the CURRENT
  // step attempt — grows across local retries so no rejected spot can be
  // re-picked, and rides along to send_correction if the AI fallback runs.
  // Reset on a new task and on step advance (capped: stale exclusions on a
  // changed layout could veto a now-correct element). Each entry is TAGGED with
  // the target_text it was rejected for — the backend only applies entries whose
  // target matches the step being located ("this rect is not <target>", not
  // "never point here again for anything"; see candidates::AvoidEntry).
  let wrongSpotAvoid = $state<{ bbox: Rect; target: string }[]>([]);
  // Flow A: how many candidate boxes are currently on screen (0 = none). Gates
  // the second-Wrong escalation (skip another local retry) and clears with the
  // rejected-spot memory — same lifecycle, same reset sites.
  let candidateCount = $state(0);
  // The diffuse AI-bbox hint ring was drawn for the current step (locator missed,
  // trusted bbox). Third picker state: the ring is visibly rejectable, so "Wrong
  // spot" shows alongside "Can't find it" — rejecting it is a model-grounding
  // fault (no locator pick exists), routed straight to the AI: no avoid-list push
  // (the ring is an inflated REGION — vetoing it could block the correction's
  // true pointer) and no local retry (the locator already ran everything).
  let hintShown = $state(false);
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
  // request_id of the most recent AI response (llm-finetuning-eval.md §5b) —
  // attached to feedback rows as the local training-data join key.
  let lastRequestId = $state("");
  let provider = $state("");
  // The model that actually handled the last AI response. For managed this is the
  // concrete model the relay routed to — the free tier tries a primary provider and
  // falls back to another on failure, so this can vary request to request; shown in
  // the debug drawer and logged with feedback. Empty until the first response.
  let routedModel = $state("");
  // Set when the screen drifted during the 5–90s AI thinking window.
  // Surfaces a soft banner over the instruction so the user knows the
  // guidance may be referring to state that no longer exists.
  let staleResponse = $state(false);
  // Managed provider (S.1 / S.2) state now lives in the billing store
  // (src/lib/billing.svelte.ts) — billing.freeRemaining/coinBalanceMicro/tier used to be
  // three $states here written from 6+ places, the root of the F1/F6 bug class.
  // Read billing.freeRemaining / billing.coinBalanceMicro / billing.tier; mutate
  // only via the store's methods. tier mirrors the account's real relay-reported
  // tier, full stop — deliberately NOT aware of free_remaining (a paying customer
  // with unused free requests must keep the paid UI; the relay's free-before-paid
  // routing is what makes that safe). History + rationale in the store.
  let showTrialExhausted = $state(false);
  // Which reason opened the modal above — free requests genuinely used up, vs.
  // a paid tier selected without enough coins (e.g. "Free" fell back to a paid
  // tier once free ran out, or Speed/Regular/Smart picked directly). Same
  // modal, same "buy coins" resolution either way, but the copy must differ:
  // telling an existing paying customer low on coins "your free trial is
  // used" is simply wrong for them. Was a real bug until 2026-07-11 — the
  // backend treated every 402 as free_trial_exhausted regardless of which the
  // relay actually meant.
  let exhaustedReason = $state<"free" | "coins">("free");
  // Checkout flow flags (billing.oauthPending / billing.checkoutPending), the
  // top-up amount picker (BillingPanel), and the whole Account-tab state cluster
  // (AccountPanel + the account store) moved out in the 2026-07-13
  // componentization pass — see src/BillingPanel.svelte, src/AccountPanel.svelte,
  // src/lib/account.svelte.ts, src/TrialExhaustedModal.svelte.

  // Phase 0.2: which app is currently shared with the AI.
  type SharedAppInfo = {
    hwnd: number;
    rect: { x: number; y: number; width: number; height: number };
    app_name: string;
    exe_name: string;
  };
  let sharedApp = $state<SharedAppInfo | null>(null);

  // ---- Blender add-on deployment (script-channel bridge) ----
  // The pack ships navisual_bridge.py, but Blender only loads add-ons from its own
  // config dir. Offer the copy when the user is actually working in Blender and the
  // add-on is missing or older than the pack's — never nag on other apps, and never
  // install without an explicit click (writing into another app's config dir is a
  // user decision; the Add-ons checkbox remains the consent gate for RUNNING it).
  type AddonStatus = {
    pack_version: number | null;
    available: boolean;
    // Scoped to the Blender the user is actually working in — a second, up-to-date
    // install must never raise the prompt (live 2026-07-19: it did, backwards).
    target_version: string | null;
    target_installed_version: number | null;
    installs: { blender_version: string; addons_dir: string; installed_version: number | null }[];
    needs_action: boolean;
  };
  let addonPrompt = $state<"hidden" | "offer" | "installing" | "done">("hidden");
  let addonMessage = $state("");
  let addonDismissed = $state(false);

  async function maybeOfferBlenderAddon() {
    if (addonDismissed || addonPrompt === "installing") return;
    if (!sharedApp || friendlyName(sharedApp.exe_name).toLowerCase() !== "blender") {
      if (addonPrompt === "offer") addonPrompt = "hidden";
      return;
    }
    try {
      const st = await invoke<AddonStatus>("blender_addon_status", {
        hwnd: sharedApp.hwnd ?? 0,
      });
      if (!st.available || !st.needs_action) {
        addonPrompt = "hidden";
        return;
      }
      // Wording follows THIS Blender's own state, not any other install's.
      const ver = st.target_version ? ` (Blender ${st.target_version})` : "";
      addonMessage =
        st.target_installed_version !== null
          ? `A newer Navisual add-on is available${ver} — updating keeps tool pointing exact.`
          : `Install the Navisual add-on${ver} for exact tool pointing (one-time setup).`;
      addonPrompt = "offer";
    } catch (_) {
      addonPrompt = "hidden";
    }
  }

  async function installBlenderAddon() {
    addonPrompt = "installing";
    try {
      const r = await invoke<{ installed: string[]; errors: string[]; needs_enable: boolean }>(
        "install_blender_addon",
        { hwnd: sharedApp?.hwnd ?? 0 },
      );
      if (r.installed.length === 0) {
        addonMessage = `Couldn't install: ${r.errors[0] ?? "no Blender installation found"}`;
      } else if (r.needs_enable) {
        addonMessage =
          "Installed. In Blender: Edit → Preferences → Add-ons, search “Navisual”, tick the checkbox. (One time only.)";
      } else {
        addonMessage = "Updated. Restart Blender to load the new version.";
      }
      addonPrompt = "done";
    } catch (e) {
      addonMessage = `Couldn't install: ${e}`;
      addonPrompt = "done";
    }
  }

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
    // UWP/Store apps all run in the shared ApplicationFrameHost.exe, so the exe
    // name is a useless label. Return "" so callers (`friendlyName(exe) || app_name`)
    // fall through to the backend-resolved app_name — the real app name derived
    // from the window title (e.g. "OneNote", "Microsoft To Do").
    if (stem === "applicationframehost") return "";
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
    training_capture_enabled: false,
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
  // The most recent real user request text — threaded to speak() as the auto-language
  // hint (the LANGUAGE rule pins the reply language to the request, so its script
  // outranks the OS locale when the reply itself is Latin-ambiguous). Not reactive —
  // read only inside applyResponse's speak call.
  let lastRequestHint = "";

  const PANEL_W = 420;
  const PANEL_H = 600;
  const ICON_SIZE = 56;
  // The panel's last known size while in normal (non-icon) mode. Restored by
  // expandToPanel() instead of the hardcoded PANEL_W/PANEL_H, so a
  // user-resized panel doesn't snap back to the default after collapsing to
  // the floating icon — and persisted across restarts too (reported live:
  // "the window size is not remembered"). Updated live by the onResized
  // listener registered in onMount; iconMode-guarded there so the 56x56 icon
  // size and the collapse/expand transitions themselves never get saved.
  const PANEL_SIZE_KEY = "navisual-panel-size-v1";
  let lastPanelSize = { width: PANEL_W, height: PANEL_H };
  let panelSizeSaveTimer: ReturnType<typeof setTimeout> | null = null;

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
        nextStep(true); // autopilot-triggered — logs "worked_auto", not "worked" (C3 + taxonomy split)
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
    try { await getCurrentWindow().setSize(new LogicalSize(lastPanelSize.width, lastPanelSize.height)); }
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
  // backend says oauth_required (anonymous session), sends the user to the
  // Account tab to sign in — Google OR email, their choice (2026-07-11: used
  // to auto-trigger Google OAuth with no alternative offered). Signed-in
  // users skip this entirely. Opens Stripe Checkout in the system browser.
  async function buyCoins(amountUsd = 20) {
    if (billing.oauthPending || billing.checkoutPending) return;
    // The checkout page opens in the system browser. The panel is
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
          // Not signed in yet — stay in-app (no browser was opened) and let
          // the user pick a sign-in method on the Account tab, which already
          // offers both Google and email. They click Buy Coins again once
          // signed in; account linking preserves free requests and any coins.
          await setPanelOnTop(true);
          settingsTab = "account";
          account.view = "signin";
          account.error = "";
          account.notice = "Sign in to buy coins — use Google below, or enter your email.";
          showSettings = true;
          return;
        }
        throw e;
      }
      billing.checkoutPending = true;
      openUrl(url);
    } catch (e) {
      addToHistory("system", "⚠️ Checkout failed: " + String(e));
      await setPanelOnTop(true); // nothing opened — restore always-on-top
    } finally {
      billing.oauthPending = false;
    }
  }

  // Re-fetch balance from the relay (after returning from Stripe Checkout).
  // Always clears the pending flags — by the time we refresh, the checkout/OAuth
  // round-trip is over (whether the user paid or cancelled), so the UI shouldn't
  // stay stuck on "Checkout open in browser…".
  async function refreshBalance() {
    if (await billing.refresh()) {
      if (billing.tier === "paid") showTrialExhausted = false;
    }
    billing.oauthPending = false;
    billing.checkoutPending = false;
    await setPanelOnTop(true); // back from the browser — restore always-on-top
  }

  // ── Account management (S.2.1) ──────────────────────────────────────────────
  // The identity/view state and every acct* handler moved to
  // src/AccountPanel.svelte + src/lib/account.svelte.ts (componentization pass,
  // 2026-07-13). App keeps only the cross-cutting pieces: buyCoins' redirect
  // into the Account tab (via the account store) and the account_changed
  // listener below.

  async function openSettings(tab: SettingsTab = "provider") {
    settingsTab = tab;
    // The Billing tab renders live balance state; the tab *button* refreshes it
    // on click, but this deep-link path (header balance chips) used to skip the
    // refresh and show stale numbers (audit F9).
    if (tab === "billing") refreshBalance();
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
    streamStepsSeen = 0;
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
    // D1: a no-step, no-question reply while steps were in flight = the AI saying
    // the task looks complete — the prior guidance succeeded. Without this label
    // the FINAL step of every successful session goes unlabeled ('worked' only
    // fires on → Next, and a finished user just walks away). Logged BEFORE the
    // state mutations below so the row carries the completed step's instruction
    // and attributes to the request that produced it (lastRequestId is still the
    // prior request here). Server kinds are constraint-pinned — migration
    // 20260716000000_feedback_task_complete.sql must be applied first; the local
    // training mirror banks the row regardless.
    if (res.ok && res.steps.length === 0 && !res.needs_input && steps.length > 0 && lastRequestId) {
      logFeedback("task_complete", "");
    }
    steps = res.steps;
    stepIndex = idx;
    currentInstruction = res.instruction;
    locateResult = res.located;
    locateTrace = res.locate_trace;
    hintShown = res.hint_shown;
    // A fresh response clears any previous candidate boxes — unless THIS response
    // drew a new set (Flow B: a first-locate ambiguity — e.g. a repeated word with
    // no distinguishing anchor — shows the known possibilities instead of a hint
    // ring; nobody is asked to choose, the user's next click resolves it).
    const cands = res.candidates ?? [];
    if (cands.length >= 2) {
      candidateCount = cands.length;
      const tgt = res.steps[idx]?.target_text ?? "";
      wrongSpotAvoid = [
        ...wrongSpotAvoid,
        ...cands.map((bbox) => ({ bbox, target: tgt })),
      ];
      addToHistory(
        "system",
        `That appears in several places — I've marked the ${cands.length} most likely (① is my best guess). Just click the one you meant. None of them? Press ✗ Wrong.`,
      );
    } else {
      candidateCount = 0;
    }
    sessionId = res.session_id;
    // Training-data join key — echoed back on feedback rows so worked/wrong
    // signals join this request's prompt/response/screenshot records. next_step
    // responses carry the ORIGINAL producing request's id (correct attribution).
    if (res.request_id) lastRequestId = res.request_id;
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
      if (!isMuted) invoke("speak", { text: cleanInstruction, lang: settingsForm.voice_language, requestHint: lastRequestHint, fallbackLocale: navigator.language }).catch(() => {});
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
    lastRequestHint = taskText;
    clearPrefill();
    wrongSpotAvoid = []; // new request context — drop the old step's rejected spots
    candidateCount = 0;
    // Focus give-back on submit: typing gave the panel focus; by the time the
    // response's pointer appears, the user's next act is clicking the TARGET —
    // without this, that first click only re-focuses the target and is eaten.
    invoke("focus_target_window").catch(() => {});
    // Keep session context when in the middle of a task; start fresh from idle/error.
    const isReply = phase === "guiding" || phase === "needs_input";
    const prevPhase = phase;
    const userEntryId = await addToHistory("user", taskText);
    currentInstruction = "";
    streamStepsSeen = 0;
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

  async function nextStep(viaAutopilot = false, skipFeedback = false) {
    // Don't allow next while an AI call is in flight — the hotkey can fire
    // even when the Next button is disabled (Svelte derived state edge case).
    if (phase === "thinking") return;
    isOverlayCleared = false;
    // Focus give-back: a mouse click on → Next focused the panel, so the user's
    // next click on the target would be eaten by activation ("click once for
    // focus, click again to act"). Hand focus straight back. No-op on the
    // hotkey/autopilot paths — the backend only acts when the PANEL holds the
    // foreground, which it doesn't there.
    if (!viaAutopilot) invoke("focus_target_window").catch(() => {});
    // A HUMAN pressing Next is an implicit success signal for the current step;
    // an AUTOPILOT-triggered advance (a screen change fired the poll) is not —
    // it's automation, not confirmation, so it logs under a DISTINCT kind
    // (worked_auto) instead of inflating the human-validated per-model success
    // rate (SDD §10; audit C3 + feedback-taxonomy split 2026-07-13). Dashboards
    // filter kind='worked' for success; worked_auto measures autopilot itself.
    // skipFeedback: the B2 already_done advance logs its own kind first — an
    // "already did that" is NOT a 'worked' confirmation of our guidance.
    if (phase === "guiding" && !skipFeedback) logFeedback(viaAutopilot ? "worked_auto" : "worked", "");
    // Step advance = new target — the rejected-spot memory is for the step it
    // was rejected on (a stale exclusion could veto a now-correct element).
    wrongSpotAvoid = [];
    candidateCount = 0;
    // Clear the previous step's warning banners. Without this, one genuine
    // stale/occlusion event early in a session re-surfaced its banner after
    // EVERY later → Next (the flag was only reset on the submit/correction
    // paths), reading as "screen changed while I was thinking" on steps where
    // nothing drifted at all — live-observed in the 2026-07-17 PowerPoint
    // session, where one Designer-pane pop armed the banner for good.
    staleResponse = false;
    pointerOccluded = false;
    const nextIdx = stepIndex + 1;
    const prevPhase = phase;
    if (nextIdx >= steps.length) {
      // Re-query AI — tell it what was just completed so it doesn't repeat.
      const completed = currentInstruction || lastCompletedInstruction;
      lastCompletedInstruction = completed;
      currentInstruction = "";
      streamStepsSeen = 0;
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
    // Tell the locator where NOT to point again — every bbox the user rejected
    // this step (accumulated across B5 local retries + shown Flow-A candidates).
    // Sent for EVERY correction category, not just wrong_spot: live 2026-07-18, a
    // "Can't find it" correction after a wrong_spot rejection re-pointed at the
    // very spot the user had just rejected (the not_found path dropped the list).
    // Rejections stand for the whole step; the list resets on step advance.
    const avoidBboxes = wrongSpotAvoid.length ? wrongSpotAvoid : null;
    const label = (category && CATEGORY_LABEL[category]) || "Wrong";
    const prevPhase = phase;
    const corrEntryId = await addToHistory("correction", rawNote ? `${label} — ${rawNote}` : `${label} — re-analysing…`);
    currentInstruction = "";
    streamStepsSeen = 0;
    staleResponse = false;
    pointerOccluded = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction", { note: note || null, avoidBboxes });
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
          request_id: lastRequestId || null,
        },
      });
    } catch (_) {
      /* offline / not signed in / not configured — feedback is best-effort */
    }
  }

  // B5 — route the ✗ Wrong retry by the layer that actually failed. The AI cannot
  // fix a locator mistake (its answer was often correct; the RANKING picked wrong),
  // so a LOCAL re-locate runs first — free and instant. Originally only the
  // ranking-prone kinds were eligible ("a deterministic pass would return the same
  // element"), but the avoid-veto now exists at EVERY deterministic pass (selection
  // B5-era; adapter with Flow A, occurrence-aware in Word), so a local retry can
  // never repeat the rejected spot for any kind: it surfaces alternatives
  // (candidate boxes) or honestly misses into the AI path. First live Flow-A test
  // (2026-07-18) hit exactly this gap — a hit_adapter Wrong went straight to the AI.
  function localRetryEligible(category: string): boolean {
    // Flow A: candidates were already shown and the user says Wrong again — every
    // shown box is in the avoid list; another local retry would surface a 4th-best
    // scrap. Escalate straight to the AI.
    if (candidateCount >= 2) return false;
    const kind = locateTrace?.final_decision?.kind;
    if (category === "wrong_spot") {
      return (
        kind === "hit_a11y" ||
        kind === "hit_ocr" ||
        kind === "hit_template" ||
        kind === "hit_adapter" ||
        kind === "hit_selection"
      );
    }
    // not_found: no pointer was drawn — by now the lazy a11y tree the original
    // attempt raced has had seconds to build, so a second look often succeeds.
    return category === "not_found" && !locateResult;
  }

  // Local re-locate, no AI call. Frank messaging per the user's design call:
  // say plainly WHAT happened (our pointer's mistake, not the AI's answer)
  // rather than silently hopping the pointer around.
  async function tryLocalRetry(category: string): Promise<boolean> {
    try {
      const res = await invoke<GuideResponse>("retry_locate", {
        stepIndex,
        avoidBboxes: wrongSpotAvoid,
      });
      if (res.located) {
        locateResult = res.located;
        locateTrace = res.locate_trace;
        hintShown = res.hint_shown;
        // A successful local re-locate just verified the target on the LIVE
        // screen — leftover stale/occlusion banners no longer apply.
        staleResponse = false;
        pointerOccluded = false;
        // Flow A: 2+ distinct possibilities → numbered boxes are on screen. The
        // user is NOT asked to pick — they just click the right one in the app
        // (the backend reads which from the app's own state). All shown boxes
        // join the rejected-spot memory so another ✗ Wrong escalates to the AI
        // avoiding every one of them.
        const cands = res.candidates ?? [];
        if (cands.length >= 2) {
          candidateCount = cands.length;
          const tgt = steps[stepIndex]?.target_text ?? "";
          wrongSpotAvoid = [
            ...wrongSpotAvoid,
            ...cands.map((bbox) => ({ bbox, target: tgt })),
          ];
          addToHistory(
            "system",
            `That was likely the pointer's mistake, not the AI's answer — I've marked the ${cands.length} most likely spots (① is my best guess, no AI request used). Just click the one you meant. Still wrong? Press ✗ Wrong to ask the AI.`,
          );
          return true;
        }
        candidateCount = 0;
        addToHistory(
          "system",
          category === "wrong_spot"
            ? "That was likely the pointer's mistake, not the AI's answer — moved to the next-best match (no AI request used). Still wrong? Press ✗ Wrong again to re-ask the AI."
            : "Took a second look and found it this time (no AI request used). Not right? Press ✗ Wrong again to re-ask the AI.",
        );
        return true;
      }
      if (category === "wrong_spot") {
        addToHistory("system", "No other match for that target on screen — asking the AI to reconsider…");
      }
    } catch (_) {
      /* fall through to the AI correction */
    }
    return false;
  }

  // Wrong button → log the reason, then retry at the failing layer: local
  // re-locate first when eligible (B5), else / on local failure the AI correction.
  async function submitWrong(category: string) {
    wrongPickerOpen = false;
    const note = task.trim();
    logFeedback(category, note);
    // B2: "Already did that" with steps remaining is deterministic — the only
    // sane response is "advance" — so spend zero AI requests answering it.
    // skipFeedback: already_done was just logged; a 'worked' on top would
    // mislabel a repeated instruction as a success. At sequence end (no next
    // step) the AI genuinely must re-plan → normal correction below.
    if (category === "already_done" && !note && stepIndex + 1 < steps.length) {
      addToHistory("system", "Skipping the already-done step — moving on (no AI request used).");
      await nextStep(false, true);
      return;
    }
    if (category === "wrong_spot" && locateResult) {
      // Remember the rejected spot regardless of which retry path runs, tagged
      // with the target it was rejected FOR (scoped avoid).
      wrongSpotAvoid = [
        ...wrongSpotAvoid,
        { bbox: locateResult.bbox, target: steps[stepIndex]?.target_text ?? "" },
      ];
    }
    // A typed note is intent FOR THE AI ("I meant the other Save") — don't
    // intercept it with a local retry that can't read it.
    if (!note && localRetryEligible(category)) {
      if (await tryLocalRetry(category)) return;
    }
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
  const TIER_LABELS: Record<string, string> = { free: "Free", speed: "Speed", regular: "Regular", smart: "Smart" };
  const TIER_COINS: Record<string, number> = { free: 0, speed: 6, regular: 12, smart: 18 };
  // Greys out a paid tier's <option> when the current coin balance can't
  // cover even one request at it — purely a UI hint (picking a disabled
  // option isn't possible, so this can't desync from the relay's own
  // insufficient_coins check; that stays the real enforcement).
  function canAffordTier(tier: string): boolean {
    return (billing.coinBalanceMicro ?? 0) >= TIER_COINS[tier] * MICRO_PER_COIN;
  }
  let providerLabel = $derived(
    // Free users have no quality tier — the Speed/Regular/Smart picker is paid-only
    // (and the relay ignores a free user's tier param), so showing "Managed (Speed)"
    // for a logged-out/free user is misleading. Show "Managed (free)" instead; only a
    // paid user sees their selected quality tier.
    settingsForm.api_provider === "managed"
      ? (billing.tier === "paid" ? `Managed (${TIER_LABELS[settingsForm.managed_tier] ?? "Regular"})` : "Managed (free)")
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
      // Was only ever set inside openSettings(), so the model/token meta line
      // under each AI response (gated on this — see the history template)
      // stayed hidden for the whole session until Settings was opened once,
      // at which point it would retroactively appear for every existing
      // history entry (their `meta` string was always computed correctly;
      // only the display gate was stuck at its false default). Reported live
      // 2026-07-11.
      debugShowInfo = init.debug_show_response_info;
    } catch (_) {}

    const sw = window.screen.availWidth;
    const sh = window.screen.availHeight;
    const margin = 24;

    // Restore the last resized panel size (cross-restart — see PANEL_SIZE_KEY's
    // doc comment). Clamped to the min the window allows (tauri.conf.json
    // minWidth/minHeight) and to the current screen's available space, in case
    // a size saved on a larger/differently-scaled monitor would otherwise be
    // replayed off-screen or absurdly oversized here.
    try {
      const saved = localStorage.getItem(PANEL_SIZE_KEY);
      if (saved) {
        const parsed = JSON.parse(saved);
        if (typeof parsed.width === "number" && typeof parsed.height === "number") {
          lastPanelSize = {
            width: Math.min(Math.max(360, parsed.width), sw - margin * 2),
            height: Math.min(Math.max(380, parsed.height), sh - margin * 2),
          };
        }
      }
    } catch (_) {}

    try {
      await getCurrentWindow().setSize(new LogicalSize(lastPanelSize.width, lastPanelSize.height));
      await getCurrentWindow().setPosition(
        new LogicalPosition(sw - lastPanelSize.width - margin, sh - lastPanelSize.height - margin)
      );
    } catch (_) {}
    try { await getCurrentWindow().show(); } catch (_) {}

    // Keep lastPanelSize in sync with the ACTUAL window size live, so
    // collapseToIcon()/expandToPanel() and the next app launch both restore
    // whatever the user last resized to — not the hardcoded PANEL_W/PANEL_H
    // default. iconMode-guarded: the 56x56 icon size, and the resize events
    // the collapse/expand transitions themselves generate, must never
    // overwrite the real panel size (iconMode flips to true/false
    // synchronously before those setSize() calls, so this always sees the
    // correct mode for the resize it's reacting to).
    getCurrentWindow().onResized(async ({ payload }) => {
      if (iconMode) return;
      try {
        const scale = await getCurrentWindow().scaleFactor();
        const logical = payload.toLogical(scale);
        if (logical.width < 100 || logical.height < 100) return; // ignore transient/minimize-adjacent events
        lastPanelSize = { width: Math.round(logical.width), height: Math.round(logical.height) };
        if (panelSizeSaveTimer) clearTimeout(panelSizeSaveTimer);
        panelSizeSaveTimer = setTimeout(() => {
          try { localStorage.setItem(PANEL_SIZE_KEY, JSON.stringify(lastPanelSize)); } catch (_) {}
        }, 400);
      } catch (_) {}
    }).catch(() => {});

    // Sync the overlay theme from saved settings so the show_ai_bbox toggle
    // is active from the first guide call without requiring the user to open
    // Settings → Apply every session.
    emitTo("overlay", "overlay:theme", {
      color: settingsForm.overlay_color,
      thickness: settingsForm.overlay_thickness,
      subtitle_enabled: settingsForm.subtitle_enabled,
      show_ai_bbox: settingsForm.debug_show_ai_bbox,
    }).catch(() => {});

    listen<{ delta: string; steps_seen: number }>("stream_chunk", (event) => {
      if (phase === "thinking" || phase === "guiding") {
        currentInstruction += event.payload.delta;
        // Live step counter: >1 means later steps are already streaming past —
        // show "Step 1 of ~N" instead of discarding that signal until completion.
        streamStepsSeen = event.payload.steps_seen ?? 0;
      }
    });

    // Phase 0.2: keep the "Shared: <App>" header chip in sync with whatever
    // window the backend is capturing.
    listen<SharedAppInfo>("app_changed", (event) => {
      const prevExe = sharedApp?.exe_name;
      sharedApp = event.payload;
      maybeShowTargetHint();
      if (event.payload.exe_name !== prevExe) maybeOfferBlenderAddon();
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
        maybeOfferBlenderAddon();
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
      // Cold-start balance fetch — invokeReady retries while Rust setup() is
      // still registering state on a fresh install.
      await billing.refresh(invokeReady);
    }

    listen<number>("balance_update", (event) => {
      billing.applyFreeRemaining(event.payload);
      if (event.payload <= 0 && billing.tier === "free") showTrialExhausted = true;
    });

    // Paid tier — coins debited server-side after each request; relay returns
    // the new µ$ balance in X-Coin-Balance and the backend forwards it here.
    listen<number>("coin_balance_update", (event) => {
      billing.applyCoinBalance(event.payload);
      // Balance just hit zero on a paid account — the modal's copy must say
      // "not enough coins", not the default "free trial used" (audit F6: this
      // used to open the modal without setting the reason, showing whichever
      // copy was last displayed).
      if (event.payload <= 0) {
        exhaustedReason = "coins";
        showTrialExhausted = true;
      }
    });

    // When the panel regains focus after a checkout, pull the fresh balance and
    // clear the pending state — covers both "paid" and "cancelled the page".
    getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused && billing.checkoutPending) refreshBalance();
    });

    listen("trial_exhausted", () => {
      billing.markFreeExhausted();
      exhaustedReason = "free";
      showTrialExhausted = true;
    });

    listen("insufficient_coins", () => {
      exhaustedReason = "coins";
      showTrialExhausted = true;
    });

    // Fires when a request billed real coins despite the "Free" quality-tier
    // preference being selected — i.e. free ran out and it silently fell back
    // to a paid tier. The billing itself is intentional (the alternative is
    // refusing a request the user could pay for), but it must not be silent —
    // reported live 2026-07-11. One-shot per request (see take_tier_auto_selected
    // in managed.rs), so this can't repeat-fire for the same charge.
    listen<[string, number]>("tier_auto_selected", (event) => {
      const [tier, priceMicro] = event.payload;
      const coins = Math.floor(priceMicro / 5_000);
      const label = TIER_LABELS[tier] ?? tier;
      addToHistory(
        "system",
        `Your free requests ran out — this used ${coins} coin${coins === 1 ? "" : "s"} (${label} tier).`,
      );
    });

    listen("oauth_complete", async () => {
      billing.oauthPending = false;
      // Refresh balance — tier is now paid if the user had pre-existing coins.
      await billing.refresh();
    });

    // Emitted after any account change (sign in/up/out, delete) so the Account
    // tab reflects the new identity if it's open.
    listen("account_changed", () => {
      if (showSettings && settingsTab === "account") account.load();
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
      {#if settingsForm.api_provider === "managed" && billing.tier === "paid" && billing.coinBalanceMicro !== null}
        <!-- Paid users: icon only, no number, no alarm styling. It's a purchased
             balance, not a countdown to being cut off — a shrinking number in the
             title bar reads as dunning, not help. The exact count is one click
             away (Billing tab, same destination as the free chip below — both
             lead somewhere actionable, not a read-only report). Free users get
             the opposite treatment just below: a visible, reddening count is an
             intentional, appropriate nudge before their trial runs out. -->
        <button class="header-balance" onclick={() => openSettings("billing")} title="View billing">🪙</button>
      {:else if settingsForm.api_provider === "managed" && billing.freeRemaining !== null}
        <button class="header-balance" class:header-balance-low={billing.freeRemaining <= 5} onclick={() => openSettings("billing")} title="Get more requests">{billing.freeRemaining} left</button>
      {/if}
      {#if pendingUpdate}
        <button class="header-update" onclick={() => openAbout("about")} title="Update available">
          ↑ {pendingUpdate.version}
        </button>
      {/if}
      <div class="header-actions">
        <!-- CSS mask-image, not inline <svg> (2026-07-13): the original inline
             <svg> markup rendered as near-invisible slivers — a genuine
             flex-item width-axis sizing failure in this WebView2 build (CSS
             width, native svg width/height attributes, a wrapper-span at
             100%, and viewBox removal all reproduced the same ~2-5px squash).
             icons/goldfish.svg, loaded via <img src>, was never affected —
             the browser treats an EXTERNALLY REFERENCED svg as an opaque
             image resource, not inline DOM subject to flex/intrinsic-ratio
             layout at all. mask-image gets the same "external resource,
             always sized right" behavior while keeping currentColor-style
             theming (paint comes from background-color on the mask, so the
             existing hover-to-red on Quit still works) — the actual files
             are public/icon-{about,settings,collapse,close}.svg. -->
        <button class="hdr-btn hdr-icon-mask hdr-icon-about" onclick={() => openAbout("about")} title="About Navisual" aria-label="About Navisual"></button>
        <button class="hdr-btn hdr-icon-mask hdr-icon-settings" onclick={() => openSettings()} title="Settings" aria-label="Settings"></button>
        <button class="hdr-btn hdr-icon-mask hdr-icon-collapse" onclick={collapseToIcon} title="Collapse to floating icon" aria-label="Collapse to floating icon"></button>
        <button class="hdr-btn hdr-btn-close hdr-icon-mask hdr-icon-close" onclick={closeWindow} title="Quit" aria-label="Quit"></button>
      </div>
    </div>

    <!-- Latest instruction (visible when guiding) -->
    {#if currentInstruction && (phase === "guiding" || phase === "needs_input" || (isThinking && currentInstruction))}
      <section class="latest-box">
        <div class="latest-header">
          {#if isThinking}
            <!-- Streaming: later steps are already flowing past — surface the live
                 count ("~" because more may follow) instead of discarding it. -->
            <span class="step-counter">{streamStepsSeen > 1 ? `Step 1 of ~${streamStepsSeen}` : "Step 1"}</span>
          {:else}
            <span class="step-counter">Step {stepIndex + 1} of {steps.length}</span>
          {/if}
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

        <!-- D6: subtle miss note — only when a target was expected but genuinely not
             found. Suppressed when Flow-B candidate boxes are on screen (boxes ARE
             the pointer's answer; "unavailable" would contradict them). -->
        {#if !locateResult && !pointerOccluded && candidateCount < 2 && steps[stepIndex]?.target_text && phase === "guiding"}
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
                <!-- D2 (three states): a verified pointer → only "Wrong spot" (something
                     to reject). Nothing drawn → only "Can't find it". The HINT RING case
                     (locator missed, trusted AI bbox drawn) shows BOTH — the ring is
                     visibly rejectable ("Wrong spot" on it = a model-grounding-fault
                     label, located=false on the row), and "Can't find it" stays valid. -->
                {#if locateResult || hintShown || candidateCount >= 2}
                  <button class="reason-chip" onclick={() => submitWrong("wrong_spot")}>Wrong spot</button>
                {/if}
                {#if !locateResult}
                  <button class="reason-chip" onclick={() => submitWrong("not_found")}>Can't find it</button>
                {/if}
                <button class="reason-chip" onclick={() => submitWrong("already_done")}>Already did that</button>
              </div>
              <p class="feedback-hint">Not one of these? Type what's wrong below, then ↩ Follow up.</p>
              <p class="feedback-hint">Wrong app? Click the correct window first, then press ✗ Wrong.</p>
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
                <!-- Flow B: a pass declared a ground-truth tie. Boxes drawn = the set
                     fired (miss) or was PROMOTED over an inside-set OCR hit — without
                     this line the drawer reads "hit_ocr" while the overlay shows the
                     adapter's full-element boxes (user-reported confusion, 2026-07-19). -->
                {#if locateTrace.ambiguity_set}
                  {@const amb = locateTrace.ambiguity_set}
                  <div class="debug-row">
                    <span class="debug-key">candidates</span>
                    <span class="debug-val">
                      {amb.source} tie · {amb.boxes.length} known
                      {#if candidateCount >= 2}
                        · {candidateCount} boxes shown{locateResult ? " (promoted over the hit — ① is its spot)" : " (pipeline missed)"}
                      {:else}
                        · not shown (a stronger answer stood alone)
                      {/if}
                    </span>
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

    <!-- Blender add-on offer: shown only while Blender is the shared app AND the
         pack ships a newer (or first) bridge. One click installs; enabling stays a
         deliberate user action inside Blender. -->
    {#if addonPrompt !== "hidden"}
      <div class="stale-banner addon-banner" role="status">
        <span class="stale-icon">🧩</span>
        <span class="stale-text">{addonMessage}</span>
        {#if addonPrompt === "offer"}
          <button class="stale-action" onclick={installBlenderAddon}>Install</button>
        {:else if addonPrompt === "installing"}
          <span class="addon-busy">Installing…</span>
        {/if}
        <button
          class="stale-dismiss"
          onclick={() => { addonPrompt = "hidden"; addonDismissed = true; }}
          title="Dismiss">✕</button>
      </div>
    {/if}

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
      <button class="btn-action btn-next" onclick={() => nextStep()} disabled={actionDisabled} title="Next step (Ctrl+`)">
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
        {@const primary = w.title || w.display_name}
        <button class="target-pick-item" class:target-pick-selected={pinnedHwnd === w.hwnd} onclick={() => selectTarget(w.hwnd)}>
          <span class="target-pick-check">{pinnedHwnd === w.hwnd ? "✓" : ""}</span>
          <!-- Primary = the window title (what the user actually sees on screen);
               subtitle = the friendly app name for identity, when it adds info. -->
          <span class="target-pick-name">{primary.length > 46 ? primary.slice(0, 44) + "…" : primary}</span>
          {#if w.display_name && w.display_name !== primary}
            <span class="target-pick-sub">{w.display_name}</span>
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
            <li><strong>The default free tier uses free AI models that may keep your requests — including the screenshot — to train their models.</strong> Paid tiers, per their providers' current policies, don't; Ollama keeps everything on your machine. (<button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>details</button>)</li>
            <li>On the free tier, a one-way hash of a device identifier counts your 30 free requests per machine — it can't identify you and isn't used on paid or your-own-key providers.</li>
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

  <!-- Trial exhausted modal (S.1) — extracted to TrialExhaustedModal.svelte -->
  <TrialExhaustedModal
    bind:open={showTrialExhausted}
    reason={exhaustedReason}
    onBuy={buyCoins}
    onRefreshBalance={refreshBalance}
  />


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
          <button class="tab-btn {settingsTab === 'account' ? 'tab-active' : ''}" onclick={() => { settingsTab = "account"; account.load(); }}>Account</button>
          <button class="tab-btn {settingsTab === 'screen-guide' ? 'tab-active' : ''}" onclick={() => (settingsTab = "screen-guide")}>Screen Guide</button>
          <button class="tab-btn {settingsTab === 'hotkeys' ? 'tab-active' : ''}" onclick={() => (settingsTab = "hotkeys")}>Hotkeys</button>
          <button class="tab-btn {settingsTab === 'audio' ? 'tab-active' : ''}" onclick={() => (settingsTab = "audio")}>Audio</button>
          {#if settingsForm.developer_mode}
            <button class="tab-btn {settingsTab === 'developer' ? 'tab-active' : ''}" onclick={() => (settingsTab = "developer")}>Developer</button>
          {/if}
        </div>

        <div class="modal-body">
          {#if settingsTab === "billing"}
            <!-- Extracted to BillingPanel.svelte (componentization pass, 2026-07-13) -->
            <BillingPanel
              provider={settingsForm.api_provider}
              onBuy={buyCoins}
              onRefreshBalance={refreshBalance}
            />
          {:else if settingsTab === "account"}
            <!-- Extracted to AccountPanel.svelte + lib/account.svelte.ts (componentization pass, 2026-07-13) -->
            <AccountPanel
              onRefreshBalance={refreshBalance}
              onSignedOut={() => addToHistory("system", "Signed out — you're back on the free tier.")}
            />
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
                Free · 30 requests included. Routed via the Navisual relay to a free-tier AI provider (the specific provider may change over time as we optimize for reliability and speed). May be slower than BYOK providers — ideal for getting started. <strong>Note:</strong> free-tier AI providers commonly retain and may train on your requests (including screenshots) as part of offering the service at no cost; paid tiers (per their providers' current policies) and Ollama do not.
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
              <!-- Quality tier / free preference. Persisted via Apply/OK, defaults to
                   "regular" for a never-configured install (SETTINGS_DEFAULTS) and
                   otherwise remembers whatever was last saved. Always shown regardless
                   of billing.tier (2026-07-11 fix — previously hidden entirely once
                   billing.tier was "paid", so a real paying customer with unused free
                   requests had no way to see or pick "Free" at all). The relay always
                   draws down any remaining free requests first, automatically,
                   regardless of this selection (2026-07-11 routing fix) — so "Free"
                   here is a preference/acknowledgment, not a hard switch; it only
                   actually matters once free is exhausted, at which point "free" isn't
                   a recognized paid-tier key on the relay and degrades safely to
                   Regular pricing with no server-side handling needed. -->
              <div class="setting-group">
                <label class="setting-label" for="tier-select">Quality tier</label>
                <select id="tier-select" class="setting-select" bind:value={settingsForm.managed_tier}>
                  <option value="free">Free — uses your free requests</option>
                  <option value="speed" disabled={!canAffordTier("speed")}>Speed — fastest · 6 coins/request{canAffordTier("speed") ? "" : " (not enough coins)"}</option>
                  <option value="regular" disabled={!canAffordTier("regular")}>Regular — balanced · 12 coins/request{canAffordTier("regular") ? "" : " (not enough coins)"}</option>
                  <option value="smart" disabled={!canAffordTier("smart")}>Smart — best grounding · 18 coins/request{canAffordTier("smart") ? "" : " (not enough coins)"}</option>
                </select>
                <p class="setting-hint">
                  {#if settingsForm.managed_tier === "free"}
                    Free requests are used automatically until they run out, no matter which tier is selected here — this only decides what happens afterward, or once you buy coins.
                  {:else if settingsForm.managed_tier === "speed"}
                    GPT-5.4-mini, falls back to Gemini 3 Flash. Cheapest; good for simple, text-heavy UIs. Coins are bought on the Billing tab.
                  {:else if settingsForm.managed_tier === "smart"}
                    Gemini 3 Pro, falls back to GPT-5.4. Best at pointing precisely on dense/visual UIs. Coins are bought on the Billing tab.
                  {:else}
                    Gemini 3.5 Flash, falls back to GPT-5.4-mini. The best all-round default. Coins are bought on the Billing tab.
                  {/if}
                </p>
              </div>
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
              {#if settingsForm.hotkey_next?.includes("Backquote")}
                <p class="setting-hint" style="margin-top: 4px;">The <strong>~ (Tilde / Backtick)</strong> key is located directly below the Esc key (top left of the keyboard).</p>
              {/if}
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
              <p class="setting-label">Training capture</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.training_capture_enabled} />
                <span>Bank complete training triples (screenshot + prompt + response + outcome) locally</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Saves the exact AI-sent screenshot per request to %APPDATA%\com.navisual.app\training\, records the AI response in prompt_log.jsonl, archives rotated logs instead of deleting them, and mirrors worked/wrong feedback locally — all joined by a per-request id. Local only, never uploaded; exempt from the 7-day debug cleanup. Disk ≈ 100–200 KB per request.</p>
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
            {#if billing.tier === "paid" && billing.coins != null}
              <p class="setting-hint">🪙 {billing.coins.toLocaleString()} coins left · {TIER_LABELS[settingsForm.managed_tier] ?? "Regular"} tier · {TIER_COINS[settingsForm.managed_tier] ?? 12} coins/request</p>
            {:else if usageManagedRemaining != null}
              <p class="setting-hint">Free tier — {usageManagedRemaining} / 30 requests left</p>
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

  :global(.hdr-btn) {
    width: 28px;
    height: 28px;
    padding: 0;
    border-radius: 6px;
    font-size: 13px;
    background: transparent;
    /* text-primary, not text-secondary: reported live as "nearly invisible" —
       these are thin glyphs with no fill weight to fall back on, so they need
       the brighter default other controls don't. Was only promoted to
       text-primary on :hover, which is exactly backwards for legibility. */
    color: var(--text-primary);
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out;
  }
  /* CSS mask-image icons (2026-07-13) — see the markup comment above
     header-actions for why: the actual icon FILES are public/icon-*.svg
     (real, standalone files, same convention as public/goldfish.svg).
     mask-image supplies the shape from the external file, which sidesteps
     the inline-<svg> sizing bug entirely since it's loaded as an opaque
     resource, not inline DOM.
     The paint lives on a ::before PSEUDO-element, not the button itself —
     found live that .hdr-icon-mask's background-color:currentColor and
     .hdr-btn/.hdr-btn-close:hover's own background (the rounded highlight
     square) were the SAME property on the SAME element, so hovering Quit
     silently overrode the icon's mask-fill color instead of tinting it red
     (the highlight square rendered; the X itself didn't turn red — the two
     roles need independent elements). ::before still resolves currentColor
     against the button's own computed color, so the hover-to-red still
     works, just without the collision. */
  :global(.hdr-icon-mask) { position: relative; }
  :global(.hdr-icon-mask::before) {
    content: "";
    position: absolute;
    inset: 0;
    margin: auto;
    width: 17px;
    height: 17px;
    background-color: currentColor;
    -webkit-mask-repeat: no-repeat;
    mask-repeat: no-repeat;
    -webkit-mask-position: center;
    mask-position: center;
    -webkit-mask-size: 17px 17px;
    mask-size: 17px 17px;
  }
  :global(.hdr-icon-about::before) { -webkit-mask-image: url(/icon-about.svg); mask-image: url(/icon-about.svg); }
  :global(.hdr-icon-settings::before) { -webkit-mask-image: url(/icon-settings.svg); mask-image: url(/icon-settings.svg); }
  :global(.hdr-icon-collapse::before) { -webkit-mask-image: url(/icon-collapse.svg); mask-image: url(/icon-collapse.svg); }
  :global(.hdr-icon-close::before) { -webkit-mask-image: url(/icon-close.svg); mask-image: url(/icon-close.svg); }
  :global(.hdr-btn:hover) { background: var(--surface-3); }
  :global(.hdr-btn-close:hover) { background: rgba(239, 68, 68, 0.2); color: var(--danger); }

  /* Point-of-purchase legal agreement line + inline links */
  :global(.legal-agree) {
    font-size: 11.5px;
    color: var(--text-tertiary);
    line-height: 1.5;
    text-align: center;
    margin-top: 8px;
  }
  :global(.legal-link) {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--text-secondary);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  :global(.legal-link:hover) { color: var(--accent-500); }

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
  /* Add-on offer: same shape as the warning banners but informational, not alarming. */
  .addon-banner {
    margin: 0 10px 8px;
    background: rgba(120, 160, 255, 0.10);
    border-color: rgba(120, 160, 255, 0.30);
  }
  .addon-banner .stale-icon { color: #8fb0ff; }
  .addon-busy { font-size: 11px; color: var(--text-tertiary, #8a8a8a); flex-shrink: 0; }
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

  :global(.btn-primary) {
    background: var(--accent-500);
    color: #fff;
    border-color: transparent;
  }
  :global(.btn-primary:hover:not(:disabled)) { background: var(--accent-400); }
  :global(.btn-primary:active) { background: var(--accent-600); }
  :global(.btn-primary:disabled) { opacity: 0.4; cursor: not-allowed; }

  :global(.btn-ghost) {
    background: var(--surface-3);
    color: var(--text-primary);
    border-color: var(--border);
  }
  :global(.btn-ghost:hover) { background: #2d2d33; }

  :global(.btn-danger) {
    background: #b91c1c;
    color: #fff;
    border-color: transparent;
  }
  :global(.btn-danger:hover:not(:disabled)) { background: #dc2626; }
  :global(.btn-danger:disabled) { opacity: 0.4; cursor: not-allowed; }

  /* ── Account tab ─────────────────────────────────── */
  :global(.acct-error) { color: #f87171; }
  :global(.acct-notice) { color: var(--accent-400); }
  :global(.acct-sep) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 14px 0;
  }
  :global(.acct-links) {
    display: flex;
    gap: 14px;
    flex-wrap: wrap;
    margin-top: 10px;
  }
  :global(.acct-danger) { color: #f87171; }
  :global(.acct-danger:hover) { color: #fca5a5; }

  :global(.btn-full) { width: 100%; }

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

  :global(.modal-backdrop) {
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

  :global(.modal) {
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

  :global(.modal-header) {
    display: flex;
    align-items: center;
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  :global(.modal-title) {
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

  :global(.modal-body) {
    padding: 12px 14px;
    flex: 1;
    overflow-y: auto;
  }

  .stub-hint, :global(.setting-hint) {
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

  :global(.setting-group) {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-bottom: 12px;
  }
  :global(.setting-group:last-child) { margin-bottom: 0; }

  :global(.setting-label) {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-tertiary);
    margin: 0;
  }

  :global(.setting-input) {
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
  :global(.setting-input:focus) { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255, 107, 53, 0.15); }
  :global(.setting-select) {
    width: 100%; font-family: inherit; font-size: 13px; padding: 7px 10px;
    border-radius: 7px; background: var(--surface-2); color: var(--text-primary);
    border: 1px solid var(--border); outline: none; box-sizing: border-box; cursor: pointer;
    transition: border-color 120ms ease-out;
    appearance: auto;
  }
  :global(.setting-select:focus) { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255,107,53,0.15); }
  :global(.setting-select:disabled) { opacity: 0.4; cursor: not-allowed; }

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
