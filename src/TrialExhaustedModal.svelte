<script lang="ts">
  // Trial-exhausted / not-enough-coins modal (S.1), extracted from App.svelte
  // (componentization pass, 2026-07-13). Uses App.svelte's :global()-ized modal
  // and button classes — no local styles. The modal copy differs by reason:
  // telling an existing paying customer low on coins "your free trial is used"
  // is simply wrong for them (audit F6).
  import { invoke } from "@tauri-apps/api/core";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { billing } from "./lib/billing.svelte";
  import PromoOffer from "./PromoOffer.svelte";
  import { t } from "./lib/i18n.svelte";

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
    // Clearing the flag only ever hid the wait; the backend kept listening for the
    // full budget, so the next sign-in reported "already in progress". Cancel it for
    // real so the two sides agree.
    if (billing.oauthPending) invoke("cancel_google_oauth").catch(() => {});
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
      aria-label={reason === "coins" ? t("trial.ariaCoins") : t("trial.ariaFree")}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      style="max-width: 320px;"
    >
      <div class="modal-header">
        <span class="modal-title">{reason === "coins" ? t("trial.titleCoins") : t("trial.titleFree")}</span>
        <button class="hdr-btn hdr-btn-close" onclick={() => (open = false)}>✕</button>
      </div>
      <div class="modal-body" style="padding: 20px; text-align: center; line-height: 1.6;">
        <p style="font-size: 2em; margin-bottom: 12px;">{reason === "coins" ? "🪙" : "🎯"}</p>
        <p style="margin-bottom: 8px; font-weight: 600;">
          {reason === "coins" ? t("trial.bodyCoins") : t("trial.bodyFree")}
        </p>

        {#if billing.oauthPending}
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            {t("trial.signingInGoogle")}
          </p>
        {:else if billing.buyPending}
          <!-- Distinct from oauthPending again: a signed-in user topping up was
               being told they were signing in with Google. -->
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            {t("trial.openingCheckout")}
          </p>
        {:else if billing.checkoutPending}
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 20px;">
            {t("trial.checkoutOpened")}
          </p>
          <button class="btn-primary btn-full" onclick={onRefreshBalance}>{t("bill.refreshBalance")}</button>
        {:else}
          <!-- The one surface a signed-out user cannot avoid: they are here because the
               free requests ran out, which is exactly the moment the offer answers. Shown
               ABOVE the top-up copy so "free coins" precedes "buy coins". -->
          <div style="text-align: left;"><PromoOffer /></div>
          <p style="font-size: 0.9em; color: var(--text-secondary); margin-bottom: 16px;">
            {t("trial.topUpBlurb")}
          </p>
          {#if reason === "coins"}
            <!-- audit F8: a paid account low on coins may still have unused free
                 requests — "buy more" alone hides that option. -->
            <p style="font-size: 0.85em; color: var(--text-secondary); margin-bottom: 12px;">
              {t("trial.stillFreeLeft")}
            </p>
          {/if}
          <button class="btn-primary btn-full" style="margin-bottom: 6px;" onclick={() => onBuy(20)}>{t("bill.buyCoins", { amount: 20 })}</button>
          <p class="legal-agree" style="margin-bottom: 14px;">
            {t("bill.agreePrefix")}
            <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/terms.html")}>{t("bill.terms")}</button>
            {t("bill.and")}
            <button class="legal-link" onclick={() => openUrl("https://navisualguide.com/privacy.html")}>{t("bill.privacy")}</button>.
          </p>
          <p style="font-size: 0.85em; color: var(--text-secondary); margin-bottom: 16px;">
            {t("trial.ownKey")}
          </p>
        {/if}

        <button class="btn-ghost btn-full" onclick={close}>{t("common.close")}</button>
      </div>
    </div>
  </div>
{/if}
