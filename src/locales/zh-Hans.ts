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
  "lang.label": "界面语言",
  "lang.systemHint": "系统语言：{locale}",
  "lang.aiNote": "Navisual AI 仍然按你输入的语言回答。",

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

  "menu.chat": "💬 对话",
  "menu.switchApp": "🎯 切换应用",
  "menu.dockLeft": "\u25e7 停靠左侧",
  "menu.dockRight": "\u25e8 停靠右侧",
  "menu.unmute": "🔊 取消静音",
  "menu.mute": "🔇 静音",
  "menu.captionOn": "💬 字幕：开",
  "menu.captionOff": "💬 字幕：关",
  "menu.showPointer": "👁 显示指针和字幕",
  "menu.clearPointer": "\u2715 清除指针和字幕",
  "menu.expand": "\u2197 展开",
  "menu.collapse": "\u229f 收起",
  "menu.quit": "\u2715 退出",

  // ── 首次运行隐私须知（同意门）─────────────────────────────────────────────
  "privacy.aria": "隐私须知",
  "privacy.title": "开始第一个任务之前",
  "privacy.lead": "Navisual 会截取你当前的窗口，并发送给你选择的 AI 服务商。",
  "privacy.b1": "它截取你指定的那个窗口 —— 如果你选的是整个屏幕，就是整个屏幕 —— 并把画面发送给你选择的 AI 服务商。",
  "privacy.b2": "它保存的任何截图都只存在你自己的电脑上 —— 绝不会存到我们的服务器。",
  "privacy.b3strong": "默认的免费档所用的 AI 模型，可能会保留你的请求 —— 包括截图 —— 用于训练。",
  "privacy.b3rest": "付费档和你自己的 API Key 不会；Ollama 则完全不离开你的电脑。",
  "privacy.b4": "指引过程中，它会记录你在那个应用里点了哪个控件 —— 只记控件的名称，绝不记它的内容。它不监控你的键盘。",
  "privacy.b5": "语音输入若你开启，会把你的音频发送给微软的语音服务。",
  "privacy.policyBefore": "",
  "privacy.policyLink": "完整隐私政策",
  "privacy.policyAfter": "是完整版本，内容以它为准 —— 捕获什么、发往哪里、保存什么、以及如何停止。你随时可以从「关于」里重新打开它。",
  "privacy.accept": "我已了解，继续",

  // ── 目标选择、标题栏芯片、停靠 (P2b) ──────────────────────────────────────
  "target.chooseApp": "选择目标应用",
  "target.chooseDockApp": "选择要占据屏幕其余部分的应用",
  "target.chipFullScreen": "正在共享你的屏幕 —— 点击切换目标",
  "target.chipPinned": "目标应用已固定 —— 点击切换或取消固定",
  "target.chipDefault": "目标应用 —— 点击切换或固定",
  "target.autoDetect": "自动识别",
  "target.autoDetectSub": "跟随当前前台窗口",
  "target.entireDesktop": "整个桌面",
  "target.entireDesktopSub": "共享整个屏幕 —— 所有窗口",
  "target.screenN": "屏幕 {n}",
  "target.minimized": "已最小化",
  "target.dockHead": "让哪个应用占据其余部分？",
  "target.pickAlsoFills": "选中一个应用后，它会同时占满屏幕的其余部分",
  "dock.fillRest": "\u2b12 其余部分放\u2026",
  "dock.undock": "\u2b1c 取消停靠",
  "dock.undockTitle": "让面板重新浮动",

  // ── 报错选择器 (P2b) ──────────────────────────────────────────────────────
  "wrong.prompt": "哪里不对？",
  "wrong.generic": "不对",
  "wrong.instruction": "指引不对",
  "wrong.spot": "位置不对",
  "wrong.notFound": "找不到",
  "wrong.alreadyDone": "已经做过了",
  "wrong.other": "其他",
  "wrong.hintOther": "都不是？在下面写清楚哪里不对，然后点 \u21a9 追问。",
  "wrong.hintApp": "应用选错了？先点一下正确的窗口，再按 \u2717 这步不对。",

  // ── 状态、路线、引导提示 (P2c) ────────────────────────────────────────────
  "status.workingOn": "正在处理",
  "status.plannedRoute": "🗺\ufe0f 计划路线",
  "status.planFootnote": "这条路线会随着 Navisual 了解更多而调整 —— 不是固定的。",
  "status.planEmpty": "还没有规划出路线 —— 等 Navisual 对后面的步骤看得更清楚时，就会出现在这里。",
  "status.copied": "📋 已复制",
  "status.copiedTitle": "文本已复制到剪贴板",
  "status.behindPanel": "\u25ce 这个位置好像被本面板挡住了 —— 把面板拖开就能看到标出的位置。",
  "status.noPointer": "\u2298 无法给出指针 —— 请按上面的说明操作",
  "status.needsInput": "💬 AI 需要你的输入 —— 请在下面写下你的回答",

  "balance.viewBilling": "查看账单",
  "balance.getMore": "获取更多次数",
  "balance.freeTier": "免费档",
  "balance.freeTierTitle": "你当前是免费档 —— 点击查看账单",
  "balance.nLeft": "还剩 {n} 次",

  "hint.targetChip": "点这里选择你想让我协助的应用。",
  "hint.collapse": "挡路了？点这里把 Navisual 缩成一个小的悬浮图标。",
  "hint.stillDriving": "Navisual 还在继续指引",
  "hint.pressBefore": "按",
  "hint.pressAfter": "进入下一步 —— 不用打开面板。",
  "hint.gotIt": "知道了",

  // ── 会话列表与导出 (P2c) ──────────────────────────────────────────────────
  "sess.saveThis": "💾 保存这次会话",
  "sess.saveThisTitle": "把这次会话 —— 步骤、截图和对话 —— 保存到一个文件夹",
  "sess.recent": "最近的任务",
  "sess.loading": "加载中\u2026",
  "sess.empty": "还没有更早的任务。用着用着就会存到这里。",
  "sess.open": "进行中",

  "exp.aria": "保存这次会话",
  "exp.title": "保存这次会话",
  "exp.savedTo": "已保存到",
  "exp.openFolderTitle": "打开这个文件夹",
  "exp.openFolder": "📂 打开文件夹",
  "exp.copied": "\u2713 已复制",
  "exp.copyPath": "\u29c9 复制路径",
  "exp.reviewBeforeSharing": "发给别人之前先把文件夹看一遍：那些截图就是你屏幕的画面。",
  "exp.nothingYet": "还没有记录。先走一两步再来。",
  "exp.fieldTitle": "标题",
  "exp.titlePlaceholder": "这次会话是关于什么的？",
  "exp.fieldFolder": "文件夹",
  "exp.browse": "浏览\u2026",
  "exp.browseTitle": "换一个文件夹",
  "exp.optClean": "保存原始截图",
  "exp.optPointer": "画上指针",
  "exp.optCaption": "把指引文字作为字幕加上",
  "exp.optCrop": "裁剪到目标应用",
  "exp.hintCropped": "—— 会去掉 Navisual 面板",
  "exp.noCleanWarning": "不保存原始截图的话，以后就无法重新生成指针或字幕 —— 它们是从未经处理的原图重建出来的。",

  "lightbox.loading": "加载中\u2026",
  "lightbox.close": "点击任意位置关闭",

  // ── 收起图标的悬停提示 (P2) ───────────────────────────────────────────────
  "icon.tipFailed": "上一次请求失败了 —— 点击展开看原因",
  "icon.tipThinking": "Navisual 正在思考\u2026",
  "icon.tipNeedsInput": "Navisual 有话问你 —— 点击展开",
  "icon.tipStepHotkey": "第 {n} 步，共 {total} 步 —— 按 {hotkey} 进入下一步 \u00b7 点击展开",
  "icon.tipStep": "第 {n} 步，共 {total} 步 —— 点击展开",
  "icon.tipExpand": "展开 Navisual",
  "icon.askAnswer": "回答 Navisual…",
  "icon.askFollowUp": "追问一句…",

  // ── 任务预填 (P2) ─────────────────────────────────────────────────────────
  "prefill.showAround": "带我熟悉一下 {app}",
  "prefill.exploreApp": "熟悉一下这个应用",

  "pack.blenderAround": "带我熟悉一下 Blender",
  "pack.blenderMove": "帮我移动一个物体",
  "pack.blenderCube": "帮我在场景里加一个立方体",
  "pack.browserFind": "帮我在这个页面上找点东西",
  "pack.browserOpen": "教我怎么打开一个网站",
  "pack.browserDownload": "帮我从这个页面下载一个文件",

  // ── 注册促销 ──────────────────────────────────────────────────────────────
  "promo.headline": "注册免费账户，送 {coins} 金币。",
  "promo.sub": "足够你试试 Navisual 中继上更快、更聪明的模型。Navisual 本身一直免费",
  "promo.ends": " —— 活动截止 {date}",
};
