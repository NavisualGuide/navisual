// Interface language. One owner, three dictionaries, no dependency.
//
// WHY NOT A LIBRARY. svelte-i18n would be the first non-Tauri runtime dependency this app
// has, and it is store-shaped (Svelte 3/4) against a codebase that is runes-shaped. What a
// library actually buys at this size is interpolation and plurals; interpolation is eight
// lines below, and the strings that would need plurals ("29 of 30 left") read better with
// the number in a slot anyway. Revisit if the string count passes ~1,000.
//
// WHAT IS NOT TRANSLATED, deliberately: the AI's own replies and captions. prompts.rs rule
// 13 pins those to the language the USER typed, because an English UI with Chinese input --
// and the reverse -- are both ordinary. Picking 简体中文 here changes the app's chrome and
// nothing the model says.

import { en, type MessageKey } from "../locales/en";
import { zhHans } from "../locales/zh-Hans";
import { zhHant } from "../locales/zh-Hant";

/** A dictionary we actually ship. `"system"` is a *setting* value, never a locale. */
export type Locale = "en" | "zh-Hans" | "zh-Hant";

/** The setting's four values, in the order the Settings row shows them. */
export const UI_LANGUAGE_CHOICES = ["system", "en", "zh-Hans", "zh-Hant"] as const;
export type UiLanguageSetting = (typeof UI_LANGUAGE_CHOICES)[number];

/** Self-describing labels: each option is written in its own language, so the row stays
 *  readable no matter which locale is currently active. That is the whole point -- someone
 *  who cannot read the current UI still has to be able to find their way out of it. */
export const UI_LANGUAGE_LABELS: Record<UiLanguageSetting, string> = {
  system: "System",
  en: "English",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
};

const DICTS: Record<Locale, Partial<Record<MessageKey, string>>> = {
  en,
  "zh-Hans": zhHans,
  "zh-Hant": zhHant,
};

/**
 * Map a BCP-47 tag onto a shipped dictionary.
 *
 * **Reads the whole tag, never the primary subtag alone.** That is not tidiness: `pick_voice`
 * filtered Chinese *voices* on a bare "zh", which made zh-CN, zh-TW and zh-HK one pool, and a
 * Taiwan voice spent four utterances reading Simplified Mandarin before anyone noticed
 * (v0.7.30). Simplified and traditional collapse into each other under exactly that shortcut,
 * and here the result would be a whole UI in the wrong script.
 *
 * Order matters: an explicit script subtag is the only unambiguous signal, so it wins over
 * the region. `zh-Hant-CN` is contradictory but should still render traditional.
 */
export function localeFromTag(tag: string): Locale {
  const parts = (tag || "").toLowerCase().split(/[-_]/).filter(Boolean);
  if (parts[0] !== "zh") return "en";
  if (parts.includes("hant")) return "zh-Hant";
  if (parts.includes("hans")) return "zh-Hans";
  if (parts.some((p) => p === "tw" || p === "hk" || p === "mo")) return "zh-Hant";
  // zh-CN, zh-SG, and a bare "zh" all mean simplified in practice.
  return "zh-Hans";
}

/** The setting plus the OS tag, resolved to the dictionary to actually use. */
export function resolveLocale(setting: string, osTag: string): Locale {
  if (setting === "en" || setting === "zh-Hans" || setting === "zh-Hant") return setting;
  // "system", empty, or anything unrecognised: follow the OS, English if it is not a
  // language we ship. An unknown *setting* failing safe to the OS rather than to English
  // matters when a future release adds a locale and the user downgrades.
  return localeFromTag(osTag);
}

class I18n {
  /** The dictionary in use. Read by `t()`, which is what makes the UI reactive. */
  locale = $state<Locale>("en");
  /** The raw setting, so Settings can show "System" rather than what it resolved to. */
  setting = $state<string>("system");
  /** What "System" resolved to, for the hint under the row. */
  systemResolved = $state<Locale>("en");

