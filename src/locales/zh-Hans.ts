// 简体中文 (Simplified Chinese).
//
// Keep the keys identical to en.ts. A key omitted here falls back to English key by key,
// which is the intended way to ship a partial translation.
//
// Terminology chosen once and used throughout:
//   coins        金币        (not 点数 — the app draws a coin and prices in whole coins)
//   managed      托管        the provider name, translated because it is descriptive
//   quality tier 质量档位
//   top up       充值
//   provider     服务商
//   sign in      登录        (mainland usage; Taiwan says 登入 — see zh-Hant.ts)
//   settings     设置        (Taiwan says 設定)

import type { MessageKey } from "./en";

// Partial on purpose: a key left out falls back to English, which is how a partial
// translation ships. Typed against MessageKey so a key that is MISSPELLED -- the failure
// that looks identical to "not translated yet" -- is a compile error instead.
export const zhHans: Partial<Record<MessageKey, string>> = {
  // ── 通用 ──────────────────────────────────────────────────────────────────
  "common.cancel": "取消",
  "common.close": "关闭",
  "common.email": "邮箱",
  "common.password": "密码",
  "common.emailPlaceholder": "you@example.com",
  "common.or": "或",

  // ── 语言设置 ──────────────────────────────────────────────────────────────
  "lang.label": "语言",
  "lang.systemHint": "系统语言：{locale}",
  "lang.aiNote": "AI 仍然按你输入的语言回答。",

  // ── 账户 ──────────────────────────────────────────────────────────────────
  "acct.signedInAs": "当前账户",
  "acct.coinsStayHere": "你的金币和购买记录都保存在这个账户下。",
  "acct.signInBlurb": "登录后，金币和购买记录可以在多台设备之间保留。",
  "acct.passwordPlaceholder": "你的密码",
  "acct.signIn": "登录",
  "acct.signingIn": "正在登录…",
  "acct.createAccount": "注册账户",
  "acct.forgotPassword": "忘记密码？",
  "acct.neverVerified": "注册过但没验证？",
  "acct.resendVerification": "重新发送验证码",
  "acct.continueGoogle": "使用 Google 登录",
  "acct.waitingGoogle": "等待 Google…",
  "acct.googleUnreachable":
    "一直没反应？你所在的网络可能访问不了 Google 登录页面 —— 上面的邮箱和密码两种情况下都能用。",
  "acct.signUpBlurb": "注册一个账户 —— 你当前的免费次数和已有金币都会保留。",
  "acct.minChars": "至少 {n} 个字符",
  "acct.sendingCode": "正在发送验证码…",
  "acct.haveAccount": "已经有账户了？去登录",
  "acct.verificationCode": "验证码",
  "acct.codeFromEmail": "邮件里的验证码",
  "acct.verifying": "正在验证…",
  "acct.verifyFinish": "验证并完成",
  "acct.resendCode": "重新发送验证码",
  "acct.forgotBlurb": "填写你的账户邮箱，我们会发送一个重置验证码。",
  "acct.sending": "正在发送…",
  "acct.sendResetCode": "发送重置验证码",
  "acct.backToSignIn": "返回登录",
  "acct.resetCode": "重置验证码",
  "acct.newPassword": "新密码",
  "acct.saving": "正在保存…",
  "acct.setNewPassword": "设置新密码",
  "acct.billing": "账单与充值",
  "acct.changePassword": "修改密码",
  "acct.currentPassword": "当前密码",
  "acct.currentPasswordPlaceholder": "你当前的密码",
  "acct.savePassword": "保存密码",
  "acct.googleManaged":
    "你通过 Google 登录 —— 密码由 Google 管理，不在 Navisual 这边。修改请前往",
  "acct.signOut": "退出登录",
  "acct.deleteAccount": "注销账户",
  "acct.deleteWarning": "这会永久删除你的账户。金币不退款，也无法恢复。",
  "acct.deleting": "正在删除…",
  "acct.deletePermanently": "永久删除",

  // 账户提示与错误
  "acct.msg.needEmailPassword": "请填写邮箱，以及至少 {n} 个字符的密码。",
  "acct.msg.codeSent": "请输入我们发送到 {email} 的验证码。已经收到过了？它 1 小时内有效。",
  "acct.msg.alreadyRegistered": "这个邮箱已经注册过了。请输入密码登录。",
  "acct.msg.needEmail": "请先填写邮箱。",
  "acct.msg.newCodeSent": "新的验证码已发送到 {email}，请在下面输入。",
  "acct.msg.passwordChanged": "密码已修改。",
  "acct.msg.needCode": "请输入邮件里的验证码。",
  "acct.msg.needBoth": "请填写邮箱和密码。",
  "acct.msg.unverifiedResent": "你的邮箱还没验证 —— 我们已重新发送验证码到 {email}，请在下面输入。",
  "acct.msg.unverified": "你的邮箱还没验证。请输入我们发送到 {email} 的验证码，或点「重新发送验证码」。",
  "acct.msg.needAccountEmail": "请填写你的账户邮箱。",
  "acct.msg.resetSent": "我们已发送重置验证码到 {email}。请连同新密码一起填写。",
  "acct.msg.needCodeAndPassword": "请输入邮件里的验证码，以及一个至少 {n} 个字符的新密码。",
  "acct.msg.passwordTooShort": "新密码至少需要 {n} 个字符。",

  // ── 账单与充值 ────────────────────────────────────────────────────────────
  "bill.wrongProvider":
    "金币只在「托管」服务商下消耗，而你当前用的是 {provider}。请在「服务商」标签页切换后再使用。",
  "bill.plan": "方案",
  "bill.planPaid": "付费（金币）",
  "bill.planFree": "免费试用",
  "bill.coinBalance": "金币余额",
  "bill.coins": "{n} 金币",
  "bill.freeRequests": "免费次数",
  "bill.freeLeft": "还剩 {n} 次，共 {total} 次",
  "bill.tierHint": "质量档位 —— 由哪个模型回答、以及每次消耗多少金币 —— 在「服务商」标签页设置。",
  "bill.topUpAmount": "充值金额",
  "bill.custom": "自定义…",
  "bill.customPlaceholder": "输入 $5–$500",
  "bill.amountRange": "金额需要在 $5–$500 之间",
  "bill.buyCoins": "购买金币（${amount}）",
  "bill.openingCheckout": "正在打开支付页…",
  "bill.checkoutOpen": "支付页已在浏览器中打开…",
  "bill.refreshBalance": "刷新余额",
  "bill.agreePrefix": "购买金币即表示你同意我们的",
  "bill.terms": "服务条款",
  "bill.and": "和",
  "bill.privacy": "隐私政策",
  "bill.checkoutHint": "支付页会在浏览器中打开；回到应用后余额会自动更新。",

  // ── 免费次数用完 / 金币不足 ───────────────────────────────────────────────
  "trial.titleCoins": "金币不足",
  "trial.titleFree": "免费次数已用完",
  "trial.ariaCoins": "金币不足",
  "trial.ariaFree": "免费试用已用完",
  "trial.bodyCoins": "当前质量档位所需的金币不足。",
  "trial.bodyFree": "你的免费次数已经用完了。",
  "trial.signingInGoogle": "正在浏览器中使用 Google 登录…",
  "trial.openingCheckout": "正在打开支付页…",
  "trial.checkoutOpened": "支付页已在浏览器中打开。付款完成后回到这里 —— 余额会自动更新。",
  "trial.topUpBlurb": "充值金币即可继续使用 Navisual 托管服务。",
  "trial.stillFreeLeft":
    "还有免费次数没用完？在「设置 → 服务商」里把质量档位切换成「免费」就能继续用。",
  "trial.ownKey":
    "也可以用你自己的 API Key 继续免费使用：设置 → 服务商 → Gemini（Google AI Studio）或 Ollama（本地）。",

  // ── 面板界面 (P2a) ────────────────────────────────────────────────────────
  "panel.about": "关于 Navisual",
  "panel.settings": "设置",
  "panel.collapse": "缩成悬浮图标",
  "panel.quit": "退出",

  "panel.next": "\u2192 下一步",
  "panel.nextTitle": "下一步（{hotkey}）",
  "panel.wrong": "\u2717 这步不对",
  "panel.wrongTitle": "这条指引不对（{hotkey}）",
  "panel.autopilotOn": "\u23f8 自动推进",
  "panel.autopilotOff": "\u2708 自动推进",
  "panel.autopilotOnTitle": "自动推进已开启 —— 点击关闭",
  "panel.autopilotOffTitle": "自动推进已关闭 —— 点击开启",
  "panel.newTask": "\uff0b 新任务",
  "panel.newTaskTitle": "清空当前会话，重新开始",
  "panel.historyTitle": "最近的任务 —— 点一个继续做",
  "panel.micStop": "停止录音（{hotkey}）",
  "panel.micStart": "语音输入（{hotkey}）",
  "panel.micDisabled": "请在「设置 → 音频」里开启语音输入",

  "panel.thinking": "思考中\u2026",
  "panel.cancelElapsed": "\u23f9 取消（{secs} 秒）",
  "panel.guideMe": "开始指引",
  "panel.sendAnswer": "\u21a9 发送回答",
  "panel.followUp": "\u21a9 追问",
  "panel.send": "\u21a9 发送",
  "panel.inputHint": "输入追问或纠正 \u00b7 \uff0b 开始新任务",

  "panel.staleText": "我思考的时候屏幕变了 —— 这条指引可能已经过时。",
  "panel.occludedText": "目标窗口不可见 —— 把它切到前面才能看到指针。",
  "panel.reanalyse": "\u21bb 重新分析",
  "panel.reanalyseTitle": "重新分析当前屏幕",
  "panel.dismiss": "关掉",

  "menu.chat": "\U0001f4ac 对话",
  "menu.switchApp": "\U0001f3af 切换应用",
  "menu.dockLeft": "\u25e7 停靠左侧",
  "menu.dockRight": "\u25e8 停靠右侧",
  "menu.unmute": "\U0001f50a 取消静音",
  "menu.mute": "\U0001f507 静音",
  "menu.captionOn": "\U0001f4ac 字幕：开",
  "menu.captionOff": "\U0001f4ac 字幕：关",
  "menu.showPointer": "\U0001f441 显示指针和字幕",
  "menu.clearPointer": "\u2715 清除指针和字幕",
  "menu.expand": "\u2197 展开",
  "menu.collapse": "\u229f 收起",
  "menu.quit": "\u2715 退出",

  // ── 注册促销 ──────────────────────────────────────────────────────────────
  "promo.headline": "注册免费账户，送 {coins} 金币。",
  "promo.sub": "足够你试试 Navisual 中继上更快、更聪明的模型。Navisual 本身一直免费",
  "promo.ends": " —— 活动截止 {date}",
};
