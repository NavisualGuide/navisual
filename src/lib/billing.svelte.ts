// Single owner of managed-billing state (design "worth doing soon" #2, 2026-07-13).
//
// freeRemaining / coinBalanceMicro / tier used to be three App.svelte $states
// written from 6+ places (get_balance parsing in onMount, refreshBalance and the
// oauth_complete listener, plus the balance_update / coin_balance_update /
// trial_exhausted listeners and sign-out) — which is exactly why the F1/F6 class
// of bugs kept appearing: one pathway forgot one field, or two pathways disagreed
// on how tier derives. Every mutation now goes through the methods below; the
// get_balance response is parsed in ONE place; nothing outside this module
// assigns these fields.
//
// UI policy (when to open the exhausted modal, which copy to show) deliberately
// stays with the UI — this store owns the FACTS (what the relay said), and
// exposes them read-only in spirit: treat the fields as read-only outside.

import { invoke } from "@tauri-apps/api/core";

/** Shape of the backend `get_balance` command's response. */
export interface BalanceInfo {
  tier: string;
  free_remaining: number;
  coin_balance_microdollars: number;
}

/** 1 coin = $0.005 = 5,000 µ$ — fixed forever (S.2 coin model). */
export const MICRO_PER_COIN = 5_000;

class Billing {
  /** Free requests remaining on this device; null = not yet known. */
  freeRemaining = $state<number | null>(null);
  /** Coin balance in µ$ (divide by MICRO_PER_COIN for coins); null = unknown. */
  coinBalanceMicro = $state<number | null>(null);
  /**
   * Whether this is a paying customer. Derived ONLY from the relay-reported
   * account tier — never from free_remaining (a paid account with unused free
   * requests is still "paid"; forcing such users into the free-tier UI was a
   * live-reported bug). Free-remaining is displayed separately regardless.
   */
  tier = $state<"free" | "paid">("free");

  /** Whole coins for display; null while the balance is unknown. */
  readonly coins = $derived(
    this.coinBalanceMicro == null ? null : Math.floor(this.coinBalanceMicro / MICRO_PER_COIN),
  );

  /** THE one place a get_balance response becomes state. */
  applyBalance(bal: BalanceInfo) {
    this.freeRemaining = bal.free_remaining;
    this.coinBalanceMicro = bal.coin_balance_microdollars;
    this.tier = bal.tier === "paid" ? "paid" : "free";
  }

  /** Relay per-request header X-Free-Remaining (backend `balance_update` event). */
  applyFreeRemaining(n: number) {
    this.freeRemaining = n;
  }

  /**
   * Relay per-request header X-Coin-Balance (backend `coin_balance_update`
   * event). Only paid requests carry it, so it also proves the account is paid.
   */
  applyCoinBalance(micro: number) {
    this.coinBalanceMicro = micro;
    this.tier = "paid";
  }

  /** Backend `trial_exhausted` event — the relay refused for used-up free quota. */
  markFreeExhausted() {
    this.freeRemaining = 0;
  }

  /**
   * Re-fetch from the relay and apply. Returns whether the fetch succeeded.
   * `invoker` exists for the cold-start path (App.svelte's invokeReady retries
   * while Rust setup() is still registering state); defaults to plain invoke.
   */
  async refresh(
    invoker: <T>(cmd: string) => Promise<T> = invoke as <T>(cmd: string) => Promise<T>,
  ): Promise<boolean> {
    try {
      const bal = await invoker<BalanceInfo>("get_balance");
      this.applyBalance(bal);
      return true;
    } catch (_) {
      return false;
    }
  }
}

/** The app-wide billing state. Import and read; mutate only via its methods. */
export const billing = new Billing();
