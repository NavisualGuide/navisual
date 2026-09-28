<script lang="ts">
  // Settings → Account tab body (S.2.1), extracted from App.svelte
  // (componentization pass, 2026-07-13). Identity + view routing live in the
  // account store (App's buyCoins oauth_required redirect and the
  // account_changed listener write it too); form fields and busy flags are
  // local — nothing outside this panel ever needs them.
  import { invoke } from "@tauri-apps/api/core";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { account } from "./lib/account.svelte";
  import { billing } from "./lib/billing.svelte";
  import PromoOffer from "./PromoOffer.svelte";
  import BillingPanel from "./BillingPanel.svelte";
  import { t } from "./lib/i18n.svelte";

  let {
    provider,
    onBuy,
    onRefreshBalance,
    onSignedOut,
  }: {
    /** Active AI provider — billing only spends on `managed`, so the panel says so. */
    provider: string;
    onBuy: (amount: number) => Promise<void>;
    onRefreshBalance: () => Promise<void>;
    /** Lets App post the "Signed out — you're back on the free tier." history line. */
    onSignedOut: () => void;
  } = $props();

  /**
   * Billing lives on this tab (merged 2026-09-06 — coins belong to an account, and
   * the two were already handing off to each other: buying while signed out used to
   * switch tabs mid-click, set the sign-in view, and make you press Buy again).
   *
   * Shown on the two RESTING views only. `signin` counts: an anonymous user lands
   * there, and they are exactly who the "N left" / "Free tier" header chips bring
   * here — hiding their remaining-request count and the top-up entry behind a
   * sign-in they have not done yet would be the regression this merge exists to
   * avoid. On `signin` the form sits above billing, so the old bounce is now just
   * scrolling up.
   *
   * Hidden mid-flow (signup / verify_signup / forgot / verify_reset): a checkout
   * button under an OTP field is a trap, since checkout needs a signed-in account
   * and would either fail or hijack the verification. Deliberately an INCLUSION
   * list — a future view added here shows no billing until it is listed, which is
   * the safe way to be wrong.
   */
  const showBilling = $derived(account.view === "account" || account.view === "signin");

  let acctEmail = $state("");
  // 6 was the floor for both this UI and GoTrue's default, which is thin for an
  // account carrying a coin balance. 8 is the usual modern minimum (NIST 800-63B);
  // raising it here only tightens things, since the server minimum is a floor and
  // this app is its only client. The complements are server-side and live in the
  // Supabase dashboard: raise GoTrue's own minimum to match, and turn on leaked-
  // password protection, which checks Have I Been Pwned WITHOUT the app ever
  // talking to a third party itself.
  const MIN_PASSWORD = 8;
  let acctPassword = $state("");
  let acctCode = $state(""); // 6-digit OTP
  let acctNewPassword = $state("");
  // Proof you are the account holder, not just someone at an unlocked machine.
  // The backend verifies it against GoTrue; this field only collects it.
  let acctCurrentPassword = $state("");
  let acctBusy = $state(false);
  let showChangePw = $state(false);
  let showDeleteConfirm = $state(false);

  function resetAcctFields() {
    acctPassword = "";
    acctCode = "";
    acctNewPassword = "";
    account.error = "";
    account.notice = "";
  }

  // Add an email + password to the current anonymous account (in-place upgrade),
  // then move to the OTP-entry step.
  async function acctSignUp() {
    if (acctBusy) return;
    account.error = ""; account.notice = "";
    if (!acctEmail.trim() || acctPassword.length < MIN_PASSWORD) {
      account.error = t("acct.msg.needEmailPassword", { n: MIN_PASSWORD });
      return;
    }
    acctBusy = true;
    try {
      await invoke("sign_up_email", { email: acctEmail.trim(), password: acctPassword });
      account.notice = t("acct.msg.codeSent", { email: acctEmail.trim() });
      account.view = "verify_signup";
    } catch (e) {
      const msg = String(e);
      if (/sign in instead/i.test(msg)) {
        // Email already belongs to a confirmed account — route to sign-in (email stays prefilled).
        account.view = "signin";
        account.notice = t("acct.msg.alreadyRegistered");
      } else {
        account.error = msg;
      }
    } finally {
      acctBusy = false;
    }
  }

  // Resend a fresh sign-up code (used by "Resend code" and the unverified-login path).
  async function acctResend() {
    if (acctBusy) return;
    account.error = ""; account.notice = "";
    if (!acctEmail.trim()) { account.error = t("acct.msg.needEmail"); return; }
    acctBusy = true;
    try {
      await invoke("resend_email_otp", { email: acctEmail.trim() });
      account.notice = t("acct.msg.newCodeSent", { email: acctEmail.trim() });
      account.view = "verify_signup";
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctVerifySignup() {
    if (acctBusy) return;
    account.error = "";
    if (acctCode.trim().length < 6) { account.error = t("acct.msg.needCode"); return; }
    acctBusy = true;
    try {
      await invoke("verify_email_otp", { email: acctEmail.trim(), token: acctCode.trim() });
      resetAcctFields();
      await account.load(true); // flow complete → leave the verify page for "account"
      await onRefreshBalance();
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctSignIn() {
    if (acctBusy) return;
    account.error = ""; account.notice = "";
    if (!acctEmail.trim() || !acctPassword) { account.error = t("acct.msg.needBoth"); return; }
    acctBusy = true;
    try {
      await invoke("sign_in_email", { email: acctEmail.trim(), password: acctPassword });
      resetAcctFields();
      await account.load(); // → "account"
      await onRefreshBalance();
    } catch (e) {
      const msg = String(e);
      if (/EMAIL_NOT_CONFIRMED/i.test(msg)) {
        // Account exists but its email was never verified → finish verification.
        account.view = "verify_signup";
        try {
          await invoke("resend_email_otp", { email: acctEmail.trim() });
          account.notice = t("acct.msg.unverifiedResent", { email: acctEmail.trim() });
        } catch {
          account.notice = t("acct.msg.unverified", { email: acctEmail.trim() });
        }
      } else {
        account.error = msg;
      }
    } finally {
      acctBusy = false;
    }
  }

  async function acctSignOut() {
    if (acctBusy) return;
    acctBusy = true; account.error = "";
    try {
      await invoke("sign_out"); // backend re-signs anonymously (free quota is per-device)
      account.info = null;
      acctEmail = "";
      resetAcctFields();
      account.view = "signin";
      showChangePw = false;
      showDeleteConfirm = false;
      await account.load();
      await onRefreshBalance(); // re-derives billing.tier from the fresh anon session's real tier ('free')
      onSignedOut();
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctForgot() {
    if (acctBusy) return;
    account.error = ""; account.notice = "";
    if (!acctEmail.trim()) { account.error = t("acct.msg.needAccountEmail"); return; }
    acctBusy = true;
    try {
      await invoke("request_password_reset", { email: acctEmail.trim() });
      account.notice = t("acct.msg.resetSent", { email: acctEmail.trim() });
      account.view = "verify_reset";
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctVerifyReset() {
    if (acctBusy) return;
    account.error = "";
    if (acctCode.trim().length < 6 || acctNewPassword.length < MIN_PASSWORD) {
      account.error = t("acct.msg.needCodeAndPassword", { n: MIN_PASSWORD });
      return;
    }
    acctBusy = true;
    try {
      await invoke("verify_password_reset", {
        email: acctEmail.trim(),
        token: acctCode.trim(),
        newPassword: acctNewPassword,
      });
      resetAcctFields();
      await account.load(true); // reset complete → leave the verify page for "account"
      await onRefreshBalance();
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctChangePassword() {
    if (acctBusy) return;
    account.error = ""; account.notice = "";
    if (acctNewPassword.length < MIN_PASSWORD) {
      account.error = t("acct.msg.passwordTooShort", { n: MIN_PASSWORD });
      return;
    }
    acctBusy = true;
    try {
      await invoke("change_password", {
        currentPassword: acctCurrentPassword,
        newPassword: acctNewPassword,
      });
      acctNewPassword = "";
      acctCurrentPassword = "";
      showChangePw = false;
      account.notice = t("acct.msg.passwordChanged");
    } catch (e) {
      account.error = String(e);
    } finally {
      acctBusy = false;
    }
  }

  async function acctDeleteAccount() {
    if (acctBusy) return;
    acctBusy = true; account.error = "";
    try {
      await invoke("delete_account"); // backend re-signs anonymously (free quota is per-device)
      account.info = null;
      acctEmail = "";
      resetAcctFields();
      showDeleteConfirm = false;
      showChangePw = false;
      account.view = "signin";
      await account.load();
      await onRefreshBalance();
    } catch (e) {
      account.error = String(e);
      showDeleteConfirm = false;
    } finally {
      acctBusy = false;
    }
  }

  // Set while the user's own Cancel is in flight. `start_google_oauth` rejects when the
  // wait is dropped, and a rejection the user asked for is not an error to show them.
  let oauthCancelled = $state(false);

  async function acctGoogle() {
    if (billing.oauthPending) return;
    billing.oauthPending = true;
    oauthCancelled = false;
    account.error = "";
    // Any second-window notice belongs to the PREVIOUS attempt.
    account.notice = "";
    try {
      await invoke("start_google_oauth");
      await account.load();
      await onRefreshBalance();
    } catch (e) {
      if (!oauthCancelled) account.error = String(e);
    } finally {
      billing.oauthPending = false;
    }
  }

  // Without this the button is disabled for the full 240 s budget, and a network that
  // cannot reach accounts.google.com makes every attempt take exactly that long.
  async function acctGoogleCancel() {
    oauthCancelled = true;
    try {
      await invoke("cancel_google_oauth");
    } catch (_) {
      // Nothing was waiting; the finally in acctGoogle still clears the flag.
    }
  }
</script>

{#if account.error}<p class="setting-hint acct-error">⚠️ {account.error}</p>{/if}
{#if account.notice}<p class="setting-hint acct-notice">{account.notice}</p>{/if}

{#if account.view === "account"}
  <div class="setting-group">
    <span class="setting-label">{t("acct.signedInAs")}</span>
    <p class="setting-hint"><strong>{account.info?.email}</strong></p>
  </div>
  {#if billing.coins !== null && billing.coins > 0}
    <!-- The number itself is in the Billing section below now; what belongs up
         here beside the identity is which account owns it. -->
    <p class="setting-hint">{t("acct.coinsStayHere")}</p>
  {/if}

{:else if account.view === "signin"}
  <!-- Above the form on purpose: it is the reason to fill the form in. PromoOffer
       renders nothing when no campaign is live or the user is already signed in, so
       there is no condition to keep in sync here. -->
  <PromoOffer />
  <p class="setting-hint">{t("acct.signInBlurb")}</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email">{t("common.email")}</label>
    <input id="acct-email" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder={t("common.emailPlaceholder")} />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-pw">{t("common.password")}</label>
    <input id="acct-pw" class="setting-input" type="password" autocomplete="current-password" bind:value={acctPassword} placeholder={t("acct.passwordPlaceholder")} />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctSignIn} disabled={acctBusy}>{acctBusy ? t("acct.signingIn") : t("acct.signIn")}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signup"; }}>{t("acct.createAccount")}</button>
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "forgot"; }}>{t("acct.forgotPassword")}</button>
  </div>
  <p class="setting-hint" style="margin-top: 8px;">
    {t("acct.neverVerified")}
    <button class="legal-link" onclick={acctResend} disabled={acctBusy}>{t("acct.resendVerification")}</button>
  </p>
  <!-- An "or", not a bare rule. This button is a second way to do the thing
       above it, but a plain separator detached it from the sign-in block and
       glued it to the Billing heading below — which has its own separator, so
       the button sat orphaned between two rules and read as billing. Reported
       live 2026-09-10: "it is not clear if it is about account or billing". -->
  <div class="acct-or">{t("common.or")}</div>
  <button class="btn-ghost" onclick={acctGoogle} disabled={billing.oauthPending}>
    <!-- "Waiting for Google", not "Signing in": through both windows the app
         is idle and the user is the one being waited on. -->
    {billing.oauthPending ? t("acct.waitingGoogle") : t("acct.continueGoogle")}
  </button>
  {#if billing.oauthPending}
    <!-- Names the likely cause rather than just offering an escape. Where the consent
         page is unreachable this is the ONLY thing on screen for four minutes, and the
         email form above it is the way out that does work. -->
    <p class="setting-hint" style="margin-top: 6px;">
      {t("acct.googleUnreachable")}
      <button class="legal-link" onclick={acctGoogleCancel}>{t("common.cancel")}</button>
    </p>
  {/if}

{:else if account.view === "signup"}
  <PromoOffer />
  <p class="setting-hint">{t("acct.signUpBlurb")}</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email-up">{t("common.email")}</label>
    <input id="acct-email-up" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder={t("common.emailPlaceholder")} />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-pw-up">{t("common.password")}</label>
    <input id="acct-pw-up" class="setting-input" type="password" autocomplete="new-password" bind:value={acctPassword} placeholder={t("acct.minChars", { n: MIN_PASSWORD })} />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctSignUp} disabled={acctBusy}>{acctBusy ? t("acct.sendingCode") : t("acct.createAccount")}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>{t("acct.haveAccount")}</button>
  </div>

{:else if account.view === "verify_signup"}
  <div class="setting-group">
    <label class="setting-label" for="acct-code">{t("acct.verificationCode")}</label>
    <input id="acct-code" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder={t("acct.codeFromEmail")} />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctVerifySignup} disabled={acctBusy}>{acctBusy ? t("acct.verifying") : t("acct.verifyFinish")}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={acctResend} disabled={acctBusy}>{t("acct.resendCode")}</button>
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>{t("common.cancel")}</button>
  </div>

{:else if account.view === "forgot"}
  <p class="setting-hint">{t("acct.forgotBlurb")}</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email-fp">{t("common.email")}</label>
    <input id="acct-email-fp" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder={t("common.emailPlaceholder")} />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctForgot} disabled={acctBusy}>{acctBusy ? t("acct.sending") : t("acct.sendResetCode")}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>{t("acct.backToSignIn")}</button>
  </div>

{:else if account.view === "verify_reset"}
  <div class="setting-group">
    <label class="setting-label" for="acct-code-r">{t("acct.resetCode")}</label>
    <input id="acct-code-r" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder={t("acct.codeFromEmail")} />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-newpw-r">{t("acct.newPassword")}</label>
    <input id="acct-newpw-r" class="setting-input" type="password" autocomplete="new-password" bind:value={acctNewPassword} placeholder={t("acct.minChars", { n: MIN_PASSWORD })} />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctVerifyReset} disabled={acctBusy}>{acctBusy ? t("acct.saving") : t("acct.setNewPassword")}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>{t("common.cancel")}</button>
  </div>
{/if}

{#if showBilling}
  <hr class="acct-sep" />
  <p class="setting-label acct-billing-heading">{t("acct.billing")}</p>
  <BillingPanel {provider} {onBuy} {onRefreshBalance} />
{/if}

{#if account.view === "account"}
  <!-- Account actions and the danger zone sit AFTER billing so the destructive
       action stays the floor of the page. Before the 2026-09-06 merge these were
       the end of the tab; appending billing under them buried "Delete account"
       mid-page, directly above a Buy button. -->
  {#if account.showChangePw}
    {#if !showChangePw}
      <div class="setting-group" style="margin-top: 12px;">
        <button class="btn-ghost" onclick={() => { showChangePw = true; account.error = ""; account.notice = ""; }}>{t("acct.changePassword")}</button>
      </div>
    {:else}
      <div class="setting-group" style="margin-top: 12px;">
        <!-- Proving you know the current one is what stops someone at an unlocked
             machine taking the account. Only shown when there IS one to prove:
             a Google account setting its first Navisual password has none, and
             the backend applies the same rule rather than trusting this form. -->
        {#if account.hasPassword}
          <label class="setting-label" for="acct-curpw">{t("acct.currentPassword")}</label>
          <input id="acct-curpw" class="setting-input" type="password" autocomplete="current-password"
            bind:value={acctCurrentPassword} placeholder={t("acct.currentPasswordPlaceholder")} />
        {/if}
        <label class="setting-label" for="acct-newpw" style="margin-top:8px;">{t("acct.newPassword")}</label>
        <input id="acct-newpw" class="setting-input" type="password" autocomplete="new-password"
          bind:value={acctNewPassword} placeholder={t("acct.minChars", { n: MIN_PASSWORD })} />
        <div style="display:flex; gap:8px; margin-top:8px;">
          <button class="btn-primary" onclick={acctChangePassword} disabled={acctBusy}>{acctBusy ? t("acct.saving") : t("acct.savePassword")}</button>
          <button class="btn-ghost" onclick={() => { showChangePw = false; acctNewPassword = ""; acctCurrentPassword = ""; }}>{t("common.cancel")}</button>
        </div>
      </div>
    {/if}
  {:else if account.isGoogle}
    <p class="setting-hint" style="margin-top: 12px;">
      {t("acct.googleManaged")}
      <button class="legal-link" onclick={() => openUrl("https://myaccount.google.com/security")}>myaccount.google.com</button>.
    </p>
  {/if}

  <div class="setting-group" style="margin-top: 12px;">
    <button class="btn-ghost" onclick={acctSignOut} disabled={acctBusy}>{t("acct.signOut")}</button>
  </div>

  <hr class="acct-sep" />
  {#if !showDeleteConfirm}
    <button class="legal-link acct-danger" onclick={() => { showDeleteConfirm = true; account.error = ""; }}>{t("acct.deleteAccount")}</button>
  {:else}
    <div class="setting-group">
      <p class="setting-hint acct-error">{t("acct.deleteWarning")}</p>
      <div style="display:flex; gap:8px; margin-top:8px;">
        <button class="btn-danger" onclick={acctDeleteAccount} disabled={acctBusy}>{acctBusy ? t("acct.deleting") : t("acct.deletePermanently")}</button>
        <button class="btn-ghost" onclick={() => (showDeleteConfirm = false)}>{t("common.cancel")}</button>
      </div>
    </div>
  {/if}
{/if}
