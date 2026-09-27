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
  "lang.aiNote": "The Navisual AI still answers in the language you type in.",

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

  "menu.chat": "💬 Chat",
  "menu.switchApp": "🎯 Switch app",
  "menu.dockLeft": "\u25e7 Dock left",
  "menu.dockRight": "\u25e8 Dock right",
  "menu.unmute": "🔊 Unmute",
  "menu.mute": "🔇 Mute",
  "menu.captionOn": "💬 Caption: on",
  "menu.captionOff": "💬 Caption: off",
  "menu.showPointer": "👁 Show pointer & caption",
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

  // ── Target picker, header chip, docking (P2b) ─────────────────────────────
  "target.chooseApp": "Choose target app",
  "target.chooseDockApp": "Choose the app to fill the rest of the screen",
  "target.chipFullScreen": "Sharing your screen \u2014 click to switch target",
  "target.chipPinned": "Target app pinned \u2014 click to switch or unpin",
  "target.chipDefault": "Target app \u2014 click to switch or pin",
  "target.autoDetect": "Auto-detect",
  "target.autoDetectSub": "follow the foreground window",
  "target.entireDesktop": "Entire desktop",
  "target.entireDesktopSub": "share the whole screen \u2014 all windows",
  "target.screenN": "Screen {n}",
  "target.minimized": "Minimized",
  "target.dockHead": "Which app should fill the rest?",
  "target.pickAlsoFills": "Picking an app also fills the rest of the screen with it",
  "dock.fillRest": "\u2b12 Fill the rest with\u2026",
  "dock.undock": "\u2b1c Undock",
  "dock.undockTitle": "Float the panel again",

  // ── The Wrong picker (P2b) ────────────────────────────────────────────────
  // These are DISPLAY labels only. The value sent to the backend is the category
  // key (wrong_spot, not_found, ...), never the label, so translating is safe.
  "wrong.prompt": "What went wrong?",
  "wrong.generic": "Wrong",
  "wrong.instruction": "Wrong instruction",
  "wrong.spot": "Wrong spot",
  "wrong.notFound": "Can't find it",
  "wrong.alreadyDone": "Already did that",
  "wrong.other": "Other",
  "wrong.hintOther": "Not one of these? Type what's wrong below, then \u21a9 Follow up.",
  "wrong.hintApp": "Wrong app? Click the correct window first, then press \u2717 Wrong.",

  // ── Status, plan, coach marks (P2c) ───────────────────────────────────────
  "status.workingOn": "Working on",
  "status.plannedRoute": "🗺\ufe0f Planned route",
  "status.planFootnote": "This adapts as Navisual learns more \u2014 not a fixed route.",
  "status.planEmpty": "No route mapped out yet \u2014 it'll appear here once Navisual has a clearer picture of the steps ahead.",
  "status.copied": "📋 copied",
  "status.copiedTitle": "Text copied to clipboard",
  "status.behindPanel": "\u25ce This looks like it's behind this panel \u2014 drag the panel aside to reveal the highlighted spot.",
  "status.noPointer": "\u2298 Pointer unavailable \u2014 follow the instruction above",
  "status.needsInput": "💬 AI needs your input \u2014 type your answer below",

  "balance.viewBilling": "View billing",
  "balance.getMore": "Get more requests",
  "balance.freeTier": "Free tier",
  "balance.freeTierTitle": "You're on the free tier \u2014 click for billing",
  "balance.nLeft": "{n} left",

  "hint.targetChip": "Click here to select the app you want me to assist with.",
  "hint.collapse": "In your way? Click here to shrink Navisual to a small floating icon.",
  "hint.stillDriving": "Navisual is still driving",
  "hint.pressBefore": "Press",
  "hint.pressAfter": "for the next step \u2014 no need to open the panel.",
  "hint.gotIt": "Got it",

  // ── Session picker and export (P2c) ───────────────────────────────────────
  "sess.saveThis": "💾 Save this session",
  "sess.saveThisTitle": "Save this session \u2014 steps, screenshots and the conversation \u2014 to a folder",
  "sess.recent": "Recent tasks",
  "sess.loading": "Loading\u2026",
  "sess.empty": "No earlier tasks yet. They're saved here as you go.",
  "sess.open": "open",

  "exp.aria": "Save this session",
  "exp.title": "Save this session",
  "exp.savedTo": "Saved to",
  "exp.openFolderTitle": "Open this folder",
  "exp.openFolder": "📂 Open folder",
  "exp.copied": "\u2713 Copied",
  "exp.copyPath": "\u29c9 Copy path",
  "exp.reviewBeforeSharing": "Look through the folder before sending it to anyone: the screenshots are pictures of your screen.",
  "exp.nothingYet": "Nothing recorded yet. Run a step or two first.",
  "exp.fieldTitle": "Title",
  "exp.titlePlaceholder": "What was this session about?",
  "exp.fieldFolder": "Folder",
  "exp.browse": "Browse\u2026",
  "exp.browseTitle": "Choose a different folder",
  "exp.optClean": "Save the plain screenshots",
  "exp.optPointer": "Add the pointer",
  "exp.optCaption": "Add the instruction as a caption",
  "exp.optCrop": "Crop to the app",
  "exp.hintCropped": "\u2014 hides the Navisual panel",
  "exp.noCleanWarning": "Without the plain screenshots you cannot re-do the pointer or caption later \u2014 those are rebuilt from the untouched originals.",

  "lightbox.loading": "Loading\u2026",
  "lightbox.close": "Click anywhere to close",

  // ── Collapsed-icon tooltip (P2) ───────────────────────────────────────────
  "icon.tipFailed": "Last request failed \u2014 click to expand and see why",
  "icon.tipThinking": "Navisual is thinking\u2026",
  "icon.tipNeedsInput": "Navisual asked you something \u2014 click to expand",
  "icon.tipStepHotkey": "Step {n} of {total} \u2014 {hotkey} for next \u00b7 click to expand",
  "icon.tipStep": "Step {n} of {total} \u2014 click to expand",
  "icon.tipExpand": "Expand Navisual",
  "icon.askAnswer": "Answer Navisual…",
  "icon.askFollowUp": "Ask a follow-up…",

  // ── Task prefill (P2) ─────────────────────────────────────────────────────
  // Following the INTERFACE language here is what keeps the AI's reply language
  // consistent for someone who never edits the box: a Chinese prefill submitted
  // unchanged is a Chinese request, so rule 13 answers in Chinese. An English
  // prefill in a Chinese panel quietly produced an English answer instead.
  "prefill.showAround": "Show me around {app}",
  "prefill.exploreApp": "Explore this app",

  // Starter tasks curated by the two bundled nav-packs.
  "pack.blenderAround": "Show me around Blender",
  "pack.blenderMove": "Help me move an object",
  "pack.blenderCube": "Help me add a cube to the scene",
  "pack.browserFind": "Help me find something on this page",
  "pack.browserOpen": "Show me how to open a website",
  "pack.browserDownload": "Help me download a file from this page",

  // ── About modal (P2) ──────────────────────────────────────────────────────
  "about.tabAbout": "About",
  "about.tabUsage": "Usage",
  "about.tagline": "The AI guides, you decide.",
  "about.disclaimer": "Navisual uses AI, which can make mistakes. Always verify each suggested action before performing it.",
  "about.userGuide": "User guide",
  "about.privacy": "Privacy",
  "about.sendFeedback": "Send feedback",
  "about.storeUpdates": "Updates are managed by the Microsoft Store.",
  "about.downloading": "Downloading\u2026 {pct}%",
  "about.installed": "\u2713 Installed \u2014 please restart Navisual",
  "about.available": "v{version} available",
  "about.installRestart": "Install & restart",
  "about.checking": "Checking for updates\u2026",
  "about.checkUpdates": "Check for updates",
  "about.license": "Licensed under FSL-1.1-Apache-2.0 \u2014 converts to Apache 2.0 two years after each release.",
  "about.blenderNotice": "The bundled Blender Nav-Pack references Blender's own icon designs (\u00a9 Blender Foundation) for on-screen guidance only. Blender is a registered trademark of the Blender Foundation. Navisual is not affiliated with or endorsed by the Blender Foundation.",

  "usage.navisualAccount": "Navisual account",
  "usage.coinsLine": "\U0001fa99 {coins} coins left \u00b7 {tier} tier \u00b7 {perReq} coins/request",
  "usage.freeLine": "Free tier \u2014 {left} / {total} requests left",
  "usage.ownKeys": "Your own keys \u2014 token usage",
  "usage.today": "Today",
  "usage.thisMonth": "This month",
  "usage.loading": "Loading\u2026",
  "usage.noneYet": "No bring-your-own-key usage recorded yet.",
  "usage.estimatedTotal": "Estimated total",
  "usage.estimateNote": "Estimates only \u2014 based on each provider's published list pricing, which is set by the provider and subject to change. Check your provider's dashboard for actual charges.",
  "usage.reset": "\u21bb Reset usage",
  "usage.ownKey": "Your own key",
  "usage.ownKeyNote": "Requests run on your own {provider} account \u2014 usage and charges are billed by your provider, not Navisual. Check your provider's dashboard for token counts and costs.",
  "usage.localModel": "Local model",
  "usage.localNote": "Running locally with Ollama \u2014 nothing is billed and no usage leaves your machine.",

  // ── Blender add-on offer (P2) ─────────────────────────────────────────────
  "addon.update": "A newer Navisual add-on is available{ver} \u2014 updating keeps tool pointing exact.",
  "addon.install": "Install the Navisual add-on{ver} for exact tool pointing (one-time setup).",
  "addon.verSuffix": " (Blender {version})",
  "addon.whatTitle": "What the add-on does, and what it can't do",
  "addon.what": "What's this?",
  "addon.installBtn": "Install",
  "addon.installing": "Installing\u2026",
  "addon.failed": "Couldn't install: {reason}",
  "addon.noBlender": "no Blender installation found",
  "addon.needsEnable": "Installed. Restart Blender (or press Refresh in Preferences \u2192 Add-ons), then search \u201cNavisual\u201d there and tick its checkbox. One time only.",
  "addon.updated": "Updated. Restart Blender to load the new version.",

  // ── Settings chrome (P3) ──────────────────────────────────────────────────
  "set.title": "Settings",
  "set.tabProvider": "Provider",
  "set.tabAccount": "Account",
  "set.tabScreenGuide": "Screen Guide",
  "set.tabHotkeys": "Hotkeys",
  "set.tabAudio": "Audio",
  "set.tabDeveloper": "Developer",
  "set.saved": "\u2713 Saved \u2014 no restart required",
  "set.applyNote": "Changes take effect when you click Apply",
  "set.resetTitle": "Restores EVERY setting on ALL tabs to its default \u2014 not just this tab. Your API keys are kept, but server addresses and model choices are not: a custom or local provider will need its URL and model set again.",
  "set.resetArmed": "Click again \u2014 resets ALL tabs",
  "set.reset": "Reset all settings",
  "set.saving": "Saving\u2026",
  "set.apply": "Apply",
  "set.ok": "OK",

  // ── Screen Guide tab (P3) ─────────────────────────────────────────────────
  "sg.taskSuggestions": "Task suggestions",
  "sg.taskSuggestionsToggle": "Prefill the task box with suggested next tasks \u2014 you can always type over them",
  "sg.pointerColor": "Pointer color",
  "sg.reset": "Reset",
  "sg.pointerThickness": "Pointer thickness \u2014 {scale}\u00d7{suffix}",
  "sg.default": " (default)",
  "sg.liveCaption": "Live caption",
  "sg.liveCaptionToggle": "Show instruction text at bottom of screen",
  "sg.autopilot": "Autopilot",
  "sg.autopilotToggle": "Automatically move to the next step when the screen changes",
  "sg.less": "Less",
  "sg.more": "More",
  "sg.sensitivityAria": "Autopilot screen-change sensitivity",
  "sg.sensitivityHint": "How much of the screen must change to auto-advance. Less ignores typing and minor updates; More reacts to smaller changes like a dialog opening.",
  "sg.savedSessions": "Saved sessions",
  "sg.savedSessionsToggle": "Keep each step's screenshot with the saved session",
  "sg.savedSessionsHint": "The only setting here that puts a picture of your screen on disk. It keeps the same cropped, masked frame the AI was shown \u2014 never the whole monitor. The most recent {n} sessions are kept; older ones go with their screenshots.",
  "sg.whatYouType": "What you type",
  "sg.whatYouTypeToggle": "Send the task I type along with step outcomes",
  "sg.whatYouTypeHint": "Sent with each step outcome, so we can see what people use Navisual for and which tasks it handles badly. Screenshots are never sent.",
  "sg.whatYouTypeLocal": "If the AI runs on your own machine or network \u2014 localhost, or a box at 192.168.x \u2014 nothing is sent, whatever this says.",
  "sg.whatYouTypeByok": "Your own API key to a provider like Anthropic or OpenAI does send it, unless you turn this off.",
  "sg.privacyPolicy": "Privacy policy",

  // ── Hotkeys tab (P3) ──────────────────────────────────────────────────────
  "hk.intro": "Click a field then press your shortcut combo. Re-registered immediately on Save \u2014 no restart needed.",
  "hk.next": "Next step",
  "hk.wrong": "Mark wrong",
  "hk.pause": "Pause / cancel",
  "hk.icon": "Toggle icon mode",
  "hk.talk": "Voice input (push-to-talk)",
  "hk.backquoteHint": "The ~ (Tilde / Backtick) key is located directly below the Esc key (top left of the keyboard).",

  // ── Audio tab (P3) ────────────────────────────────────────────────────────
  "au.output": "Audio output (TTS)",
  "au.outputToggle": "Enable text-to-speech for instructions",
  "au.preferredVoice": "Preferred voice (optional)",
  "au.voiceAuto": "Auto \u2014 match the language",
  "au.voicesLoading": "Loading voices\u2026",
  "au.voiceHint": "Auto speaks each reply in its own language (using an installed voice for it). A picked voice is used for replies in its language; replies in other languages still auto-pick a matching voice.",
  "au.input": "Voice input",
  "au.inputToggle": "Enable \U0001f3a4 push-to-talk",
  "au.inputHint": "Uses the WebView2 Web Speech API \u2014 audio is sent to Microsoft's online speech service; requires internet and microphone permission.",
  "au.voiceLanguage": "Voice language",
  "au.autoDetect": "Auto-detect",
  "au.voiceLangHint": "Sets both the TTS voice language and the voice-input language. Auto-detect speaks each reply in its own language and uses your OS language for voice input.",

  // ── Signup promotion ──────────────────────────────────────────────────────
  "promo.headline": "Create a free account and get {coins} coins.",
  "promo.sub": "Enough to try the faster, smarter models on the Navisual relay. Navisual stays free to use",
  "promo.ends": " — offer ends {date}",
};

/** Every key the reference dictionary defines. Typing the call site and the other
 *  dictionaries against this is what turns a mistyped key from a string that silently never
 *  renders into a compile error. */
export type MessageKey = keyof typeof en;
