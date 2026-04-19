<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let status = $state<"idle" | "pinging" | "ok" | "error">("idle");
  let reply = $state("");
  let echoInput = $state("Hello, sidecar!");
  let echoReply = $state("");

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
</script>

<main>
  <header>
    <span class="dot"></span>
    <h1>AI Navigator</h1>
    <span class="tag">v0.4.0-alpha · Phase A</span>
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

    <button class="primary" onclick={ping} disabled={status === "pinging"}>
      Ping Sidecar
    </button>

    {#if reply}
      <pre class="reply">{reply}</pre>
    {/if}
  </section>

  <section class="card">
    <div class="row">
      <span class="label">Round-trip</span>
    </div>
    <input bind:value={echoInput} placeholder="Type something to echo…" />
    <button class="ghost" onclick={echo}>Send</button>
    {#if echoReply}
      <pre class="reply">{echoReply}</pre>
    {/if}
  </section>

  <footer>
    Svelte → Rust → Python — IPC wired end to end.
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
    gap: 16px;
    overflow: hidden;
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

  .label {
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
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
  .status-pinging { color: var(--warning); background: rgba(245, 158, 11, 0.12); }

  button {
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    padding: 10px 16px;
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

  input {
    font-family: inherit;
    font-size: 13px;
    padding: 10px 12px;
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
    padding: 10px 12px;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-left: 2px solid var(--accent-500);
    border-radius: 8px;
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 120px;
    overflow-y: auto;
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
