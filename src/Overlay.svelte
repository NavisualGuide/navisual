<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { DEFAULT_THICKNESS, strokeScale as weightOf } from "./lib/overlay-weight";

  type Rect = { x: number; y: number; width: number; height: number };
  type OverlayUpdate = {
    kind: "box" | "subtitle" | "app_boundary" | "hint" | "candidates" | "none";
    bbox: Rect | null;
    text: string | null;
    virtual_origin: [number, number];
    virtual_size: [number, number];
    active_screen: Rect | null;
    work_area: Rect | null;
    ai_bbox: Rect | null;
    // Flow A: ranked candidate boxes (strongest first) for kind === "candidates".
    candidates: Rect[];
  };
  type OverlayTheme = {
    color: string;
    thickness: number;
    subtitle_enabled: boolean;
    /// Developer toggle: draw the AI-returned target_bbox as a distinct
    /// cyan dashed box alongside the production pointer.
    show_ai_bbox: boolean;
  };

  let canvas: HTMLCanvasElement;
  let currentUpdate: OverlayUpdate | null = null;
  let animFrame: number | null = null;
  let animStart = 0;

  // Phase 0.2: brief animated outline of the captured app's window. Lives
  // alongside the main overlay so it doesn't replace the locator highlight.
  let appBoundary: OverlayUpdate | null = null;
  let appBoundaryStart = 0;
  // Must match lib.rs's APP_BOUNDARY_DURATION_MS (no shared constant across
  // the Rust/Svelte boundary — keep both in sync by hand if this changes).
  const APP_BOUNDARY_DURATION_MS = 3_000; // 250ms flash + 1.75s solid + 1s ease-out fade
  // Plain object — NOT $state. drawBox/drawHint read this in rAF callbacks where
  // Svelte's reactive getters don't fire; mutating fields in-place ensures every
  // frame sees the latest values without any signal overhead.
  let theme: OverlayTheme = {
    color: "#FF6B35",
    thickness: 4,
    subtitle_enabled: true,
    show_ai_bbox: false,
  };

  // Pointer thickness: one weight multiplier applied to every stroke below. The widths
  // here are hand-tuned ratios, not free numbers — see src/lib/overlay-weight.ts for why
  // this scales rather than sets them, and why it is independent of display DPI.
  const strokeScale = () => weightOf(theme.thickness);

  /** Smallest mark we will draw, on either axis, in logical px.
   *
   *  A 17x17 toolbar glyph or an 8 px-tall OCR word gets a mark barely bigger than
   *  itself, which is exactly the target you most need help finding. Measured over
   *  196 real locates: the median mark is 55 px on its short axis and a 36 px floor
   *  lifts only the bottom 18% — a floor, not a redesign. (The alternative, a second
   *  pointer style for small targets, was rejected: it made the mark's appearance
   *  depend on a size judgement the model was making blind.) */
  const MIN_MARK = 36;

  /** The rect the MARK is built on: the located rect, padded, then floored.
   *
   *  The brackets used to sit exactly on the located rect and the ripples only 8 px
   *  past it, which reads as tracing the control rather than pointing at it -- and a
   *  locator that returns a tight text rect (an OCR word, a label without its button)
   *  made it look cramped. Measured against a live pointer on OrcaSlicer's "New
   *  Project": the on-screen mark ran ~102x39 around an ~87x22 element, which this
   *  reproduces. The located rect itself is never modified, so the crosshair and the
   *  reported position still name the real element.
   *
   *  THREE RENDERERS MUST AGREE. This, `mark_rect` in src-tauri/src/session_export.rs
   *  (the export) and the $pad/$MIN_MARK block in tools/annotate-session.ps1 (the
   *  re-annotator) all have to produce the same picture -- the script exists to redo
   *  what the export already did, and the export exists to reproduce what was on
   *  screen. Two divergences already shipped unnoticed. The Rust side pins the
   *  geometry in `mark_geometry_is_pinned` and renders both implementations against
   *  each other in `exporter_and_annotator_draw_the_same_pointer`; if you change the
   *  numbers here, change them there and re-run those tests.
   *
   *  This REPLACES a flat `padding = 12` that used to be applied at the render
   *  dispatch, in physical px, before the DPR divide -- which is why the exported
   *  pointer looked tighter than the on-screen one for so long: the exporter drew
   *  from the located rect and never knew about it. Padding lives here now, in
   *  logical px, so all three renderers can share one rule. Geometry, not weight, so
   *  none of it scales with Pointer thickness -- see src/lib/overlay-weight.ts. */
  function markRect(bx: number, by: number, bw: number, bh: number) {
    const pad = Math.min(20, Math.max(12, Math.min(bw, bh) * 0.45));
    let pw = bw + pad * 2, ph = bh + pad * 2;
    const cx = bx + bw / 2, cy = by + bh / 2;
    pw = Math.max(pw, MIN_MARK);
    ph = Math.max(ph, MIN_MARK);
    return { px: cx - pw / 2, py: cy - ph / 2, pw, ph };
  }

  function hexToRgb(hex: string): [number, number, number] {
    const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
    return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : [255, 107, 53];
  }

  // Device-pixel ratio for the overlay canvas: the backing buffer is physical px
  // (virtual_size) while CSS displays it at logical px, so width/clientWidth is the
  // DPR of the monitor it's on — 1 at 100%, 2 at 200%, 1.5 at 150%. Used to keep
  // fixed-pixel decorations (caption + pointer) a constant perceived size on any DPI.
  function dprOf(ctx: CanvasRenderingContext2D): number {
    return ctx.canvas.clientWidth > 0
      ? ctx.canvas.width / ctx.canvas.clientWidth
      : window.devicePixelRatio || 1;
  }

  // Combined A+B pointer: ripple rings from center (A) + bold corner brackets (B)
  // + animated scan line (B) + subtle crosshair center dot.
  function drawBox(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    // High-DPI: draw in logical coordinates (divide the physical bbox by the DPR,
    // then ctx.scale back up) so every fixed-pixel decoration — bracket arms, line
    // widths, ring gaps, crosshair, corner dots — is a constant perceived size.
    // Element-proportional terms (bw*0.38, max(bw,bh)*0.7) and the bbox position are
    // unchanged, and scale=1 on a 100% display makes this an exact no-op.
    const scale = dprOf(ctx);
    ctx.save();
    ctx.scale(scale, scale);
    bx /= scale; by /= scale; bw /= scale; bh /= scale;
    const [r, g, b] = hexToRgb(theme.color);
    const k = strokeScale();
    const pulse = (Math.sin(t / 700) + 1) / 2;
    const cx = bx + bw / 2;
    const cy = by + bh / 2;
    const { px, py, pw, ph } = markRect(bx, by, bw, bh);

    // ── 1. RIPPLE RINGS from element center ──────────────────────────────
    // Ellipse, not circle: a circle sized by max(bw,bh) balloons hugely past a
    // long, thin element's short axis (e.g. a full-width row) — the ring's base
    // shape now follows the element's own aspect ratio, and the outward growth
    // is capped by the SHORTER axis so it stays a modest puff on any shape
    // instead of an extreme one on long/thin targets.
    const baseRx = pw / 2 + 8;
    const baseRy = ph / 2 + 8;
    const growth = Math.min(pw, ph) * 0.7;
    // On a wide, thin row (a sidebar/list item — bw ≫ bh) cap how far the ring grows on its
    // SHORT axis so it stays a flat puff hugging the row instead of a tall oval that swallows
    // the items above and below (live 2026-07-24: the Settings sidebar ring overshot its
    // neighbours). Square/tall targets keep the full, circular growth.
    const ryGrowth = pw > ph * 2 ? Math.min(growth, ph * 0.4) : growth;
    for (let i = 0; i < 3; i++) {
      const phase = ((t / 1500 + i / 3) % 1);
      const rx = baseRx + phase * growth;
      const ry = baseRy + phase * ryGrowth;
      const alpha  = (1 - phase) * 0.55;
      ctx.beginPath();
      ctx.ellipse(cx, cy, Math.max(rx, 0.1), Math.max(ry, 0.1), 0, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
      ctx.lineWidth = (2.5 - phase * 1.8) * k;
      ctx.shadowColor = theme.color;
      ctx.shadowBlur = 6;
      ctx.stroke();
      ctx.shadowBlur = 0;
    }

    // ── 2. CORNER BRACKETS (no full border) ──────────────────────────────
    const arm = Math.min(26, Math.max(14, Math.min(pw * 0.38, ph * 0.5)));
    ctx.lineCap = "square";
    ctx.lineJoin = "miter";

    function bracket(ox: number, oy: number, dx: number, dy: number) {
      // Shadow layer
      ctx.strokeStyle = "rgba(0,0,0,0.75)";
      ctx.lineWidth = 5.5 * k;
      ctx.beginPath();
      ctx.moveTo(ox + dx * arm, oy); ctx.lineTo(ox, oy); ctx.lineTo(ox, oy + dy * arm);
      ctx.stroke();
      // Accent layer
      ctx.strokeStyle = theme.color;
      ctx.lineWidth = 3 * k;
      ctx.shadowColor = theme.color;
      ctx.shadowBlur = 8 + pulse * 16;
      ctx.stroke();
      ctx.shadowBlur = 0;
      // Corner dot — a stroke-weight sibling of the bracket, not layout, so it scales
      // with it; a fixed dot under a 2x bracket reads as a nick in the corner.
      ctx.fillStyle = theme.color;
      ctx.shadowColor = theme.color;
      ctx.shadowBlur = 10 + pulse * 8;
      ctx.beginPath(); ctx.arc(ox, oy, 3.5 * k, 0, Math.PI * 2); ctx.fill();
      ctx.shadowBlur = 0;
    }

    bracket(px,      py,      1, 1);
    bracket(px + pw, py,     -1, 1);
    bracket(px,      py + ph, 1, -1);
    bracket(px + pw, py + ph,-1, -1);

    // ── 3. SCAN LINE sweeping top→bottom ─────────────────────────────────
    const scanPhase = (t / 1500) % 1;
    const scanY     = py + scanPhase * ph;
    const scanAlpha = Math.sin(scanPhase * Math.PI) * 0.7;
    const grad = ctx.createLinearGradient(px, 0, px + pw, 0);
    grad.addColorStop(0,    `rgba(${r},${g},${b},0)`);
    grad.addColorStop(0.15, `rgba(${r},${g},${b},${scanAlpha})`);
    grad.addColorStop(0.5,  `rgba(255, 210, 140, ${scanAlpha})`);
    grad.addColorStop(0.85, `rgba(${r},${g},${b},${scanAlpha})`);
    grad.addColorStop(1,    `rgba(${r},${g},${b},0)`);
    ctx.fillStyle = grad;
    ctx.fillRect(px, scanY - 1.5 * k, pw, 3 * k);
    // Bright core of the scan line
    ctx.strokeStyle = `rgba(255, 230, 170, ${scanAlpha * 0.9})`;
    ctx.lineWidth = 1 * k;
    ctx.shadowColor = "rgba(255, 180, 80, 0.9)";
    ctx.shadowBlur = 8;
    ctx.beginPath(); ctx.moveTo(px, scanY); ctx.lineTo(px + pw, scanY); ctx.stroke();
    ctx.shadowBlur = 0;

    // ── 4. CROSSHAIR dot at center (subtle) ──────────────────────────────
    const cr = 5;
    ctx.strokeStyle = `rgba(${r},${g},${b},${0.35 + pulse * 0.25})`;
    ctx.lineWidth = 1.5 * k;
    ctx.lineCap = "round";
    ctx.shadowColor = theme.color; ctx.shadowBlur = 5;
    ctx.beginPath(); ctx.moveTo(cx - cr, cy); ctx.lineTo(cx + cr, cy); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(cx, cy - cr); ctx.lineTo(cx, cy + cr); ctx.stroke();
    ctx.shadowBlur = 0;
    ctx.restore();
  }

  // Draw subtitle strip confined to a single screen.
  // Strip width fits the text content rather than spanning the full screen.
  /** Lines the caption may grow to before it is cut with an ellipsis. The full
   *  instruction is always in the panel; the caption is a glance, not a document.
   *  Measured on 655 real instructions: median 85 chars, p95 348, max 1,119 (a
   *  Markdown pros-and-cons answer). Three lines of the measure set below hold
   *  ~220 Latin / ~165 CJK characters -- past p90 -- and stop a long answer from
   *  climbing a quarter of the way up the screen. (This paragraph used to quote
   *  110 chars a line off the old full-width measure; narrowing the strip is why
   *  it no longer does. A number in a comment ages with the code around it.) */
  const CAPTION_MAX_LINES = 3;

  /** Instruction text is read aloud and drawn as a caption, so Markdown that a
   *  model slips in (**bold**, ### headings, * bullets, `code`) is only noise
   *  there. Strip the markers; keep the words. */
  function plainCaption(text: string): string {
    return text
      .replace(/^#{1,6}\s+/gm, "")
      .replace(/\*\*(.+?)\*\*/g, "$1")
      .replace(/__(.+?)__/g, "$1")
      .replace(/`([^`]+)`/g, "$1")
      .replace(/^\s*[*\-•]\s+/gm, "")
      .replace(/\s+/g, " ")
      .trim();
  }

  /** Line-break opportunities.
   *
   *  `text.split(" ")` was the entire wrapper, and it is a Latin assumption:
   *  Chinese, Japanese and Thai put no spaces between words, so a CJK caption
   *  arrived as ONE token, became ONE line, and `fillText`'s maxWidth argument
   *  then CONDENSED the glyphs to make it fit -- the squashed full-width strip
   *  reported 2026-09-07. The three-line clamp never fired either: one token is
   *  one line, so there was nothing to clamp. Same text, same length, in English
   *  wrapped and read fine, which is why this survived every earlier look.
   *
   *  `Intl.Segmenter` gives real word boundaries in every script (WebView2 here
   *  is Chromium 152). Spaces come back as their own segments, so the caller
   *  CONCATENATES and can never insert a space that was not in the text -- which
   *  is the whole point, since joining CJK segments with " " would be wrong.
   *  The fallback only has to be reasonable, not correct. */
  const SEGMENTER = (() => {
    try { return new Intl.Segmenter(undefined, { granularity: "word" }); }
    catch { return null; }
  })();
  const HAS_CJK = /[\u3000-\u303F\u3040-\u30FF\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF\uFF00-\uFFEF]/;
  function breakTokens(text: string): string[] {
    if (SEGMENTER) return Array.from(SEGMENTER.segment(text), (seg) => seg.segment);
    const out: string[] = [];
    for (const chunk of text.split(/(\s+)/)) {
      if (!chunk) continue;
      if (HAS_CJK.test(chunk)) out.push(...Array.from(chunk));
      else out.push(chunk);
    }
    return out;
  }

  /** CJK typography: a line never OPENS with closing punctuation. The segmenter
   *  hands these back as their own segments, so without this a break lands
   *  before a comma and the next line starts with it. */
  const NO_LINE_START = /^[、。，．！？：；」』）】〉》”’.,!?:;)\]}…]/;

  function drawSubtitle(
    ctx: CanvasRenderingContext2D,
    canvasW: number, canvasH: number,
    ox: number, oy: number,
    activeScreen: Rect | null,
    workArea: Rect | null,
    rawText: string,
  ) {
    const text = plainCaption(rawText);
    const sx = activeScreen ? (activeScreen.x - ox) : 0;
    const sy = activeScreen ? (activeScreen.y - oy) : 0;
    const sw = activeScreen ? activeScreen.width  : canvasW;
    const sh = activeScreen ? activeScreen.height : canvasH;
    // Bottom edge the strip must stay above: the work area's, so the taskbar
    // stays visible and clickable. Falls back to the monitor's own bottom.
    const bottom = workArea ? (workArea.y - oy) + workArea.height : sy + sh;

    // Scale every caption metric by the DPR so the strip is a constant *logical*
    // size on any display (a fixed-px font would be half-size at 200%). See dprOf.
    const scale = dprOf(ctx);

    const hPad = 22 * scale;
    const vPad = 12 * scale;
    const r = 10 * scale;
    // A caption is read at a glance, so line LENGTH matters more than fitting it
    // all on one line: 78% of a 1080p screen is ~110 characters, roughly twice a
    // comfortable measure and the reason a long answer looked like a wall. Capped
    // to a LOGICAL width so a line holds the same number of characters at 100% and
    // 200% (the font is 18 logical px too); the proportional bound still governs a
    // narrow screen. ~74 Latin / ~55 CJK characters a line, ~3x that over the clamp.
    const maxTextW = Math.min(sw * 0.78, 1000 * scale);
    const cx = sx + sw / 2;

    // Measure text first so strip width can fit the content
    ctx.font = `600 ${Math.round(18 * scale)}px 'Inter Variable', Inter, -apple-system, 'Segoe UI', sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    // Pre-split any single token that is itself wider than the strip -- a long
    // URL, or a script the fallback could not break -- so it wraps by character
    // instead of being condensed by fillText's maxWidth.
    const tokens: string[] = [];
    for (const tok of breakTokens(text)) {
      if (ctx.measureText(tok).width <= maxTextW) tokens.push(tok);
      else for (const ch of tok) tokens.push(ch);
    }

    const lines: string[] = [];
    let line = "";
    for (const tok of tokens) {
      const test = line + tok;
      if (line && ctx.measureText(test).width > maxTextW) {
        lines.push(line.trimEnd());
        // Never open the next line with the space we happened to break at.
        line = tok.trim() ? tok : "";
      } else {
        line = test;
      }
    }
    if (line.trim()) lines.push(line.trim());

    // Pull orphaned closing punctuation back onto the line before it. One glyph
    // of overhang is what hanging punctuation is; the strip cap absorbs it.
    for (let i = 1; i < lines.length; i++) {
      while (NO_LINE_START.test(lines[i])) {
        lines[i - 1] += lines[i][0];
        lines[i] = lines[i].slice(1);
      }
    }
    // A line that was nothing but punctuation is now empty; do not draw a blank
    // row and do not let it spend one of the three.
    for (let i = lines.length - 1; i > 0; i--) if (!lines[i]) lines.splice(i, 1);

    // Clamp: keep the first CAPTION_MAX_LINES lines and ellipsize the last one so
    // it still fits its width. The panel carries the full text.
    if (lines.length > CAPTION_MAX_LINES) {
      lines.length = CAPTION_MAX_LINES;
      let last = lines[CAPTION_MAX_LINES - 1];
      while (last.length > 1 && ctx.measureText(last + "…").width > maxTextW) last = last.slice(0, -1).trimEnd();
      lines[CAPTION_MAX_LINES - 1] = last + "…";
    }

    const lineH = 22 * scale;
    const maxLineW = Math.max(...lines.map(l => ctx.measureText(l).width));
    const stripW = Math.min(maxLineW + hPad * 2, maxTextW + hPad * 2);
    const stripH = lines.length * lineH + vPad * 2;
    const left   = cx - stripW / 2;
    const right  = cx + stripW / 2;
    const stripY = bottom - stripH - 12 * scale;

    ctx.fillStyle = "rgba(0,0,0,0.52)";
    ctx.beginPath();
    ctx.moveTo(left + r, stripY);
    ctx.lineTo(right - r, stripY);
    ctx.quadraticCurveTo(right, stripY, right, stripY + r);
    ctx.lineTo(right, stripY + stripH - r);
    ctx.quadraticCurveTo(right, stripY + stripH, right - r, stripY + stripH);
    ctx.lineTo(left + r, stripY + stripH);
    ctx.quadraticCurveTo(left, stripY + stripH, left, stripY + stripH - r);
    ctx.lineTo(left, stripY + r);
    ctx.quadraticCurveTo(left, stripY, left + r, stripY);
    ctx.closePath();
    ctx.fill();

    ctx.fillStyle = "#FFFFFF";
    const startY = stripY + vPad + lineH / 2;
    for (let i = 0; i < lines.length; i++) {
      ctx.fillText(lines[i], cx, startY + i * lineH, maxTextW);
    }
  }

  /**
   * Phase 0.2: draw the "shared app" boundary overlay.
   * Three-stage animation over APP_BOUNDARY_DURATION_MS (3 s):
   *   0..250ms  — flash in at full opacity with inner glow
   *   250..2000ms — hold at full opacity (solid outline)
   *   2000..3000ms — ease-out cubic fade to 0
   * A new capture replaces appBoundary immediately, resetting the timer.
   * Also cleared early (bbox goes null) if the backend detects the window
   * was minimized mid-flash — see track.rs's `watch_boundary`.
   * Returns true while the animation is still running, false when complete.
   */
  function drawAppBoundary(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    age: number,
  ): boolean {
    if (age >= APP_BOUNDARY_DURATION_MS) return false;

    const flashEnd = 250;
    const fadeStart = APP_BOUNDARY_DURATION_MS - 1_000; // 2000ms
    let opacity = 1.0;
    if (age > fadeStart) {
      const fadeProgress = (age - fadeStart) / 1_000;
      // ease-out cubic
      const eased = 1 - Math.pow(1 - fadeProgress, 3);
      opacity = 1 - eased;
    }

    const [r, g, b] = hexToRgb(theme.color);
    // Same weight model as the pointer so one setting means one thing everywhere;
    // at the default thickness this is 4, exactly what it was before.
    const lw = Math.max(2, DEFAULT_THICKNESS * strokeScale());

    // Inset the rect by half the widest stroke so the centered outline sits just
    // INSIDE the window edge instead of straddling it — on a fullscreen window the
    // straddling outer half is what bleeds onto the adjacent monitor. The soft glow
    // beyond is clipped to the active screen by the caller (renderFrame).
    const inset = lw * 1.1;
    bx += inset; by += inset;
    bw = Math.max(0, bw - inset * 2);
    bh = Math.max(0, bh - inset * 2);

    // Subtle inset accent fill during the flash phase only
    if (age < flashEnd) {
      const flashFill = (1 - age / flashEnd) * 0.10;
      ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${flashFill})`;
      ctx.fillRect(bx, by, bw, bh);
    }

    // Outer dark shadow for contrast against any background
    ctx.shadowBlur = 0;
    ctx.strokeStyle = `rgba(0, 0, 0, ${0.55 * opacity})`;
    ctx.lineWidth = lw * 2.2;
    ctx.lineJoin = "round";
    ctx.strokeRect(bx, by, bw, bh);

    // Accent outline with glow
    ctx.shadowColor = theme.color;
    ctx.shadowBlur = 12 * opacity + (age < flashEnd ? 14 : 0);
    ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${opacity})`;
    ctx.lineWidth = lw;
    ctx.strokeRect(bx, by, bw, bh);
    ctx.shadowBlur = 0;

    return true;
  }

  /**
   * Hint pointer — drawn when A11y and OCR both miss but the AI returned a
   * `target_bbox`. Same family as `drawBox` (ripple rings + corner brackets,
   * same pulse cadence + accent colour) so it feels like the main pointer.
   * Differences signal "approximate, not pinpointed":
   *   - Dashed brackets (instead of solid)
   *   - No scan line (no active confirmed-target sweep)
   *   - No crosshair, no corner dots (no exact-centre cues)
   *   - Slightly softer alphas
   * No label/tag — the user shouldn't need to know whether the pointer came
   * from the local locator or from the AI fallback.
   */
  // Flow A — ranked candidate boxes (kind "candidates"). Deliberately NOT the
  // confident-pointer language: no ripple rings, dashed outlines, a number chip
  // per box, opacity fading with rank (① strongest). The user is never asked to
  // click these — they act in the app as usual and the backend reads the result.
  function drawCandidates(
    ctx: CanvasRenderingContext2D,
    candidates: Rect[],
    ox: number, oy: number,
    t: number,
  ) {
    const scale = dprOf(ctx);
    const [r, g, b] = hexToRgb(theme.color);
    const k = strokeScale();
    const pulse = (Math.sin(t / 600) + 1) / 2;
    candidates.forEach((c, i) => {
      const pad = 8;
      ctx.save();
      ctx.scale(scale, scale);
      const bx = (c.x - ox - pad) / scale;
      const by = (c.y - oy - pad) / scale;
      const bw = (c.width + pad * 2) / scale;
      const bh = (c.height + pad * 2) / scale;
      // Rank styling: ① near-solid + gentle pulse, later ranks dimmer.
      const alpha = i === 0 ? 0.85 + pulse * 0.15 : Math.max(0.35, 0.65 - i * 0.15);
      ctx.setLineDash(i === 0 ? [] : [6, 5]);
      // Shadow pass for contrast on any background.
      ctx.strokeStyle = `rgba(0,0,0,${alpha * 0.6})`;
      ctx.lineWidth = 4 * k;
      roundRectPath(ctx, bx, by, bw, bh, 6);
      ctx.stroke();
      ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
      ctx.lineWidth = 2.2 * k;
      roundRectPath(ctx, bx, by, bw, bh, 6);
      ctx.stroke();
      ctx.setLineDash([]);
      // Number chip at the top-left corner.
      const chipR = 11;
      const chipX = bx + 2;
      const chipY = by + 2;
      ctx.beginPath();
      ctx.arc(chipX, chipY, chipR, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${Math.min(1, alpha + 0.1)})`;
      ctx.shadowColor = "rgba(0,0,0,0.5)";
      ctx.shadowBlur = 4;
      ctx.fill();
      ctx.shadowBlur = 0;
      ctx.fillStyle = "#fff";
      ctx.font = `700 ${chipR + 2}px 'Inter Variable', Inter, 'Segoe UI', sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(String(i + 1), chipX, chipY + 0.5);
      ctx.restore();
    });
  }

  function roundRectPath(
    ctx: CanvasRenderingContext2D,
    x: number, y: number, w: number, h: number, radius: number,
  ) {
    const rr = Math.min(radius, w / 2, h / 2);
    ctx.beginPath();
    ctx.moveTo(x + rr, y);
    ctx.arcTo(x + w, y, x + w, y + h, rr);
    ctx.arcTo(x + w, y + h, x, y + h, rr);
    ctx.arcTo(x, y + h, x, y, rr);
    ctx.arcTo(x, y, x + w, y, rr);
    ctx.closePath();
  }

  function drawHint(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    // High-DPI: draw in logical coordinates (see drawBox). Paired ctx.restore() below.
    const scale = dprOf(ctx);
    ctx.save();
    ctx.scale(scale, scale);
    bx /= scale; by /= scale; bw /= scale; bh /= scale;
    const [r, g, b] = hexToRgb(theme.color);
    const k = strokeScale();
    const pulse = (Math.sin(t / 700) + 1) / 2;
    const cx = bx + bw / 2;
    const cy = by + bh / 2;
    const { px, py, pw, ph } = markRect(bx, by, bw, bh);

    // ── RIPPLE RINGS — same animation as drawBox, slightly fainter ──
    // Ellipse, not circle — see drawBox's identical fix for why.
    const baseRx = pw / 2 + 8;
    const baseRy = ph / 2 + 8;
    const growth = Math.min(pw, ph) * 0.7;
    // On a wide, thin row (a sidebar/list item — bw ≫ bh) cap how far the ring grows on its
    // SHORT axis so it stays a flat puff hugging the row instead of a tall oval that swallows
    // the items above and below (live 2026-07-24: the Settings sidebar ring overshot its
    // neighbours). Square/tall targets keep the full, circular growth.
    const ryGrowth = pw > ph * 2 ? Math.min(growth, ph * 0.4) : growth;
    for (let i = 0; i < 3; i++) {
      const phase = ((t / 1500 + i / 3) % 1);
      const rx = baseRx + phase * growth;
      const ry = baseRy + phase * ryGrowth;
      const alpha  = (1 - phase) * 0.40;
      ctx.beginPath();
      ctx.ellipse(cx, cy, Math.max(rx, 0.1), Math.max(ry, 0.1), 0, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
      ctx.lineWidth = (2 - phase * 1.4) * k;
      ctx.shadowColor = theme.color;
      ctx.shadowBlur = 6;
      ctx.stroke();
      ctx.shadowBlur = 0;
    }

    // ── DASHED CORNER BRACKETS — looser, more tentative than drawBox ──
    const arm = Math.min(26, Math.max(14, Math.min(pw * 0.38, ph * 0.5)));
    ctx.save();
    ctx.lineCap = "butt";
    ctx.lineJoin = "miter";
    ctx.setLineDash([5, 4]);

    function bracket(ox: number, oy: number, dx: number, dy: number) {
      // Shadow layer for contrast on any background
      ctx.strokeStyle = "rgba(0,0,0,0.65)";
      ctx.lineWidth = 4.5 * k;
      ctx.beginPath();
      ctx.moveTo(ox + dx * arm, oy); ctx.lineTo(ox, oy); ctx.lineTo(ox, oy + dy * arm);
      ctx.stroke();
      // Accent layer
      ctx.strokeStyle = `rgba(${r}, ${g}, ${b}, ${0.72 + pulse * 0.15})`;
      ctx.lineWidth = 2.5 * k;
      ctx.shadowColor = theme.color;
      ctx.shadowBlur = 6 + pulse * 8;
      ctx.stroke();
      ctx.shadowBlur = 0;
    }

    bracket(px,      py,      1, 1);
    bracket(px + pw, py,     -1, 1);
    bracket(px,      py + ph, 1, -1);
    bracket(px + pw, py + ph,-1, -1);
    ctx.restore();   // inner: dash settings
    ctx.restore();   // outer: high-DPI transform
  }

  /**
   * Developer overlay — draw the raw AI-returned bounding box.
   *
   * Visually distinct from the production pointer:
   *   - Cyan (#00D9FF) instead of accent orange
   *   - Animated marching-ants dashed border (no corner brackets, no rings)
   *   - "AI" tag in the top-left corner
   *
   * Purpose: compare the AI's spatial prediction against the local locator's
   * actual finding. When they disagree, the locator may be picking the wrong
   * element OR the AI's coordinate output may be miscalibrated.
   */
  function drawAiBbox(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    const cyan = "#00D9FF";

    // Marching-ants dash offset (animated leftward to make the box feel "live")
    const dashOffset = -((t / 35) % 16);

    // Outer dark stroke for contrast against any background
    ctx.save();
    ctx.lineCap = "butt";
    ctx.lineJoin = "miter";

    ctx.strokeStyle = "rgba(0, 0, 0, 0.75)";
    ctx.lineWidth = 4;
    ctx.setLineDash([]);
    ctx.strokeRect(bx, by, bw, bh);

    // Cyan dashed accent
    ctx.strokeStyle = cyan;
    ctx.lineWidth = 2;
    ctx.setLineDash([10, 6]);
    ctx.lineDashOffset = dashOffset;
    ctx.shadowColor = cyan;
    ctx.shadowBlur = 6;
    ctx.strokeRect(bx, by, bw, bh);
    ctx.shadowBlur = 0;
    ctx.setLineDash([]);

    // "AI" tag — top-left corner, slightly outside the box
    const tagPad = 6;
    const tagFont = "bold 11px 'JetBrains Mono', ui-monospace, monospace";
    ctx.font = tagFont;
    const label = "AI";
    const labelW = ctx.measureText(label).width;
    const tagW = labelW + tagPad * 2;
    const tagH = 18;
    const tagX = bx;
    const tagY = by - tagH - 2;

    ctx.fillStyle = "rgba(0, 0, 0, 0.75)";
    ctx.fillRect(tagX, tagY, tagW, tagH);
    ctx.strokeStyle = cyan;
    ctx.lineWidth = 1;
    ctx.strokeRect(tagX + 0.5, tagY + 0.5, tagW - 1, tagH - 1);

    ctx.fillStyle = cyan;
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillText(label, tagX + tagPad, tagY + tagH / 2);

    ctx.restore();
  }

  function renderFrame(timestamp: number) {
    if (!canvas) return;
    const ctx = canvas.getContext("2d")!;

    // Pick a virtual_origin/size — prefer currentUpdate, fall back to appBoundary.
    const reference = currentUpdate ?? appBoundary;
    if (!reference) {
      // Nothing active at all — animFrame must go back to null here, or the
      // "if (animFrame === null)" re-arm check in the app_boundary listener
      // below will wrongly believe a loop is still running (it holds this
      // frame's own now-spent id) and silently skip scheduling the next flash.
      animFrame = null;
      return;
    }
    const [ox, oy] = reference.virtual_origin;
    const [vw, vh] = reference.virtual_size;

    // Only reassign canvas dimensions when they actually change.
    // Assigning canvas.width/height clears the entire canvas even when the
    // value is unchanged — doing it every frame means a single throttled rAF
    // (e.g. Windows de-prioritising the overlay WebView when the panel
    // collapses) wipes the drawn box and never redraws it.
    if (canvas.width !== vw || canvas.height !== vh) {
      canvas.width = vw;
      canvas.height = vh;
    }
    ctx.clearRect(0, 0, vw, vh);

    let needNextFrame = false;

    // Phase 0.2: app-boundary flash overlay — sits beneath the locator
    // highlight, auto-clears after APP_BOUNDARY_DURATION_MS.
    if (appBoundary && appBoundary.bbox) {
      const ageMs = timestamp - appBoundaryStart;
      const abBox = appBoundary.bbox;
      const abx = abBox.x - ox;
      const aby = abBox.y - oy;
      const abw = abBox.width;
      const abh = abBox.height;
      // Confine the boundary (stroke + glow) to the monitor the app is on, so a
      // fullscreen window's outline never bleeds onto an adjacent screen.
      const abScreen = appBoundary.active_screen;
      ctx.save();
      if (abScreen) {
        ctx.beginPath();
        ctx.rect(abScreen.x - ox, abScreen.y - oy, abScreen.width, abScreen.height);
        ctx.clip();
      }
      const stillRunning = drawAppBoundary(ctx, abx, aby, abw, abh, ageMs);
      ctx.restore();
      if (stillRunning) {
        needNextFrame = true;
      } else {
        appBoundary = null;
      }
    }

    const u = currentUpdate;
    if (u) {
      const t = timestamp - animStart;

      if (u.kind === "none") {
        // No locator overlay — but still draw the subtitle caption if present
        // so the instruction text is always visible on screen.
        if (theme.subtitle_enabled && u.text) {
          drawSubtitle(ctx, vw, vh, ox, oy, u.active_screen, u.work_area, u.text);
        }
        // Developer: still draw the AI-bbox even when the locator failed —
        // this is the case where the comparison is most useful.
        if (theme.show_ai_bbox && u.ai_bbox) {
          const ax = u.ai_bbox.x - ox;
          const ay = u.ai_bbox.y - oy;
          drawAiBbox(ctx, ax, ay, u.ai_bbox.width, u.ai_bbox.height, t);
          animFrame = requestAnimationFrame(renderFrame);
          return;
        }
        animFrame = needNextFrame ? requestAnimationFrame(renderFrame) : null;
        return;
      }

      // Subtitle is drawn alongside every overlay kind (box, hint, subtitle).
      // Rust always passes step.instruction as u.text.
      if (theme.subtitle_enabled && u.text) {
        drawSubtitle(ctx, vw, vh, ox, oy, u.active_screen, u.work_area, u.text);
      }

      // Subtitle-only step — no bbox to locate, but AI bbox dev toggle may still apply.
      if (u.kind === "subtitle") {
        if (theme.show_ai_bbox && u.ai_bbox) {
          const ax = u.ai_bbox.x - ox;
          const ay = u.ai_bbox.y - oy;
          drawAiBbox(ctx, ax, ay, u.ai_bbox.width, u.ai_bbox.height, t);
          animFrame = requestAnimationFrame(renderFrame);
          return;
        }
        animFrame = needNextFrame ? requestAnimationFrame(renderFrame) : null;
        return;
      }

      if (u.kind === "candidates" && u.candidates && u.candidates.length >= 2) {
        // Flow A: every ranked candidate gets a numbered dashed box; the user's
        // next real click in the app resolves the ambiguity — no picker UI.
        drawCandidates(ctx, u.candidates, ox, oy, t);
      } else if (u.bbox) {
        // The located rect, unpadded — markRect() inside each draw function owns
        // how far the mark stands off it, so the exporter can share the same rule.
        const bx = u.bbox.x - ox;
        const by = u.bbox.y - oy;
        const bw = u.bbox.width;
        const bh = u.bbox.height;

        if (u.kind === "hint") drawHint(ctx, bx, by, bw, bh, t);
        else if (u.kind !== "app_boundary") drawBox(ctx, bx, by, bw, bh, t);
      }

      // Developer: AI-returned bbox in cyan dashed alongside the production pointer.
      // No padding — show exactly what the AI returned, to-the-pixel.
      if (theme.show_ai_bbox && u.ai_bbox) {
        const ax = u.ai_bbox.x - ox;
        const ay = u.ai_bbox.y - oy;
        drawAiBbox(ctx, ax, ay, u.ai_bbox.width, u.ai_bbox.height, t);
      }

      if (u.bbox || (theme.show_ai_bbox && u.ai_bbox)) {
        animFrame = requestAnimationFrame(renderFrame);
        return;
      }
    }

    // Reached when only appBoundary was active (no currentUpdate) or it just
    // finished — must explicitly null animFrame, not just skip re-arming it,
    // so a later boundary-only flash isn't blocked by a stale non-null id.
    animFrame = needNextFrame ? requestAnimationFrame(renderFrame) : null;
  }

  function startAnimation(update: OverlayUpdate) {
    if (animFrame !== null) { cancelAnimationFrame(animFrame); animFrame = null; }
    currentUpdate = update;

    // Kind=none AND nothing to draw → real clear path.
    // Must also check for subtitle text: a completion step has no locator target
    // (kind=none) but still carries instruction text that should appear as caption.
    const hasAiBboxToDraw = theme.show_ai_bbox && update.ai_bbox;
    const hasSubtitleToDraw = theme.subtitle_enabled && update.text;
    if (update.kind === "none" && !hasAiBboxToDraw && !hasSubtitleToDraw) {
      if (canvas) {
        const ctx = canvas.getContext("2d");
        if (ctx) ctx.clearRect(0, 0, canvas.width, canvas.height);
      }
      return;
    }

    animStart = performance.now();
    animFrame = requestAnimationFrame(renderFrame);
  }

  onMount(async () => {
    // Tell Rust this page really loaded. The overlay window is created hidden and
    // is shown ONLY once this lands (and geometry is applied) — because an
    // overlay whose page failed to load is not a transparent canvas, it is an
    // opaque browser error page covering every monitor, always on top, over even
    // Task Manager. Reported live 2026-09-07; escaping it needed Win+Tab.
    invoke("overlay_script_alive").catch(() => {});

    await listen<OverlayUpdate>("overlay:update", (event) => {
      // Phase 0.2: AppBoundary is a transient flash, not a replacement for
      // the locator overlay. Run it on its own animation track.
      if (event.payload.kind === "app_boundary") {
        appBoundary = event.payload;
        appBoundaryStart = performance.now();
        if (animFrame === null) animFrame = requestAnimationFrame(renderFrame);
        return;
      }
      startAnimation(event.payload);
    });
    await listen<OverlayTheme>("overlay:theme", (event) => {
      // Mutate fields in-place so all active rAF callbacks immediately see the new values.
      theme.color = event.payload.color;
      theme.thickness = event.payload.thickness;
      theme.subtitle_enabled = event.payload.subtitle_enabled;
      theme.show_ai_bbox = event.payload.show_ai_bbox ?? false;
    });
  });
</script>

<canvas bind:this={canvas}></canvas>

<style>
  :global(html, body) {
    margin: 0;
    padding: 0;
    background: transparent;
    overflow: hidden;
    width: 100vw;
    height: 100vh;
    pointer-events: none;
  }

  canvas {
    position: fixed;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
</style>
