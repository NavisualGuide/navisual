// Display names for the API providers, in one place because three surfaces name a
// provider at the user: the Usage tab's rows, its "your own key" note, and the
// Billing panel's "coins are only spent on Managed, and you are on X" warning.
//
// Brands stay in Latin script in every locale -- "Anthropic" is not translated.
// `custom` is the one entry that is a DESCRIPTION rather than a name, so it comes
// from the dictionary, and from the same key the provider picker's own option uses:
// a sentence telling you to go switch providers has to name the thing exactly as
// the dropdown you are being sent to does (CLAUDE.md rule 8).
import { t } from "./i18n.svelte";

const BRANDS: Record<string, string> = {
  managed: "Navisual",
  anthropic: "Anthropic",
  gemini: "Google Gemini",
  openai: "OpenAI",
  deepseek: "DeepSeek",
  qwen: "Qwen",
  ollama: "Ollama",
};

export function providerName(id: string): string {
  if (id === "custom") return t("pv.optCustom");
  return BRANDS[id] ?? id;
}