  /** Apply a setting. Call on startup and whenever the setting changes -- it is cheap and
   *  idempotent, and applying it live is what makes the picker feel like a picker. */
  apply(setting: string, osTag: string) {
    this.setting = setting || "system";
    this.systemResolved = localeFromTag(osTag);
    this.locale = resolveLocale(this.setting, osTag);
  }
}

export const i18n = new I18n();

/**
 * Look up `key`, filling `{name}` slots from `vars`.
 *
 * A key missing from the active dictionary falls back to **English**, not to a blank and not
 * to the key: a half-translated release should read as mixed, which is obvious and harmless,
 * rather than as missing, which looks broken. A key missing from English too is a bug, so it
 * returns the key itself and says so in dev.
 */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const dict = DICTS[i18n.locale];
  let s: string | undefined = dict[key];
  if (s === undefined) s = en[key];
  if (s === undefined) {
    // Unconditional, not dev-only: reaching here means the key is absent from ENGLISH too,
    // which is a bug in any build. (`import.meta.env` is untyped in this project -- no
    // vite/client types -- and a console.warn is not worth pulling them in for.)
    console.warn(`[i18n] missing key: ${key}`);
    return key;
  }
  if (vars) {
    // split/join rather than a RegExp: no escaping question, and no chance a value
    // containing "$&" rewrites itself.
    for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  }
  return s;
}

/**
 * Translate a nav-pack's curated starter task.
 *
 * **Stopgap.** The right home for this is `pack.json` itself, as a
 * `starter_tasks_<locale>` map, so a pack can ship its own languages and a
 * third-party pack is not stuck with whatever the app happens to know. Until that
 * exists, the two bundled packs are translated here by their English text.
 *
 * Anything not in this table falls through unchanged, which is the correct
 * behaviour rather than a gap: a pack authored in Japanese should show its
 * Japanese, and one authored in English is merely untranslated. It also means an
 * edit to a bundled pack silently stops matching and falls back to English -- safe,
 * but the reason the real fix belongs in the pack format.
 */
const PACK_STARTERS: Record<string, MessageKey> = {
  "Show me around Blender": "pack.blenderAround",
  "Help me move an object": "pack.blenderMove",
  "Help me add a cube to the scene": "pack.blenderCube",
  "Help me find something on this page": "pack.browserFind",
  "Show me how to open a website": "pack.browserOpen",
  "Help me download a file from this page": "pack.browserDownload",
};

export function packStarter(authored: string): string {
  const key = PACK_STARTERS[authored];
  return key ? t(key) : authored;
}

/**
 * The BCP-47 tag to hand `Intl`, derived from the active dictionary.
 *
 * Numbers and dates used to be formatted with `undefined`, i.e. the OS locale, and that
 * was the right call while the panel had no language of its own: there was no better
 * signal. Now there is one, and leaving them on the OS creates a mixture the app chose --
 * an English UI on a Chinese desktop rendering a Chinese date, and the reverse. Both are
 * worse than the original, because before v0.7.31 nobody had picked a language for the app
 * to disagree with.
 *
 * English maps to en-GB, not en-US: this project writes dates day-first ("19 June 2026")
 * everywhere else, including the site's legal pages.
 */
export function intlLocale(): string {
  return i18n.locale === "zh-Hans" ? "zh-CN" : i18n.locale === "zh-Hant" ? "zh-TW" : "en-GB";
}

/** Numbers and dates move TOGETHER. Splitting them is what made one sentence disagree with
 *  itself when the promo deadline was pinned and the coin count beside it was not (rev
 *  2.115, reverted the same day). */
export function fmtNum(n: number): string {
  return n.toLocaleString(intlLocale());
}

export function fmtDate(d: Date, opts?: Intl.DateTimeFormatOptions): string {
  return d.toLocaleDateString(intlLocale(), opts);
}
