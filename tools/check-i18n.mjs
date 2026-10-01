// Translation coverage as a GATE, not a sweep.
//
// v0.7.31 shipped the multi-language UI and v0.7.32 spent a whole release finding what
// the first four extraction passes had missed -- 392 keys became 488, and one file every
// user opens (HotkeyInput.svelte) had never been scanned at all. Each of those passes
// believed it was complete. That is the argument for this file: a sweep tells you what
// was missing on the day it ran, a gate tells you the moment someone adds another one.
//
// Three checks, two of which can fail the build:
//
//   1. Svelte literals  -- user-visible text that never reaches t(). FAILS on anything
//      not in the baseline. This is the hole v0.7.32 was: nothing else in the toolchain
//      can see it, because an English string is perfectly valid TypeScript.
//   2. Rust user text   -- sentences built in the backend, which bypass the dictionary
//      entirely (it lives in the frontend). FAILS on new ones. Return a stable code and
//      let the panel translate it.
//   3. Locale key diff  -- keys present in en.ts and missing elsewhere. WARNS only, on
//      purpose: en.ts is the documented fallback, so a gap renders English rather than
//      blank, and failing the build over a translation-in-progress would make people
//      skip the gate rather than use it.
//
// The baseline (tools/i18n-baseline.json) holds what was already English when this
// landed, including the surfaces that are English BY DECISION -- the Developer tab and
// the locate-trace drawer (china-strategy.md section 6). Adding to it is a deliberate
// act with a diff, which is the point: it is a decision someone makes, not an omission
// nobody notices.
//
// Usage:  node tools/check-i18n.mjs [--update-baseline]

import { readFileSync, writeFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, relative, sep } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const BASELINE = join(ROOT, "tools", "i18n-baseline.json");
const UPDATE = process.argv.includes("--update-baseline");

// ---------------------------------------------------------------------------- helpers

function walk(dir, ext, out = []) {
  if (!existsSync(dir)) return out;
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) {
      if (name === "node_modules" || name === "target" || name === ".git") continue;
      walk(p, ext, out);
    } else if (name.endsWith(ext)) {
      out.push(p);
    }
  }
  return out;
}

const rel = (p) => relative(ROOT, p).split(sep).join("/");

// ------------------------------------------------------------------ 1. locale key sets

function keysOf(file) {
  const src = readFileSync(file, "utf8");
  const keys = new Set();
  // Dictionary entries are `"surface.thing": "text",` -- quoted keys, one per line.
  for (const m of src.matchAll(/^\s*"([a-zA-Z0-9_.]+)":/gm)) keys.add(m[1]);
  return keys;
}

const localeDir = join(ROOT, "src", "locales");
const en = keysOf(join(localeDir, "en.ts"));
const locales = ["zh-Hans", "zh-Hant"].map((name) => ({
  name,
  keys: keysOf(join(localeDir, `${name}.ts`)),
}));

// ------------------------------------------------- 2. referenced keys + dead-key check

const svelteFiles = walk(join(ROOT, "src"), ".svelte");
const tsFiles = walk(join(ROOT, "src"), ".ts").filter((f) => !f.includes(`locales${sep}`));
const used = new Set();
for (const f of [...svelteFiles, ...tsFiles]) {
  const src = readFileSync(f, "utf8");
  // ANY quoted string that happens to be a known key counts as a reference. Deliberately
  // broad: a key reaches t() by several routes -- a literal call, a lookup table
  // (ERROR_CODES, PACK_STARTERS), or as a fallback argument passed by name
  // (errText(res.error, "msg.guideFailed")). A narrower pattern missed that last shape
  // and reported three live keys as dead on this file's first run. Dead keys are only a
  // WARNING, so the cost of over-counting is a missed cleanup, while the cost of
  // under-counting is sending someone to delete a key that is in use.
  for (const m of src.matchAll(/"([a-zA-Z0-9_.]+)"/g)) {
    if (en.has(m[1])) used.add(m[1]);
  }
}

// --------------------------------------------------------- 3. untranslated Svelte text

// Strip what cannot contain user-visible markup text.
function markupOnly(src) {
  return src
    .replace(/<script[\s\S]*?<\/script>/g, "")
    .replace(/<style[\s\S]*?<\/style>/g, "")
    .replace(/<!--[\s\S]*?-->/g, "");
}

// A string worth translating: has letters, and is not an identifier/class/unit soup.
function looksUserFacing(s) {
  const t = s.trim();
  if (t.length < 2) return false;
  if (!/[A-Za-z]/.test(t)) return false;            // symbols, numbers, emoji alone
  if (/^[a-z0-9-]+$/.test(t) && !t.includes(" ")) return false; // css-ish single token
  if (/^[A-Z_]+$/.test(t)) return false;            // CONSTANT_NAME
  if (/^https?:\/\//.test(t)) return false;
  return /[A-Za-z]{2,}/.test(t);
}

const svelteHits = [];
for (const f of svelteFiles) {
  const src = markupOnly(readFileSync(f, "utf8"));
  const lines = src.split("\n");

  lines.forEach((line, i) => {
    // (a) attributes that render to the user
    for (const m of line.matchAll(/\b(title|placeholder|aria-label|alt)\s*=\s*"([^"{}]+)"/g)) {
      if (looksUserFacing(m[2])) {
        svelteHits.push({ file: rel(f), line: i + 1, text: m[2].trim(), kind: m[1] });
      }
    }
    // (b) text nodes: between a closing `>` and an opening `<`, with no mustache in it
    for (const m of line.matchAll(/>([^<>{}]+)</g)) {
      if (looksUserFacing(m[1])) {
        svelteHits.push({ file: rel(f), line: i + 1, text: m[1].trim(), kind: "text" });
      }
    }
  });
}

