// Humanize a Tauri accelerator ("Ctrl+KeyE", "Ctrl+Backquote") for display:
// strip the Key/Digit prefix and map punctuation codes to their printed symbol,
// so it reads "Ctrl+E", "Ctrl+`". The stored value stays the raw accelerator —
// this is display-only.

const KEY_SYMBOL: Record<string, string> = {
  Backquote: "`", Minus: "-", Equal: "=", Backslash: "\\", Slash: "/",
  Comma: ",", Period: ".", Semicolon: ";", Quote: "'",
  BracketLeft: "[", BracketRight: "]", Space: "Space",
  Escape: "Esc", Delete: "Del", ArrowUp: "↑", ArrowDown: "↓",
  ArrowLeft: "←", ArrowRight: "→",
};

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
