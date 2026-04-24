<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { register, unregisterAll } from "@tauri-apps/plugin-global-shortcut";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  type Rect = { x: number; y: number; width: number; height: number };

  type LocateResult = {
    bbox: Rect;
    name: string;
    role: string;
    confidence: number;
  };

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

  type AppPhase =
    | "idle"
    | "thinking"
    | "guiding"
    | "needs_input"
    | "error";

  let task = $state("");
  let phase = $state<AppPhase>("idle");
  let errorMsg = $state("");

  let steps = $state<GuidanceStep[]>([]);
  let stepIndex = $state(0);
  let currentInstruction = $state("");
  let locateResult = $state<LocateResult | null>(null);
  let sessionId = $state("");
  let provider = $state("");

  let elapsedMs = $state(0);
  let elapsedTimer: ReturnType<typeof setInterval> | null = null;
  let elapsedStart = 0;

  // Monotonically incrementing token; any response whose token doesn't match
  // the current one was generated after the user cancelled — ignore it.
  let requestToken = 0;

  function startTimer() {
    elapsedStart = performance.now();
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = setInterval(() => {
      elapsedMs = Math.round(performance.now() - elapsedStart);
    }, 200);
  }

  function stopTimer() {
    if (elapsedTimer) {
      clearInterval(elapsedTimer);
      elapsedTimer = null;
    }
  }

  function cancelRequest() {
    requestToken++;           // invalidate any in-flight response
    stopTimer();
    invoke("clear_overlay").catch(() => {});
    invoke("speak", { text: "" }).catch(() => {}); // stop TTS
    phase = "idle";
  }

  function closeWindow() {
    getCurrentWindow().close();
  }

  function applyResponse(res: GuideResponse, idx: number, token: number) {
    if (token !== requestToken) return; // stale — user cancelled
    steps = res.steps;
    stepIndex = idx;
    currentInstruction = res.instruction;
    locateResult = res.located;
    sessionId = res.session_id;
    if (res.provider) provider = res.provider;
    phase = res.needs_input ? "needs_input" : "guiding";
    if (res.instruction) invoke("speak", { text: res.instruction }).catch(() => {});
  }

  async function guide() {
    if (!task.trim()) return;
    phase = "thinking";
    errorMsg = "";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("guide", { task: task.trim() });
      stopTimer();
      if (token !== requestToken) return;
      if (!res.ok) {
        phase = "error";
        errorMsg = res.error ?? "guide failed";
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = "error";
      errorMsg = String(e);
    }
  }

  async function nextStep() {
    const nextIdx = stepIndex + 1;
    if (nextIdx >= steps.length) {
      task = "";
      phase = "thinking";
      errorMsg = "";
      startTimer();
      const token = ++requestToken;
      try {
        const res = await invoke<GuideResponse>("guide", { task: "" });
        stopTimer();
        if (token !== requestToken) return;
        if (!res.ok) {
          phase = "error";
          errorMsg = res.error ?? "re-query failed";
          return;
        }
        applyResponse(res, 0, token);
      } catch (e) {
        stopTimer();
        if (token !== requestToken) return;
        phase = "error";
        errorMsg = String(e);
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
      errorMsg = String(e);
    }
  }

  async function correction() {
    phase = "thinking";
    errorMsg = "";
    startTimer();
    const token = ++requestToken;
    try {
      const res = await invoke<GuideResponse>("send_correction");
      stopTimer();
      if (token !== requestToken) return;
      if (!res.ok) {
        phase = "error";
        errorMsg = res.error ?? "correction failed";
        return;
      }
      applyResponse(res, 0, token);
    } catch (e) {
      stopTimer();
      if (token !== requestToken) return;
      phase = "error";
      errorMsg = String(e);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && phase === "idle") {
      e.preventDefault();
      guide();
    }
  }

  let statusLabel = $derived(
    phase === "idle" ? "idle"
    : phase === "thinking" ? `thinking · ${(elapsedMs / 1000).toFixed(1)}s`
    : phase === "guiding" ? "guiding"
    : phase === "needs_input" ? "needs input"
    : "error"
  );

  onMount(async () => {
    try {
      // Alt+` — advance to next step
      await register("Alt+Backquote", () => {
        if (phase === "guiding" || phase === "needs_input") nextStep();
      });
      // Alt+E — report wrong / correction
      await register("Alt+KeyE", () => {
        if (phase === "guiding" || phase === "needs_input") correction();
      });
      // Alt+S — pause: cancel any in-flight request and return to idle
      await register("Alt+KeyS", () => {
        if (phase === "guiding" || phase === "needs_input" || phase === "thinking") {
          cancelRequest();
        }
      });
    } catch (e) {
      console.warn("global shortcut registration failed:", e);
    }
  });

  onDestroy(async () => {
    await unregisterAll().catch(() => {});
  });
</script>

<main>
  <header>
    <span class="dot"></span>
    <h1>AI Navigator</h1>
    <span class="tag">v0.4.0-alpha{provider ? ` · ${provider}` : ""}</span>
    <button class="close-btn" onclick={closeWindow} title="Close">✕</button>
  </header>

  <section class="card task-card">
    <textarea
      bind:value={task}
      onkeydown={handleKeydown}
      placeholder="What do you need help with?"
      rows={3}
      disabled={phase === "thinking"}
    ></textarea>
    {#if phase === "thinking"}
      <button class="ghost full-width" onclick={cancelRequest}>Cancel</button>
    {:else}
      <button
        class="primary full-width"
        onclick={guide}
        disabled={!task.trim()}
      >
        Guide me
      </button>
    {/if}
  </section>

  {#if phase === "guiding" || phase === "needs_input" || phase === "thinking"}
    {#if steps.length > 0 && currentInstruction}
      <section class="card instruction-card">
        <div class="step-counter">Step {stepIndex + 1} of {steps.length}</div>
        <p class="instruction-text">{currentInstruction}</p>

        {#if locateResult}
          <div class="locate-meta">
            <span class="badge {locateResult.role === 'Ocr' ? 'badge-warn' : 'badge-ok'}">
              {locateResult.role}
            </span>
            <span class="conf">{(locateResult.confidence * 100).toFixed(0)}% · {locateResult.name}</span>
          </div>
        {:else if steps[stepIndex]?.target_text}
          <div class="locate-meta">
            <span class="badge badge-miss">not located</span>
            <span class="conf">{steps[stepIndex].target_text}</span>
          </div>
        {/if}

        <div class="action-row">
          <button class="primary" onclick={nextStep} disabled={phase === "thinking"}>
            Next →
          </button>
          <button class="danger" onclick={correction} disabled={phase === "thinking"}>
            ✗ Wrong
          </button>
        </div>
      </section>
    {/if}
  {/if}

  {#if phase === "error"}
    <section class="card error-card">
      <p class="error-text">{errorMsg}</p>
      <button class="ghost" onclick={() => { phase = "idle"; errorMsg = ""; }}>
        Dismiss
      </button>
    </section>
  {/if}

  <footer>
    <span class="status-dot status-{phase}"></span>
    <span class="status-label">{statusLabel}</span>
    {#if sessionId}
      <span class="session-id">{sessionId.slice(0, 8)}</span>
    {/if}
  </footer>
</main>

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
    font-family: Inter, -apple-system, "Segoe UI", Roboto, sans-serif;
    color-scheme: dark;
  }

  :global(body) {
    margin: 0;
    background: transparent;
    color: var(--text-primary);
    -webkit-font-smoothing: antialiased;
  }

  main {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 20px;
    height: calc(100vh - 40px);
    margin: 8px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  h1 {
    font-size: 14px;
    font-weight: 600;
    margin: 0;
    letter-spacing: 0.01em;
  }

  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--accent-500);
    box-shadow: 0 0 12px rgba(255, 107, 53, 0.5);
    flex-shrink: 0;
  }

  .tag {
    margin-left: auto;
    font-size: 11px;
    font-weight: 500;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    white-space: nowrap;
  }

  .close-btn {
    margin-left: 8px;
    width: 22px;
    height: 22px;
    padding: 0;
    border-radius: 50%;
    font-size: 11px;
    background: transparent;
    color: var(--text-tertiary);
    border: none;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .close-btn:hover {
    background: rgba(239, 68, 68, 0.2);
    color: var(--danger);
  }

  .card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .task-card {
    flex-shrink: 0;
  }

  .instruction-card {
    flex: 1;
    overflow: hidden;
  }

  .error-card {
    border-color: rgba(239, 68, 68, 0.3);
    background: rgba(239, 68, 68, 0.06);
    flex-shrink: 0;
  }

  textarea {
    font-family: inherit;
    font-size: 13px;
    padding: 10px;
    border-radius: 8px;
    background: var(--surface-1);
    color: var(--text-primary);
    border: 1px solid var(--border);
    outline: none;
    resize: none;
    line-height: 1.5;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }

  textarea:focus {
    border-color: var(--accent-500);
    box-shadow: 0 0 0 3px rgba(255, 107, 53, 0.18);
  }

  textarea:disabled {
    opacity: 0.5;
  }

  .step-counter {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .instruction-text {
    font-size: 16px;
    font-weight: 500;
    line-height: 1.55;
    color: var(--text-primary);
    margin: 0;
    flex: 1;
  }

  .locate-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .badge {
    font-size: 10px;
    font-weight: 600;
    padding: 2px 7px;
    border-radius: 9999px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .badge-ok {
    background: rgba(34, 197, 94, 0.15);
    color: var(--success);
  }

  .badge-warn {
    background: rgba(245, 158, 11, 0.15);
    color: var(--warning);
  }

  .badge-miss {
    background: rgba(239, 68, 68, 0.12);
    color: var(--danger);
  }

  .conf {
    font-size: 11px;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .action-row {
    display: flex;
    gap: 8px;
    margin-top: auto;
  }

  .action-row button {
    flex: 1;
  }

  .error-text {
    font-size: 13px;
    color: var(--danger);
    margin: 0;
    word-break: break-word;
  }

  footer {
    margin-top: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }

  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--text-tertiary);
  }

  .status-dot.status-idle { background: var(--text-tertiary); }
  .status-dot.status-thinking { background: var(--warning); animation: pulse 1s ease-in-out infinite; }
  .status-dot.status-guiding { background: var(--success); }
  .status-dot.status-needs_input { background: var(--accent-500); }
  .status-dot.status-error { background: var(--danger); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
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
    opacity: 0.6;
  }

  button {
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    padding: 8px 12px;
    border-radius: 8px;
    cursor: pointer;
    border: 1px solid transparent;
    transition: background-color 120ms ease-out, border-color 120ms ease-out;
  }

  button.primary {
    background: var(--accent-500);
    color: #fff;
  }
  button.primary:hover { background: var(--accent-400); }
  button.primary:active { background: var(--accent-600); }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }

  button.danger {
    background: rgba(239, 68, 68, 0.15);
    color: var(--danger);
    border-color: rgba(239, 68, 68, 0.3);
  }
  button.danger:hover { background: rgba(239, 68, 68, 0.25); }
  button.danger:disabled { opacity: 0.5; cursor: not-allowed; }

  button.ghost {
    background: var(--surface-3);
    color: var(--text-primary);
    border-color: var(--border);
  }
  button.ghost:hover { background: #2d2d33; }

  .full-width {
    width: 100%;
  }
</style>
