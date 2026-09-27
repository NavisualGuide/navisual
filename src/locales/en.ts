// English — the reference dictionary.
//
// Every other locale falls back to this one key by key (see i18n.svelte.ts), so a key that
// exists here and nowhere else renders in English rather than blank. That makes a partial
// translation read as mixed, which is obvious and harmless, instead of broken.
//
// Keys are `surface.thing`. Interpolation slots are `{name}` and are filled by `t()`.
//
// SCOPE: phase 1 is the consent and money surfaces -- nobody should agree to terms or pay
// for something in a language they cannot read. The main panel chrome and the Settings tabs
// are phases 2 and 3; their strings are still inline in the components.

export const en = {
  // ── Shared ────────────────────────────────────────────────────────────────
  "common.cancel": "Cancel",
  "common.close": "Close",
  "common.email": "Email",
  "common.password": "Password",
  "common.emailPlaceholder": "you@example.com",
  "common.or": "or",

  // ── Language setting ──────────────────────────────────────────────────────
  "lang.label": "Interface language",
  "lang.systemHint": "System language: {locale}",
  "lang.aiNote": "The AI still answers in the language you type in.",

  // ── Account ───────────────────────────────────────────────────────────────
  "acct.signedInAs": "Signed in as",
  "acct.coinsStayHere": "Your coins and purchases stay with this account.",
  "acct.signInBlurb": "Sign in to keep your coins and purchases across devices.",
  "acct.passwordPlaceholder": "Your password",
  "acct.signIn": "Sign in",
  "acct.signingIn": "Signing in…",
  "acct.createAccount": "Create account",
  "acct.forgotPassword": "Forgot password?",
  "acct.neverVerified": "Signed up but never verified?",
  "acct.resendVerification": "Resend verification code",
  "acct.continueGoogle": "Continue with Google",
  "acct.waitingGoogle": "Waiting for Google…",
  "acct.googleUnreachable":
    "Nothing happening? The Google sign-in page may be unreachable from your network — the email and password above work either way.",
  "acct.signUpBlurb": "Create an account — your current free requests and any coins carry over.",
  "acct.minChars": "At least {n} characters",
  "acct.sendingCode": "Sending code…",
  "acct.haveAccount": "Already have an account? Sign in",
  "acct.verificationCode": "Verification code",
  "acct.codeFromEmail": "Code from email",
  "acct.verifying": "Verifying…",
  "acct.verifyFinish": "Verify & finish",
  "acct.resendCode": "Resend code",
  "acct.forgotBlurb": "Enter your account email and we'll send a reset code.",
  "acct.sending": "Sending…",
  "acct.sendResetCode": "Send reset code",
  "acct.backToSignIn": "Back to sign in",
  "acct.resetCode": "Reset code",
  "acct.newPassword": "New password",
  "acct.saving": "Saving…",
  "acct.setNewPassword": "Set new password",
  "acct.billing": "Billing",
  "acct.changePassword": "Change password",
  "acct.currentPassword": "Current password",
  "acct.currentPasswordPlaceholder": "Your current password",
  "acct.savePassword": "Save password",
  "acct.googleManaged":
    "Signed in with Google — your password is managed by Google, not Navisual. Change it at",
  "acct.signOut": "Sign out",
  "acct.deleteAccount": "Delete account",
  "acct.deleteWarning":
    "This permanently deletes your account. Coins are not refunded and cannot be recovered.",
  "acct.deleting": "Deleting…",
  "acct.deletePermanently": "Delete permanently",

  // Account messages (script-side notices and errors)
  "acct.msg.needEmailPassword": "Enter an email and a password of at least {n} characters.",
  "acct.msg.codeSent":
    "Enter the verification code we emailed to {email}. Already requested one? It's valid for 1 hour.",
  "acct.msg.alreadyRegistered": "This email already has an account. Enter your password to sign in.",
  "acct.msg.needEmail": "Enter your email first.",
  "acct.msg.newCodeSent": "New code sent to {email}. Enter it below.",
  "acct.msg.passwordChanged": "Password changed.",
  "acct.msg.needCode": "Enter the code from your email.",
  "acct.msg.needBoth": "Enter your email and password.",
  "acct.msg.unverifiedResent":
    "Your email isn't verified yet — we sent a new code to {email}. Enter it below.",
  "acct.msg.unverified":
    "Your email isn't verified yet. Enter the code we emailed to {email}, or tap Resend code.",
  "acct.msg.needAccountEmail": "Enter your account email.",
  "acct.msg.resetSent": "We sent a reset code to {email}. Enter it with your new password.",
  "acct.msg.needCodeAndPassword":
    "Enter the code from your email and a new password (min {n} characters).",
  "acct.msg.passwordTooShort": "New password must be at least {n} characters.",

  // ── Billing ───────────────────────────────────────────────────────────────
  "bill.wrongProvider":
    "Coins are spent by the Managed provider, and you're on {provider}. Switch on the Provider tab to use them.",
  "bill.plan": "Plan",
  "bill.planPaid": "Paid (coins)",
  "bill.planFree": "Free trial",
  "bill.coinBalance": "Coin balance",
  "bill.coins": "{n} coins",
  "bill.freeRequests": "Free requests",
  "bill.freeLeft": "{n} of {total} left",
  "bill.tierHint": "Quality tier — which model answers, and its coin cost — is on the Provider tab.",
  "bill.topUpAmount": "Top-up amount",
  "bill.custom": "Custom…",
  "bill.customPlaceholder": "Enter $5–$500",
  "bill.amountRange": "Amount must be $5–$500",
  "bill.buyCoins": "Buy coins (${amount})",
  "bill.openingCheckout": "Opening checkout…",
  "bill.checkoutOpen": "Checkout open in browser…",
  "bill.refreshBalance": "Refresh balance",
  "bill.agreePrefix": "By buying coins you agree to our",
  "bill.terms": "Terms",
  "bill.and": "and",
  "bill.privacy": "Privacy Policy",
  "bill.checkoutHint": "Checkout opens in your browser; your balance updates when you return.",

  // ── Trial exhausted / not enough coins ────────────────────────────────────
  "trial.titleCoins": "Not enough coins",
  "trial.titleFree": "Free trial used",
  "trial.ariaCoins": "Not enough coins",
  "trial.ariaFree": "Free trial exhausted",
  "trial.bodyCoins": "Not enough coins for this quality tier.",
  "trial.bodyFree": "Your free requests have been used.",
  "trial.signingInGoogle": "Signing in with Google in your browser…",
  "trial.openingCheckout": "Opening checkout…",
  "trial.checkoutOpened":
    "Checkout opened in your browser. Come back once you've paid — your balance will update automatically.",
  "trial.topUpBlurb": "Top up with coins to continue on the Navisual managed relay.",
  "trial.stillFreeLeft":
    "Still have free requests left? Switch Quality tier to Free on Settings → Provider to use them instead.",
  "trial.ownKey":
    "Or keep going free with your own key: Settings → Provider → Gemini (Google AI Studio) or Ollama (local).",

  // ── Panel chrome (P2a) ────────────────────────────────────────────────────
  "panel.about": "About Navisual",
  "panel.settings": "Settings",
  "panel.collapse": "Collapse to floating icon",
  "panel.quit": "Quit",

  "panel.next": "\u2192 Next",
  "panel.nextTitle": "Next step ({hotkey})",
  "panel.wrong": "\u2717 This is wrong",
  "panel.wrongTitle": "This guidance is wrong ({hotkey})",
  "panel.autopilotOn": "\u23f8 Autopilot",
  "panel.autopilotOff": "\u2708 Autopilot",
  "panel.autopilotOnTitle": "Autopilot on \u2014 click to turn off",
  "panel.autopilotOffTitle": "Autopilot off \u2014 click to turn on",
  "panel.newTask": "\uff0b New task",
  "panel.newTaskTitle": "Clear session and start fresh",
  "panel.historyTitle": "Recent tasks \u2014 reopen one to carry on",
  "panel.micStop": "Stop recording ({hotkey})",
  "panel.micStart": "Voice input ({hotkey})",
  "panel.micDisabled": "Enable voice input in Settings \u2192 Audio",

  "panel.thinking": "Thinking\u2026",
  "panel.cancelElapsed": "\u23f9 Cancel ({secs}s)",
  "panel.guideMe": "Guide me",
  "panel.sendAnswer": "\u21a9 Send answer",
  "panel.followUp": "\u21a9 Follow up",
  "panel.send": "\u21a9 Send",
  "panel.inputHint": "Type a follow-up or correction \u00b7 \uff0b for a new task",

  "panel.staleText": "Screen changed while I was thinking \u2014 this guidance may be out of date.",
  "panel.occludedText": "Target window isn't visible \u2014 bring it to the front to see the pointer.",
  "panel.reanalyse": "\u21bb Re-analyse",
  "panel.reanalyseTitle": "Re-analyse the current screen",
  "panel.dismiss": "Dismiss",

  "menu.chat": "\U0001f4ac Chat",
  "menu.switchApp": "\U0001f3af Switch app",
  "menu.dockLeft": "\u25e7 Dock left",
  "menu.dockRight": "\u25e8 Dock right",
  "menu.unmute": "\U0001f50a Unmute",
  "menu.mute": "\U0001f507 Mute",
  "menu.captionOn": "\U0001f4ac Caption: on",
  "menu.captionOff": "\U0001f4ac Caption: off",
  "menu.showPointer": "\U0001f441 Show pointer & caption",
  "menu.clearPointer": "\u2715 Clear pointer & caption",
  "menu.expand": "\u2197 Expand",
  "menu.collapse": "\u229f Collapse",
  "menu.quit": "\u2715 Quit",

  // ── First-run privacy notice (consent gate) ───────────────────────────────
  // The policy page is the single source of truth; this list carries only facts
  // that describe what Navisual IS, never counts, key names or mechanisms.
  "privacy.aria": "Privacy notice",
  "privacy.title": "Before your first task",
  "privacy.lead": "Navisual captures your active window and sends it to the AI provider you've selected.",
  "privacy.b1": "It captures the window you point it at \u2014 or your whole screen, if you pick that \u2014 and sends the picture to the AI provider you choose.",
  "privacy.b2": "Any screenshot it saves is saved on your own computer \u2014 never on our servers.",
  "privacy.b3strong": "The default free tier uses AI models that may keep your requests \u2014 including the screenshot \u2014 to train on.",
  "privacy.b3rest": "Paid tiers and your own API key don't; Ollama never leaves your machine.",
  "privacy.b4": "While guiding, it notes which control you click in that app \u2014 the control's name, never its contents. It does not monitor your keyboard.",
  "privacy.b5": "Voice input, if you turn it on, sends your audio to Microsoft's speech service.",
  "privacy.policyBefore": "The",
  "privacy.policyLink": "full privacy policy",
  "privacy.policyAfter": "is the complete and authoritative version \u2014 what is captured, where it goes, what is stored, and how to stop it. You can reopen it any time from About.",
  "privacy.accept": "I understand \u2014 continue",

  // ── Signup promotion ──────────────────────────────────────────────────────
  "promo.headline": "Create a free account and get {coins} coins.",
  "promo.sub": "Enough to try the faster, smarter models on the Navisual relay. Navisual stays free to use",
  "promo.ends": " — offer ends {date}",
};

/** Every key the reference dictionary defines. Typing the call site and the other
 *  dictionaries against this is what turns a mistyped key from a string that silently never
 *  renders into a compile error. */
export type MessageKey = keyof typeof en;
