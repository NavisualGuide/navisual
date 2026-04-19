<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type CaptureResult = {
    jpeg_base64: string;
    width: number;
    height: number;
    crop_rect: { x: number; y: number; width: number; height: number } | null;
    bytes: number;
    elapsed_ms: number;
  };

  let status = $state<"idle" | "pinging" | "ok" | "error">("idle");
  let reply = $state("");
  let echoInput = $state("Hello, sidecar!");
  let echoReply = $state("");

  let captureStatus = $state<"idle" | "capturing" | "ok" | "error">("idle");
  let captureResult = $state<CaptureResult | null>(null);
  let captureError = $state("");

  async function ping() {
    status = "pinging";
    reply = "";
    try {
      reply = await invoke<string>("ping_sidecar");
      status = "ok";
    } catch (e) {
      reply = String(e);
      status = "error";
    }
  }

  async function echo() {
    try {
      echoReply = await invoke<string>("sidecar_echo", { text: echoInput });
    } catch (e) {
      echoReply = String(e);
    }
  }

  async function capture(mode: "screen" | "active") {
    captureStatus = "capturing";
    captureError = "";
    captureResult = null;
    try {
      const cmd = mode === "screen" ? "capture_screen" : "capture_active_window";
      captureResult = await invoke<CaptureResult>(cmd, { quality: 80 });
      captureStatus = "ok";
    } catch (e) {
      captureError = String(e);
      captureStatus = "error";
    }
  }
</script>

<main>
  <header>
    <span class="dot"></span>
    <h1>AI Navigator</h1>
    <span class="tag">v0.4.0-alpha · Phase C.1</span>
  </header>

  <section class="card">
    <div class="row">
      <span class="label">Sidecar</span>
      <span class="status status-{status}">
        {#if status === "idle"}not pinged yet{/if}
        {#if status === "pinging"}pinging…{/if}
        {#if status === "ok"}online{/if}
        {#if status === "error"}error{/if}
      </span>
    </div>

    <div class="button-row">
      <button class="primary" onclick={ping} disabled={status === "pinging"}>
        Ping
      </button>
      <button class="ghost" onclick={echo}>Echo</button>
    </div>

    <input bind:value={echoInput} placeholder="Echo text…" />

    {#if reply}
      <pre class="reply">{reply}</pre>
    {/if}
    {#if echoReply}
      <pre class="reply">{echoReply}</pre>
    {/if}
  </section>

  <section class="card">
    <div class="row">
      <span class="label">Capture</span>
      <span class="status status-{captureStatus}">
        {#if captureStatus === "idle"}ready{/if}
        {#if captureStatus === "capturing"}capturing…{/if}
        {#if captureStatus === "ok"}ok{/if}
        {#if captureStatus === "error"}error{/if}
      </span>
    </div>

    <div class="button-row">
      <button class="primary" onclick={() => capture("screen")} disabled={captureStatus === "capturing"}>
        Full screen
      </button>
      <button class="ghost" onclick={() => capture("active")} disabled={captureStatus === "capturing"}>
        Active window
      </button>
    </div>

    {#if captureResult}
      <div class="capture-meta">
        <span>{captureResult.width}×{captureResult.height}</span>
        <span>{(captureResult.bytes / 1024).toFixed(1)} KB</span>
        <span>{captureResult.elapsed_ms} ms</span>
      </div>
      {#if captureResult.crop_rect}
        <div class="capture-meta">
          <span class="label-sm">crop</span>
          <span>x={captureResult.crop_rect.x} y={captureResult.crop_rect.y}</span>
          <span>{captureResult.crop_rect.width}×{captureResult.crop_rect.height}</span>
        </div>
      {/if}
      <img class="preview" src={`data:image/jpeg;base64,${captureResult.jpeg_base64}`} alt="capture" />
    {/if}
    {#if captureError}
      <pre class="reply error">{captureError}</pre>
    {/if}
  </section>

  <footer>
    Svelte → Rust → Python — capture via xcap + DWM.
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
    overflow-y: auto;
  }

  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
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
  }

  .tag {
    margin-left: auto;
    font-size: 11px;
    font-weight: 500;
    color: var(--text-tertiary);
    font-family: "JetBrains Mono", ui-monospace, monospace;
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

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .button-row {
    display: flex;
    gap: 8px;
  }
  .button-row button { flex: 1; }

  .label {
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .label-sm {
    font-size: 10px;
    font-weight: 500;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .status {
    font-size: 12px;
    font-weight: 500;
    padding: 2px 8px;
    border-radius: 9999px;
    background: var(--surface-3);
    color: var(--text-secondary);
  }
  .status-ok { color: var(--success); background: rgba(34, 197, 94, 0.12); }
  .status-error { color: var(--danger); background: rgba(239, 68, 68, 0.12); }
  .status-pinging, .status-capturing { color: var(--warning); background: rgba(245, 158, 11, 0.12); }

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

  button.ghost {
    background: var(--surface-3);
    color: var(--text-primary);
    border-color: var(--border);
  }
  button.ghost:hover { background: #2d2d33; }
  button.ghost:disabled { opacity: 0.5; cursor: not-allowed; }

  input {
    font-family: inherit;
    font-size: 13px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-1);
    color: var(--text-primary);
    border: 1px solid var(--border);
    outline: none;
    transition: border-color 120ms ease-out, box-shadow 120ms ease-out;
  }
  input:focus {
    border-color: var(--accent-500);
    box-shadow: 0 0 0 3px rgba(255, 107, 53, 0.18);
  }

  .reply {
    margin: 0;
    padding: 8px 10px;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-left: 2px solid var(--accent-500);
    border-radius: 8px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 90px;
    overflow-y: auto;
  }
  .reply.error { border-left-color: var(--danger); color: var(--danger); }

  .capture-meta {
    display: flex;
    gap: 10px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 10px;
    color: var(--text-tertiary);
  }

  .preview {
    width: 100%;
    height: auto;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface-0);
  }

  footer {
    margin-top: auto;
    font-size: 11px;
    color: var(--text-tertiary);
    text-align: center;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
</style>
