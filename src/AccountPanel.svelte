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

  let {
    onRefreshBalance,
    onSignedOut,
  }: {
    onRefreshBalance: () => Promise<void>;
    /** Lets App post the "Signed out — you're back on the free tier." history line. */
    onSignedOut: () => void;
  } = $props();

  let acctEmail = $state("");
  let acctPassword = $state("");
  let acctCode = $state(""); // 6-digit OTP
  let acctNewPassword = $state("");
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
    if (!acctEmail.trim() || acctPassword.length < 6) {
      account.error = "Enter an email and a password of at least 6 characters.";
      return;
    }
    acctBusy = true;
    try {
      await invoke("sign_up_email", { email: acctEmail.trim(), password: acctPassword });
      account.notice = `Enter the verification code we emailed to ${acctEmail.trim()}. Already requested one? It's valid for 1 hour.`;
      account.view = "verify_signup";
    } catch (e) {
      const msg = String(e);
      if (/sign in instead/i.test(msg)) {
        // Email already belongs to a confirmed account — route to sign-in (email stays prefilled).
        account.view = "signin";
        account.notice = "This email already has an account. Enter your password to sign in.";
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
    if (!acctEmail.trim()) { account.error = "Enter your email first."; return; }
    acctBusy = true;
    try {
      await invoke("resend_email_otp", { email: acctEmail.trim() });
      account.notice = `New code sent to ${acctEmail.trim()}. Enter it below.`;
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
    if (acctCode.trim().length < 6) { account.error = "Enter the code from your email."; return; }
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
    if (!acctEmail.trim() || !acctPassword) { account.error = "Enter your email and password."; return; }
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
          account.notice = `Your email isn't verified yet — we sent a new code to ${acctEmail.trim()}. Enter it below.`;
        } catch {
          account.notice = `Your email isn't verified yet. Enter the code we emailed to ${acctEmail.trim()}, or tap Resend code.`;
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
    if (!acctEmail.trim()) { account.error = "Enter your account email."; return; }
    acctBusy = true;
    try {
      await invoke("request_password_reset", { email: acctEmail.trim() });
      account.notice = `We sent a reset code to ${acctEmail.trim()}. Enter it with your new password.`;
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
    if (acctCode.trim().length < 6 || acctNewPassword.length < 6) {
      account.error = "Enter the code from your email and a new password (min 6 characters).";
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
    if (acctNewPassword.length < 6) { account.error = "New password must be at least 6 characters."; return; }
    acctBusy = true;
    try {
      await invoke("change_password", { newPassword: acctNewPassword });
      acctNewPassword = "";
      showChangePw = false;
      account.notice = "Password changed.";
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

  async function acctGoogle() {
    if (billing.oauthPending) return;
    billing.oauthPending = true;
    account.error = "";
    try {
      await invoke("start_google_oauth");
      await account.load();
      await onRefreshBalance();
    } catch (e) {
      account.error = String(e);
    } finally {
      billing.oauthPending = false;
    }
  }
</script>

{#if account.error}<p class="setting-hint acct-error">⚠️ {account.error}</p>{/if}
{#if account.notice}<p class="setting-hint acct-notice">{account.notice}</p>{/if}

{#if account.view === "account"}
  <div class="setting-group">
    <span class="setting-label">Signed in as</span>
    <p class="setting-hint"><strong>{account.info?.email}</strong></p>
  </div>
  {#if billing.coins !== null && billing.coins > 0}
    <p class="setting-hint">{billing.coins} coins · your balance and purchases stay with this account.</p>
  {/if}

  {#if account.showChangePw}
    {#if !showChangePw}
      <div class="setting-group" style="margin-top: 12px;">
        <button class="btn-ghost" onclick={() => { showChangePw = true; account.error = ""; account.notice = ""; }}>Change password</button>
      </div>
    {:else}
      <div class="setting-group" style="margin-top: 12px;">
        <label class="setting-label" for="acct-newpw">New password</label>
        <input id="acct-newpw" class="setting-input" type="password" bind:value={acctNewPassword} placeholder="At least 6 characters" />
        <div style="display:flex; gap:8px; margin-top:8px;">
          <button class="btn-primary" onclick={acctChangePassword} disabled={acctBusy}>{acctBusy ? "Saving…" : "Save password"}</button>
          <button class="btn-ghost" onclick={() => { showChangePw = false; acctNewPassword = ""; }}>Cancel</button>
        </div>
      </div>
    {/if}
  {:else if account.isGoogle}
    <p class="setting-hint" style="margin-top: 12px;">
      Signed in with Google — your password is managed by Google, not Navisual. Change it at
      <button class="legal-link" onclick={() => openUrl("https://myaccount.google.com/security")}>myaccount.google.com</button>.
    </p>
  {/if}

  <div class="setting-group" style="margin-top: 12px;">
    <button class="btn-ghost" onclick={acctSignOut} disabled={acctBusy}>Sign out</button>
  </div>

  <hr class="acct-sep" />
  {#if !showDeleteConfirm}
    <button class="legal-link acct-danger" onclick={() => { showDeleteConfirm = true; account.error = ""; }}>Delete account</button>
  {:else}
    <div class="setting-group">
      <p class="setting-hint acct-error">This permanently deletes your account. Coins are <strong>not</strong> refunded and cannot be recovered.</p>
      <div style="display:flex; gap:8px; margin-top:8px;">
        <button class="btn-danger" onclick={acctDeleteAccount} disabled={acctBusy}>{acctBusy ? "Deleting…" : "Delete permanently"}</button>
        <button class="btn-ghost" onclick={() => (showDeleteConfirm = false)}>Cancel</button>
      </div>
    </div>
  {/if}

{:else if account.view === "signin"}
  <p class="setting-hint">Sign in to keep your coins and purchases across devices.</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email">Email</label>
    <input id="acct-email" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-pw">Password</label>
    <input id="acct-pw" class="setting-input" type="password" autocomplete="current-password" bind:value={acctPassword} placeholder="Your password" />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctSignIn} disabled={acctBusy}>{acctBusy ? "Signing in…" : "Sign in"}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signup"; }}>Create account</button>
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "forgot"; }}>Forgot password?</button>
  </div>
  <p class="setting-hint" style="margin-top: 8px;">
    Signed up but never verified?
    <button class="legal-link" onclick={acctResend} disabled={acctBusy}>Resend verification code</button>
  </p>
  <hr class="acct-sep" />
  <button class="btn-ghost" onclick={acctGoogle} disabled={billing.oauthPending}>
    {billing.oauthPending ? "Signing in…" : "Continue with Google"}
  </button>

{:else if account.view === "signup"}
  <p class="setting-hint">Create an account — your current free requests and any coins carry over.</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email-up">Email</label>
    <input id="acct-email-up" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-pw-up">Password</label>
    <input id="acct-pw-up" class="setting-input" type="password" autocomplete="new-password" bind:value={acctPassword} placeholder="At least 6 characters" />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctSignUp} disabled={acctBusy}>{acctBusy ? "Sending code…" : "Create account"}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>Already have an account? Sign in</button>
  </div>

{:else if account.view === "verify_signup"}
  <div class="setting-group">
    <label class="setting-label" for="acct-code">Verification code</label>
    <input id="acct-code" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder="Code from email" />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctVerifySignup} disabled={acctBusy}>{acctBusy ? "Verifying…" : "Verify & finish"}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={acctResend} disabled={acctBusy}>Resend code</button>
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>Cancel</button>
  </div>

{:else if account.view === "forgot"}
  <p class="setting-hint">Enter your account email and we'll send a reset code.</p>
  <div class="setting-group">
    <label class="setting-label" for="acct-email-fp">Email</label>
    <input id="acct-email-fp" class="setting-input" type="email" autocomplete="username" bind:value={acctEmail} placeholder="you@example.com" />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctForgot} disabled={acctBusy}>{acctBusy ? "Sending…" : "Send reset code"}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>Back to sign in</button>
  </div>

{:else if account.view === "verify_reset"}
  <div class="setting-group">
    <label class="setting-label" for="acct-code-r">Reset code</label>
    <input id="acct-code-r" class="setting-input" inputmode="numeric" maxlength="10" bind:value={acctCode} placeholder="Code from email" />
  </div>
  <div class="setting-group">
    <label class="setting-label" for="acct-newpw-r">New password</label>
    <input id="acct-newpw-r" class="setting-input" type="password" autocomplete="new-password" bind:value={acctNewPassword} placeholder="At least 6 characters" />
  </div>
  <div class="setting-group" style="margin-top: 10px;">
    <button class="btn-primary" onclick={acctVerifyReset} disabled={acctBusy}>{acctBusy ? "Saving…" : "Set new password"}</button>
  </div>
  <div class="acct-links">
    <button class="legal-link" onclick={() => { resetAcctFields(); account.view = "signin"; }}>Cancel</button>
  </div>
{/if}
