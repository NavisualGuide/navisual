<script lang="ts">
  // Settings → Billing tab body (S.2), extracted from App.svelte
  // (componentization pass, 2026-07-13). Amount-picker state is local; balance
  // facts come from the billing store; checkout itself stays in App (onBuy) —
  // it manipulates the panel window and Settings visibility.
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { billing } from "./lib/billing.svelte";
  import PromoOffer from "./PromoOffer.svelte";
  import { t } from "./lib/i18n.svelte";

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

<!-- First thing in the tab: someone reading about buying coins should know they can
     get some for free first. -->
<PromoOffer />

{#if provider !== "managed"}
  <!-- Promoted from the last grey line on the page to the first thing in the
       section. It is the only ACTIONABLE fact here — coins bought now cannot be
       spent until the provider changes — and it was buried under three
       paragraphs of explanatory prose. -->
  <p class="bill-warn">{t("bill.wrongProvider", { provider })}</p>
{/if}

<!-- Label/value pairs on one line each rather than stacked. The stacked form cost
     two lines per fact and read as six unrelated headings; these are three facts
     about one account and belong in one block that can be scanned down. -->
<dl class="bill-facts">
  <div>
    <dt>{t("bill.plan")}</dt>
    <dd>{billing.tier === "paid" ? t("bill.planPaid") : t("bill.planFree")}</dd>
  </div>
  {#if billing.coins !== null && billing.coins > 0}
    <div>
      <dt>{t("bill.coinBalance")}</dt>
      <dd class="bill-value">{t("bill.coins", { n: billing.coins ?? 0 })}</dd>
    </div>
  {/if}
  <div>
    <dt>{t("bill.freeRequests")}</dt>
    <!-- A dash ALONE in the value slot means "not loaded yet"; a dash inside the sentence
         does not survive translation. `还剩 — 次` reads as 还剩一次 -- "one left" -- because
         the dash is close enough to 一 to be misread, which turns a missing value into a
         confident and wrong statement about someone's balance. Reported live 2026-09-27. -->
    <dd>{billing.freeRemaining === null ? "—" : t("bill.freeLeft", { n: billing.freeRemaining, total: 30 })}</dd>
  </div>
</dl>
<p class="setting-hint">{t("bill.tierHint")}</p>

<!-- Amount picker -->
<div class="setting-group" style="margin-top: 14px;">
  <label class="setting-label" for="amount-select">{t("bill.topUpAmount")}</label>
  <select id="amount-select" class="setting-select" bind:value={buyAmount}>
    <option value={5}>$5 · 1,000 coins</option>
    <option value={10}>$10 · 2,000 coins</option>
    <option value={20}>$20 · 4,000 coins</option>
    <option value={50}>$50 · 10,000 coins</option>
    <option value="custom">{t("bill.custom")}</option>
  </select>
  {#if buyAmount === "custom"}
    <input
      class="setting-input" type="number" min="5" max="500" step="1"
      bind:value={customAmount} placeholder={t("bill.customPlaceholder")} style="margin-top: 8px;" />
    <p class="setting-hint">
      {amountValid
        ? t("bill.coins", { n: (customAmount * 200).toLocaleString() })
        : t("bill.amountRange")}
    </p>
  {/if}
</div>

<div class="setting-group" style="margin-top: 12px;">
  <button class="btn-primary" onclick={() => onBuy(effectiveAmount)} disabled={billing.buyPending || billing.oauthPending || billing.checkoutPending || !amountValid}>
    {billing.buyPending ? t("bill.openingCheckout") : billing.checkoutPending ? t("bill.checkoutOpen") : t("bill.buyCoins", { amount: effectiveAmount })}
  </button>
  {#if billing.checkoutPending}
    <button class="btn-ghost" style="margin-top: 8px;" onclick={onRefreshBalance}>{t("bill.refreshBalance")}</button>
  {/if}
</div>
<p class="setting-hint legal-agree">
  {t("bill.agreePrefix")}
  <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/terms.html")}>{t("bill.terms")}</button>
  {t("bill.and")}
  <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>{t("bill.privacy")}</button>.
</p>
<!-- Was three sentences. "If you're not signed in yet, Google sign-in runs first"
     described the pre-merge flow and is now wrong twice over: sign-in lives on this
     same tab, and it is no longer Google-only. The wrong-provider note moved to the
     callout at the top. What is left is the one thing a buyer cannot predict. -->
<p class="setting-hint" style="margin-top: 8px;">
  {t("bill.checkoutHint")}
</p>

<style>
  /* Billing-only, so scoped here rather than added to App's globals. */
  .bill-facts {
    margin: 0 0 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .bill-facts > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .bill-facts dt {
    color: var(--text-tertiary);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .bill-facts dd {
    margin: 0;
    color: var(--text-secondary);
    text-align: right;
  }
  /* The balance is the number people open this page to read. */
  .bill-value {
    color: var(--text-primary);
    font-weight: 600;
  }
  .bill-warn {
    margin: 0 0 12px;
    padding: 8px 10px;
    border: 1px solid var(--warning);
    border-left-width: 3px;
    border-radius: 10px;
    background: color-mix(in srgb, var(--warning) 12%, transparent);
    color: var(--text-secondary);
    line-height: 1.45;
  }
</style>