// --------------------------------------------------------- 4. user-facing Rust strings

const rustFiles = walk(join(ROOT, "src-tauri", "src"), ".rs");
const rustHits = [];
for (const f of rustFiles) {
  const src = readFileSync(f, "utf8");
  const lines = src.split("\n");
  // Strings that become a message the user reads. Joined continuations so a wrapped
  // Rust literal is seen whole -- the first sweep of this missed one by scanning lines.
  const joined = src.replace(/\\\s*\n\s*/g, "");
  for (const m of joined.matchAll(
    /(?:error:\s*Some\(|Err\(|\.ok_or_else\(\|\|\s*)\s*\n?\s*"((?:\\.|[^"\\])+)"/g,
  )) {
    const text = m[1].replace(/\s+/g, " ").trim();
    if (text.length >= 30 && /^[A-Z]/.test(text) && (text.match(/ /g) || []).length >= 4) {
      const line = joined.slice(0, m.index).split("\n").length;
      rustHits.push({ file: rel(f), line, text });
    }
  }
  void lines;
}

// ------------------------------------------------------------------------- 5. baseline

const key = (h) => `${h.file}:${h.text}`;
const base = existsSync(BASELINE)
  ? JSON.parse(readFileSync(BASELINE, "utf8"))
  : { svelte: [], rust: [] };

if (UPDATE) {
  writeFileSync(
    BASELINE,
    JSON.stringify(
      {
        _comment:
          "Text that was already English when the i18n gate landed, plus the surfaces " +
          "that are English BY DECISION (Developer tab, locate-trace drawer). Adding an " +
          "entry is a deliberate act with a diff. Regenerate with: node tools/check-i18n.mjs --update-baseline",
        svelte: [...new Set(svelteHits.map(key))].sort(),
        rust: [...new Set(rustHits.map(key))].sort(),
      },
      null,
      2,
    ) + "\n",
  );
  console.log(
    `baseline written: ${new Set(svelteHits.map(key)).size} svelte, ` +
      `${new Set(rustHits.map(key)).size} rust`,
  );
  process.exit(0);
}

const baseSvelte = new Set(base.svelte ?? []);
const baseRust = new Set(base.rust ?? []);
const newSvelte = svelteHits.filter((h) => !baseSvelte.has(key(h)));
const newRust = rustHits.filter((h) => !baseRust.has(key(h)));

// ---------------------------------------------------------------------------- 6. report

let failed = false;

if (newSvelte.length) {
  failed = true;
  console.error(`\nUNTRANSLATED UI TEXT (${newSvelte.length}) — wrap it in t(), or add a key:\n`);
  for (const h of newSvelte.slice(0, 40)) {
    console.error(`  ${h.file}:${h.line}  [${h.kind}]  ${JSON.stringify(h.text)}`);
  }
  if (newSvelte.length > 40) console.error(`  ... and ${newSvelte.length - 40} more`);
}

if (newRust.length) {
  failed = true;
  console.error(
    `\nUSER-FACING TEXT BUILT IN RUST (${newRust.length}) — the dictionary lives in the` +
      ` frontend, so return a stable code and translate it there:\n`,
  );
  for (const h of newRust) {
    console.error(`  ${h.file}:${h.line}  ${JSON.stringify(h.text.slice(0, 90))}`);
  }
}

// Warnings: never fail. en.ts is the documented fallback.
for (const { name, keys } of locales) {
  const missing = [...en].filter((k) => !keys.has(k));
  if (missing.length) {
    console.warn(
      `\nwarning: ${missing.length} key(s) not yet in ${name} — these render English:`,
    );
    console.warn("  " + missing.slice(0, 12).join(", ") + (missing.length > 12 ? ", ..." : ""));
  }
}
const dead = [...en].filter((k) => !used.has(k));
if (dead.length) {
  console.warn(`\nwarning: ${dead.length} key(s) in en.ts that nothing references:`);
  console.warn("  " + dead.slice(0, 12).join(", ") + (dead.length > 12 ? ", ..." : ""));
}

if (failed) {
  console.error(
    "\ni18n gate FAILED. If a string is English by decision, add it with" +
      " `node tools/check-i18n.mjs --update-baseline` so the choice shows up in a diff.\n",
  );
  process.exit(1);
}

console.log(
  `i18n ok — ${en.size} keys, ${svelteHits.length} baselined UI strings, ` +
    `${rustHits.length} baselined Rust strings.`,
);
