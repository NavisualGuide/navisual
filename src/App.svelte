<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { register, unregisterAll } from "@tauri-apps/plugin-global-shortcut";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { LogicalSize, LogicalPosition } from "@tauri-apps/api/dpi";
  import { listen } from "@tauri-apps/api/event";

  type Rect = { x: number; y: number; width: number; height: number };
  type LocateResult = { bbox: Rect; name: string; role: string; confidence: number };
  type GuidanceStep = {
    instruction: string;
    target_text: string | null;
    target_role: string | null;
    target_nearby_text: string | null;
    target_zone_x: number | null;
    target_zone_y: number | null;
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
    error: string | null;
  };
  type AppPhase = "idle" | "thinking" | "guiding" | "needs_input" | "error";
  type HistoryRole = "user" | "ai" | "correction" | "system" | "error";
  type HistoryEntry = { id: number; role: HistoryRole; text: string; meta?: string };
  type SettingsTab = "provider" | "overlay" | "hotkeys";

  // Core state
  let task = $state("");
  let phase = $state<AppPhase>("idle");

  let steps = $state<GuidanceStep[]>([]);
  let stepIndex = $state(0);
  let currentInstruction = $state("");
  let locateResult = $state<LocateResult | null>(null);
  let sessionId = $state("");
  let provider = $state("");

  // UI state
  let iconMode = $state(false);
  let showSettings = $state(false);
  let settingsTab = $state<SettingsTab>("provider");
  let history = $state<HistoryEntry[]>([]);
  let historyEl: HTMLElement | null = $state(null);

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

  async function addToHistory(role: HistoryRole, text: string, meta?: string) {
    history.push({ id: Date.now(), role, text, meta });
    await tick();
    if (historyEl) historyEl.scrollTop = historyEl.scrollHeight;
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

  async function newSession() {
    cancelRequest();
    task = "";
    steps = [];
    stepIndex = 0;
    currentInstruction = "";
    locateResult = null;
    sessionId = "";
    history = [];
    await addToHistory("system", "New session started");
  }

  function applyResponse(res: GuideResponse, idx: number, token: number) {
    if (token !== requestToken) return;
    steps = res.steps;
    stepIndex = idx;
    currentInstruction = res.instruction;
    locateResult = res.located;
    sessionId = res.session_id;
    if (res.provider) provider = res.provider;
    phase = res.needs_input ? "needs_input" : "guiding";
    if (res.instruction) {
      let meta: string | undefined;
      if (res.located) {
        meta = `${res.located.role} · ${(res.located.confidence * 100).toFixed(0)}% · ${res.located.name}`;
      } else if (steps[idx]?.target_text) {
        meta = `not located · "${steps[idx].target_text}"`;
      }
      addToHistory("ai", res.instruction, meta);
      invoke("speak", { text: res.instruction }).catch(() => {});
    }
  }

  async function guide() {
    if (!task.trim()) return;
    const taskText = task.trim();
    await addToHistory("user", taskText);
    currentInstruction = "";
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("guide", { task: taskText });
      stopTimer();
      if (token !== requestToken) return;
      if (!res.ok) {
        phase = "error";
        addToHistory("error", res.error ?? "guide failed");
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = "error";
      addToHistory("error", String(e));
    }
  }

  async function nextStep() {
    const nextIdx = stepIndex + 1;
    if (nextIdx >= steps.length) {
      currentInstruction = "";
      phase = "thinking";
      startTimer();
      const token = ++requestToken;
      try {
        const res = await invoke<GuideResponse>("guide", { task: "" });
        stopTimer();
        if (token !== requestToken) return;
        if (!res.ok) {
          phase = "error";
          addToHistory("error", res.error ?? "re-query failed");
          return;
        }
        applyResponse(res, 0, token);
      } catch (e) {
        stopTimer();
        if (token !== requestToken) return;
        phase = "error";
        addToHistory("error", String(e));
      }
      return;
    }

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
      phase = "error";
      addToHistory("error", String(e));
    }
  }

  async function correction() {
    addToHistory("correction", "Marked wrong — re-analysing…");
    currentInstruction = "";
    phase = "thinking";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction");
      stopTimer();
      if (token !== requestToken) return;
      if (!res.ok) {
        phase = "error";
        addToHistory("error", res.error ?? "correction failed");
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = "error";
      addToHistory("error", String(e));
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && (phase === "idle" || phase === "needs_input")) {
      e.preventDefault();
      guide();
    }
  }

  let statusLabel = $derived(
    phase === "idle"        ? "idle"
    : phase === "thinking"  ? `thinking · ${(elapsedMs / 1000).toFixed(1)}s`
    : phase === "guiding"   ? `step ${stepIndex + 1}/${steps.length}`
    : phase === "needs_input" ? "needs input"
    : "error"
  );

  let actionDisabled = $derived(phase !== "guiding" && phase !== "needs_input");
  let isThinking = $derived(phase === "thinking");

  onMount(async () => {
    // Position bottom-right then show — panel starts hidden (visible:false in
    // tauri.conf.json) so the user never sees a blank frame at 0,0 while
    // WebView2 initialises. We show only once the UI is fully painted.
    try {
      const sw = window.screen.availWidth;
      const sh = window.screen.availHeight;
      const margin = 24;
      await getCurrentWindow().setPosition(
        new LogicalPosition(sw - PANEL_W - margin, sh - PANEL_H - margin)
      );
      await getCurrentWindow().show();
    } catch (_) {}

    listen<{ delta: string }>("stream_chunk", (event) => {
      if (phase === "thinking" || phase === "guiding") {
        currentInstruction += event.payload.delta;
      }
    });

    // Unregister any stale shortcuts from a previous mount (e.g. HMR).
    await unregisterAll().catch(() => {});

    // Debounce helper — Tauri's RegisterHotKey fires on every keydown repeat,
    // so without a gate the user gets multiple triggers from one key press.
    function debounced(fn: () => void, ms = 350): () => void {
      let last = 0;
      return () => { const now = Date.now(); if (now - last < ms) return; last = now; fn(); };
    }

    // Register each shortcut independently so one failure doesn't kill the rest.
    const shortcuts: Array<[string, () => void]> = [
      ["Alt+Backquote", debounced(() => { if (!actionDisabled) nextStep(); })],
      ["Alt+KeyE",      debounced(() => { if (!actionDisabled) correction(); })],
      ["Alt+KeyS",      debounced(() => { if (phase !== "idle") cancelRequest(); })],
      ["Alt+KeyQ",      debounced(() => { if (iconMode) expandToPanel(); else collapseToIcon(); })],
    ];
    for (const [key, handler] of shortcuts) {
      try { await register(key, handler); }
      catch (e) { console.warn(`shortcut ${key} failed:`, e); }
    }

    await addToHistory("system", "AI Navigator ready");
  });

  onDestroy(async () => {
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
    title="Expand AI Navigator (Alt+Q)"
  >
    <span class="icon-glow"></span>
  </button>
{:else}
  <main>
    <!-- Title bar: onmousedown → startDragging() (more reliable than data-tauri-drag-region on WebView2) -->
    <div class="titlebar" role="toolbar" tabindex="-1" onmousedown={handleHeaderMousedown}>
      <span class="header-dot"></span>
      <span class="header-title">AI Navigator</span>
      {#if provider}
        <span class="header-provider">{provider}</span>
      {/if}
      <div class="header-actions">
        <button class="hdr-btn" onclick={() => { showSettings = true; }} title="Settings (E.6)">⚙</button>
        <button class="hdr-btn" onclick={newSession} title="New session">＋</button>
        <button class="hdr-btn" onclick={collapseToIcon} title="Collapse to icon (Alt+Q)">⊟</button>
        <button class="hdr-btn hdr-btn-close" onclick={closeWindow} title="Quit">✕</button>
      </div>
    </div>

    <!-- Latest instruction (visible when guiding) -->
    {#if currentInstruction && (phase === "guiding" || phase === "needs_input" || (isThinking && currentInstruction))}
      <section class="latest-box">
        <div class="latest-header">
          <span class="step-counter">Step {stepIndex + 1} of {steps.length}</span>
          {#if locateResult}
            <span class="badge badge-{locateResult.role === 'Ocr' ? 'warn' : 'ok'}">
              {locateResult.role}
            </span>
            <span class="conf">{(locateResult.confidence * 100).toFixed(0)}%</span>
          {:else if steps[stepIndex]?.target_text}
            <span class="badge badge-miss">not located</span>
          {/if}
        </div>
        <p class="latest-text">{currentInstruction}</p>
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
            {#if entry.meta}
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

    <!-- Task input -->
    <section class="task-section">
      <textarea
        bind:value={task}
        onkeydown={handleKeydown}
        placeholder={phase === "needs_input" ? "Type your reply…" : "What do you need help with?"}
        rows={2}
        disabled={isThinking}
      ></textarea>
      {#if isThinking}
        <button class="btn-ghost btn-full" onclick={cancelRequest}>⏹ Cancel ({(elapsedMs / 1000).toFixed(1)}s)</button>
      {:else if phase === "needs_input"}
        <button class="btn-primary btn-full" onclick={guide} disabled={!task.trim()}>↩ Reply</button>
      {:else}
        <button class="btn-primary btn-full" onclick={guide} disabled={!task.trim() || isThinking}>Guide me</button>
      {/if}
    </section>

    <!-- Action row (always visible) -->
    <div class="action-row">
      <button class="btn-action btn-next" onclick={nextStep} disabled={actionDisabled} title="Alt+`">
        → Next
      </button>
      <button class="btn-action btn-wrong" onclick={correction} disabled={actionDisabled} title="Alt+E">
        ✗ Wrong
      </button>
      <button class="btn-action btn-pause" onclick={cancelRequest} disabled={phase === "idle"} title="Alt+S">
        ⏸ Pause
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
        <span>Alt+` Next</span>
        <span>Alt+E Wrong</span>
        <span>Alt+S Pause</span>
        <span>Alt+Q Icon</span>
      </div>
    </footer>
  </main>

  <!-- Settings modal (stub for E.6) -->
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
          <button class="tab-btn {settingsTab === 'overlay' ? 'tab-active' : ''}" onclick={() => (settingsTab = "overlay")}>Overlay</button>
          <button class="tab-btn {settingsTab === 'hotkeys' ? 'tab-active' : ''}" onclick={() => (settingsTab = "hotkeys")}>Hotkeys</button>
        </div>
        <div class="modal-body">
          {#if settingsTab === "provider"}
            <p class="stub-note">Provider settings coming in Phase E.6.</p>
            <p class="stub-hint">Configure your API provider and keys in <code>.env</code> for now.</p>
          {:else if settingsTab === "overlay"}
            <p class="stub-note">Overlay settings coming in Phase E.6.</p>
            <p class="stub-hint">Color, opacity, and thickness controls will appear here.</p>
          {:else}
            <p class="stub-note">Hotkey settings coming in Phase E.6.</p>
            <div class="hotkey-preview">
              <div class="hk-row"><kbd>Alt+`</kbd><span>Next step</span></div>
              <div class="hk-row"><kbd>Alt+E</kbd><span>Mark wrong</span></div>
              <div class="hk-row"><kbd>Alt+S</kbd><span>Pause / cancel</span></div>
              <div class="hk-row"><kbd>Alt+Q</kbd><span>Toggle icon mode</span></div>
            </div>
          {/if}
        </div>
        <div class="modal-footer">
          <button class="btn-ghost" onclick={() => (showSettings = false)}>Close</button>
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

  .header-actions {
    margin-left: auto;
    display: flex;
    gap: 2px;
    flex-shrink: 0;
  }

  .hdr-btn {
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: 5px;
    font-size: 12px;
    background: transparent;
    color: var(--text-tertiary);
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

  .task-section {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
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

  .btn-wrong {
    background: rgba(239, 68, 68, 0.12);
    color: var(--danger);
    border-color: rgba(239, 68, 68, 0.22);
  }
  .btn-wrong:not(:disabled):hover { background: rgba(239, 68, 68, 0.22); }

  .btn-pause {
    background: rgba(245, 158, 11, 0.12);
    color: var(--warning);
    border-color: rgba(245, 158, 11, 0.22);
  }
  .btn-pause:not(:disabled):hover { background: rgba(245, 158, 11, 0.22); }

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

  .stub-note {
    font-size: 13px;
    color: var(--text-secondary);
    margin: 0 0 8px 0;
  }
  .stub-hint {
    font-size: 12px;
    color: var(--text-tertiary);
    margin: 0;
  }
  .stub-hint code {
    background: var(--surface-3);
    padding: 1px 5px;
    border-radius: 4px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 11px;
  }

  .hotkey-preview {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 10px;
  }
  .hk-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--text-secondary);
  }
  kbd {
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 7px;
    font-size: 11px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    color: var(--text-primary);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .modal-footer {
    padding: 10px 14px;
    border-top: 1px solid var(--border);
    display: flex;
    justify-content: flex-end;
    flex-shrink: 0;
  }
</style>
