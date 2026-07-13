// Account identity + Account-tab view routing (S.2.1), shared between
// App.svelte (buyCoins' oauth_required redirect, the account_changed listener)
// and AccountPanel.svelte (the tab UI itself). Form fields and busy flags stay
// local to the panel — this store holds only what crosses that boundary.

import { invoke } from "@tauri-apps/api/core";

export type AccountInfo = { email: string | null; is_anonymous: boolean; providers: string[] };
export type AccountView =
  | "signin"
  | "signup"
  | "verify_signup"
  | "forgot"
  | "verify_reset"
  | "account";

class Account {
  info = $state<AccountInfo | null>(null);
  view = $state<AccountView>("signin");
  error = $state("");
  notice = $state("");

  /** Signed in with a real (non-anonymous) email account? */
  readonly signedIn = $derived(!!this.info && !this.info.is_anonymous && !!this.info.email);
  /** How they authenticated — a Google (OAuth) account's password is managed by
   *  Google, so "Change password" is only offered to email-credential accounts. */
  readonly isGoogle = $derived(!!this.info && this.info.providers.includes("google"));
  readonly hasPassword = $derived(!!this.info && this.info.providers.includes("email"));
  readonly showChangePw = $derived(this.hasPassword || !this.isGoogle);

  /**
   * Fetch the current identity and pick the right Account view. Called when the
   * Account tab opens and on every `account_changed` event. Pass force=true from
   * a flow that has just *completed* (verify success) so it advances past the
   * don't-yank-the-user-out-of-a-multi-step-flow guard.
   */
  async load(force = false) {
    try {
      this.info = await invoke<AccountInfo>("get_account_info");
    } catch (_) {
      this.info = null;
    }
    if (!force && (this.view === "verify_signup" || this.view === "verify_reset" || this.view === "forgot")) {
      return;
    }
    this.view = this.signedIn ? "account" : "signin";
  }
}

/** The app-wide account identity/view state. */
export const account = new Account();
