// 繁體中文 (Traditional Chinese, Taiwan usage).
//
// NOT a character conversion of zh-Hans. The differences that matter here are vocabulary,
// not glyphs, and a 簡→繁 converter gets every one of them wrong:
//   設定 / 设置      settings
//   登入 / 登录      sign in
//   登出 / 退出登录  sign out
//   預設 / 默认      default
//   重新整理 / 刷新  refresh
//   網路 / 网络      network
//   帳戶 / 账户      account
//   資料 / 数据      data
// Hong Kong and Macau also resolve here; where HK usage differs from Taiwan the Taiwan form
// is used, which is the ordinary trade-off for one zh-Hant dictionary.

import type { MessageKey } from "./en";

// Partial on purpose: a key left out falls back to English, which is how a partial
// translation ships. Typed against MessageKey so a key that is MISSPELLED -- the failure
// that looks identical to "not translated yet" -- is a compile error instead.
export const zhHant: Partial<Record<MessageKey, string>> = {
  // ── 通用 ──────────────────────────────────────────────────────────────────
  "common.cancel": "取消",
  "common.close": "關閉",
  "common.email": "電子郵件",
  "common.password": "密碼",
  "common.emailPlaceholder": "you@example.com",
  "common.or": "或",

  // ── 語言設定 ──────────────────────────────────────────────────────────────
  "lang.label": "介面語言",
  "lang.systemHint": "系統語言：{locale}",
  "lang.aiNote": "AI 仍然會用你輸入的語言回答。",

  // ── 帳戶 ──────────────────────────────────────────────────────────────────
  "acct.signedInAs": "目前帳戶",
  "acct.coinsStayHere": "你的金幣與購買紀錄都保存在這個帳戶底下。",
  "acct.signInBlurb": "登入後，金幣與購買紀錄可以在多台裝置之間保留。",
  "acct.passwordPlaceholder": "你的密碼",
  "acct.signIn": "登入",
  "acct.signingIn": "登入中…",
  "acct.createAccount": "註冊帳戶",
  "acct.forgotPassword": "忘記密碼？",
  "acct.neverVerified": "註冊過但沒驗證？",
  "acct.resendVerification": "重新寄送驗證碼",
  "acct.continueGoogle": "使用 Google 登入",
  "acct.waitingGoogle": "等待 Google…",
  "acct.googleUnreachable":
    "一直沒有反應？你所在的網路可能連不上 Google 登入頁面 —— 上面的電子郵件與密碼在兩種情況下都可以用。",
  "acct.signUpBlurb": "註冊一個帳戶 —— 你目前的免費次數與已有金幣都會保留。",
  "acct.minChars": "至少 {n} 個字元",
  "acct.sendingCode": "正在寄送驗證碼…",
  "acct.haveAccount": "已經有帳戶了？前往登入",
  "acct.verificationCode": "驗證碼",
  "acct.codeFromEmail": "郵件中的驗證碼",
  "acct.verifying": "驗證中…",
  "acct.verifyFinish": "驗證並完成",
  "acct.resendCode": "重新寄送驗證碼",
  "acct.forgotBlurb": "填寫你的帳戶電子郵件，我們會寄送一組重設驗證碼。",
  "acct.sending": "寄送中…",
  "acct.sendResetCode": "寄送重設驗證碼",
  "acct.backToSignIn": "返回登入",
  "acct.resetCode": "重設驗證碼",
  "acct.newPassword": "新密碼",
  "acct.saving": "儲存中…",
  "acct.setNewPassword": "設定新密碼",
  "acct.billing": "帳單與儲值",
  "acct.changePassword": "變更密碼",
  "acct.currentPassword": "目前密碼",
  "acct.currentPasswordPlaceholder": "你目前的密碼",
  "acct.savePassword": "儲存密碼",
  "acct.googleManaged":
    "你是透過 Google 登入 —— 密碼由 Google 管理，不在 Navisual 這邊。要變更請前往",
  "acct.signOut": "登出",
  "acct.deleteAccount": "刪除帳戶",
  "acct.deleteWarning": "這會永久刪除你的帳戶。金幣不退款，也無法復原。",
  "acct.deleting": "刪除中…",
  "acct.deletePermanently": "永久刪除",

  // 帳戶提示與錯誤
  "acct.msg.needEmailPassword": "請填寫電子郵件，以及至少 {n} 個字元的密碼。",
  "acct.msg.codeSent": "請輸入我們寄到 {email} 的驗證碼。已經收到過了？它 1 小時內有效。",
  "acct.msg.alreadyRegistered": "這個電子郵件已經註冊過了。請輸入密碼登入。",
  "acct.msg.needEmail": "請先填寫電子郵件。",
  "acct.msg.newCodeSent": "新的驗證碼已寄到 {email}，請在下面輸入。",
  "acct.msg.passwordChanged": "密碼已變更。",
  "acct.msg.needCode": "請輸入郵件中的驗證碼。",
  "acct.msg.needBoth": "請填寫電子郵件與密碼。",
  "acct.msg.unverifiedResent": "你的電子郵件還沒驗證 —— 我們已重新寄送驗證碼到 {email}，請在下面輸入。",
  "acct.msg.unverified": "你的電子郵件還沒驗證。請輸入我們寄到 {email} 的驗證碼，或點「重新寄送驗證碼」。",
  "acct.msg.needAccountEmail": "請填寫你的帳戶電子郵件。",
  "acct.msg.resetSent": "我們已寄送重設驗證碼到 {email}。請連同新密碼一起填寫。",
  "acct.msg.needCodeAndPassword": "請輸入郵件中的驗證碼，以及一組至少 {n} 個字元的新密碼。",
  "acct.msg.passwordTooShort": "新密碼至少需要 {n} 個字元。",

  // ── 帳單與儲值 ────────────────────────────────────────────────────────────
  "bill.wrongProvider":
    "金幣只在「託管」服務商底下消耗，而你目前用的是 {provider}。請先到「服務商」分頁切換再使用。",
  "bill.plan": "方案",
  "bill.planPaid": "付費（金幣）",
  "bill.planFree": "免費試用",
  "bill.coinBalance": "金幣餘額",
  "bill.coins": "{n} 金幣",
  "bill.freeRequests": "免費次數",
  "bill.freeLeft": "還剩 {n} 次，共 {total} 次",
  "bill.tierHint": "品質等級 —— 由哪個模型回答、以及每次消耗多少金幣 —— 在「服務商」分頁設定。",
  "bill.topUpAmount": "儲值金額",
  "bill.custom": "自訂…",
  "bill.customPlaceholder": "輸入 $5–$500",
  "bill.amountRange": "金額必須介於 $5–$500",
  "bill.buyCoins": "購買金幣（${amount}）",
  "bill.openingCheckout": "正在開啟付款頁…",
  "bill.checkoutOpen": "付款頁已在瀏覽器開啟…",
  "bill.refreshBalance": "重新整理餘額",
  "bill.agreePrefix": "購買金幣即表示你同意我們的",
  "bill.terms": "服務條款",
  "bill.and": "與",
  "bill.privacy": "隱私權政策",
  "bill.checkoutHint": "付款頁會在瀏覽器開啟；回到應用程式後餘額會自動更新。",

  // ── 免費次數用完 / 金幣不足 ───────────────────────────────────────────────
  "trial.titleCoins": "金幣不足",
  "trial.titleFree": "免費次數已用完",
  "trial.ariaCoins": "金幣不足",
  "trial.ariaFree": "免費試用已用完",
  "trial.bodyCoins": "目前品質等級所需的金幣不足。",
  "trial.bodyFree": "你的免費次數已經用完了。",
  "trial.signingInGoogle": "正在瀏覽器中使用 Google 登入…",
  "trial.openingCheckout": "正在開啟付款頁…",
  "trial.checkoutOpened": "付款頁已在瀏覽器開啟。付款完成後回到這裡 —— 餘額會自動更新。",
  "trial.topUpBlurb": "儲值金幣即可繼續使用 Navisual 託管服務。",
  "trial.stillFreeLeft":
    "還有免費次數沒用完？在「設定 → 服務商」把品質等級切換成「免費」就能繼續使用。",
  "trial.ownKey":
    "也可以用你自己的 API Key 繼續免費使用：設定 → 服務商 → Gemini（Google AI Studio）或 Ollama（本機）。",

  // ── 面板介面 (P2a) ────────────────────────────────────────────────────────
  "panel.about": "關於 Navisual",
  "panel.settings": "設定",
  "panel.collapse": "縮成浮動圖示",
  "panel.quit": "結束",

  "panel.next": "\u2192 下一步",
  "panel.nextTitle": "下一步（{hotkey}）",
  "panel.wrong": "\u2717 這步不對",
  "panel.wrongTitle": "這條指引不對（{hotkey}）",
  "panel.autopilotOn": "\u23f8 自動推進",
  "panel.autopilotOff": "\u2708 自動推進",
  "panel.autopilotOnTitle": "自動推進已開啟 —— 點擊關閉",
  "panel.autopilotOffTitle": "自動推進已關閉 —— 點擊開啟",
  "panel.newTask": "\uff0b 新工作",
  "panel.newTaskTitle": "清空目前工作階段，重新開始",
  "panel.historyTitle": "最近的工作 —— 點一個繼續做",
  "panel.micStop": "停止錄音（{hotkey}）",
  "panel.micStart": "語音輸入（{hotkey}）",
  "panel.micDisabled": "請在「設定 → 音訊」裡開啟語音輸入",

  "panel.thinking": "思考中\u2026",
  "panel.cancelElapsed": "\u23f9 取消（{secs} 秒）",
  "panel.guideMe": "開始指引",
  "panel.sendAnswer": "\u21a9 送出回答",
  "panel.followUp": "\u21a9 追問",
  "panel.send": "\u21a9 送出",
  "panel.inputHint": "輸入追問或更正 \u00b7 \uff0b 開始新工作",

  "panel.staleText": "我思考的時候畫面變了 —— 這條指引可能已經過時。",
  "panel.occludedText": "目標視窗看不到 —— 把它切到前面才能看到指標。",
  "panel.reanalyse": "\u21bb 重新分析",
  "panel.reanalyseTitle": "重新分析目前畫面",
  "panel.dismiss": "關掉",

  "menu.chat": "\U0001f4ac 對話",
  "menu.switchApp": "\U0001f3af 切換應用程式",
  "menu.dockLeft": "\u25e7 停靠左側",
  "menu.dockRight": "\u25e8 停靠右側",
  "menu.unmute": "\U0001f50a 取消靜音",
  "menu.mute": "\U0001f507 靜音",
  "menu.captionOn": "\U0001f4ac 字幕：開",
  "menu.captionOff": "\U0001f4ac 字幕：關",
  "menu.showPointer": "\U0001f441 顯示指標和字幕",
  "menu.clearPointer": "\u2715 清除指標和字幕",
  "menu.expand": "\u2197 展開",
  "menu.collapse": "\u229f 收合",
  "menu.quit": "\u2715 結束",

  // ── 首次執行隱私須知（同意關卡）───────────────────────────────────────────
  "privacy.aria": "隱私須知",
  "privacy.title": "開始第一個工作之前",
  "privacy.lead": "Navisual 會擷取你目前的視窗，並傳送給你選擇的 AI 服務商。",
  "privacy.b1": "它擷取你指定的那個視窗 —— 如果你選的是整個螢幕，就是整個螢幕 —— 並把畫面傳送給你選擇的 AI 服務商。",
  "privacy.b2": "它儲存的任何螢幕截圖都只存在你自己的電腦上 —— 絕不會存到我們的伺服器。",
  "privacy.b3strong": "預設的免費方案所用的 AI 模型，可能會保留你的請求 —— 包括螢幕截圖 —— 用於訓練。",
  "privacy.b3rest": "付費方案和你自己的 API Key 不會；Ollama 則完全不離開你的電腦。",
  "privacy.b4": "指引過程中，它會記錄你在那個應用程式裡點了哪個控制項 —— 只記控制項的名稱，絕不記它的內容。它不監控你的鍵盤。",
  "privacy.b5": "語音輸入若你開啟，會把你的音訊傳送給微軟的語音服務。",
  "privacy.policyBefore": "",
  "privacy.policyLink": "完整隱私權政策",
  "privacy.policyAfter": "是完整版本，內容以它為準 —— 擷取什麼、傳往哪裡、儲存什麼、以及如何停止。你隨時可以從「關於」重新開啟它。",
  "privacy.accept": "我已瞭解，繼續",

  // ── 目標選擇、標題列標籤、停靠 (P2b) ──────────────────────────────────────
  "target.chooseApp": "選擇目標應用程式",
  "target.chooseDockApp": "選擇要占滿螢幕其餘部分的應用程式",
  "target.chipFullScreen": "正在分享你的螢幕 —— 點擊切換目標",
  "target.chipPinned": "目標應用程式已釘選 —— 點擊切換或取消釘選",
  "target.chipDefault": "目標應用程式 —— 點擊切換或釘選",
  "target.autoDetect": "自動偵測",
  "target.autoDetectSub": "跟隨目前最前面的視窗",
  "target.entireDesktop": "整個桌面",
  "target.entireDesktopSub": "分享整個螢幕 —— 所有視窗",
  "target.screenN": "螢幕 {n}",
  "target.minimized": "已最小化",
  "target.dockHead": "要讓哪個應用程式占滿其餘部分？",
  "target.pickAlsoFills": "選了應用程式之後，它會同時占滿螢幕的其餘部分",
  "dock.fillRest": "\u2b12 其餘部分放\u2026",
  "dock.undock": "\u2b1c 取消停靠",
  "dock.undockTitle": "讓面板重新浮動",

  // ── 回報錯誤選單 (P2b) ────────────────────────────────────────────────────
  "wrong.prompt": "哪裡不對？",
  "wrong.generic": "不對",
  "wrong.instruction": "指引不對",
  "wrong.spot": "位置不對",
  "wrong.notFound": "找不到",
  "wrong.alreadyDone": "已經做過了",
  "wrong.other": "其他",
  "wrong.hintOther": "都不是？在下面寫清楚哪裡不對，然後點 \u21a9 追問。",
  "wrong.hintApp": "應用程式選錯了？先點一下正確的視窗，再按 \u2717 這步不對。",

  // ── 註冊優惠 ──────────────────────────────────────────────────────────────
  "promo.headline": "註冊免費帳戶，送 {coins} 金幣。",
  "promo.sub": "足夠你試試 Navisual 中繼上更快、更聰明的模型。Navisual 本身一直免費",
  "promo.ends": " —— 活動截止 {date}",
};
