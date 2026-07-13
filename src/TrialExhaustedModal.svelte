<script lang="ts">
  // Trial-exhausted / not-enough-coins modal (S.1), extracted from App.svelte
  // (componentization pass, 2026-07-13). Uses App.svelte's :global()-ized modal
  // and button classes — no local styles. The modal copy differs by reason:
  // telling an existing paying customer low on coins "your free trial is used"
  // is simply wrong for them (audit F6).
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { billing } from "./lib/billing.svelte";

  let {
    open = $bindable(false),
    reason,
    onBuy,
    onRefreshBalance,
  }: {
    open: boolean;
    /** "free" = free requests used up · "coins" = paid tier without enough coins. */
    reason: "free" | "coins";
    onBuy: (amountUsd: number) => void;
    onRefreshBalance: () => void;
  } = $props();

  function close() {
    open = false;
    billing.oauthPending = false;
    billing.checkoutPending = false;
  }
</script>

{#if open}
  <div
    class="modal-backdrop"
    role="presentation"
    onclick={() => (open = false)}
    onkeydown={(e) => { if (e.key === "Escape") open = false; }}
  >
    <div
      class="modal"
      role="dialog"
      tabindex="-1"
      aria-modal="true"
      aria-label={reason === "coins" ? "Not enough coins" : "Free trial exhausted"}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      style="max-width: 320px;"
    >
      <div class="modal-header">
        <span class="modal-title">{reason === "coins" ? "Not enough coins" : "Free trial used"}</span>
        <button class="hdr-btn hdr-btn-close" onclick={() => (open = false)}>✕</button>
      </div>
      <div class="modal-body" style="padding: 20px; text-align: center; line-height: 1.6;">
        <p style="font-size: 2em; margin-bottom: 12px;">{reason === "coins" ? "🪙" : "🎯"}</p>
        <p style="margin-bottom: 8px; font-weight: 600;">
          {reason === "coins" ? "Not enough coins for this quality tier." : "Your free requests have been used."}
        </p>

        {#if billing.oauthPending}
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            Signing in with Google in your browser…
          </p>
        {:else if billing.checkoutPending}
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            Checkout opened in your browser. Come back once you've paid — your balance will update automatically.
          </p>
          <button class="btn-primary btn-full" onclick={onRefreshBalance}>Refresh balance</button>
        {:else}
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 16px;">
            Top up with coins to continue on the Navisual managed relay.
          </p>
          {#if reason === "coins"}
            <!-- audit F8: a paid account low on coins may still have unused free
                 requests — "buy more" alone hides that option. -->
            <p style="font-size: 0.85em; color: var(--text-secondary); margin-bottom: 12px;">
              Still have free requests left? Switch <strong>Quality tier</strong> to
              <strong>Free</strong> on Settings → Provider to use them instead.
            </p>
          {/if}
          <button class="btn-primary btn-full" style="margin-bottom: 6px;" onclick={() => onBuy(20)}>Buy coins ($20)</button>
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

        <button class="btn-ghost btn-full" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}
