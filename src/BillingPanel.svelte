<script lang="ts">
  // Settings → Billing tab body (S.2), extracted from App.svelte
  // (componentization pass, 2026-07-13). Amount-picker state is local; balance
  // facts come from the billing store; checkout itself stays in App (onBuy) —
  // it manipulates the panel window and Settings visibility.
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { billing } from "./lib/billing.svelte";

  let {
    provider,
    onBuy,
    onRefreshBalance,
  }: {
    /** settingsForm.api_provider — for the "you're not on Managed" note. */
    provider: string;
    onBuy: (amountUsd: number) => void;
    onRefreshBalance: () => void;
  } = $props();

  let buyAmount = $state<number | "custom">(20); // USD top-up; "custom" reveals a field
  let customAmount = $state(20); // USD entered when buyAmount === "custom"
  let effectiveAmount = $derived(buyAmount === "custom" ? customAmount : buyAmount);
  let amountValid = $derived(effectiveAmount >= 5 && effectiveAmount <= 500);
</script>

<div class="setting-group">
  <span class="setting-label">Account</span>
  <p class="setting-hint">{billing.tier === "paid" ? "Paid (coins)" : "Free trial"}</p>
</div>
{#if billing.coins !== null && billing.coins > 0}
  <div class="setting-group">
    <span class="setting-label">Coin balance</span>
    <p class="setting-hint">{billing.coins} coins</p>
  </div>
{/if}
<div class="setting-group">
  <span class="setting-label">Free requests</span>
  <p class="setting-hint">{billing.freeRemaining ?? "—"} remaining of 30</p>
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
  <button class="btn-primary" onclick={() => onBuy(effectiveAmount)} disabled={billing.oauthPending || billing.checkoutPending || !amountValid}>
    {billing.oauthPending ? "Signing in…" : billing.checkoutPending ? "Checkout open in browser…" : `Buy coins ($${effectiveAmount})`}
  </button>
  {#if billing.checkoutPending}
    <button class="btn-ghost" style="margin-top: 8px;" onclick={onRefreshBalance}>Refresh balance</button>
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
  {#if provider !== "managed"}
    <br /><br />Note: you're currently on the <strong>{provider}</strong>
    provider. Switch to <strong>Managed</strong> on the Provider tab to spend coins.
  {/if}
</p>
