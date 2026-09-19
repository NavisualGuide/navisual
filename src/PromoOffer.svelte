<script lang="ts">
  // The signup promotion, in one place.
  //
  // ONE COMPONENT, THREE SURFACES (Account, Billing, the exhausted modal) because rule 18
  // is really about copies drifting: three hand-written versions of an offer would
  // disagree the first time the amount was retuned, and the amount is deliberately
  // retunable with a single UPDATE on promo_campaigns.
  //
  // NOTHING IS HARDCODED HERE. The number and the deadline come from the relay's balance
  // response (billing.promoOffer), so closing or resizing the campaign changes what the
  // app says without a release. When no campaign is live the component renders nothing at
  // all, which is what retires the message everywhere at once.
  import { billing } from "./lib/billing.svelte";
  import { account } from "./lib/account.svelte";

  // Signed-in users are not shown the offer. Not because it would be wrong -- a signed-in
  // user has already claimed or already missed it -- but because an offer you cannot act
  // on is noise, and this sits in surfaces they use for other reasons.
  const show = $derived(!!billing.promoOffer && !account.signedIn);

  // "until 12 December", or nothing for an open-ended campaign. Deliberately a date and
  // not a countdown: a ticking clock is urgency marketing, and the window here exists to
  // protect the price anchor, not to rush anyone.
  const deadline = $derived.by(() => {
    const iso = billing.promoOffer?.ends_at;
    if (!iso) return "";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "";
    return d.toLocaleDateString(undefined, { day: "numeric", month: "long" });
  });
</script>

{#if show}
  <div class="promo">
    <span class="promo-gift" aria-hidden="true">🎁</span>
    <div class="promo-text">
      <strong>Create a free account and get {billing.promoOffer!.coins.toLocaleString()} coins.</strong>
      <span class="promo-sub">
        Enough to try the faster, smarter models on the Navisual relay. Navisual stays free
        to use{deadline ? ` — offer ends ${deadline}` : ""}.
      </span>
    </div>
  </div>
{/if}

<style>
  .promo {
    display: flex;
    gap: 9px;
    align-items: flex-start;
    margin: 0 0 12px;
    padding: 10px 11px;
    border-radius: var(--r-sm);
    /* A flat accent wash over the surface rather than color-mix(), which this codebase
       has never shipped -- same blend, syntax with no support question. */
    background:
      linear-gradient(rgba(255, 107, 53, 0.1), rgba(255, 107, 53, 0.1)),
      var(--surface-3);
    border-left: 3px solid var(--accent-500);
  }
  .promo-gift {
    font-size: 15px;
    line-height: 1.3;
    flex: 0 0 auto;
  }
  .promo-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .promo-text strong {
    font-size: 12px;
    color: var(--text-primary);
  }
  .promo-sub {
    font-size: 11px;
    line-height: 1.45;
    color: var(--text-secondary);
  }
</style>
