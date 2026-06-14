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
  .hotkey-input {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    border-radius: 6px;
    border: 1px solid rgba(255,255,255,0.15);
    background: rgba(255,255,255,0.04);
    cursor: pointer;
    outline: none;
    min-width: 160px;
    font-size: 12px;
    transition: border-color 0.15s, background 0.15s;
    user-select: none;
  }
  .hotkey-input:hover {
    border-color: rgba(255,255,255,0.3);
    background: rgba(255,255,255,0.07);
  }
  .hotkey-input.recording {
    border-color: #FF6B35;
    background: rgba(255,107,53,0.12);
    animation: pulse 1s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(255,107,53,0.4); }
    50%       { box-shadow: 0 0 0 4px rgba(255,107,53,0); }
  }
  .hotkey-badge {
    font-family: monospace;
    font-size: 11px;
    background: rgba(255,255,255,0.1);
    border-radius: 4px;
    padding: 2px 6px;
    letter-spacing: 0.02em;
  }
  .click-hint {
    color: rgba(255,255,255,0.35);
    font-size: 10px;
  }
  .recording-hint {
    color: #FF6B35;
    font-size: 11px;
    font-style: italic;
  }
  .clear-btn {
    margin-left: auto;
    width: 18px;
    height: 18px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: rgba(255,255,255,0.06);
    color: rgba(255,255,255,0.45);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .clear-btn:hover {
    background: rgba(255,107,53,0.18);
    color: #FF6B35;
  }
</style>
