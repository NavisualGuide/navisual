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
  import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
  import { check as checkUpdate, type Update } from "@tauri-apps/plugin-updater";
  import HotkeyInput from "./HotkeyInput.svelte";
  import { prettyHotkey } from "./lib/hotkey";
  import { DEFAULT_THICKNESS, strokeScale } from "./lib/overlay-weight";
  import { billing, MICRO_PER_COIN } from "./lib/billing.svelte";
  import { account } from "./lib/account.svelte";
  import TrialExhaustedModal from "./TrialExhaustedModal.svelte";
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
    /** The goal the backend is working toward (stage 4). Reflects a goal promoted from a
     *  needs_input reply, not necessarily the opening message. */
    goal: string;
    /** A short route overview toward `goal` — the map-app "whole trip" view, not
     *  turn-by-turn (that's `steps`). Model-maintained; empty until the model has
     *  offered one. Shown when the user clicks the goal card. */
    plan_outline: string[];
    /** How many leading `plan_outline` milestones are already done — always
     *  <= plan_outline.length. Drives the current-milestone highlight and the
     *  goal card's progress bar. */
    plan_completed_count: number;
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
  type SettingsTab = "provider" | "screen-guide" | "hotkeys" | "audio" | "developer" | "account";
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
    autopilot_min_cells: number;
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
    debug_diagnostics_enabled: boolean;
    debug_log_files_enabled: boolean;
    training_capture_enabled: boolean;
    session_export_enabled: boolean;
    task_suggestions: boolean;
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
  // The trusted AI region sits under OUR panel — the target is likely hidden behind it, so we
  // show the hint ring there (it draws over the panel) and suggest sliding the panel aside.
  let behindPanel = $state(false);
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
  // Stage 4: the goal the backend is working toward, surfaced so drift is a one-glance
  // catch instead of a three-wrong-steps discovery.
  let sessionGoal = $state("");
  // Nav-app "route overview" for the goal above — a handful of model-maintained
  // milestones, distinct from the turn-by-turn steps[]. Empty until the model has
  // offered one (small/one-step tasks may never get one, by design). Revised
  // wholesale by the backend, never accumulated — see Session::set_plan_outline.
  let sessionPlanOutline = $state<string[]>([]);
  // How many leading sessionPlanOutline entries are done — drives the current-
  // milestone highlight (expanded view) and the goal card's progress bar
  // (collapsed view). Always <= sessionPlanOutline.length (backend-clamped).
  let sessionPlanCompletedCount = $state(0);
  // Whether the route overview is expanded in place under the goal card. No
  // persisted "pin" state (simplified 2026-08-22, see the markup comment) — just
  // an open/closed toggle, reset per session same as the rest of this dashboard.
  let planExpanded = $state(false);
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
  // Dismissals are PER BLENDER VERSION, and only an OFFER can be dismissed. A single
  // session-wide flag (v1) meant dismissing the post-install "Installed — tick the
  // checkbox" note silenced every future offer, including for a different Blender
  // (live 2026-07-19: quiet on both installs after a manual uninstall).
  let addonDismissedFor = $state<string[]>([]);
  // Which Blender version the visible offer is about (so ✕ dismisses just that one).
  let addonOfferVersion = $state<string | null>(null);

  async function maybeOfferBlenderAddon() {
    if (addonPrompt === "installing") return;
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
      if (st.target_version && addonDismissedFor.includes(st.target_version)) {
        addonPrompt = "hidden";
        return;
      }
      // Wording follows THIS Blender's own state, not any other install's.
      const ver = st.target_version ? ` (Blender ${st.target_version})` : "";
      addonMessage =
        st.target_installed_version !== null
          ? `A newer Navisual add-on is available${ver} — updating keeps tool pointing exact.`
          : `Install the Navisual add-on${ver} for exact tool pointing (one-time setup).`;
      addonOfferVersion = st.target_version;
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
        // Blender scans scripts/addons at STARTUP (or on Preferences → Add-ons →
        // Refresh), so a file copied into a running Blender is invisible until then —
        // omitting this step left the user hunting for an add-on that was already on
        // disk (live 2026-07-19). Restart is the instruction they verified; Refresh is
        // the faster alternative for anyone who spots the button.
        addonMessage =
          "Installed. Restart Blender (or press Refresh in Preferences → Add-ons), then search “Navisual” there and tick its checkbox. One time only.";
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

  /** Grow the task box to fit its content, up to the CSS `max-height` cap.
   *
   * The box was a fixed 2 rows with `resize: none`, so pasting anything long showed two
   * lines and no sign of how much was hidden — bad for the paste-instructions flow, which
   * is the whole point of "paste an answer that doesn't match your screen".
   *
   * Setting height to `auto` first is load-bearing: without it `scrollHeight` can only
   * ever report the current (larger) height, so the box would grow and never shrink back.
   * The cap is CSS `max-height`, which beats this inline height, so past the cap the box
   * stops growing and scrolls instead of pushing the buttons off the panel. */
  function autoGrowTaskInput() {
    const el = taskInputEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }

  // Runs for programmatic changes too — prefill, clearing on a new session, and the
  // needs_input switch — not just typing, so the box is never left the wrong size.
  $effect(() => {
    task;
    phase;
    tick().then(autoGrowTaskInput);
  });

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
  // "target" = pick what Navisual assists with; "dock" = pick what fills the
  // space beside a docked panel. Same list, different verb.
  let targetPickerMode = $state<"target" | "dock">("target");
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

  // WebView2 can finish loading App.svelte and reach onMount invocations before
  // Rust setup() calls handle.manage(AppState) — its very last statement. Until
  // then Tauri rejects every state-touching command with "state not managed",
  // so all of onMount's backend reads have to wait for that moment.
  //
  // This used to be a per-call retry budget: "8 × 150ms = 1.2s, comfortably
  // longer than any observed cold start". It was not. Measured over 121 launches
  // in the shipped logs, the race is BIMODAL: setup wins 110 of them, and in the
  // other 11 the frontend wins by a median of 18s (max 30s) — only one was under
  // 1.2s. So the old budget did not shave a rare tail, it missed almost the whole
  // failure mode, and a merely larger constant would miss it too.
  //
  // What that cost, on the launch that prompted this (2026-09-07, first invoke
  // 08:11:34, manage() 08:11:52): get_settings, the dock restore, the app chip,
  // sign-in and the balance fetch ALL failed and fell back to defaults for the
  // whole session. Every one of them was swallowed by a `catch (_) {}` except
  // sign_in_anon, whose warning in the conversation was the only visible trace.
  //
  // The wait is now a condition rather than an attempt count: one shared gate
  // that polls until state is managed. Waiting is safe because manage() is
  // unconditional and one-way — the only run where it never happens is one where
  // setup() panicked, and the ceiling exists purely so that build reports a real
  // error instead of hanging forever.
  const BACKEND_READY_CEILING_MS = 120_000;
  let backendReadyGate: Promise<void> | null = null;
  function waitForBackend(): Promise<void> {
    backendReadyGate ??= (async () => {
      const t0 = Date.now();
      for (;;) {
        try {
          await invoke("backend_ready");
          const waited = Date.now() - t0;
          if (waited > 500) console.info(`[startup] backend state ready after ${waited}ms`);
          return;
        } catch (e) {
          // Anything that is not the startup race is a real failure. Stop waiting
          // and let the caller's own error handling see it on the next invoke.
          if (!String(e).includes("state not managed")) return;
          if (Date.now() - t0 > BACKEND_READY_CEILING_MS) {
            console.error("[startup] backend state never became ready — giving up");
            return;
          }
          await new Promise((r) => setTimeout(r, 100));
        }
      }
    })();
    return backendReadyGate;
  }

  // Boot timing, for one open question (2026-09-07): a dev build takes ~24s from
  // process start to its first invoke while release takes ~1s. Measurement has
  // ruled out every workload explanation -- Chrome loads this identical dev page
  // in 1.36s, the warm module graph serves in 151ms, the whole Svelte compile is
  // 918ms -- plus a stale WebView2 lock, the localhost/::1 resolution order and
  // proxy auto-detection. The launch distribution is bimodal with nothing between
  // 5s and 10s, which is the shape of a wait, not of work.
  //
  // Reported at two moments because they bracket the load: "mount" fires from
  // onMount (module scripts are deferred, so DCL/load may still read 0 there),
  // "load" after the window load event, when every mark is final. Plain invoke,
  // never invokeReady -- this has to answer during the race it measures.
  function reportBootTiming(phase: string) {
    try {
      const n = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
      invoke("report_boot_timing", {
        phase,
        timeOriginMs: performance.timeOrigin,
        connectMs: n ? n.connectEnd - n.connectStart : -1,
        ttfbMs: n ? n.responseStart - n.requestStart : -1,
        domInteractiveMs: n ? n.domInteractive : -1,
        domContentLoadedMs: n ? n.domContentLoadedEventEnd : -1,
        loadEventEndMs: n ? n.loadEventEnd : -1,
        nowMs: performance.now(),
      }).catch(() => {});
    } catch (_) {}
  }

  /** invoke(), held until Rust has managed AppState. Startup paths only. */
  async function invokeReady<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    await waitForBackend();
    return invoke<T>(cmd, args);
  }

  type VoiceInfo = { id: string; name: string; };
  let availableVoices = $state<VoiceInfo[]>([]);

  function handlePanelContextMenu(e: MouseEvent) {
    // App-wide (see the <svelte:window> binding). Escape hatches, in order of
    // how often they matter:
    //  - a live text selection: right-click is how people reach Copy, and the
    //    conversation is selectable on purpose. WebView2's own menu is contextual
    //    on a selection (Copy / Search / Print / Inspect), so it earns its place
    //    there — which is why this beats building a custom menu for one item.
    //    Ctrl+C already worked and still does; this is about discoverability.
    //  - text fields: cut/copy/paste is expected there;
    //  - Shift held: the browser convention for "give me the native menu
    //    anyway", so Inspect stays one gesture away.
    // NOT gated on developer_mode any more. It was, and that was the bug: this
    // machine runs NAVISUAL_DEV=true permanently, so the suppression never
    // applied for the only person who would ever notice it (live report
    // 2026-09-07, browser menu still opening over the panel).
    if (e.shiftKey) return;
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed && sel.toString().trim()) return;
    const t = e.target as HTMLElement | null;
    if (t && t.closest("textarea, input, [contenteditable]")) return;
    e.preventDefault();
  }

  async function openTargetPicker(mode: "target" | "dock" = "target") {
    targetPickerMode = mode;
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

  // One-time coach mark on the header's collapse button — a real test user
  // (not a developer, didn't read the README) never discovered the panel could
  // shrink to a small floating icon, and found the full panel hard to work
  // around on a single monitor. Shown once ever, fired the moment the FIRST
  // real guidance response lands — that's exactly when the panel is full of
  // content and most likely to be in the user's way, so the tip appears right
  // when it becomes useful rather than as an unread launch-time splash.
  // Clicking the bubble collapses the panel directly, same as the target hint.
  const COLLAPSE_HINT_KEY = "navisual-collapse-hint-v1";
  let showCollapseHint = $state(false);
  function maybeShowCollapseHint() {
    if (showCollapseHint) return;
    if (localStorage.getItem(COLLAPSE_HINT_KEY)) return;
    localStorage.setItem(COLLAPSE_HINT_KEY, "1");
    showCollapseHint = true;
    setTimeout(() => (showCollapseHint = false), 12_000);
  }
  function dismissCollapseHint() {
    showCollapseHint = false;
  }

  async function selectTarget(hwnd: number | null) {
    targetPickerOpen = false;
    targetPickerMode = "target";
    fullScreenTarget = false;
    if (hwnd === null) {
      await invoke("unpin_target_window");
      pinnedHwnd = null;
    } else {
      await invoke("pin_target_window", { hwnd });
      pinnedHwnd = hwnd;
      // While docked, "the app I'm being guided through" and "the app filling the rest
      // of the screen" are the same choice — so the always-visible header chip does
      // both, rather than making the dock version live only in the ··· menu, which is
      // exactly where this project keeps losing actions. (The explicit
      // "Fill the rest with…" entry stays for anyone who looks there first.)
      if (dockSide) {
        dockPartner = hwnd;
        saveDock();
        try { await invoke("dock_fill", { hwnd, side: dockSide }); }
        catch (e) { console.error("dock_fill:", e); }
      }
    }
  }

  // Full-screen capture target — the user-initiated full-screen target. `monitorIndex`
  // pins a single screen (multi-monitor); `null` shares the whole desktop (single
  // monitor). Sticky like a pin; survives new tasks until the user picks a window or
  // Auto-detect again.
  async function selectDesktop(monitorIndex: number | null) {
    targetPickerOpen = false;
    targetPickerMode = "target";
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
  // True when running under MSIX package identity (Microsoft Store / sideloaded
  // MSIX) rather than the NSIS install or dev. The Store requires updates to flow
  // through the Store, so a packaged build must never self-update — this gates the
  // whole updater path off at runtime, which is why ONE binary can serve both
  // channels instead of maintaining a separate Store build. See the Rust
  // `is_packaged` command + navisual-internal/docs/msix-store-spike.md.
  let isPackaged = $state(false);
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
    subtitle_enabled: true, auto_advance: false, autopilot_min_cells: 16,
    tts_enabled: true, tts_voice: "", voice_input_enabled: false, voice_language: "auto",
    hotkey_next: "Ctrl+Backquote", hotkey_wrong: "Ctrl+KeyE",
    hotkey_pause: "", hotkey_icon: "Ctrl+Shift+Backquote", hotkey_talk: "Ctrl+KeyD",
    debug_screenshot_enabled: false,
    debug_diagnostics_enabled: false,
    debug_log_files_enabled: false,
    training_capture_enabled: false,
    session_export_enabled: false,
    task_suggestions: true,
    developer_mode: false,
  };
  let settingsForm = $state<SettingsPayload>({ ...SETTINGS_DEFAULTS });
  let settingsSaving = $state(false);
  let settingsError = $state<string | null>(null);
  let settingsSaved = $state(false);
  const MODEL_PRESETS_ANTHROPIC = ["claude-haiku-4-5-20251001","claude-sonnet-4-6","claude-opus-4-7"];
  const MODEL_PRESETS_GEMINI    = ["gemini-2.5-flash","gemini-2.5-flash-lite","gemini-3.5-flash","gemini-3.1-pro-preview"];
  const MODEL_PRESETS_OPENAI    = ["gpt-5.5","gpt-5.4-mini"];
  const MODEL_PRESETS_DEEPSEEK  = ["deepseek-v4-flash","deepseek-v4-pro","deepseek-v4-flash-vision-exp"];
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

  // ── Session export (session-export-design.md) ─────────────────────────────
  // The backend keeps the last 30 steps of conversation in memory at all times.
  // Nothing reaches disk until this dialog writes it, which is what makes the
  // shipped promise — "nothing is written unless you choose to save it" — true
  // rather than aspirational. So this dialog IS the consent step, and it shows
  // what will be written before it writes anything.
  type ExportStepInfo = {
    index: number; turn: number; instruction: string;
    has_frame: boolean; pointer: string; user_kind: string; user_typed: boolean;
  };
  type ExportStatus = {
    turns: number; steps: number; frames: number; empty: boolean;
    detail: ExportStepInfo[]; app: string | null;
    suggested_title: string; suggested_slug: string;
    thin_warning: string | null; destination: string;
  };
  let showExport = $state(false);
  let exportStatus = $state<ExportStatus | null>(null);
  let exportTitle = $state("");
  let exportDest = $state("");
  let exportCropToApp = $state(false);
  // All three default ON — "save everything, decide later". The clean copy is the
  // only artifact that cannot be regenerated, and the annotated one is what you
  // actually paste into a walkthrough, so the useful default is both.
  let exportSaveClean = $state(true);
  let exportDrawPointer = $state(true);
  let exportDrawCaption = $state(true);
  let exportRedacted = $state<number[]>([]);
  let exportBusy = $state(false);
  let exportError = $state("");
  let exportDone = $state("");
  // Brief "✓ Copied" confirmation on the export path's copy button.
  let exportPathCopied = $state(false);
  let exportCopyTimer: ReturnType<typeof setTimeout> | null = null;
  // Two dead ends before this one, both worth recording.
  //
  // `openUrl` is for URLs — the opener plugin's default permission covers mailto/tel/
  // http(s) only, so a Windows path was rejected as both wrong-scheme and not-a-URL.
  // `openPath` is the filesystem entry point, but it enforces a path SCOPE, and no
  // useful scope exists here: the export folder is whatever the user picked in a
  // native dialog, so anything broad enough to always work is `**`.
  //
  // `revealItemInDir` takes no scope and is already granted by `opener:default`. It
  // opens the CONTAINING folder with the item selected — so revealing a file inside
  // the export folder opens the export folder itself, which is what "Open folder"
  // should do. Revealing the folder would show its parent instead. Falls back to that
  // if the file is missing.
  async function openExportFolder() {
    if (!exportDone) return;
    const sep = exportDone.includes("\\") ? "\\" : "/";
    try {
      await revealItemInDir(`${exportDone}${sep}session.md`);
    } catch {
      try {
        await revealItemInDir(exportDone);
      } catch (e) {
        exportError = `Couldn't open the folder: ${e}`;
      }
    }
  }

  async function copyExportPath() {
    if (!exportDone) return;
    try {
      await invoke("copy_text", { text: exportDone });
      exportPathCopied = true;
      if (exportCopyTimer) clearTimeout(exportCopyTimer);
      exportCopyTimer = setTimeout(() => (exportPathCopied = false), 1800);
    } catch (e) {
      exportError = `Couldn't copy the path: ${e}`;
    }
  }

  async function openExport() {
    exportError = ""; exportDone = ""; exportRedacted = [];
    try {
      const s = await invoke<ExportStatus>("export_status");
      exportStatus = s;
      exportTitle = s.suggested_title;
      exportDest = s.destination;
      showExport = true;
    } catch (e) {
      exportError = String(e);
      showExport = true;
    }
  }

  function toggleRedact(i: number) {
    exportRedacted = exportRedacted.includes(i)
      ? exportRedacted.filter((x) => x !== i)
      : [...exportRedacted, i];
  }

  async function chooseExportFolder() {
    try {
      const picked = await invoke<string | null>("pick_export_folder");
      // null is a cancelled dialog, which is the normal path, not an error.
      if (picked) exportDest = picked;
    } catch (e) {
      exportError = String(e);
    }
  }

  async function runExport() {
    exportBusy = true; exportError = ""; exportDone = "";
    try {
      const out = await invoke<string>("export_session", {
        destination: exportDest || null,
        title: exportTitle,
        slug: null,
        cropToApp: exportCropToApp,
        saveClean: exportSaveClean,
        drawPointer: exportDrawPointer,
        drawCaption: exportDrawCaption,
        redactedSteps: exportRedacted,
      });
      exportDone = out;
    } catch (e) {
      exportError = String(e);
    } finally {
      exportBusy = false;
    }
  }
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

  // ─── Side-by-side dock ────────────────────────────────────────────────────
  //
  // Dock the panel to one quarter of the screen edge, full height, and give the
  // rest to one chosen app. Windows 11 can resize two snapped windows together
  // via their shared divider, but never for an always-on-top window (it is not
  // put in a snap group), and a quarter-width full-height zone isn't a layout
  // Windows offers anyway — so Navisual owns both edges here and mirrors its own
  // resize onto the partner, which makes the shared edge a real divider at any
  // ratio without giving up always-on-top.
  //
  // Only the *arrangement* is remembered across restarts, never the partner
  // window: silently dragging someone's browser around at launch is the panel-
  // nudging instinct this project already rejected once. The panel returns to
  // its dock (which is what a floating panel could never do — its position was
  // always reset to bottom-right); re-filling the space is one click.
  const DOCK_KEY = "navisual-dock-v1";
  type DockSide = "left" | "right";
  let dockSide = $state<DockSide | null>(null);
  let dockPartner = $state<number | null>(null);
  // Physical px. The panel's width while docked — i.e. where the user last put
  // the divider — kept apart from lastPanelSize so docking never overwrites the
  // floating size we restore on undock.
  let dockWidth: number | null = null;
  let preDockSize: { width: number; height: number } | null = null;
  let dockSaveTimer: ReturnType<typeof setTimeout> | null = null;

  function saveDock() {
    try {
      if (dockSide) {
        localStorage.setItem(DOCK_KEY, JSON.stringify({ side: dockSide, width: dockWidth, floating: preDockSize }));
      } else {
        localStorage.removeItem(DOCK_KEY);
      }
    } catch (_) {}
  }
  function saveDockSoon() {
    if (dockSaveTimer) clearTimeout(dockSaveTimer);
    dockSaveTimer = setTimeout(saveDock, 400);
  }

  // Move the panel to `side`. `width` (physical px) reuses a divider position
  // the user already chose; omitted, the backend picks the default quarter.
  //
  // `dockSide` is claimed BEFORE the move, not after: the move fires an
  // onResized event, and that handler branches on `dockSide` to decide whether
  // it is looking at a divider drag or at the user resizing a floating panel.
  // Setting it afterwards let the dock's own resize race in as a "floating"
  // one, which overwrote the very size undock() exists to restore (seen live:
  // undock came back at the docked width). Reverted if the dock fails.
  async function applyDock(side: DockSide, width: number | null) {
    const prev = dockSide;
    dockSide = side;
    try {
      const layout = await invoke<{ panel: { width: number } } | null>("dock_panel", {
        side,
        width: width ?? null,
      });
      if (!layout) { dockSide = prev; return false; }
      dockWidth = layout.panel.width;
      return true;
    } catch (e) {
      dockSide = prev;
      throw e;
    }
  }

  async function dockPanel(side: DockSide) {
    showQuickMenu = false;
    if (iconMode) await expandToPanel();
    // Remember what to come back to before the dock overwrites the live size.
    if (!dockSide) preDockSize = { ...lastPanelSize };
    if (!(await applyDock(side, dockWidth))) return;
    saveDock();
    // The other half of the arrangement: which app fills the rest.
    openTargetPicker("dock");
  }

  // `reposition: false` is the drag case — the user has just put the panel somewhere
  // deliberately, so restoring it to the pre-dock size and parking it bottom-right
  // would be yanking it straight back out of their hands. Only the menu's Undock,
  // which the user asked for with no place of their own in mind, repositions.
  async function undock(reposition = true) {
    showQuickMenu = false;
    const restore = preDockSize ?? lastPanelSize;
    dockSide = null;
    dockPartner = null;
    dockWidth = null;
    preDockSize = null;
    saveDock();
    if (!reposition) return;
    try {
      const sw = window.screen.availWidth;
      const sh = window.screen.availHeight;
      const margin = 24;
      const w = Math.min(Math.max(360, restore.width), sw - margin * 2);
      const h = Math.min(Math.max(380, restore.height), sh - margin * 2);
      lastPanelSize = { width: w, height: h };
      localStorage.setItem(PANEL_SIZE_KEY, JSON.stringify(lastPanelSize));
      await getCurrentWindow().setSize(new LogicalSize(w, h));
      await getCurrentWindow().setPosition(new LogicalPosition(sw - w - margin, sh - h - margin));
    } catch (e) { console.error("undock:", e); }
  }

  // Give the chosen app everything the panel isn't using, and make it the
  // guidance target too — docking an app beside the panel is a plain statement
  // of what the user is working in.
  async function fillDockPartner(hwnd: number) {
    targetPickerOpen = false;
    targetPickerMode = "target";
    if (!dockSide) return;
    dockPartner = hwnd;
    saveDock();
    try { await invoke("dock_fill", { hwnd, side: dockSide }); }
    catch (e) { console.error("dock_fill:", e); }
    await selectTarget(hwnd);
  }

  // The divider. Every panel resize while docked pushes the partner's edge to
  // match, so dragging the panel's inner border drags the shared border.
  // Coalesced to one call per frame — a drag fires resize events far faster
  // than SetWindowPos needs to run, and the partner only ever needs the latest.
  // Moving the panel out of its dock, or resizing it from any edge that isn't the
  // divider, means it is no longer docked — so stop claiming it is. Checked rather
  // than assumed, because a divider drag IS a move and a resize: the backend compares
  // the panel against the dock invariant (outer edge pinned, full work-area height),
  // which a divider drag preserves and a titlebar drag does not. Debounced so a drag
  // is judged once it settles, never mid-flight.
  let dockVerifyTimer: ReturnType<typeof setTimeout> | null = null;
  function verifyDockSoon() {
    if (!dockSide) return;
    if (dockVerifyTimer) clearTimeout(dockVerifyTimer);
    dockVerifyTimer = setTimeout(async () => {
      if (!dockSide) return;
      try {
        const intact = await invoke<boolean>("dock_is_intact", { side: dockSide });
        if (!intact) await undock(false);
      } catch (_) { /* couldn't measure — leave the dock alone rather than guess */ }
    }, 350);
  }

  let dockSyncQueued = false;
  function scheduleDockSync() {
    if (dockSyncQueued || !dockSide || dockPartner === null) return;
    dockSyncQueued = true;
    requestAnimationFrame(async () => {
      dockSyncQueued = false;
      if (!dockSide || dockPartner === null) return;
      try { await invoke("dock_fill", { hwnd: dockPartner, side: dockSide }); }
      catch (_) { /* window closed under us — the next pick re-establishes it */ }
    });
  }

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
  // Applied autopilot sensitivity (cells-of-1024 that must change to fire) — mirrors the
  // "apply on Apply" pattern of autoAdvanceEnabled; passed to check_screen_changed each poll.
  let autopilotMinCells = $state(16);

  // Autopilot on-demand polling.
  let screenChangeDebounce = 0;
  let autopilotInterval: ReturnType<typeof setInterval> | null = null;
  // Runaway guard: an animated window (a playing video, a big progress spinner) changes
  // constantly, so the more-sensitive block-diff detector could fire every 5 s forever —
  // each fire burning an AI request. Count auto-advances that produce NO visible progress
  // (same instruction, same step); real progress resets it. After the cap, pause autopilot.
  let autopilotStalls = 0;
  const AUTOPILOT_MAX_STALLS = 3;

  function startAutopilotPolling() {
    if (autopilotInterval !== null) return;
    autopilotStalls = 0;
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
        const res = await invoke<{ changed: boolean }>("check_screen_changed", {
          minCells: autopilotMinCells,
        });
        if (!res.changed) return;
        // Re-check phase after the await — guarding against the small race where
        // a manual Cancel or new task fired while the capture was in flight.
        if (phase !== "guiding") return;
        const currentStep = steps[stepIndex];
        if (!currentStep) return;
        screenChangeDebounce = Date.now();
        addToHistory("system", "Screen changed — checking next step…");
        // Snapshot progress markers, advance, then judge whether anything actually moved.
        const prevInstr = currentInstruction;
        const prevIdx = stepIndex;
        await nextStep(true); // autopilot-triggered — logs "worked_auto", not "worked" (C3 + taxonomy split)
        // `phase` was narrowed to "guiding" by the guard above; it can change across the await
        // (nextStep may land on needs_input), so widen it back before comparing.
        const progressed =
          (phase as AppPhase) === "needs_input" ||
          currentInstruction !== prevInstr ||
          stepIndex !== prevIdx;
        if (progressed) {
          autopilotStalls = 0;
        } else if (++autopilotStalls >= AUTOPILOT_MAX_STALLS) {
          autopilotStalls = 0;
          autoAdvanceEnabled = false;
          stopAutopilotPolling();
          addToHistory(
            "system",
            "Autopilot paused — the screen kept changing without a clear next step. Turn it back on when you're ready.",
          );
        }
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
    // Store builds never self-update — see isPackaged. Guarded here rather than
    // only at the call sites so no future caller can reintroduce the network hit.
    if (isPackaged) return;
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
      show_ai_bbox: settingsForm.debug_diagnostics_enabled,
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
      show_ai_bbox: settingsForm.debug_diagnostics_enabled,
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
  // Right-click OR long-press opens the menu.
  //
  // CORRECTION (2026-09-06): an earlier version of this comment claimed WebView2
  // delivers no right-button events to the page. That was wrong, and wrong in an
  // avoidable way — it generalised from SYNTHETIC `mouse_event` right-clicks that
  // were not landing, and never tried a real one. A real right-click works, and
  // opens this menu.
  //
  // What was actually missing is `oncontextmenu` + preventDefault: WebView2's own
  // menu (Back / Refresh / Save as / Print / Inspect) otherwise opens ON TOP of
  // ours. `preventDefault()` on pointerdown does NOT suppress it — only the
  // `contextmenu` event does. The expanded panel suppresses it too now (see
  // handlePanelContextMenu) — except in text fields, and on Shift+right-click.
  //
  // Long-press is kept alongside it — same action, and the gesture that works on a
  // 48px target where a stray click should only ever expand.
  const ICON_LONG_PRESS_MS = 450;
  let _iconLongPressTimer: ReturnType<typeof setTimeout> | null = null;
  let _iconLongPressFired = false;

  function cancelIconLongPress() {
    if (_iconLongPressTimer) { clearTimeout(_iconLongPressTimer); _iconLongPressTimer = null; }
  }

  function handleIconPointerdown(e: PointerEvent) {
    if (e.button === 2) { e.preventDefault(); openIconMenu(e); return; }
    if (e.button !== 0) return;
    _iconStartX = e.screenX; _iconStartY = e.screenY; _iconDragged = false;
    _iconLongPressFired = false;
    // A surface is already open: no long-press timer (the click dismisses it), but the
    // press is still tracked above so a DRAG from here still works -- see pointermove.
    if (iconSurface) return;
    cancelIconLongPress();
    _iconLongPressTimer = setTimeout(() => {
      _iconLongPressTimer = null;
      if (_iconDragged) return;      // they were moving the icon, not holding it
      _iconLongPressFired = true;    // so the click that follows doesn't also expand
      openIconMenu();
    }, ICON_LONG_PRESS_MS);
  }

  function handleIconPointerup() {
    cancelIconLongPress();
  }
  async function handleIconPointermove(e: PointerEvent) {
    if (_iconDragged || e.buttons !== 1) return;
    if (Math.hypot(e.screenX - _iconStartX, e.screenY - _iconStartY) > 4) {
      _iconDragged = true;
      cancelIconLongPress(); // moving the icon is a drag, never a menu
      // Dragging with a surface open has to CLOSE it first, and finish closing
      // before the drag starts. `startDragging()` enters the OS modal move loop,
      // which blurs the WebView -- so the blur handler's own close would run inside
      // that loop, where the shrink and reposition are swallowed, leaving a
      // menu-sized transparent window with no menu in it (reported live). Closing
      // here, awaited, means the drag begins from a real icon-sized window.
      //
      // Refusing to drag at all was tried instead and was worse: dragging the icon
      // is how it gets out of the way, and losing that because a menu happens to be
      // open is a bigger loss than the ghost window ever was.
      if (iconSurface) await closeIconSurface();
      try { await getCurrentWindow().startDragging(); } catch (_) {}
    }
  }
  function handleIconClick() {
    cancelIconLongPress();
    if (_iconDragged) return;
    // The long press already opened the menu; the click that ends it must not then
    // expand the panel out from under it.
    if (_iconLongPressFired) { _iconLongPressFired = false; return; }
    // With a surface open the window IS the surface, so there is no "outside" to
    // click — the fish is the only place left to dismiss from.
    if (iconSurface) { closeIconSurface(); return; }
    expandToPanel();
  }

  // Set when a request fails, so the COLLAPSED icon can say so. AI failures revert
  // `phase` to what it was and write a warning line into the history -- exactly the
  // wrong shape for a collapsed session, where the history is not on screen and the
  // fish would otherwise sit looking idle after a Gemini API error (reported live).
  // Cleared whenever a new attempt starts, so it always describes the LAST one.
  let lastRequestFailed = $state(false);

  // ── Collapsed-icon surfaces (menu / chat) ─────────────────────────────────
  //
  // The collapsed window is ICON_SIZE square, and nothing can paint outside its
  // own window — so a context menu, a chat box, or any hint bubble richer than a
  // native tooltip is simply not renderable at 56px. They all need the same thing:
  // grow the window, then shrink it back.
  //
  // The fish must not move while that happens. It is what the user just clicked,
  // and having it jump out from under the cursor to make room for its own menu
  // would be the worst kind of surprise. So the window ORIGIN is adjusted by
  // exactly the amount it grew, in whichever direction keeps the fish still —
  // which also gives the edge flip any context menu needs, for free.
  type IconSurface = null | "menu" | "chat" | "hint";
  let iconSurface = $state<IconSurface>(null);
  let iconFlipX = $state(false);
  let iconFlipY = $state(false);
  let iconChatText = $state("");
  // Where the icon sat before it grew, in logical px, so shrinking puts it back.
  let iconRestorePos: { x: number; y: number } | null = null;

  const ICON_MENU_W = 190;
  // 56 for the fish plus four ~34px rows and the surface's own padding. Measured
  // rather than guessed: at 172 the Quit row was cut in half.
  const ICON_MENU_H = 212;
  const ICON_HINT_W = 268;
  // 56 for the fish, then the title, two lines of body and the Got-it button.
  // Third surface in a row whose first guess was too short — a fixed-size window
  // gives CSS nowhere to overflow to, so these are measured on screen, not reasoned.
  const ICON_HINT_H = 168;
  const ICON_CHAT_W = 340;
  // 56 for the fish, then the input, the send row and the surface's padding. At
  // 104 only 48px was left below the fish for all three.
  const ICON_CHAT_H = 152;

  async function growIconWindow(w: number, h: number) {
    const win = getCurrentWindow();
    try {
      const scale = await win.scaleFactor();
      const pos = await win.outerPosition();
      const x = pos.x / scale;
      const y = pos.y / scale;
      // Flip against the monitor the icon is actually on, not the primary — the
      // panel is frequently parked on a second screen.
      const mon = await currentMonitor();
      const mx = mon ? mon.position.x / scale : 0;
      const my = mon ? mon.position.y / scale : 0;
      const mw = mon ? mon.size.width / scale : window.screen.availWidth;
      const mh = mon ? mon.size.height / scale : window.screen.availHeight;

      iconFlipX = x + w > mx + mw;
      iconFlipY = y + h > my + mh;
      iconRestorePos = { x, y };

      await win.setSize(new LogicalSize(w, h));
      await win.setPosition(
        new LogicalPosition(
          iconFlipX ? x - (w - ICON_SIZE) : x,
          iconFlipY ? y - (h - ICON_SIZE) : y,
        ),
      );
    } catch (e) { console.error("growIconWindow:", e); }
  }

  async function closeIconSurface() {
    if (!iconSurface) return;
    iconSurface = null;
    iconChatText = "";
    const win = getCurrentWindow();
    try {
      await win.setSize(new LogicalSize(ICON_SIZE, ICON_SIZE));
      if (iconRestorePos) {
        await win.setPosition(new LogicalPosition(iconRestorePos.x, iconRestorePos.y));
      }
    } catch (e) { console.error("closeIconSurface:", e); }
    iconRestorePos = null;
    iconFlipX = false;
    iconFlipY = false;
  }

  async function openIconMenu(e?: Event) {
    e?.preventDefault();
    if (iconSurface === "menu") { await closeIconSurface(); return; }
    if (iconSurface) await closeIconSurface();
    await growIconWindow(ICON_MENU_W, ICON_MENU_H);
    iconSurface = "menu";
  }

  async function openIconChat() {
    if (iconSurface) await closeIconSurface();
    await growIconWindow(ICON_CHAT_W, ICON_CHAT_H);
    iconSurface = "chat";
    // The window has to exist at its new size before the input can take focus.
    setTimeout(() => iconChatInput?.focus(), 60);
  }
  let iconChatInput = $state<HTMLInputElement | undefined>(undefined);

  // Ask a follow-up without ever leaving collapsed mode — the point of the whole
  // surface. Submitting shrinks straight back to the fish, whose ring and thinking
  // arc then report what the answer is doing.
  async function submitIconChat() {
    const text = iconChatText.trim();
    if (!text) return;
    await closeIconSurface();
    task = text;
    await submitTask();
  }

  // The collapsed icon has no room for the status bar's shortcut legend, which is
  // the whole reason Ctrl+~ was invisible here despite always having worked. Hover
  // carries it permanently (see `iconTitle`), but hovering is something you have to
  // think to do — so the key is also shown ONCE, unprompted, the first time a step
  // lands while collapsed. Same mechanism as the target-chip and collapse hints:
  // localStorage-gated, shown once ever, and it takes itself away.
  const ICON_HOTKEY_HINT_KEY = "navisual-icon-hotkey-hint-v3";
  let iconHintTimer: ReturnType<typeof setTimeout> | null = null;

  async function showIconHotkeyHint() {
    if (iconSurface) return;                       // never interrupt a menu or a chat
    await growIconWindow(ICON_HINT_W, ICON_HINT_H);
    iconSurface = "hint";
    if (iconHintTimer) clearTimeout(iconHintTimer);
    iconHintTimer = setTimeout(() => { if (iconSurface === "hint") closeIconSurface(); }, 7000);
  }

  $effect(() => {
    // Deliberately reads phase/iconMode so it re-evaluates as they change; the
    // localStorage write makes it fire once ever regardless of how often that is.
    if (!iconMode || phase !== "guiding" || !settingsForm.hotkey_next) return;
    try {
      if (localStorage.getItem(ICON_HOTKEY_HINT_KEY)) return;
      localStorage.setItem(ICON_HOTKEY_HINT_KEY, "1");
    } catch (_) { return; }
    showIconHotkeyHint();
  });

  function iconMenuAction(fn: () => void) {
    closeIconSurface().then(fn);
  }

  // ── Collapsed-icon state ──────────────────────────────────────────────────
  // Circumference of the r=25.5 ring, so the progress arc can be driven by
  // stroke-dashoffset. 25.5 sits just outside the 48px fish inside a 56px window.
  const ICON_RING_C = 2 * Math.PI * 25.5;

  // How far through the work we are, 0..1, or null when there is nothing honest
  // to show. The model's own route overview is preferred — it describes the task
  // — but `plan_outline` is optional and often absent, so the local step counter
  // is the fallback rather than the primary.
  // Both sources need MORE THAN ONE unit before a ring means anything. A
  // single-milestone plan sits at 0/1 for the whole task and then vanishes —
  // measured live on a real session, where the panel read "1 of 1" while the ring
  // drew nothing at all, which looks exactly like a broken feature. And the
  // alternative reading, (completed + 1) / total, would show a FULL ring for a task
  // that has not been started. Neither is worth drawing: with one unit there is no
  // journey to show, so the icon stays clean and the tooltip still says where you
  // are. Progress is completed/total, never position, so the ring only ever
  // overstates when it is genuinely full.
  let iconProgress = $derived.by(() => {
    if (sessionPlanOutline.length > 1) {
      return Math.min(sessionPlanCompletedCount / sessionPlanOutline.length, 1);
    }
    if (steps.length > 1) return Math.min(stepIndex / steps.length, 1);
    return null;
  });

  // The collapsed icon has no room for a legend, so its tooltip carries what the
  // status bar would have said — including the shortcut, which already works while
  // collapsed and was simply never visible there.
  let iconTitle = $derived(
    lastRequestFailed ? "Last request failed — click to expand and see why"
    : phase === "thinking" ? "Navisual is thinking…"
    : phase === "needs_input" ? "Navisual asked you something — click to expand"
    : iconProgress !== null && settingsForm.hotkey_next
      ? `Step ${stepIndex + 1} of ${steps.length} — ${prettyHotkey(settingsForm.hotkey_next)} for next · click to expand`
    : iconProgress !== null
      ? `Step ${stepIndex + 1} of ${steps.length} — click to expand`
    : "Expand Navisual"
  );

  async function collapseToIcon() {
    dismissCollapseHint(); // they found it — the coach mark is no longer needed
    iconSurface = null;    // never collapse into a stale menu/chat size
    iconRestorePos = null;
    iconFlipX = false;
    iconFlipY = false;
    iconMode = true;
    // A Windows accent border round the expanded panel looks like a window. Round a
    // 56px transparent square holding a goldfish it looks like a box someone drew.
    //
    // AWAITED, and before the resize. Removing the frame styles changes how much of
    // the window rect the client occupies, so a size applied first is computed
    // against the old frame: the window stayed 72x65 around a 56x56 icon, and the
    // leftover transparent L showed whatever was behind it -- reported as a stray
    // close button appearing next to the fish.
    try { await invoke("set_panel_border", { enabled: false }); } catch (_) {}
    try { await getCurrentWindow().setSize(new LogicalSize(ICON_SIZE, ICON_SIZE)); }
    catch (e) { console.error("collapseToIcon:", e); }
  }

  async function expandToPanel() {
    iconSurface = null;
    iconRestorePos = null;
    iconFlipX = false;
    iconFlipY = false;
    iconMode = false;
    // Restore the frame BEFORE sizing, for the same reason collapse suppresses it
    // before sizing: the size is computed against whichever frame is in effect.
    try { await invoke("set_panel_border", { enabled: true }); } catch (_) {}
    try {
      // A docked panel expands back into its dock at the width the user left it,
      // not into a floating window parked wherever the icon happened to be.
      if (dockSide) { await applyDock(dockSide, dockWidth); scheduleDockSync(); return; }
      await getCurrentWindow().setSize(new LogicalSize(lastPanelSize.width, lastPanelSize.height));
    }
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
    if (billing.oauthPending || billing.buyPending || billing.checkoutPending) return;
    // Settings used to close HERE, before the call, which doubled as the
    // double-submit guard. It now closes only once a browser is actually
    // opening (below), so the guard has to be explicit -- and on its OWN flag,
    // since oauthPending still means "Google sign-in" to the Account panel.
    billing.buyPending = true;
    try {
      let url: string;
      try {
        url = await invoke<string>("create_checkout", { amountUsd });
      } catch (e) {
        if (String(e).includes("oauth_required")) {
          // Not signed in yet, and NOTHING opened in the browser — the user
          // picks a sign-in method here (Google or email), then presses Buy
          // again; account linking preserves free requests and any coins.
          //
          // This used to slam Settings shut and reopen it on the Account tab,
          // and that visible Billing→Account switch WAS the feedback. Once
          // Billing merged into Account (2026-09-06) both assignments became
          // no-ops and the close/reopen degraded into a flicker ending exactly
          // where it started — reported live as "Buy coins button not working",
          // because from the outside nothing happened. So: don't move them,
          // tell them why, and put the cursor in the field they need. Settings
          // is opened only if closed — the trial-exhausted modal arrives here
          // with no Settings open and still needs it raised.
          await setPanelOnTop(true);
          settingsTab = "account";
          account.view = "signin";
          account.error = "";
          account.notice = "Sign in to buy coins — use Google below, or enter your email.";
          showSettings = true;
          await tick();
          document.getElementById("acct-email")?.focus();
          return;
        }
        throw e;
      }
      // A browser is opening NOW, so get out of its way: the panel is
      // alwaysOnTop and would sit over the checkout page even when that has
      // focus, and the open Settings modal covers the draggable titlebar.
      // refreshBalance() restores both when the user returns (auto via the
      // focus listener, or the manual button).
      showSettings = false;
      await setPanelOnTop(false);
      billing.checkoutPending = true;
      openUrl(url);
    } catch (e) {
      // Settings is still open on this path (it no longer closes up front), so
      // the history line would land behind the modal. Put the reason in the
      // panel as well, where the click happened.
      account.error = "Checkout failed: " + String(e);
      addToHistory("system", "⚠️ Checkout failed: " + String(e));
      await setPanelOnTop(true); // nothing opened — restore always-on-top
    } finally {
      billing.buyPending = false;
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
    // These two are cleared by things arriving from OUTSIDE (an oauth_complete
    // event, a return from the browser), so they need this defensive reset.
    // buyPending deliberately is not: it is owned by buyCoins' try/finally and
    // clearing it here could only re-open the double-submit it exists to prevent.
    billing.oauthPending = false;
    billing.checkoutPending = false;
    await setPanelOnTop(true); // back from the browser — restore always-on-top
  }

  // Live-reported: switching Provider to Managed mid-session (started on a BYOK
  // provider, e.g. via .env) left every paid quality tier stuck on "not enough
  // coins" — billing.coinBalanceMicro stays null forever because the ONLY balance
  // fetch was gated behind `api_provider === "managed"` at cold-start (onMount),
  // which this session's provider never was. Switching to the Billing tab (which
  // fetches its own balance on mount) and back "fixed" it, confirming the data was
  // reachable, just never asked for. Mirrors the cold-start sign-in+refresh here so
  // a live provider switch gets the same treatment a fresh launch on Managed does.
  async function handleProviderChange() {
    if (settingsForm.api_provider === "ollama" && ollamaModels.length === 0) refreshOllamaModels();
    if (settingsForm.api_provider === "managed" && billing.coinBalanceMicro == null) {
      try { await invoke("sign_in_anon"); } catch (_) {}
      await billing.refresh();
    }
  }

  // ── Account management (S.2.1) ──────────────────────────────────────────────
  // The identity/view state and every acct* handler moved to
  // src/AccountPanel.svelte + src/lib/account.svelte.ts (componentization pass,
  // 2026-07-13). App keeps only the cross-cutting pieces: buyCoins' redirect
  // into the Account tab (via the account store) and the account_changed
  // listener below.

  async function openSettings(tab: SettingsTab = "provider") {
    settingsTab = tab;
    // The Account tab renders live balance + identity state; the tab *button*
    // refreshes both on click, but this deep-link path (the header balance chips,
    // which are the main route to buying coins) used to skip the refresh and show
    // stale numbers (audit F9). Billing moved onto this tab on 2026-09-06, so the
    // chips now land here and both loads have to run.
    if (tab === "account") { refreshBalance(); account.load(); }
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
      debugShowInfo = data.debug_diagnostics_enabled;
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

  // Two-click confirm for the all-tabs reset: the first click arms it (the button relabels to
  // spell out the scope), a second click within 4s performs it. Prevents a reset-all when the
  // user thought it only touched the current tab.
  let resetArmed = $state(false);
  let resetArmTimer: ReturnType<typeof setTimeout> | null = null;
  function handleResetClick() {
    if (!resetArmed) {
      resetArmed = true;
      if (resetArmTimer) clearTimeout(resetArmTimer);
      resetArmTimer = setTimeout(() => (resetArmed = false), 4000);
      return;
    }
    if (resetArmTimer) clearTimeout(resetArmTimer);
    resetArmed = false;
    resetSettings();
  }

  async function resetSettings() {
    settingsError = null;
    // Restore everything to defaults but preserve API keys so the user
    // doesn't lose credentials they've already entered.
    // Every *_API_KEY the backend knows about — save_settings skips empty key
    // fields, so a key omitted here is not lost, it just goes blank in the UI
    // until Settings is reopened. That still reads as "my key was wiped", which
    // is why the list must stay complete rather than nearly complete.
    const preserved = {
      anthropic_api_key: settingsForm.anthropic_api_key,
      gemini_api_key: settingsForm.gemini_api_key,
      openai_api_key: settingsForm.openai_api_key,
      deepseek_api_key: settingsForm.deepseek_api_key,
      qwen_api_key: settingsForm.qwen_api_key,
      custom_api_key: settingsForm.custom_api_key,
    };
    // Ask the BACKEND what the defaults are rather than trusting this file's
    // SETTINGS_DEFAULTS copy of them. That copy had silently drifted from
    // config.rs, so Reset was installing claude-sonnet-4-6, gemini-2.5-flash,
    // gpt-5.5 and qwen3.6-plus over the current defaults — and gemini-2.5-flash
    // is no longer even an option in its own dropdown. SETTINGS_DEFAULTS stays as
    // the pre-load placeholder (and the fallback if this call fails), but it is no
    // longer what a reset writes, so the drift cannot come back.
    let defaults = SETTINGS_DEFAULTS;
    try {
      defaults = await invoke<SettingsPayload>("get_default_settings");
    } catch (e) {
      settingsError = `Couldn't read the shipped defaults (${e}); reset used this build's built-in copy.`;
    }
    settingsForm = { ...defaults, ...preserved };
    syncCustomModelFlags();
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
      autopilotMinCells = settingsForm.autopilot_min_cells;
      if (autoAdvanceEnabled) startAutopilotPolling(); else stopAutopilotPolling();
      isMuted = !settingsForm.tts_enabled;
      debugShowInfo = settingsForm.debug_diagnostics_enabled;
      await emitTo("overlay", "overlay:theme", {
        color: settingsForm.overlay_color,
        thickness: settingsForm.overlay_thickness,
        subtitle_enabled: settingsForm.subtitle_enabled,
        show_ai_bbox: settingsForm.debug_diagnostics_enabled,
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
    planExpanded = false;
    // The goal card is gated on sessionGoal alone, so leaving it set kept the
    // FINISHED task's "Working on …" pinned above a freshly-emptied session
    // (live-reported) — the plan underneath it was already being cleared here,
    // which just made the leftover card look emptied rather than stale.
    sessionGoal = "";
    sessionPlanOutline = [];
    sessionPlanCompletedCount = 0;
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
    sessionGoal = res.goal ?? "";
    sessionPlanOutline = res.plan_outline ?? [];
    sessionPlanCompletedCount = res.plan_completed_count ?? 0;
    phase = res.needs_input ? "needs_input" : "guiding";
    if (phase === "guiding") maybeShowCollapseHint();
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
    // The debug screenshot is still SAVED to disk when the capture setting is on (backend
    // writes it), but its path is no longer surfaced in the conversation — it was clutter even
    // for dev use; the files are in %LOCALAPPDATA%\com.navisual.app\debug.
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
    behindPanel = false;
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
        lastRequestFailed = true;
          addToHistory("system", "⚠️ " + (res.error ?? "guide failed"));
        if (taskText !== "") task = taskText;
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = prevPhase;
      lastRequestFailed = true;
        addToHistory("system", "⚠️ " + String(e));
      if (taskText !== "") task = taskText;
    }
  }

  async function nextStep(viaAutopilot = false, skipFeedback = false) {
    // Don't allow next while an AI call is in flight — the hotkey can fire
    // even when the Next button is disabled (Svelte derived state edge case).
    if (phase === "thinking") return;
    lastRequestFailed = false; // this attempt supersedes whatever the last one did
    // Any manual advance means the user is engaged — clear the autopilot runaway counter so
    // a genuine hands-on session can't accumulate stalls toward a spurious pause.
    if (!viaAutopilot) autopilotStalls = 0;
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
    behindPanel = false;
    const nextIdx = stepIndex + 1;
    const prevPhase = phase;
    if (nextIdx >= steps.length) {
      // Re-query AI — tell it what was just completed so it doesn't repeat.
      //
      // UNLESS the AI was asking a question. Then `currentInstruction` IS the
      // question, and the old code reported it back as
      // `[User completed: "Which cell are you looking for?"]` — telling the model
      // the user had completed the model's own unanswered question. That is not
      // merely noise: prompt Rule 17 instructs the model to TRUST a completion
      // claim and advance without second-guessing it (many real actions leave no
      // visible trace), so it is a false confirmation aimed squarely at the one
      // rule built to be believed. Seen in the wild once, on a long Chinese
      // question echoed back whole as a completed step.
      //
      // It also actively fights the screenshot, which is the honest signal here:
      // the capture taken with this very click is current, and often already
      // shows what the question was asking about. So say what actually happened
      // and point the model at the picture rather than contradicting it.
      const unanswered = prevPhase === "needs_input" ? currentInstruction : "";
      const completed = unanswered ? "" : (currentInstruction || lastCompletedInstruction);
      // Never bank a question as the last completed step — it would be re-sent as
      // a completion on the following turn too.
      if (!unanswered) lastCompletedInstruction = completed;
      currentInstruction = "";
      streamStepsSeen = 0;
      phase = "thinking";
      startTimer();
      const token = ++requestToken;
      // Create a history entry so the screenshot thumbnail has somewhere to live.
      const reQueryId = await addToHistory("system",
        unanswered ? "↷ Skipped the question — re-analysing…"
        : completed ? `✓ Completed — re-analysing…` : "Re-analysing…");
      try {
        const res = await invoke<GuideResponse>("guide", {
          task: unanswered
            ? `[The user did not answer your question: "${unanswered}" — they pressed Next to move on. `
              + `The screenshot accompanying this message is CURRENT and is the reliable signal: read it, `
              + `and if it already answers the question, act on that. Otherwise proceed with the most `
              + `reasonable assumption and say which assumption you made. Do not treat the question as `
              + `answered or as a completed step, and do not simply repeat it — if you truly cannot `
              + `continue without an answer, ask again in a shorter, simpler form.]`
            : completed ? `[User completed: "${completed}"]` : "",
          isReply: false,
        });
        stopTimer();
        if (token !== requestToken) return;
        if (res.chat_thumb_b64) attachThumb(reQueryId, res.chat_thumb_b64);
        if (!res.ok) {
          phase = prevPhase;
          lastRequestFailed = true;
          addToHistory("system", "⚠️ " + (res.error ?? "re-query failed"));
          return;
        }
        applyResponse(res, 0, token);
      } catch (e) {
        stopTimer();
        if (token !== requestToken) return;
        phase = prevPhase;
        lastRequestFailed = true;
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
      lastRequestFailed = true;
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
    behindPanel = false;
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction", { note: note || null, avoidBboxes, reason: category ?? null });
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
      lastRequestFailed = true;
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
    lastRequestFailed = false;
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
    // Before anything else, so "report" is the earliest the frontend could speak.
    reportBootTiming("mount");
    if (document.readyState === "complete") reportBootTiming("load");
    else window.addEventListener("load", () => reportBootTiming("load"), { once: true });

    getVersion().then(v => { appVersion = v; }).catch(() => {});
    // Resolve packaging BEFORE arming the update check — awaited, not fire-and-forget,
    // so a Store build can't race the 5s timer and phone home once on launch.
    // Plain invoke, not invokeReady: is_packaged takes no State, so it answers
    // during the startup race and must not queue behind the readiness gate.
    try { isPackaged = await invoke<boolean>("is_packaged"); } catch (_) {}
    if (!isPackaged) setTimeout(() => checkForUpdates(), 5000);

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
      autopilotMinCells = init.autopilot_min_cells ?? 16;
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
      debugShowInfo = init.debug_diagnostics_enabled;
    } catch (e) {
      // Everything below runs on SETTINGS_DEFAULTS from here — wrong provider in
      // the header, default hotkeys registered, the user's autopilot sensitivity
      // and TTS choice ignored. That was silent until 2026-09-07, when a launch
      // lost the manage() race and the only visible symptom was an unrelated-
      // looking sign-in warning. Opening Settings re-reads from disk and repairs
      // the form, so this degrades rather than destroys — but the user has to be
      // told which state they are in.
      addToHistory("system", "⚠️ Couldn't load your settings — running on defaults for now. Open Settings to reload them. (" + String(e) + ")");
    }

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

    // A docked panel goes back to its dock instead. This is the one piece of
    // panel geometry that used to be thrown away on every launch: the size was
    // restored above, but the position was always re-derived as bottom-right,
    // so a deliberately-placed panel drifted off its edge every restart.
    // The partner app is deliberately NOT restored — see DOCK_KEY.
    try {
      const savedDock = localStorage.getItem(DOCK_KEY);
      if (savedDock) {
        const d = JSON.parse(savedDock);
        if (d.side === "left" || d.side === "right") {
          preDockSize = d.floating ?? null;
          await applyDock(d.side, typeof d.width === "number" ? d.width : null);
        }
      }
    } catch (_) {}

    try {
      if (!dockSide) {
        await getCurrentWindow().setSize(new LogicalSize(lastPanelSize.width, lastPanelSize.height));
        await getCurrentWindow().setPosition(
          new LogicalPosition(sw - lastPanelSize.width - margin, sh - lastPanelSize.height - margin)
        );
      }
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
    getCurrentWindow().onMoved(() => {
      if (iconMode) return;
      verifyDockSoon();
    }).catch(() => {});

    getCurrentWindow().onResized(async ({ payload }) => {
      if (iconMode) return;
      try {
        const scale = await getCurrentWindow().scaleFactor();
        const logical = payload.toLogical(scale);
        if (logical.width < 100 || logical.height < 100) return; // ignore transient/minimize-adjacent events
        if (dockSide) {
          // Docked: this resize IS a divider drag. Record where the user put the
          // border (physical px, the space the backend works in) and push the
          // partner's edge to follow. Deliberately does NOT touch lastPanelSize
          // — that's the floating size undock() restores.
          dockWidth = Math.round(payload.width);
          saveDockSoon();
          scheduleDockSync();
          verifyDockSoon();
          return;
        }
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
      show_ai_bbox: settingsForm.debug_diagnostics_enabled,
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
      const prevHwnd = sharedApp?.hwnd;
      sharedApp = event.payload;
      maybeShowTargetHint();
      // Re-check on ANY target change, not just a different exe: switching from one
      // Blender to another (5.1 closed, 3.6 opened) keeps exe_name "blender", and the
      // exe-only guard skipped the check entirely (live 2026-07-19). The status call
      // is two local file reads — cheap enough for every target change.
      if (event.payload.exe_name !== prevExe || event.payload.hwnd !== prevHwnd) {
        maybeOfferBlenderAddon();
      }
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

    // The taskbar button's minimize is intercepted in Rust (see
    // capture::intercept_panel_minimize) and arrives here instead, making the taskbar
    // button the same toggle the header button and the Icon hotkey already are:
    // expanded → collapse, collapsed → restore. It never minimizes, which is the
    // state being avoided; and it is never a dead click, which is what a plain
    // "collapse or do nothing" would have made it once already collapsed.
    // A menu that survives the window losing focus is a menu that gets left open
    // behind whatever the user switched to — and while collapsed it would also be
    // holding the window at menu size, so the fish would be the wrong shape when
    // they came back.
    window.addEventListener("blur", () => { if (iconSurface) closeIconSurface(); });
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && iconSurface) { e.preventDefault(); closeIconSurface(); }
    });

    listen("panel:collapse_requested", () => {
      if (iconMode) expandToPanel(); else collapseToIcon();
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

    // A returning Google user gets TWO browser trips from one click: the
    // in-place link is refused ("already linked to another user"), and the
    // fallback opens a fresh sign-in window. Without this the panel just keeps
    // saying it is working while the thing it waits for is a window the user
    // has not noticed. `account.load()` clears the notice on the signed-out →
    // signed-in transition, so it never outlives the flow it describes.
    listen("oauth_second_window", () => {
      account.notice =
        "A second Google window has opened — choose your account there to finish. " +
        "(This Google account already has a Navisual account, so you are being signed in to it.)";
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
    // The tracked window was DESTROYED (a dialog we pointed at was dismissed, or the app
    // closed) — not occluded. Clear the pointer quietly; do NOT show the "bring it to the
    // front" occlusion banner, which is misleading when the target is gone, not hidden.
    listen("target_dismissed", () => {
      pointerOccluded = false;
    });
    // The trusted AI region sits under OUR own panel — the target is likely hidden behind it.
    // We don't move the panel (the user stays in control); the hint ring draws over the panel to
    // show roughly where it is, and this prompts the user to slide the panel aside.
    listen("pointer_behind_panel", () => {
      behindPanel = true;
    });

    lastAppliedModel = providerLabel;
    await addToHistory("system", `Navisual ready — using ${providerLabel}`);
  });

  onDestroy(async () => {
    stopAutopilotPolling();
    await unregisterAll().catch(() => {});
  });
</script>

<!-- Right-click: WebView2's built-in browser menu (Back / Reload / Inspect…)
     breaks the native-app feel and offers nothing a user of this app wants, so it
     is suppressed app-wide — except inside text fields, and on Shift+right-click.
     On the WINDOW, not on <main>: Settings, About, the target picker and the
     lightbox all render OUTSIDE <main> (it has overflow:hidden), so a handler
     there caught the panel body and missed every dialog. Covers icon mode too;
     the fish's own handler still runs and still opens its menu. -->
<svelte:window oncontextmenu={handlePanelContextMenu} />

{#if iconMode}
  <!-- Icon mode: goldfish icon — mousedown starts drag; click expands.
       The ring and the thinking state are here rather than in the panel because
       collapsed is exactly when the panel can't tell you anything: pressing Ctrl+~
       moved the pointer and changed the caption, but the fish itself sat inert,
       which is what made the shortcut feel like it went nowhere. -->
  <div class="icon-shell" class:icon-flip-x={iconFlipX} class:icon-flip-y={iconFlipY}>
  <button
    class="icon-btn"
    class:icon-thinking={phase === "thinking"}
    onclick={handleIconClick}
    oncontextmenu={(e) => e.preventDefault()}
    onpointerdown={handleIconPointerdown}
    onpointerup={handleIconPointerup}
    onpointermove={handleIconPointermove}
    title={iconTitle}
  >
    {#if phase === "thinking"}
      <!-- A short arc chasing its own tail: the press is acknowledged immediately,
           before the answer that will eventually move the ring. -->
      <svg class="icon-ring icon-ring-spin" viewBox="0 0 56 56" aria-hidden="true">
        <circle class="icon-ring-track" cx="28" cy="28" r="25.5" />
        <circle class="icon-ring-arc" cx="28" cy="28" r="25.5" />
      </svg>
    {:else if lastRequestFailed}
      <!-- A failed request reverts `phase` and writes a warning into the history,
           which a collapsed user cannot see. Without this the fish just sits there
           looking idle after e.g. a Gemini API error, and the session appears to
           have quietly stopped. -->
      <svg class="icon-ring" viewBox="0 0 56 56" aria-hidden="true">
        <circle class="icon-ring-failed" cx="28" cy="28" r="25.5" />
      </svg>
      <span class="icon-ask icon-ask-failed">!</span>
    {:else if phase === "needs_input"}
      <!-- Rare — ~3% of first turns in real use once the grounding battery is
           excluded — so it earns no mechanism of its own and reuses the ring slot:
           a full amber ring and a "?" over the fish. Without it a collapsed user is
           simply never told a question is waiting, which is the one state where the
           panel has something to say and no way to say it. -->
      <svg class="icon-ring" viewBox="0 0 56 56" aria-hidden="true">
        <circle class="icon-ring-asking" cx="28" cy="28" r="25.5" />
      </svg>
      <span class="icon-ask">?</span>
    {:else if iconProgress !== null}
      <svg class="icon-ring" viewBox="0 0 56 56" aria-hidden="true">
        <circle class="icon-ring-track" cx="28" cy="28" r="25.5" />
        <circle
          class="icon-ring-value"
          cx="28" cy="28" r="25.5"
          style="stroke-dasharray: {ICON_RING_C}; stroke-dashoffset: {ICON_RING_C * (1 - iconProgress)};"
        />
      </svg>
    {/if}
    <img src="/goldfish.svg" class="icon-fish" alt="Navisual" draggable="false" />
  </button>
  {#if iconSurface === "menu"}
    <!-- Each item carries its shortcut, so the menu TEACHES the keyboard path
         rather than competing with it - the whole reason the collapsed icon exists
         is that Ctrl+~ already drives a session and was never visible here.
         Wrong is deliberately absent: 2 corrections in 574 AI calls and 0 local
         retries in 66 locate traces make it dead pixels. -->
    <div class="icon-menu" role="menu">
      <button class="icon-menu-item" role="menuitem"
        disabled={actionDisabled}
        onclick={() => iconMenuAction(() => nextStep())}>
        <span>→ Next</span>
        {#if settingsForm.hotkey_next}<kbd class="hk-key">{prettyHotkey(settingsForm.hotkey_next)}</kbd>{/if}
      </button>
      <button class="icon-menu-item" role="menuitem" onclick={openIconChat}>
        <span>💬 Chat</span>
      </button>
      <button class="icon-menu-item" role="menuitem"
        onclick={() => iconMenuAction(expandToPanel)}>
        <span>↗ Expand</span>
        {#if settingsForm.hotkey_icon}<kbd class="hk-key">{prettyHotkey(settingsForm.hotkey_icon)}</kbd>{/if}
      </button>
      <button class="icon-menu-item icon-menu-quit" role="menuitem"
        onclick={() => iconMenuAction(closeWindow)}>
        <span>✕ Quit</span>
      </button>
    </div>
  {:else if iconSurface === "hint"}
    <!-- Shown once, unprompted, the first time a step arrives while collapsed. -->
    <div class="icon-hint">
      <div class="icon-hint-title">Navisual is still driving</div>
      <div class="icon-hint-body">
        Press <kbd class="hk-key">{prettyHotkey(settingsForm.hotkey_next)}</kbd> for the next step —
        no need to open the panel.
      </div>
      <button class="icon-hint-got-it" onclick={closeIconSurface}>Got it</button>
    </div>
  {:else if iconSurface === "chat"}
    <!-- Ask something without leaving collapsed mode. Submitting shrinks straight
         back to the fish, whose thinking arc then reports what the answer is doing. -->
    <div class="icon-chat">
      <input
        bind:this={iconChatInput}
        bind:value={iconChatText}
        class="icon-chat-input"
        placeholder={phase === "needs_input" ? "Answer Navisual…" : "Ask a follow-up…"}
        onkeydown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submitIconChat(); }
          else if (e.key === "Escape") { e.preventDefault(); closeIconSurface(); }
        }}
      />
      <div class="icon-chat-row">
        <button class="icon-chat-send" onclick={submitIconChat} disabled={!iconChatText.trim()}>↩ Send</button>
        <button class="icon-chat-cancel" onclick={closeIconSurface}>Esc</button>
      </div>
    </div>
  {/if}
  </div>
{:else}
  <main>
    <!-- Title bar: onmousedown → startDragging() (more reliable than data-tauri-drag-region on WebView2) -->
    <div class="titlebar" role="toolbar" tabindex="-1" onmousedown={handleHeaderMousedown}>
      <!-- The mark, not a dot. This was a 9px orange circle that carried no state
           (the real status light is .status-dot, in the status bar) and said
           nothing -- which became the problem the moment a narrow panel hid the
           wordmark beside it and left the dot standing in for the product name.
           The goldfish says it at any width, so the wordmark below is now cheap
           to drop rather than load-bearing.
           <img src>, never inline <svg>: this WebView2 build squashes an inline
           svg flex child to ~2px, and an externally referenced file is treated as
           an opaque image resource instead. Same path the collapsed icon and the
           conversation label already use.
           The alt carries the name and the visible wordmark is aria-hidden, so
           the product is announced exactly once at every width -- `display: none`
           removes the wordmark from the accessibility tree too, which would
           otherwise leave a narrow panel with no accessible name at all. -->
      <img src="/goldfish.svg" class="header-mark" alt="Navisual" draggable="false" />
      <span class="header-title" aria-hidden="true">Navisual</span>
      <button
        class="header-shared"
        class:header-shared-pinned={pinnedHwnd !== null || fullScreenTarget}
        title={fullScreenTarget ? "Sharing your screen — click to switch target" : pinnedHwnd !== null ? "Target app pinned — click to switch or unpin" : "Target app — click to switch or pin"}
        onmousedown={(e) => e.stopPropagation()}
        onclick={() => openTargetPicker()}
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
             a related treatment just below: the count stays HIDDEN while the
             trial is comfortable — a plain "Free tier" label sits there instead
             so the user always knows which tier they're on — and the count only
             surfaces (reddening) once ≤ 5 remain. A constant meter in the title
             bar reads as a countdown and makes the free experience itself feel
             metered (strategy §4.3); the number earns its place only when it's
             an actual, timely nudge. -->
        <button class="header-balance" onclick={() => openSettings("account")} title="View billing">🪙</button>
      {:else if settingsForm.api_provider === "managed" && billing.freeRemaining !== null && billing.freeRemaining <= 5}
        <button class="header-balance header-balance-low" onclick={() => openSettings("account")} title="Get more requests">{billing.freeRemaining} left</button>
      {:else if settingsForm.api_provider === "managed" && billing.freeRemaining !== null}
        <button class="header-balance header-balance-free" onclick={() => openSettings("account")} title="You're on the free tier — click for billing">Free tier</button>
      {/if}
      {#if pendingUpdate}
        <!-- The version sits in its own span so a narrow panel can drop it and
             keep the arrow: the chip's job is "there is an update", and the
             number is detail the tooltip and About both carry. -->
        <button class="header-update" onclick={() => openAbout("about")} title="Update available — v{pendingUpdate.version}">
          ↑<span class="header-update-version"> {pendingUpdate.version}</span>
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

    <!-- The goal the AI is working toward, and its route overview. Deliberately OUTSIDE
         latest-box: submitTask/nextStep/correction all blank currentInstruction the
         instant "thinking" starts (so the old instruction doesn't linger next to a new
         one being streamed), which used to take this whole dashboard down with it —
         reported live: it vanished on every single "Next"/"Follow up" click, not just
         while genuinely idle. Gated on sessionGoal alone so it survives across the
         entire session, updating in place once each response lands, exactly like the
         "revise, don't erase" goal/plan_outline fields it displays. -->
    <!-- Nav-app "route overview": the goal card is deliberately more prominent than a
         status line — seeing the path forward ahead of time is what builds trust, the
         same reason a map app shows the whole route before turn-by-turn starts. Click
         it to see the plan; the plan itself is expected to change as Navisual learns
         more (it's part of the memory system, not a fixed itinerary computed once). -->
    {#if sessionGoal}
      <button
        class="goal-card"
        onclick={() => (planExpanded = !planExpanded)}
        title={planExpanded ? "Hide the planned route" : "What Navisual thinks you're trying to do — click to see the planned route"}
      >
        <span class="goal-card-icon" aria-hidden="true">🗺️</span>
        <span class="goal-card-body">
          <span class="goal-label">Working on</span>
          <span class="goal-text">{sessionGoal}</span>
          <!-- Collapsed: the list itself isn't visible here, so a compact bar stands
               in for it — "how long is the journey" at a glance. Hidden once expanded,
               since the full highlighted list right below already shows this. -->
          {#if !planExpanded && sessionPlanOutline.length > 0}
            <span class="goal-card-progress">
              <span class="goal-card-progress-track">
                <span class="goal-card-progress-fill" style={`width: ${(sessionPlanCompletedCount / sessionPlanOutline.length) * 100}%`}></span>
              </span>
              <span class="goal-card-progress-label">{Math.min(sessionPlanCompletedCount + 1, sessionPlanOutline.length)} of {sessionPlanOutline.length}</span>
            </span>
          {/if}
        </span>
        <span class="goal-card-chevron" class:goal-card-chevron-open={planExpanded}>›</span>
      </button>
    {/if}

    <!-- Route overview — expands in place under the goal card on click, collapses
         back to the progress bar above on close. No separate "pin" concept: a
         simplification (2026-08-22) over the earlier click-opens-a-modal-popover +
         pin-to-make-it-stay design, which needed two controls to do what one
         expand/collapse toggle already does. -->
    {#if planExpanded && sessionGoal}
      <div class="plan-inline">
        <div class="plan-inline-header">
          <span class="plan-inline-title">🗺️ Planned route</span>
          <button class="plan-inline-close" onclick={() => (planExpanded = false)} title="Close" aria-label="Close">✕</button>
        </div>
        {#if sessionPlanOutline.length > 0}
          <ol class="plan-overview-list">
            {#each sessionPlanOutline as milestone, i (i)}
              <li class:plan-item-done={i < sessionPlanCompletedCount} class:plan-item-current={i === sessionPlanCompletedCount}>
                <span class="plan-item-marker" aria-hidden="true">{i < sessionPlanCompletedCount ? "✓" : i === sessionPlanCompletedCount ? "▸" : ""}</span>
                {milestone}
              </li>
            {/each}
          </ol>
          <p class="plan-overview-footnote">This adapts as Navisual learns more — not a fixed route.</p>
        {:else}
          <p class="plan-overview-empty">No route mapped out yet — it'll appear here once Navisual has a clearer picture of the steps ahead.</p>
        {/if}
      </div>
    {/if}

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
          <!-- Promoted out of the ··· quick-menu (2026-08-20): a real test user never
               found "Clear" hidden behind ···, and the pointer/caption covering the
               screen was exactly what she wanted to dismiss. Always visible here
               instead, right where the thing it clears is showing. -->
          {#if !isThinking}
            {#if isOverlayCleared}
              <button class="clear-toggle-btn" onclick={quickShowScreen} title="Show the pointer and caption again">
                👁 Show
              </button>
            {:else}
              <button class="clear-toggle-btn" onclick={quickClearScreen} title="Hide the pointer and caption so you can see the screen clearly">
                ✕ Clear
              </button>
            {/if}
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
          {#if behindPanel}
            <p class="miss-note">◎ This looks like it's behind this panel — drag the panel aside to reveal the highlighted spot.</p>
          {:else}
            <p class="miss-note">⊘ Pointer unavailable — follow the instruction above</p>
          {/if}
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
        {#if settingsForm.debug_diagnostics_enabled && locateTrace}
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
            {#if entry.role === "ai"}
              <!-- The product's own mark rather than the word "AI": it is what the
                   user is talking to, and at this size a repeated 8-letter word
                   down the column would be both wide and noisy where every other
                   label is one short word. <img>, never inline <svg> — see the
                   header-actions note on this WebView2 build squashing inline SVG
                   flex children to ~2px. -->
              <img src="/goldfish.svg" class="h-label-fish" alt="Navisual" title="Navisual" draggable="false" />
            {:else}
              {entry.role === "user" ? "You"
              : entry.role === "correction" ? "Wrong"
              : entry.role === "error" ? "Error"
              : "·"}
            {/if}
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
          <!-- Disclosure AT the consent point: what the add-on is, what it may read, and
               that it can never act. The banner is too small to carry it, so it links to
               the guide's Blender section (privacy.html §3 is the formal version). -->
          <button
            class="stale-action addon-what"
            onclick={() => openUrl("https://navisualguide.com/docs.html#apps")}
            title="What the add-on does, and what it can't do">What's this?</button>
          <button class="stale-action" onclick={installBlenderAddon}>Install</button>
        {:else if addonPrompt === "installing"}
          <span class="addon-busy">Installing…</span>
        {/if}
        <button
          class="stale-dismiss"
          onclick={() => {
            // Only silence a rejected OFFER, and only for that Blender version —
            // closing the post-install note must not suppress future offers.
            if (addonPrompt === "offer" && addonOfferVersion) {
              addonDismissedFor = [...addonDismissedFor, addonOfferVersion];
            }
            addonPrompt = "hidden";
          }}
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
            // Synchronous so a paste resizes in the same frame it lands — the $effect
            // above also fires, but a tick later, which reads as a visible jump.
            autoGrowTaskInput();
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
        {#if dockSide}
          <button class="qm-btn" onclick={() => { showQuickMenu = false; openTargetPicker("dock"); }}
            title="Give the rest of the screen to an app">
            ⬒ Fill the rest with…
          </button>
          <button class="qm-btn qm-active" onclick={() => undock()}
            title="Float the panel again">
            ⬜ Undock
          </button>
        {:else}
          <button class="qm-btn" onclick={() => dockPanel("left")}
            title="Put Navisual down the left quarter of the screen, full height, and give the rest to one app">
            ◧ Dock left
          </button>
          <button class="qm-btn" onclick={() => dockPanel("right")}
            title="Put Navisual down the right quarter of the screen, full height, and give the rest to one app">
            ◨ Dock right
          </button>
        {/if}
        <button class="qm-btn" class:qm-active={isMuted} onclick={toggleMute}>
          {isMuted ? "🔇 Unmute" : "🔊 Mute"}
        </button>
        <button class="qm-btn" class:qm-active={settingsForm.subtitle_enabled} onclick={quickToggleSubtitle}>
          💬 {settingsForm.subtitle_enabled ? "Caption: on" : "Caption: off"}
        </button>
        {#if settingsForm.session_export_enabled}
          <!-- Developer-gated. The ring buffer behind it runs regardless, so the
               menu item appearing mid-session reveals a session already recorded. -->
          <button class="qm-btn" onclick={() => { showQuickMenu = false; openExport(); }}
            title="Save this session — steps, screenshots and the conversation — to a folder">
            💾 Save this session
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
        title={settingsForm.voice_input_enabled ? (isRecording ? `Stop recording (${prettyHotkey(settingsForm.hotkey_talk)})` : `Voice input (${prettyHotkey(settingsForm.hotkey_talk)})`) : "Enable voice input in Settings → Audio"}>
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
    <div class="target-picker-backdrop" role="presentation" onclick={() => { targetPickerOpen = false; targetPickerMode = "target"; }}></div>
    <div class="target-picker" role="listbox" aria-label={targetPickerMode === "dock" ? "Choose the app to fill the rest of the screen" : "Choose target app"}>
      {#if targetPickerMode === "dock"}
        <!-- Dock mode reuses the same window list with a different verb. No
             Auto-detect and no whole-screen entries: neither names a window to
             put in the space beside the panel. -->
        <div class="target-pick-head">Which app should fill the rest?</div>
      {:else}
        {#if dockSide}
          <div class="target-pick-head">Picking an app also fills the rest of the screen with it</div>
        {/if}
        <button class="target-pick-item" class:target-pick-selected={pinnedHwnd === null && !fullScreenTarget} onclick={() => selectTarget(null)}>
          <span class="target-pick-check">{pinnedHwnd === null && !fullScreenTarget ? "✓" : ""}</span>
          <span class="target-pick-name">Auto-detect</span>
          <span class="target-pick-sub">follow the foreground window</span>
        </button>
      {/if}
      {#each targetWindows as w (w.hwnd)}
        {@const primary = w.title || w.display_name}
        {@const chosen = targetPickerMode === "dock" ? dockPartner === w.hwnd : pinnedHwnd === w.hwnd}
        <button class="target-pick-item" class:target-pick-selected={chosen}
          onclick={() => (targetPickerMode === "dock" ? fillDockPartner(w.hwnd) : selectTarget(w.hwnd))}>
          <span class="target-pick-check">{chosen ? "✓" : ""}</span>
          <!-- Primary = the window title (what the user actually sees on screen);
               subtitle = the friendly app name for identity, when it adds info. -->
          <span class="target-pick-name">{primary.length > 46 ? primary.slice(0, 44) + "…" : primary}</span>
          {#if w.display_name && w.display_name !== primary}
            <span class="target-pick-sub">{w.display_name}</span>
          {/if}
        </button>
      {/each}
      {#if targetPickerMode === "dock"}
        <!-- nothing further: a screen isn't a window to dock beside the panel -->
      {:else if monitors.length > 1}
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
    <button class="target-hint" onclick={() => openTargetPicker()}>
      <span class="target-hint-arrow"></span>
      Click here to select the app you want me to assist with.
    </button>
  {/if}

  <!-- One-time coach mark pointing at the collapse button; clicking it -->
  <!-- collapses the panel directly, same as the button it describes. -->
  {#if showCollapseHint && !iconMode && !showPrivacyDisclosure}
    <button class="collapse-hint" onclick={collapseToIcon}>
      <span class="collapse-hint-arrow"></span>
      In your way? Click here to shrink Navisual to a small floating icon.
    </button>
  {/if}

  <!-- Screenshot lightbox — panel window is temporarily expanded to fit -->
  <!-- Session export. This dialog is the consent step: the ring buffer is
       memory-only until the button at the bottom is pressed. -->
  {#if showExport}
    <!-- Closing is driven from the backdrop by comparing target to currentTarget,
         rather than a stopPropagation handler on the panel. That keeps the panel
         free of a click handler it does not need, which is what the a11y rule is
         actually pointing at. -->
    <div class="modal-backdrop" role="presentation"
      onclick={(e) => { if (e.target === e.currentTarget) showExport = false; }}>
      <div class="export-panel" role="dialog" aria-modal="true" tabindex="-1"
        aria-label="Save this session"
        onkeydown={(e) => { if (e.key === "Escape") showExport = false; }}>
        <div class="export-head">
          <strong>Save this session</strong>
          <button class="export-x" onclick={() => (showExport = false)} title="Close">✕</button>
        </div>

        {#if exportError}
          <p class="export-err">{exportError}</p>
        {/if}

        {#if exportDone}
          <p class="export-ok">
            Saved to<br />
            <!-- The path is the whole point of this screen, and it was previously a
                 dead <code> block you had to retype or hand-select. Now: click to open
                 the folder, or copy it for a terminal. -->
            <button
              class="export-path"
              title="Open this folder"
              onclick={openExportFolder}>{exportDone}</button>
          </p>
          <div class="export-path-actions">
            <button class="btn-ghost" onclick={openExportFolder}>📂 Open folder</button>
            <button class="btn-ghost" onclick={copyExportPath}>
              {exportPathCopied ? "✓ Copied" : "⧉ Copy path"}
            </button>
          </div>
          <p class="export-note">
            <code>steps/</code> holds the untouched screenshots and
            <code>steps-annotated/</code> the marked-up ones, alongside a readable
            <code>session.md</code> and the full record in <code>session.json</code>.
            You can redo the pointer or caption any time with
            <code>tools/annotate-session.ps1</code> — nothing needs re-running.
          </p>
          <p class="export-note">
            Look through the folder before sending it to anyone: the screenshots are
            pictures of your screen.
          </p>
        {:else if exportStatus && exportStatus.empty}
          <p class="export-note">Nothing recorded yet. Run a step or two first.</p>
        {:else if exportStatus}
          <p class="export-note">
            {exportStatus.steps} step{exportStatus.steps === 1 ? "" : "s"} across
            {exportStatus.turns} exchange{exportStatus.turns === 1 ? "" : "s"},
            {exportStatus.frames} with a screenshot{exportStatus.app ? ` · ${exportStatus.app}` : ""}
          </p>

          {#if exportStatus.thin_warning}
            <!-- Advisory only. A thin session still exports: it is a valid record
                 even when it would make a poor article. -->
            <p class="export-thin">{exportStatus.thin_warning}</p>
          {/if}

          <label class="export-field">
            <span>Title</span>
            <input type="text" bind:value={exportTitle} placeholder="What was this session about?" />
          </label>

          <label class="export-field">
            <span>Folder</span>
            <span class="export-dest">
              <input type="text" bind:value={exportDest} spellcheck="false" />
              <button onclick={chooseExportFolder} title="Choose a different folder">Browse…</button>
            </span>
          </label>

          <div class="export-opts">
            <label>
              <input type="checkbox" bind:checked={exportSaveClean} />
              Save the plain screenshots <span class="export-hint">— steps/, untouched</span>
            </label>
            <label>
              <input type="checkbox" bind:checked={exportDrawPointer} />
              Add the pointer <span class="export-hint">— steps-annotated/</span>
            </label>
            <label>
              <input type="checkbox" bind:checked={exportDrawCaption} />
              Add the instruction as a caption <span class="export-hint">— steps-annotated/</span>
            </label>
            <label>
              <input type="checkbox" bind:checked={exportCropToApp} />
              Crop to the app <span class="export-hint">— hides the Navisual panel</span>
            </label>
          </div>
          {#if !exportSaveClean}
            <!-- Worth saying out loud: the annotated copies are derived, the plain
                 ones are not. Turning this off is the one choice here that cannot
                 be undone later without re-running the session. -->
            <p class="export-thin">
              Without the plain screenshots you cannot re-do the pointer or caption later —
              those are rebuilt from the untouched originals.
            </p>
          {/if}

          <div class="export-list">
            {#each exportStatus.detail as d (d.index)}
              <div class="export-row" class:export-redacted={exportRedacted.includes(d.index)}>
                <span class="export-n">{d.index + 1}</span>
                <span class="export-instr" title={d.instruction}>{d.instruction}</span>
                <span class="export-tag" class:export-miss={d.pointer === "miss"}>{d.pointer}</span>
                {#if !d.user_typed}<span class="export-tag export-auto">{d.user_kind}</span>{/if}
                <button class="export-drop" onclick={() => toggleRedact(d.index)}
                  title={exportRedacted.includes(d.index) ? "Include this screenshot" : "Leave this screenshot out"}>
                  {exportRedacted.includes(d.index) ? "restore" : "drop"}
                </button>
              </div>
            {/each}
          </div>

          <button class="export-go" onclick={runExport} disabled={exportBusy}>
            {exportBusy ? "Saving…" : "Save to folder"}
          </button>
        {/if}
      </div>
    </div>
  {/if}

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
          <!-- SINGLE SOURCE OF TRUTH: navisualguide.com/privacy.html
               (repo NavisualGuide.github.io, file privacy.html — its §1 carries a
               matching comment listing these same five facts, so the pointer works
               from either end).
               This list used to restate the policy in full and drifted from it — the
               2026-09-06 audit found it still promising "full-screen needs your
               permission each time" (that consent loop was removed in v0.5.23) and
               pointing at a Pause hotkey that ships unset. Every one of those was a
               volatile SPECIFIC: a count, a key name, a mechanism.

               So this now carries only facts that describe what Navisual IS, which
               change when the product changes rather than when an implementation
               detail does. Numbers, key names and per-feature mechanics live on the
               policy page and nowhere else. It still has to stand alone offline —
               this is a consent gate shown before the first capture, so it cannot be
               a bare link. -->
          <ul style="margin: 0 0 14px 0; padding-left: 18px; color: var(--text-secondary); font-size: 0.92em;">
            <li>It captures the window you point it at — or your whole screen, if you pick that — and sends the picture to the AI provider you choose.</li>
            <li>Screenshots are held in memory. Nothing is written to your disk unless you save it yourself.</li>
            <li><strong>The default free tier uses AI models that may keep your requests — including the screenshot — to train on.</strong> Paid tiers and your own API key don't; Ollama never leaves your machine.</li>
            <li>While guiding, it notes which control you click in that app — the control's name, never its contents. It does not monitor your keyboard.</li>
            <li>Voice input, if you turn it on, sends your audio to Microsoft's speech service.</li>
          </ul>
          <p style="margin: 0 0 14px 0; font-size: 0.85em; color: var(--text-tertiary);">
            The <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>full privacy policy</button>
            is the complete and authoritative version — what is captured, where it goes, what is
            stored, and how to stop it. You can reopen it any time from About.
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
          <!-- Billing merged into Account (2026-09-06). Coins belong to an account,
               both were the only two live views outside the Apply/OK form, and buying
               while signed out already switched from one tab to the other mid-click. -->
          <button class="tab-btn {settingsTab === 'account' ? 'tab-active' : ''}" onclick={() => { settingsTab = "account"; account.load(); refreshBalance(); }}>Account</button>
          <button class="tab-btn {settingsTab === 'screen-guide' ? 'tab-active' : ''}" onclick={() => (settingsTab = "screen-guide")}>Screen Guide</button>
          <button class="tab-btn {settingsTab === 'hotkeys' ? 'tab-active' : ''}" onclick={() => (settingsTab = "hotkeys")}>Hotkeys</button>
          <button class="tab-btn {settingsTab === 'audio' ? 'tab-active' : ''}" onclick={() => (settingsTab = "audio")}>Audio</button>
          {#if settingsForm.developer_mode}
            <button class="tab-btn {settingsTab === 'developer' ? 'tab-active' : ''}" onclick={() => (settingsTab = "developer")}>Developer</button>
          {/if}
        </div>

        <div class="modal-body">
          {#if settingsTab === "account"}
            <!-- Extracted to AccountPanel.svelte + lib/account.svelte.ts (componentization pass,
                 2026-07-13). It renders BillingPanel itself since the 2026-09-06 merge. -->
            <AccountPanel
              provider={settingsForm.api_provider}
              onBuy={buyCoins}
              onRefreshBalance={refreshBalance}
              onSignedOut={() => addToHistory("system", "Signed out — you're back on the free tier.")}
            />
          {:else if settingsTab === "provider"}
            <!-- Provider selector — grouped so it scales as providers + paid tiers grow -->
            <div class="setting-group">
              <label class="setting-label" for="provider-select">Provider</label>
              <select id="provider-select" class="setting-select"
                bind:value={settingsForm.api_provider}
                onchange={handleProviderChange}>
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
                  <!-- Model names must match the relay's TIER_ROUTES (relay/index.ts).
                       All six were stale: the v0.7.12 roster change (2026-08-19) moved
                       every tier onto the GPT-5.6 / Gemini 3.7 generation and this text
                       was never updated, so a paying customer read superseded names for
                       three weeks while deciding what their coins buy. -->
                  {:else if settingsForm.managed_tier === "speed"}
                    GPT-5.6 Luna, falls back to Gemini 3.5 Flash-Lite. Cheapest; good for simple, text-heavy UIs. Coins are bought on the Account tab.
                  {:else if settingsForm.managed_tier === "smart"}
                    GPT-5.6 Terra, falls back to Gemini 3.7 Flash. Reasoning-enabled; the strongest on ambiguous or visually dense screens. Coins are bought on the Account tab.
                  {:else}
                    Gemini 3.7 Flash, falls back to GPT-5.6 Terra. The best all-round default — measured on real sessions at 92% on-target pointing. Coins are bought on the Account tab.
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
                  <option value="claude-sonnet-5">claude-sonnet-5 (recommended)</option>
                  <option value="claude-opus-5">claude-opus-5 (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customAnthropic}
                  <input class="setting-input" type="text" bind:value={settingsForm.anthropic_model}
                    placeholder="e.g. claude-sonnet-5" spellcheck="false" style="margin-top:6px" />
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
                  <option value="gemini-3.7-flash">gemini-3.7-flash (recommended)</option>
                  <!-- 3.8 is listed but NOT the default. It is newer and the family
                       prior is strong, but 3.7 is the one with measured grounding
                       here (100% bbox, 92% hit, median 1px — model-comparison.md),
                       and 3.8's model card says thinking cannot be set below "low",
                       which is the shape that broke forced tool calls on OpenAI's
                       reasoning models. Promote it once it has its own numbers. -->
                  <option value="gemini-3.8-flash">gemini-3.8-flash (newest — untested here)</option>
                  <option value="gemini-2.5-flash-lite">gemini-2.5-flash-lite (fast)</option>
                  <option value="gemini-3.1-pro-preview">gemini-3.1-pro-preview (best quality)</option>
                  <option value="gemini-2.5-flash">gemini-2.5-flash</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customGemini}
                  <input class="setting-input" type="text" bind:value={settingsForm.gemini_model}
                    placeholder="e.g. gemini-3.7-flash" spellcheck="false" style="margin-top:6px" />
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
                  <option value="gpt-5.6-terra">gpt-5.6-terra (recommended)</option>
                  <option value="gpt-5.6-luna">gpt-5.6-luna (fast)</option>
                  <option value="gpt-5.6-sol">gpt-5.6-sol (best quality)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customOpenAI}
                  <input class="setting-input" type="text" bind:value={settingsForm.openai_model}
                    placeholder="e.g. gpt-5.6-terra" spellcheck="false" style="margin-top:6px" />
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
                  <option value="deepseek-v4-flash">deepseek-v4-flash (recommended, text-only)</option>
                  <option value="deepseek-v4-pro">deepseek-v4-pro (best quality, text-only)</option>
                  <option value="deepseek-v4-flash-vision-exp">deepseek-v4-flash-vision-exp (experimental, sees the screen)</option>
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
                  <option value="qwen3.8-max">qwen3.8-max (recommended)</option>
                  <option value="qwen3.7-flash">qwen3.7-flash (fast, cheapest vision pick)</option>
                  <option value="qwen3.5-omni-plus">qwen3.5-omni-plus (multimodal)</option>
                  <option value="__custom__">Custom model…</option>
                </select>
                {#if customQwen}
                  <input class="setting-input" type="text" bind:value={settingsForm.qwen_model}
                    placeholder="e.g. qwen3.8-max" spellcheck="false" style="margin-top:6px" />
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
              <label class="setting-label" for="overlay-color">Pointer color</label>
              <div class="color-row">
                <input id="overlay-color" class="color-picker" type="color" bind:value={settingsForm.overlay_color} />
                <span class="color-hex">{settingsForm.overlay_color}</span>
                <button class="key-toggle" onclick={() => (settingsForm.overlay_color = "#FF6B35")}>Reset</button>
              </div>
            </div>
            <div class="setting-group">
              <label class="setting-label" for="overlay-thickness">
                Pointer thickness — {strokeScale(settingsForm.overlay_thickness).toFixed(2)}×{settingsForm.overlay_thickness === DEFAULT_THICKNESS ? " (default)" : ""}
              </label>
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
              <div class="sensitivity-row">
                <span class="sensitivity-end">Less</span>
                <!-- Slider shows SENSITIVITY (right = more); stored value is autopilot_min_cells
                     (how many of 1024 cells must change — lower = more sensitive), so reverse:
                     min_cells = 46 − slider. Band 6–40 cells ≈ 0.6–4% of the window. -->
                <input
                  class="sensitivity-slider"
                  type="range" min="6" max="40" step="2"
                  value={46 - settingsForm.autopilot_min_cells}
                  oninput={(e) => (settingsForm.autopilot_min_cells = 46 - Number(e.currentTarget.value))}
                  aria-label="Autopilot screen-change sensitivity" />
                <span class="sensitivity-end">More</span>
              </div>
              <p class="setting-hint" style="margin-top:4px">
                Sensitivity of screen-change detection — how much of the screen must change to
                auto-advance. <strong>Less</strong> ignores small changes (typing, minor updates);
                <strong>More</strong> reacts to smaller ones like a dialog opening. The default is a
                good balance.
              </p>
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
            <!-- Developer tab — gated by NAVISUAL_DEV=true.
                 Consolidated 2026-09-04 from seven switches to four. The seven were
                 never seven decisions: nobody wants the locate drawer without the
                 response info beside it, or one JSONL log and not the other. They
                 are grouped by CONSEQUENCE — what turning each one on actually
                 costs you — because that is what the reader is deciding about. -->
            <div class="setting-group">
              <p class="setting-label">Diagnostics on screen</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_diagnostics_enabled} />
                <span>Locate-trace drawer, per-response info line, and the AI's target_bbox on the overlay</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Was three separate switches. Costs nothing and writes nothing — it only changes what is displayed.</p>
            </div>

            <div class="setting-group" style="margin-top:12px;border-top:1px solid rgba(255,255,255,0.07);padding-top:12px">
              <p class="setting-label">Diagnostic logs</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.debug_log_files_enabled} />
                <span>Append every locate and every prompt to <code>locate_log.jsonl</code> / <code>prompt_log.jsonl</code></span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Text only, in %LOCALAPPDATA%\com.navisual.app\. One switch for both: a locate trace without the prompt that caused it answers half a question, and the <code>toolsnalyze-*.ps1</code> scripts read both.</p>

              <label class="toggle-row" style="margin-top:10px">
                <input type="checkbox" bind:checked={settingsForm.debug_screenshot_enabled} />
                <span>Also save screenshots, OCR inputs and per-request prompt text</span>
              </label>
              <p class="stub-hint" style="margin-top:4px"><strong>Kept separate on purpose.</strong> The logs above are text; this writes pictures of whatever is on your screen, a few hundred KB each. Merging them would mean switching on locate diagnostics quietly starts capturing your screen to disk.</p>
              <button class="btn-ghost" style="margin-top:8px;font-size:12px;padding:5px 10px"
                onclick={() => invoke("open_debug_folder").catch(() => {})}>
                📂 Open debug folder
              </button>
            </div>

            <div class="setting-group" style="margin-top:12px;border-top:1px solid rgba(255,255,255,0.07);padding-top:12px">
              <p class="setting-label">Training capture</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.training_capture_enabled} />
                <span>Bank complete training triples (screenshot + prompt + response + outcome) locally</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Saves the exact AI-sent screenshot per request to %LOCALAPPDATA%\com.navisual.app	raining\, records the AI response in prompt_log.jsonl, archives rotated logs instead of deleting them, and mirrors worked/wrong feedback locally — all joined by a per-request id. Local only, never uploaded; exempt from the 7-day debug cleanup. Disk ≈ 100–200 KB per request.</p>
            </div>

            <div class="setting-group" style="margin-top:12px;border-top:1px solid rgba(255,255,255,0.07);padding-top:12px">
              <p class="setting-label">Session export</p>
              <label class="toggle-row">
                <input type="checkbox" bind:checked={settingsForm.session_export_enabled} />
                <span>Show 💾 Save this session in the ··· menu</span>
              </label>
              <p class="stub-hint" style="margin-top:4px">Writes the last 30 steps — screenshots, the conversation, and the locator outcome per step — to a folder you choose. The last 30 steps are <em>always</em> held in memory whether this is on or off, so switching it on mid-session finds the session already recorded rather than starting from empty. Nothing reaches disk until you press Save.</p>
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
            <button
              class="btn-ghost btn-reset"
              class:btn-reset-armed={resetArmed}
              onclick={handleResetClick}
              title="Restores EVERY setting on ALL tabs to its default — not just this tab. Your API keys are kept, but server addresses and model choices are not: a custom or local provider will need its URL and model set again.">
              {resetArmed ? "Click again — resets ALL tabs" : "Reset all settings"}
            </button>
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
          <p class="about-tagline">The AI guides, you decide.</p>
          <p class="about-disclaimer">Navisual uses AI, which can make mistakes. Always verify each suggested action before performing it.</p>
          <div class="about-links">
            <button class="about-link" onclick={() => openUrl("https://navisualguide.com/docs.html")}>User guide</button>
            <!-- The first-run notice is shown once per install and has no reopen path,
                 so without this the policy was unreachable from inside a screen-reading
                 app. It is also what makes the notice's "reopen it any time from About"
                 true. -->
            <button class="about-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>Privacy</button>
            <button class="about-link" onclick={() => openUrl("https://navisualguide.com")}>navisualguide.com</button>
            <button class="about-link" onclick={() => openUrl("https://github.com/NavisualGuide/navisual")}>GitHub</button>
            <button class="about-link" onclick={openFeedbackEmail}>Send feedback</button>
          </div>

          <!-- Update section. A Store build manages updates through the Store, so
               it gets a plain statement of fact instead of a control that would
               either do nothing or violate Store policy. -->
          <div class="about-update">
            {#if isPackaged}
              <span class="update-status">Updates are managed by the Microsoft Store.</span>
            {:else if updateStatus === "downloading"}
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
    --surface-4: #2f2f35;
    --border: rgba(255, 255, 255, 0.08);
    --border-strong: rgba(255, 255, 255, 0.14);
    --text-primary: #f5f5f7;
    --text-secondary: #a1a1aa;
    --text-tertiary: #6b6b73;
    /* Aliases for names that were already in use below but never defined — a
       `var(--accent)` with no fallback resolves to NOTHING, so the balance chip
       was silently inheriting its text colour. */
    --text-muted: #6b6b73;
    --bg-secondary: #1c1c20;
    --accent: #ff6b35;
    --accent-500: #ff6b35;
    --accent-400: #ff8555;
    --accent-600: #e55520;
    --accent-soft: rgba(255, 107, 53, 0.14);
    /* Disabled fills. Dimming a COLOURED button with opacity blends its own fill
       into the surface behind it: --accent-500 at 0.4 over --surface-1 lands on
       rgb(114,55,34), a muddy brown that reads as a dirty version of the enabled
       colour rather than as "unavailable" -- and the white label dims with it,
       so the one part still worth reading is the part that fades. A disabled
       control gets an explicit desaturated pill instead. Grey buttons and
       inputs keep plain opacity; they have no colour to muddy. */
    /* Text ON an accent fill. #ff6b35 is a LIGHT colour -- relative luminance
       0.32 -- so white on it is only **2.8:1**, under every WCAG bar including
       the 3:1 floor for large text, and reported directly by a colour-weak user
       as needing more contrast. Near-black on the same fill is 6.6:1 and leaves
       the brand orange exactly as it is. Darkening the accent until white works
       would need roughly #b8410f, which is a different colour, not a shade --
       so the "on" colour moves instead, which is what every design system that
       ships a warm accent does. Warm-tinted rather than pure black so it reads
       as part of the fill. Do NOT use these on --danger (#ef4444 / #b91c1c):
       those are dark enough that white is already the right pairing. */
    --on-accent: #18110d;
    --on-accent-dim: rgba(24, 17, 13, 0.72);
    --disabled-fill: #2b2b31;
    --disabled-text: #7a7a83;
    --accent-soft-strong: rgba(255, 107, 53, 0.24);
    --success: #22c55e;
    --danger: #ef4444;
    --warning: #f59e0b;
    --info: #0ea5e9;
    /* Radius scale. Cards use --r-md, the shell and modals --r-lg, every
       standalone control is a pill. */
    --r-sm: 8px;
    --r-md: 12px;
    --r-lg: 16px;
    --r-pill: 999px;
    /* Inter is now actually shipped (main.ts imports @fontsource-variable/inter).
       Before, this stack fell through -apple-system (Mac-only) to Segoe UI on
       every Windows machine — which is most of why the panel read as a
       Microsoft app. cv11 = single-storey a. */
    font-family: "Inter Variable", Inter, -apple-system, "Segoe UI", Roboto, sans-serif;
    font-feature-settings: "cv11";
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

  /* The shell fills whatever size the window currently is — ICON_SIZE square
     normally, larger while a menu or chat surface is open. The fish is pinned to
     the corner the window grew AWAY from, which is what keeps it physically still
     on screen while the window changes size around it. */
  .icon-shell {
    position: fixed;
    inset: 0;
  }
  .icon-shell .icon-btn {
    position: absolute;
    inset: auto auto auto 0;
    top: 0;
    width: 56px;
    height: 56px;
  }
  .icon-shell.icon-flip-x .icon-btn { left: auto; right: 0; }
  .icon-shell.icon-flip-y .icon-btn { top: auto; bottom: 0; }

  .icon-menu,
  .icon-chat,
  .icon-hint {
    position: absolute;
    left: 0;
    right: 0;
    top: 56px;
    bottom: 0;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.55);
    padding: 4px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow: hidden;
  }
  /* Grown upward: the surface sits above the fish instead of below it. */
  .icon-shell.icon-flip-y .icon-menu,
  .icon-shell.icon-flip-y .icon-chat,
  .icon-shell.icon-flip-y .icon-hint { top: 0; bottom: 56px; }

  .icon-menu-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    padding: 7px 9px;
    background: none;
    border: none;
    border-radius: var(--r-sm);
    color: var(--text-primary);
    font-size: 12px;
    font-weight: 500;
    text-align: left;
    cursor: pointer;
    white-space: nowrap;
  }
  .icon-menu-item:hover:not(:disabled) { background: var(--surface-3); }
  .icon-menu-item:disabled { opacity: 0.4; cursor: default; }
  .icon-menu-quit:hover { background: rgba(239, 68, 68, 0.18); color: var(--danger); }

  /* needs_input: a full amber ring plus a "?" badge, reusing the ring slot. */
  .icon-ring-asking {
    stroke: var(--warning, #f59e0b);
    animation: icon-ask-pulse 1800ms ease-in-out infinite;
  }
  @keyframes icon-ask-pulse {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.45; }
  }
  .icon-ring-failed {
    stroke: var(--danger, #ef4444);
    animation: icon-ask-pulse 1800ms ease-in-out infinite;
  }
  .icon-ask-failed { background: var(--danger, #ef4444); color: #fff; }

  .icon-ask {
    position: absolute;
    right: 2px;
    bottom: 2px;
    min-width: 15px;
    height: 15px;
    padding: 0 3px;
    box-sizing: border-box;
    border-radius: 8px;
    background: var(--warning, #f59e0b);
    color: #1a1205;
    font-size: 11px;
    font-weight: 800;
    line-height: 15px;
    text-align: center;
    pointer-events: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .icon-ring-asking { animation: none; }
  }

  .icon-hint { padding: 9px 10px; gap: 0; }
  .icon-hint-title {
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent-400, #ff6b35);
    margin-bottom: 5px;
  }
  .icon-hint-body {
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .icon-hint-got-it {
    align-self: flex-end;
    margin-top: auto;
    padding: 4px 10px;
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
  }
  .icon-hint-got-it:hover { background: var(--surface-3); }

  .icon-chat-input {
    width: 100%;
    box-sizing: border-box;
    padding: 6px 8px;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 12px;
    font-family: inherit;
    outline: none;
  }
  .icon-chat-input:focus { border-color: var(--accent-400, #ff6b35); }
  .icon-chat-row {
    display: flex;
    gap: 6px;
    margin-top: 6px;
  }
  .icon-chat-send {
    flex: 1;
    padding: 6px 8px;
    background: var(--accent-500, #ff6b35);
    border: none;
    border-radius: 6px;
    color: var(--on-accent);
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }
  .icon-chat-send:disabled {
    background: var(--disabled-fill);
    color: var(--disabled-text);
    cursor: default;
  }
  .icon-chat-cancel {
    padding: 6px 8px;
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-tertiary);
    font-size: 11px;
    cursor: pointer;
  }

  /* Fill the collapsed window (ICON_SIZE square, transparent) and CENTRE the icon inside it.
     The fixed 64px button in a 56px window couldn't centre — it jammed into the top-left and
     the overflow exposed the corner. Filling + flex-centring makes it robust to the window size. */
  .icon-btn {
    position: fixed;
    inset: 0;
    width: 100%;
    height: 100%;
    background: none;
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    /* Softer/tighter shadow so it isn't clipped by the small window and doesn't read as a
       light halo at the corner; keep it centred (no downward offset). */
    filter: drop-shadow(0 0 6px rgba(255, 107, 53, 0.45));
    transition: filter 160ms ease-out, transform 160ms ease-out;
  }
  .icon-btn:hover {
    filter: drop-shadow(0 0 9px rgba(255, 107, 53, 0.7));
    transform: scale(1.06);
  }
  /* Sized to leave a small margin inside the window so the rounded icon centres cleanly and
     the shadow has room. 48/56 ≈ the SVG's own corner ratio (rx 112/512) → radius ≈ 11px. */
  /* 44, not 48: the ring orbits at r=25.5 inside a 56px window, so a 48px fish
     leaves ~1.5px and the ring cut across its rounded corners (seen live). 44
     gives the ring a clear 2px lane and is indistinguishable at a glance. */
  .icon-fish {
    display: block;
    width: 44px;
    height: 44px;
    border-radius: 10px;
    pointer-events: none;
    user-select: none;
  }

  /* Ring sits in the 4px gap between the 48px fish and the 56px window, so it
     never covers the artwork. Rotated so 0% starts at 12 o'clock. */
  .icon-ring {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    transform: rotate(-90deg);
    pointer-events: none;
    overflow: visible;
  }
  .icon-ring circle {
    fill: none;
    stroke-width: 3;
    stroke-linecap: round;
  }
  /* The track is load-bearing, not decoration. Progress is completed/total, so a
     task on its first step is legitimately 0% — and a 0% value arc is
     indistinguishable from no ring at all, at exactly the moment someone
     collapses. The visible track is what says "there IS a ring, and you are at
     the start of it" rather than "this feature is missing". */
  .icon-ring-track { stroke: rgba(255, 255, 255, 0.26); }
  .icon-ring-value {
    stroke: var(--accent-400, #ff6b35);
    /* The whole point of the ring: a keypress visibly moves it. Without the
       transition the arc teleports and the press still reads as nothing. */
    transition: stroke-dashoffset 320ms cubic-bezier(0.4, 0, 0.2, 1);
  }
  /* A short arc chasing its tail while an AI call is in flight. */
  .icon-ring-arc {
    stroke: var(--accent-400, #ff6b35);
    stroke-dasharray: 40 120;
    transform-origin: 50% 50%;
    animation: icon-ring-spin 900ms linear infinite;
  }
  @keyframes icon-ring-spin {
    to { transform: rotate(360deg); }
  }
  /* A slow breath on the fish itself, so the thinking state reads even at a glance
     from the far side of the screen where the 2.5px ring may not. */
  .icon-btn.icon-thinking .icon-fish {
    animation: icon-fish-breathe 1600ms ease-in-out infinite;
  }
  @keyframes icon-fish-breathe {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.72; transform: scale(0.94); }
  }
  @media (prefers-reduced-motion: reduce) {
    .icon-ring-arc { animation-duration: 2400ms; }
    .icon-btn.icon-thinking .icon-fish { animation: none; opacity: 0.8; }
    .icon-ring-value { transition: none; }
  }

  /* ── Panel ──────────────────────────────────────── */

  main {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
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
    padding: 12px 14px 8px;
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

  .header-mark {
    display: block;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    flex-shrink: 0;
    user-select: none;
  }

  .header-title {
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -0.01em;
    flex-shrink: 0;
  }

  .header-balance {
    font-size: 11px;
    font-weight: 500;
    color: var(--accent-400);
    font-family: inherit;
    background: var(--accent-soft);
    border: none;
    border-radius: var(--r-pill);
    padding: 3px 9px;
  }
  .header-balance:hover {
    background: var(--accent-soft-strong);
  }
  .header-balance-low {
    color: #ff4040;
    background: rgba(255, 64, 64, 0.15);
  }
  .header-balance-low:hover {
    background: rgba(255, 64, 64, 0.25);
  }
  /* Free-tier label while the count is hidden: informational, not a nudge —
     muted, no accent tint, so it reads as a status not a call to action. */
  .header-balance-free {
    color: var(--text-tertiary, #8a8a8a);
    background: rgba(255, 255, 255, 0.06);
    font-family: inherit;
  }
  .header-balance-free:hover {
    background: rgba(255, 255, 255, 0.12);
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
    padding: 2px 8px;
    border-radius: var(--r-pill);
  }

  /* Phase 0.2: "Shared: <App>" indicator chip. */
  .header-shared {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 500;
    color: var(--text-secondary);
    background: var(--surface-3);
    border: 1px solid transparent;
    padding: 3px 9px 3px 8px;
    border-radius: var(--r-pill);
    flex-shrink: 1;
    /* A FLOOR, not min-width: 0 -- reported live as "the switch app is hidden
       here" on a 360px docked panel with an update pending.
       This chip was the only item in the titlebar that could shrink to nothing,
       so it absorbed every pixel of shortfall while .header-actions (126px of
       icons), the wordmark and the update chip all refused to give any. Measured
       at 360px with an update pending: the chip rendered 19px wide against 46px
       of its own chrome (8+9 padding, 6px dot, two 6px gaps, 11px caret), so
       only the dot survived the overflow clip. At 400px it was still 45px --
       label zero. The user lost both the name of the app being guided and the
       only always-visible way to switch it.
       78px keeps roughly five characters plus the ellipsis, which is enough to
       name the app. Below that the media queries under this rule give the space
       back by dropping things that are decoration. */
    min-width: 78px;
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out;
  }

  /* ── Titlebar priority under pressure ──────────────────────────────────────
     Something has to give in a 360px titlebar, and the order is the point.
     Keep: the four action buttons (no other route to Quit or Settings), and the
     target chip (what Navisual is looking at, and the only switch control).
     Give up, in this order: the wordmark, then the update chip's version number,
     then the informational free-tier label. Each is something the user either
     already knows or can read one click away; the chip is neither. */

  /* Detail goes before identity. "↑ 0.7.22" (61px with its margin) becomes a
     bare "↑" -- still a visible nudge, still clickable, and the version is in
     its tooltip and in About. The "Free tier" label goes with it: it states a
     thing Settings also states. The reddening "N left" chip is deliberately NOT
     hidden -- that one is timely and actionable, and is the whole reason the
     count surfaces at all. */
  @media (max-width: 460px) {
    .header-update-version { display: none; }
    .header-balance-free { display: none; }
  }

  /* The wordmark is last, and only at widths where nothing else is left to give
     -- on instruction, over the update chip. It costs little by the time it
     goes: .header-mark is still there, and a goldfish is not an ambiguous dot.
     The app chip beside it keeps its label at every width, which is the point:
     the panel should never stop saying which app it is guiding. */
  @media (max-width: 410px) {
    .header-title { display: none; }
  }
  .header-shared:hover { background: var(--surface-4); color: var(--text-primary); }
  .header-shared-pinned { background: var(--accent-soft); color: var(--accent-400); }
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
    border: 1px solid var(--border-strong);
    border-radius: var(--r-md);
    padding: 10px 12px;
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
    border-color: var(--accent-500);
  }
  .target-hint-arrow {
    position: absolute;
    top: -5px;
    left: 96px;
    width: 8px;
    height: 8px;
    transform: rotate(45deg);
    background: var(--surface-2);
    border-left: 1px solid var(--border-strong);
    border-top: 1px solid var(--border-strong);
  }
  /* One-time coach mark anchored under the header's collapse button (2nd
     icon from the right, before Close). Same treatment as .target-hint. */
  .collapse-hint {
    position: fixed;
    top: 38px;
    right: 8px;
    max-width: 220px;
    text-align: left;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-md);
    padding: 10px 12px;
    font-size: 11.5px;
    font-weight: 400;
    line-height: 1.45;
    color: var(--text-secondary);
    z-index: 997;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    cursor: pointer;
  }
  .collapse-hint:hover {
    color: var(--text-primary);
    border-color: var(--accent-500);
  }
  .collapse-hint-arrow {
    position: absolute;
    top: -5px;
    right: 40px;
    width: 8px;
    height: 8px;
    transform: rotate(45deg);
    background: var(--surface-2);
    border-left: 1px solid var(--border-strong);
    border-top: 1px solid var(--border-strong);
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
    border-radius: var(--r-md);
    padding: 6px;
    z-index: 999;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.55);
  }
  /* Dock mode has no "Auto-detect" row to lead with, so the list needs a line
     saying what picking one of these will do. */
  .target-pick-head {
    padding: 6px 8px 7px;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-secondary);
    border-bottom: 1px solid var(--border);
    margin-bottom: 3px;
  }
  .target-pick-item {
    display: grid;
    grid-template-columns: 14px 1fr;
    grid-template-rows: auto auto;
    align-items: center;
    column-gap: 6px;
    width: 100%;
    padding: 6px 8px;
    border-radius: var(--r-sm);
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
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: var(--r-sm);
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
    margin: 6px 12px 0;
    padding: 12px 14px 12px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    flex-shrink: 0;
  }

  .latest-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }

  /* The goal card — deliberately more significant than a status line (was
     .goal-line: a thin baseline-aligned row that read as metadata, easy to
     miss entirely). Clickable: opens the plan-overview popover. */
  .goal-card {
    display: flex;
    align-items: center;
    gap: 10px;
    width: auto;
    align-self: stretch;
    margin: 6px 12px 0;
    padding: 10px 12px;
    background: var(--surface-2);
    border: none;
    border-radius: var(--r-md);
    color: inherit;
    font-family: inherit;
    text-align: left;
    cursor: pointer;
    transition: background 0.12s;
  }
  .goal-card:hover {
    background: var(--surface-3);
  }
  .goal-card-icon {
    flex-shrink: 0;
    font-size: 16px;
    line-height: 1;
  }
  .goal-card-body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }
  .goal-card-chevron {
    flex-shrink: 0;
    font-size: 18px;
    color: var(--text-tertiary);
    transition: transform 0.15s;
  }
  .goal-card-chevron-open {
    transform: rotate(90deg);
  }
  .goal-label {
    flex: 0 0 auto;
    font-size: 11px;
    font-weight: 500;
    color: var(--text-tertiary);
  }
  .goal-text {
    color: var(--text-primary);
    font-size: 13.5px;
    font-weight: 600;
    line-height: 1.35;
    letter-spacing: -0.005em;
    /* Long goals wrap rather than truncate — a clipped goal is exactly as unverifiable
       as no goal, which would defeat showing it. */
    overflow-wrap: anywhere;
  }
  /* "How long is the journey" at a glance, unpinned only — the pinned inline card
     shows the same information as a highlighted list instead. */
  .goal-card-progress {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 4px;
  }
  .goal-card-progress-track {
    flex: 1;
    height: 3px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.08);
    overflow: hidden;
  }
  .goal-card-progress-fill {
    display: block;
    height: 100%;
    background: var(--accent-500, #ff6b35);
    border-radius: 2px;
    transition: width 0.2s ease-out;
  }
  .goal-card-progress-label {
    flex-shrink: 0;
    font-size: 0.68em;
    font-weight: 600;
    color: var(--text-tertiary);
    font-variant-numeric: tabular-nums;
  }

  /* Route overview — the map-app "whole route" list, shown inline under the
     goal card once expanded. */
  .plan-overview-list {
    margin: 0;
    padding-left: 20px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    line-height: 1.4;
    color: var(--text-primary);
  }
  /* Progress highlight — "where we are" on the route, per the user's request that
     pinning the plan should show current position, not just a static list. */
  .plan-item-marker {
    display: inline-block;
    width: 14px;
    font-weight: 700;
  }
  .plan-item-done {
    color: var(--text-tertiary);
  }
  .plan-item-done .plan-item-marker {
    color: var(--accent-500, #ff6b35);
  }
  .plan-item-current {
    color: var(--text-primary);
    font-weight: 700;
  }
  .plan-item-current .plan-item-marker {
    color: var(--accent-500, #ff6b35);
  }
  .plan-overview-footnote {
    margin: 10px 0 0;
    font-size: 11px;
    color: var(--text-tertiary);
    line-height: 1.4;
  }
  .plan-overview-empty {
    margin: 4px 0 0;
    font-size: 12.5px;
    color: var(--text-secondary);
    line-height: 1.5;
  }
  /* Route overview, expanded in place — normal document flow, right under the
     goal card, instead of a floating modal. */
  .plan-inline {
    margin: 6px 12px 0;
    padding: 12px 14px;
    background: var(--surface-2);
    border: none;
    border-radius: var(--r-md);
  }
  .plan-inline-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .plan-inline-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
  }
  .plan-inline-close {
    background: none;
    border: none;
    color: var(--text-tertiary);
    font-size: 12px;
    cursor: pointer;
    padding: 2px 4px;
  }
  .plan-inline-close:hover { color: var(--text-primary); }
  .step-counter {
    font-size: 11px;
    font-weight: 500;
    color: var(--text-tertiary);
    flex-shrink: 0;
  }

  /* Promoted out of the ··· quick-menu — always visible whenever there's a
     pointer/caption on screen to hide or bring back. */
  .clear-toggle-btn {
    margin-left: auto;
    flex-shrink: 0;
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-pill);
    font-size: 11px;
    font-weight: 500;
    padding: 3px 10px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s;
  }
  .clear-toggle-btn:hover {
    background: var(--surface-3);
    color: var(--text-primary);
  }

  .latest-text {
    font-size: 15px;
    font-weight: 500;
    line-height: 1.45;
    letter-spacing: -0.005em;
    color: var(--text-primary);
    margin: 0;
    /* A very long answer (max seen: 1,119 chars) scrolls inside the card rather
       than pushing the conversation and the input off the bottom of the panel. */
    max-height: 38vh;
    overflow-y: auto;
  }

  .miss-note {
    font-size: 11px;
    color: var(--text-muted, #6b7280);
    margin: 4px 0 0;
  }

  /* ── Feedback: mark-wrong footer + reason chips ───── */

  .wrong-footer {
    margin-top: 10px;
  }
  .wrong-btn {
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-pill);
    font-size: 12px;
    font-weight: 500;
    padding: 5px 12px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
  }
  .wrong-btn:hover { background: rgba(239, 68, 68, 0.12); color: var(--danger); border-color: rgba(239, 68, 68, 0.35); }

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
    background: var(--surface-3);
    color: var(--text-secondary);
    border: 1px solid transparent;
    border-radius: var(--r-pill);
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
    border-radius: var(--r-sm);
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
  /* Post-install instructions are three steps — let them wrap to a readable block
     rather than truncating into one cramped line. */
  .addon-banner .stale-text { line-height: 1.45; }
  .addon-banner .stale-action {
    border-color: rgba(140, 175, 255, 0.5);
    color: #a8c2ff;
  }
  /* Secondary to Install — informational, not the call to action. */
  .addon-banner .stale-action.addon-what {
    border-color: transparent;
    color: var(--text-tertiary, #8a8a8a);
    text-decoration: underline;
    padding-left: 2px;
    padding-right: 2px;
  }
  .addon-banner .stale-action.addon-what:hover { color: #a8c2ff; }
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
    padding: 10px 12px 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }

  .history::-webkit-scrollbar { width: 4px; }
  .history::-webkit-scrollbar-track { background: transparent; }
  .history::-webkit-scrollbar-thumb { background: var(--surface-3); border-radius: 2px; }

  .h-entry {
    position: relative;
    display: flex;
    gap: 8px;
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
    border-radius: var(--r-sm);
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
    font-weight: 600;
    font-size: 10px;
    flex-shrink: 0;
    color: var(--text-tertiary);
  }

  .h-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    max-width: 86%;
  }

  .h-text { color: var(--text-secondary); word-break: break-word; }
  .h-meta { font-size: 11px; color: var(--text-tertiary); font-family: "JetBrains Mono", ui-monospace, monospace; }

  /* Sized to sit on the same baseline as the text labels beside it, and pinned
     to the right of the 34px column like they are. */
  .h-label-fish {
    width: 20px;
    height: 20px;
    display: block;
    border-radius: 6px;
    /* Sit on the first line of the message, not at the bottom of a paragraph. */
    margin-top: 0;
  }

  /* Who said what, shown the way every messaging app the user already knows
     shows it: YOU are a filled bubble on the right, NAVISUAL is plain text on
     the left beside its mark, and system notes sit centred and quiet between.
     Position and fill are the marker, so the text label becomes screen-reader
     only rather than a caps tag in a gutter. (Redesign 2026-09-07.) */
  .h-user { flex-direction: row-reverse; }
  .h-user .h-label,
  .h-correction .h-label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  .h-user .h-body {
    background: var(--accent-500);
    padding: 8px 12px;
    border-radius: 16px 16px 4px 16px;
  }
  .h-user .h-text { color: var(--on-accent); }
  .h-user .h-meta { color: var(--on-accent-dim); }

  .h-ai .h-text  { color: var(--text-primary); }

  .h-correction { flex-direction: row-reverse; }
  .h-correction .h-body {
    background: rgba(245, 158, 11, 0.16);
    padding: 8px 12px;
    border-radius: 16px 16px 4px 16px;
  }
  .h-correction .h-text { color: var(--warning); }

  .h-system, .h-error, .h-thinking { justify-content: center; }
  .h-system .h-label, .h-error .h-label, .h-thinking .h-label { display: none; }
  .h-system .h-body, .h-error .h-body { max-width: 100%; align-items: center; }
  .h-system .h-text { color: var(--text-tertiary); font-size: 12px; text-align: center; }
  .h-error .h-text  { color: var(--danger); font-size: 12px; text-align: center; }
  .h-thinking .h-text { color: var(--text-tertiary); font-size: 12px; }

  @keyframes thinking-fade {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }
  .thinking-dots { animation: thinking-fade 1.2s ease-in-out infinite; }

  /* ── Task input ──────────────────────────────────── */

  /* ── Badge variants ──────────────────────────────── */
  .badge-clip {
    background: rgba(14, 165, 233, 0.14);
    color: var(--info);
    border: none;
    font-size: 10.5px;
    padding: 2px 8px;
    border-radius: var(--r-pill);
    font-weight: 500;
    flex-shrink: 0;
  }

  .task-section {
    padding: 8px 12px 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex-shrink: 0;
  }

  .input-hint {
    font-size: 11px;
    color: var(--text-tertiary);
    padding: 0 2px;
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
    border-radius: var(--r-md);
    border: 1px solid var(--border);
    background: var(--surface-2);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
  }
  .suggest-item {
    text-align: left;
    font-family: inherit;
    font-size: 12px;
    padding: 6px 8px;
    border-radius: var(--r-sm);
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
    font-size: 13.5px;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: var(--text-primary);
    border: 1px solid transparent;
    outline: none;
    resize: none;
    line-height: 1.5;
    /* Auto-grow (autoGrowTaskInput) sets an inline height; this clamps it. The cap is
       in vh, which inside the WebView means a share of the PANEL, so it adapts when the
       user resizes the window instead of a fixed pixel guess. Past the cap the box
       scrolls rather than pushing the buttons off the bottom. */
    max-height: 32vh;
    overflow-y: auto;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }
  textarea:focus { border-color: var(--accent-500); box-shadow: 0 0 0 3px var(--accent-soft); }
  textarea:disabled { opacity: 0.45; }

  /* ── Action row ──────────────────────────────────── */

  .action-row {
    display: flex;
    gap: 6px;
    padding: 0 12px 10px;
    flex-shrink: 0;
  }

  .btn-action {
    flex: 1;
    padding: 8px 6px;
    border-radius: var(--r-pill);
    font-size: 12.5px;
    font-weight: 600;
    cursor: pointer;
    border: 1px solid transparent;
    background: var(--surface-3);
    color: var(--text-secondary);
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out, opacity 120ms ease-out;
  }
  .btn-action:disabled { opacity: 0.35; cursor: not-allowed; }

  .btn-next {
    background: var(--accent-soft);
    color: var(--accent-400);
  }
  .btn-next:not(:disabled):hover { background: var(--accent-soft-strong); }

  .btn-more {
    flex: 0 0 34px;
    padding: 8px 0;
    letter-spacing: 0.12em;
    font-size: 11px;
  }
  .btn-more:hover { background: var(--surface-4); color: var(--text-primary); }

  .btn-more-open {
    background: var(--surface-4) !important;
    color: var(--text-primary) !important;
  }

  .btn-mic {
    flex: 0 0 34px;
    padding: 8px 0;
    font-size: 13px;
  }
  .btn-mic:hover:not(:disabled) { background: var(--surface-4); color: var(--text-primary); }
  .btn-mic-active {
    background: rgba(239, 68, 68, 0.18) !important;
    border-color: rgba(239, 68, 68, 0.35) !important;
    animation: pulse 0.9s ease-in-out infinite;
  }

  /* Autopilot ON lights up in the accent; OFF is the same quiet pill as its
     neighbours. One accent doing one job, instead of green-vs-amber. */
  .btn-pause {
    background: var(--accent-soft);
    color: var(--accent-400);
  }
  .btn-pause:not(:disabled):hover { background: var(--accent-soft-strong); }

  .btn-resume {
    background: var(--surface-3);
    color: var(--text-secondary);
  }
  .btn-resume:hover { background: var(--surface-4); color: var(--text-primary); }

  .btn-new {
    background: var(--surface-3);
    color: var(--text-secondary);
  }
  .btn-new:hover { background: var(--surface-4); color: var(--text-primary); }

  /* ── Quick-action menu ───────────────────────────── */

  /* ── Session export dialog ─────────────────────────────────────────────── */
  .export-panel {
    width: min(560px, 94vw);
    max-height: 86vh;
    overflow-y: auto;
    background: var(--surface, #1b1b1f);
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 10px;
    padding: 16px 18px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .export-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 15px;
  }
  .export-x {
    background: none;
    border: 0;
    color: var(--text-tertiary, #8a8a92);
    cursor: pointer;
    font-size: 14px;
  }
  .export-note { font-size: 12.5px; color: var(--text-secondary, #a0a0a8); line-height: 1.5; margin: 0; }
  .export-err { font-size: 12.5px; color: #ff6b6b; margin: 0; }
  .export-ok { font-size: 12.5px; margin: 0; line-height: 1.6; }
  /* The path reads as the monospace block it always was, but is now a control:
     click to open the folder. Kept full-width and wrapping — truncating the one
     string the screen exists to give you would be a strange saving. */
  .export-path {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    padding: 2px 0;
    margin: 0;
    font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
    font-size: 11.5px;
    line-height: 1.45;
    word-break: break-all;
    color: var(--accent-400, #ff8555);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .export-path:hover { color: var(--accent-500, #ff6b35); }
  .export-path-actions {
    display: flex;
    gap: 8px;
    margin: 8px 0 4px;
  }
  .export-path-actions .btn-ghost { flex: 1; padding: 6px 8px; font-size: 12px; }
  /* Advisory, not a blocker — deliberately not styled as an error. */
  .export-thin {
    font-size: 12px;
    line-height: 1.5;
    margin: 0;
    padding: 8px 10px;
    border-left: 3px solid #d19a34;
    background: rgba(209, 154, 52, 0.08);
    color: var(--text-secondary, #a0a0a8);
  }
  .export-field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; }
  .export-field input[type="text"] {
    width: 100%;
    box-sizing: border-box;
    padding: 6px 8px;
    font-size: 12.5px;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    background: rgba(0, 0, 0, 0.25);
    color: inherit;
  }
  .export-dest { display: flex; gap: 6px; }
  .export-dest button {
    flex: 0 0 auto;
    padding: 6px 10px;
    font-size: 12px;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    background: rgba(255, 255, 255, 0.05);
    color: inherit;
    cursor: pointer;
  }
  .export-opts { display: flex; flex-direction: column; gap: 6px; font-size: 12px; }
  .export-opts label { display: flex; align-items: baseline; gap: 6px; }
  /* Names the folder each option writes to, so the checkbox and the result on
     disk are obviously the same thing. */
  .export-hint { color: var(--text-tertiary, #8a8a92); font-size: 11px; }
  .export-list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 6px;
    max-height: 220px;
    overflow-y: auto;
  }
  .export-row {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 8px;
    font-size: 11.5px;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.05));
  }
  .export-row:last-child { border-bottom: none; }
  .export-redacted { opacity: 0.4; text-decoration: line-through; }
  .export-n { flex: 0 0 18px; color: var(--text-tertiary, #8a8a92); }
  .export-instr {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .export-tag {
    flex: 0 0 auto;
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.07);
    color: var(--text-tertiary, #8a8a92);
  }
  /* A miss is the row worth noticing: it is the one with no pointer on it. */
  .export-miss { background: rgba(209, 154, 52, 0.18); color: #e0b25a; }
  .export-auto { background: rgba(120, 160, 255, 0.15); color: #8fb0ff; }
  .export-drop {
    flex: 0 0 auto;
    background: none;
    border: 0;
    color: var(--text-tertiary, #8a8a92);
    cursor: pointer;
    font-size: 11px;
    text-decoration: underline;
  }
  .export-go {
    margin-top: 2px;
    padding: 8px;
    font-size: 13px;
    font-weight: 600;
    border-radius: 7px;
    border: 0;
    background: var(--accent, #ff6b35);
    color: var(--on-accent);
    cursor: pointer;
  }
  .export-go:disabled {
    background: var(--disabled-fill);
    color: var(--disabled-text);
    cursor: default;
  }

  .quick-menu {
    display: flex;
    /* WRAP, or the last items are silently clipped. A flex item will not shrink
       below its label's intrinsic width, so this row overflows the panel once it
       holds enough entries — and a docked panel is only a quarter of the screen.
       Live report: with the panel docked and session export on, the menu held six
       items and "Save this session" sat off the right edge, present in the DOM and
       unreachable. Losing actions in this menu is a recurring fault here (it is why
       Clear and Wrong were promoted out of it), so the row grows downward instead. */
    flex-wrap: wrap;
    gap: 6px;
    padding: 0 12px 8px;
    flex-shrink: 0;
  }

  .qm-btn {
    flex: 1;
    padding: 7px 10px;
    border-radius: var(--r-pill);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    background: var(--surface-3);
    color: var(--text-secondary);
    font-family: inherit;
    transition: background 120ms ease-out, color 120ms ease-out;
    white-space: nowrap;
  }
  .qm-btn:hover:not(:disabled) { background: var(--surface-4); color: var(--text-primary); }
  .qm-btn:disabled { opacity: 0.35; cursor: not-allowed; }

  .qm-active {
    background: var(--accent-soft) !important;
    color: var(--accent-400) !important;
  }

  /* ── Footer ──────────────────────────────────────── */

  footer {
    padding: 4px 14px 10px;
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
    background: rgba(255, 255, 255, 0.08);
    border: none;
    border-radius: 5px;
    padding: 3px 6px;
  }
  .hk-none {
    font-size: 10px;
    color: var(--text-tertiary);
    font-style: italic;
    opacity: 0.7;
  }
  .hk-label {
    font-size: 10.5px;
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
    padding: 8px 14px;
    border-radius: var(--r-pill);
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 120ms ease-out, border-color 120ms ease-out, color 120ms ease-out;
  }

  :global(.btn-primary) {
    background: var(--accent-500);
    color: var(--on-accent);
    font-weight: 600;
    border-color: transparent;
  }
  :global(.btn-primary:hover:not(:disabled)) { background: var(--accent-400); }
  :global(.btn-primary:active:not(:disabled)) { background: var(--accent-600); }
  :global(.btn-primary:disabled) {
    background: var(--disabled-fill);
    color: var(--disabled-text);
    cursor: not-allowed;
  }

  :global(.btn-ghost) {
    background: var(--surface-3);
    color: var(--text-primary);
    border-color: transparent;
  }
  :global(.btn-ghost:hover) { background: var(--surface-4); }

  :global(.btn-danger) {
    background: #b91c1c;
    color: #fff;
    border-color: transparent;
  }
  :global(.btn-danger:hover:not(:disabled)) { background: #dc2626; }
  :global(.btn-danger:disabled) {
    background: var(--disabled-fill);
    color: var(--disabled-text);
    cursor: not-allowed;
  }

  /* ── Account tab ─────────────────────────────────── */
  :global(.acct-error) { color: #f87171; }
  :global(.acct-notice) { color: var(--accent-400); }
  :global(.acct-sep) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 14px 0;
  }
  /* A labelled divider: the same rule as .acct-sep with a word sitting on it.
     Binds "Continue with Google" to the sign-in form above rather than to the
     Billing block below. */
  :global(.acct-or) {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 14px 0;
    color: var(--text-tertiary);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  :global(.acct-or)::before,
  :global(.acct-or)::after {
    content: "";
    flex: 1;
    border-top: 1px solid var(--border);
  }
  /* Section heading for the Billing block merged into the Account tab. Keeps the
     word "Billing" on screen even though it left the tab strip, so anyone
     eye-scanning for it still finds it. */
  :global(.acct-billing-heading) {
    display: block;
    margin: 0 0 10px;
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
    background: rgba(0, 0, 0, 0.5);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    border-radius: var(--r-lg);
  }

  :global(.modal) {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
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
    padding: 14px 16px 6px;
    flex-shrink: 0;
  }
  :global(.modal-title) {
    font-size: 15px;
    font-weight: 600;
    letter-spacing: -0.01em;
    flex: 1;
  }

  /* A row of pills, every one of them visible.
     Underlined tabs marked which one was ACTIVE but nothing marked the row as a
     set of choices, so it did not read as tabs. A tray with only the selected
     item filled fixed half of that and left the other half: the four unselected
     entries were still bare text on a background, which is the part that was
     reported second. Every tab is a pill now -- filled, hairlined, and shaped
     like the app's other standalone controls (the redesign's own rule: every
     standalone control is a pill) -- so the row is legible as a control before
     you know which one is on. The tray went with it; it was grouping items that
     now group themselves, and a container around containers is one layer too
     many. Content-sized and centred rather than stretched, because six tabs
     cannot fit a 400px modal at any sane size and a lone full-width "Developer"
     bar on the second row is a worse answer than a centred chip. */
  .modal-tabs {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 4px;
    /* Settings' body adds 14px of its own and About's adds 24px. 12px at the
       sides rather than the body's 16px: see the budget on .tab-btn's padding. */
    margin: 2px 12px 8px;
    flex-shrink: 0;
  }
  .tab-btn {
    /* Sized to its own label, never stretched and never squeezed: `flex: 1` is
       flex-basis 0, which divides the row into equal columns and ignores what is
       written on them -- that is what broke "Screen Guide" across two lines once
       the Developer tab made it six. With `nowrap` the label is the item's
       min-content width, so a row that cannot hold them all wraps WHOLE TABS. */
    flex: 0 0 auto;
    white-space: nowrap;
    /* 8px, and the row is a hair's breadth from wrapping -- so this is measured,
       not chosen. Bundled Inter at 500/12.5px (canvas measureText, 2026-09-07):
       Provider 50.2, Account 50.0, Screen Guide 80.5, Hotkeys 48.9, Audio 34.8,
       Developer 61.0. Five tabs = 265 + 5x16 padding + 4x4 gaps = 361px into the
       366px a 400px modal leaves at 12px side margins. 12px padding reads better
       as a pill and comes to 401px -- it would have wrapped the row for every
       NON-developer, which is the whole population this fits for. Six tabs are
       442px and wrap whatever we do; that is the second row, and it is the price
       of a tab named "Screen Guide" rather than a reason to rename it.
       Anything that lengthens a label or adds a tab needs re-measuring. */
    padding: 6px 8px;
    font-size: 12.5px;
    font-weight: 500;
    background: var(--surface-3);
    color: var(--text-secondary);
    border: none;
    box-shadow: inset 0 0 0 1px var(--border);
    border-radius: var(--r-pill);
    cursor: pointer;
    transition: background 120ms ease-out, color 120ms ease-out, box-shadow 120ms ease-out;
  }
  .tab-btn:hover:not(.tab-active) {
    background: var(--surface-4);
    color: var(--text-primary);
  }
  .tab-active {
    /* Four cues and only one is colour: a warmer fill, a BRIGHT ring (the accent
       reads 4.2:1 against the resting hairline on luminance alone), brighter
       text (0.898 vs 0.360) and a heavier weight. Marking selection by accent
       TEXT was the first draft and was wrong for it -- --accent-400 and
       --text-secondary sit at nearly the same relative luminance, so that is
       very nearly a pure hue change, and the person who reported this row is
       colour weak. Stronger fill and ring than before, since the resting pills
       are no longer bare and the selected one has to stay clearly ahead. */
    background: var(--accent-soft-strong);
    box-shadow: inset 0 0 0 1px var(--accent-500);
    color: var(--text-primary);
    font-weight: 600;
  }

  :global(.modal-body) {
    padding: 14px 16px;
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
  .btn-reset-armed {
    opacity: 1;
    color: #ff4040;
    border-color: rgba(255, 64, 64, 0.5);
    background: rgba(255, 64, 64, 0.12);
  }

  /* ── Settings form elements ──────────────────────── */

  :global(.setting-group) {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-bottom: 12px;
  }
  :global(.setting-group:last-child) { margin-bottom: 0; }

  :global(.setting-label) {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-secondary);
    margin: 0;
  }

  :global(.setting-input) {
    width: 100%;
    font-family: inherit;
    font-size: 13px;
    padding: 8px 12px;
    border-radius: 10px;
    background: var(--surface-2);
    color: var(--text-primary);
    border: 1px solid var(--border);
    outline: none;
    box-sizing: border-box;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }
  :global(.setting-input:focus) { border-color: var(--accent-500); box-shadow: 0 0 0 2px rgba(255, 107, 53, 0.15); }
  :global(.setting-select) {
    width: 100%; font-family: inherit; font-size: 13px; padding: 8px 12px;
    border-radius: 10px; background: var(--surface-2); color: var(--text-primary);
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
    padding: 7px 12px;
    font-size: 11px;
    font-weight: 600;
    border-radius: var(--r-pill);
    background: var(--surface-3);
    color: var(--text-secondary);
    border: 1px solid transparent;
    cursor: pointer;
    flex-shrink: 0;
    font-family: inherit;
    white-space: nowrap;
    transition: background 120ms ease-out;
  }
  .key-toggle:hover { background: var(--surface-4); color: var(--text-primary); }

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

  .sensitivity-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    padding-left: 24px; /* align under the checkbox label text */
  }

  .sensitivity-end {
    font-size: 11px;
    color: var(--text-tertiary, #8a8a8a);
    flex-shrink: 0;
  }

  .sensitivity-slider {
    flex: 1;
    min-width: 0;
    accent-color: var(--accent-500);
    cursor: pointer;
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
    padding: 2px 8px;
    border-radius: var(--r-pill);
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
    border: 1px solid transparent;
    border-radius: var(--r-pill);
    color: var(--text-primary);
    font-size: 12px;
    padding: 6px 12px;
    cursor: pointer;
    transition: background 0.15s;
  }

  .about-link:hover {
    background: var(--surface-4);
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
    border-radius: var(--r-md);
    border: none;
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
    border: none;
    border-radius: var(--r-pill);
    padding: 3px 9px;
    cursor: pointer;
    margin-right: 4px;
    flex-shrink: 0;
  }

  .header-update:hover {
    background: rgba(245, 158, 11, 0.2);
  }
</style>
