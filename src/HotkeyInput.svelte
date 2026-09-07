<script lang="ts">
  import { prettyHotkey } from "./lib/hotkey";

  let { value = $bindable("") }: { value: string } = $props();

  let recording = $state(false);
  // `value` stays the raw Tauri accelerator (needed for registration);
  // the badge shows the humanized form. See lib/hotkey.
  let displayValue = $derived(value ? prettyHotkey(value) : "—");

  // Convert a KeyboardEvent into a Tauri accelerator string like "Ctrl+Shift+KeyE"
  function eventToAccelerator(e: KeyboardEvent): string | null {
    const mods: string[] = [];
    if (e.ctrlKey)  mods.push("Ctrl");
    if (e.shiftKey) mods.push("Shift");
    if (e.altKey)   mods.push("Alt");
    if (e.metaKey)  mods.push("Super");

    // Ignore bare modifier keypresses
    if (["Control","Shift","Alt","Meta"].includes(e.key)) return null;
    // Require at least one modifier
    if (mods.length === 0) return null;

    // Map browser code → Tauri key name
    const code = e.code; // e.g. "KeyE", "Backquote", "Space", "F5"
    mods.push(code);
    return mods.join("+");
  }

  function onKeyDown(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    const accel = eventToAccelerator(e);
    if (accel) {
      value = accel;
      recording = false;
    }
  }

  function startRecording() {
    recording = true;
  }

  function clear(e: MouseEvent) {
    e.stopPropagation(); // don't trigger startRecording on the parent
    value = "";
    recording = false;
  }

  function onBlur() {
    recording = false;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="hotkey-input {recording ? 'recording' : ''}"
  tabindex="0"
  role="button"
  onclick={startRecording}
  onkeydown={onKeyDown}
  onblur={onBlur}
  aria-label="Hotkey: {displayValue}. Click to record."
>
  {#if recording}
    <span class="recording-hint">Press combo…</span>
  {:else}
    <span class="hotkey-badge">{displayValue}</span>
    <span class="click-hint">click to change</span>
    {#if value}
      <button
        type="button"
        class="clear-btn"
        title="Clear — set to none"
        aria-label="Clear hotkey"
        onclick={clear}
      >×</button>
    {/if}
  {/if}
</div>

<style>
  /* Tokens from App.svelte's :root — this used to hard-code #FF6B35 and generic
     monospace, so it drifted from the panel whenever the palette moved. */
  .hotkey-input {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-radius: var(--r-md, 12px);
    border: 1px solid var(--border, rgba(255,255,255,0.08));
    background: var(--surface-2, #1c1c20);
    cursor: pointer;
    outline: none;
    min-width: 160px;
    font-size: 12px;
    transition: border-color 0.15s, background 0.15s;
    user-select: none;
  }
  .hotkey-input:hover {
    border-color: var(--border-strong, rgba(255,255,255,0.14));
    background: var(--surface-3, #26262b);
  }
  .hotkey-input.recording {
    border-color: var(--accent-500, #ff6b35);
    background: var(--accent-soft, rgba(255,107,53,0.14));
    animation: pulse 1s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(255,107,53,0.4); }
    50%       { box-shadow: 0 0 0 4px rgba(255,107,53,0); }
  }
  .hotkey-badge {
    font-family: "JetBrains Mono", ui-monospace, monospace;
    font-size: 11px;
    background: rgba(255,255,255,0.08);
    border-radius: 6px;
    padding: 2px 7px;
    letter-spacing: 0.02em;
  }
  .click-hint {
    color: var(--text-tertiary, #6b6b73);
    font-size: 10px;
  }
  .recording-hint {
    color: var(--accent-500, #ff6b35);
    font-size: 11px;
  }
  .clear-btn {
    margin-left: auto;
    width: 20px;
    height: 20px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 999px;
    background: rgba(255,255,255,0.06);
    color: var(--text-tertiary, #6b6b73);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .clear-btn:hover {
    background: var(--accent-soft, rgba(255,107,53,0.14));
    color: var(--accent-500, #ff6b35);
  }
</style>
