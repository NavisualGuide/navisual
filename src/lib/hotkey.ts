// Humanize a Tauri accelerator ("Ctrl+KeyE", "Ctrl+Backquote") for display:
// strip the Key/Digit prefix and map punctuation codes to their printed symbol,
// so it reads "Ctrl+E", "Ctrl+`". The stored value stays the raw accelerator —
// this is display-only.

const KEY_SYMBOL: Record<string, string> = {
  Backquote: "~", Minus: "-", Equal: "=", Backslash: "\\", Slash: "/",
  Comma: ",", Period: ".", Semicolon: ";", Quote: "'",
  BracketLeft: "[", BracketRight: "]", Space: "Space",
  Escape: "Esc", Delete: "Del", ArrowUp: "↑", ArrowDown: "↓",
  ArrowLeft: "←", ArrowRight: "→",
};

// ── Matching a step's wording against Navisual's own hotkeys ─────────────────
// A global hotkey takes its keys from every app. When a step tells the user to press one of
// them -- Word's Ctrl+E (Center) was Navisual's Wrong key in L1, 2026-10-12 -- the app never
// sees the keypress. Both sides are reduced to one canonical combo ("ctrl+e") so the panel can
// release that hotkey for the step.

const MOD_ORDER = ["ctrl", "alt", "shift", "win"] as const;

const MOD_ALIAS: Record<string, string> = {
  ctrl: "ctrl", control: "ctrl", commandorcontrol: "ctrl", cmdorctrl: "ctrl",
  alt: "alt", option: "alt",
  shift: "shift",
  win: "win", windows: "win", super: "win", meta: "win",
};

// Accelerator key codes and the words a step uses for the same key.
const KEY_ALIAS: Record<string, string> = {
  backquote: "backquote", "`": "backquote", "~": "backquote",
  escape: "esc", esc: "esc", delete: "delete", del: "delete",
  enter: "enter", return: "enter", space: "space", tab: "tab",
  home: "home", end: "end", backspace: "backspace", insert: "insert",
  pageup: "pageup", pagedown: "pagedown",
  arrowup: "up", arrowdown: "down", arrowleft: "left", arrowright: "right",
  up: "up", down: "down", left: "left", right: "right",
  minus: "-", "-": "-", equal: "=", "=": "=", comma: ",", ",": ",",
  period: ".", ".": ".", slash: "/", "/": "/", semicolon: ";", ";": ";",
};

function canonical(mods: string[], key: string): string {
  const set = new Set(mods.map((m) => MOD_ALIAS[m.toLowerCase()]).filter(Boolean));
  const k = key.toLowerCase();
  const keyName = KEY_ALIAS[k] ?? k;
  return [...MOD_ORDER.filter((m) => set.has(m)), keyName].join("+");
}

/** "Ctrl+KeyE" -> "ctrl+e"; "" for an unset hotkey. */
export function acceleratorCombo(accel: string): string {
  if (!accel) return "";
  const parts = accel.split("+").map((p) => p.trim()).filter(Boolean);
  const key = parts.pop() ?? "";
  let k = key;
  if (/^Key[A-Z]$/.test(key)) k = key.slice(3);
  else if (/^Digit[0-9]$/.test(key)) k = key.slice(5);
  return canonical(parts, k);
}

// One or more modifiers joined by "+", then a key: "Ctrl+E", "Ctrl + Shift + E", "Control+`".
// Only combos with a modifier count -- a bare letter in a sentence is not a keypress.
const COMBO_RE =
  /\b((?:(?:ctrl|control|alt|shift|win|windows)\s*\+\s*)+)(f(?:1[0-2]|[1-9])\b|(?:esc|escape|delete|del|enter|return|space|tab|home|end|backspace|insert|pageup|pagedown|up|down|left|right)\b|[a-z0-9](?![a-z0-9])|[`~\-=,./;])/gi;

/** Every modifier combo a step's text names, canonical. */
export function combosInText(text: string): Set<string> {
  const out = new Set<string>();
  for (const m of text.matchAll(COMBO_RE)) {
    const mods = m[1].split("+").map((s) => s.trim()).filter(Boolean);
    out.add(canonical(mods, m[2]));
  }
  return out;
}

export function prettyHotkey(accel: string): string {
  if (!accel) return "";
  return accel
    .split("+")
    .map((part) => {
      if (KEY_SYMBOL[part]) return KEY_SYMBOL[part];
      if (/^Key[A-Z]$/.test(part)) return part.slice(3);
      if (/^Digit[0-9]$/.test(part)) return part.slice(5);
      return part; // modifiers (Ctrl/Shift/Alt/Super), F-keys, etc.
    })
    .join("+");
}
