<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  type Rect = { x: number; y: number; width: number; height: number };
  type OverlayUpdate = {
    kind: "arrow" | "box" | "subtitle" | "app_boundary" | "none";
    bbox: Rect | null;
    text: string | null;
    virtual_origin: [number, number];
    virtual_size: [number, number];
    active_screen: Rect | null;
  };
  type OverlayTheme = { color: string; thickness: number; subtitle_enabled: boolean };
  type GridUpdate = {
    capture_rect: Rect | null;
    virtual_origin: [number, number];
    virtual_size: [number, number];
    highlighted_cell: string | null;
    cols: number;
    rows: number;
  };

  let canvas: HTMLCanvasElement;
  let currentUpdate: OverlayUpdate | null = null;
  let gridUpdate: GridUpdate | null = null;
  let animFrame: number | null = null;
  let animStart = 0;

  // Phase 0.2: brief animated outline of the captured app's window. Lives
  // alongside the main overlay so it doesn't replace the locator highlight.
  let appBoundary: OverlayUpdate | null = null;
  let appBoundaryStart = 0;
  const APP_BOUNDARY_DURATION_MS = 10_000; // 9s solid + 1s ease-out fade
  // Plain object — NOT $state. drawBox/drawArrow read this in rAF callbacks where
  // Svelte's reactive getters don't fire; mutating fields in-place ensures every
  // frame sees the latest values without any signal overhead.
  let theme: OverlayTheme = { color: "#FF6B35", thickness: 4, subtitle_enabled: true };

  function hexToRgb(hex: string): [number, number, number] {
    const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
    return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : [255, 107, 53];
  }

  // Draw the highlight box with a pulsing glow.
  // t = milliseconds elapsed since the box first appeared.
  function drawBox(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    const [r, g, b] = hexToRgb(theme.color);
    const pulse = (Math.sin(t / 700) + 1) / 2; // 0→1, ~4.4 s period
    const lw = Math.max(1, theme.thickness);

    // Subtle accent fill
    ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${0.07 + pulse * 0.07})`;
    ctx.fillRect(bx, by, bw, bh);

    // Dark shadow outline — ensures contrast on any background color
    ctx.shadowBlur = 0;
    ctx.strokeStyle = "rgba(0, 0, 0, 0.7)";
    ctx.lineWidth = lw * 2.25;
    ctx.lineJoin = "round";
    ctx.strokeRect(bx, by, bw, bh);

    // Pulsing accent border
    ctx.shadowColor = theme.color;
    ctx.shadowBlur = 8 + pulse * 18;
    ctx.strokeStyle = theme.color;
    ctx.lineWidth = lw;
    ctx.strokeRect(bx, by, bw, bh);
    ctx.shadowBlur = 0;

    // Corner L-marks: white base layer + accent glow layer
    const tick = Math.min(24, Math.max(12, Math.min(bw * 0.35, bh * 0.35)));
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    function corners(color: string, width: number, glow: number) {
      ctx.strokeStyle = color;
      ctx.lineWidth = width;
      ctx.shadowColor = glow ? theme.color : "transparent";
      ctx.shadowBlur = glow;
      // top-left
      ctx.beginPath(); ctx.moveTo(bx, by + tick); ctx.lineTo(bx, by); ctx.lineTo(bx + tick, by); ctx.stroke();
      // top-right
      ctx.beginPath(); ctx.moveTo(bx + bw - tick, by); ctx.lineTo(bx + bw, by); ctx.lineTo(bx + bw, by + tick); ctx.stroke();
      // bottom-left
      ctx.beginPath(); ctx.moveTo(bx, by + bh - tick); ctx.lineTo(bx, by + bh); ctx.lineTo(bx + tick, by + bh); ctx.stroke();
      // bottom-right (note: last lineTo goes UP, not back across — fixes original bug)
      ctx.beginPath(); ctx.moveTo(bx + bw - tick, by + bh); ctx.lineTo(bx + bw, by + bh); ctx.lineTo(bx + bw, by + bh - tick); ctx.stroke();
      ctx.shadowBlur = 0;
    }

    corners("#FFFFFF", 3.5, 0);
    corners(theme.color, 2.5, 4 + pulse * 10);
  }

  function drawArrow(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    drawBox(ctx, bx, by, bw, bh, t);

    const pulse = (Math.sin(t / 700) + 1) / 2;
    const cx = bx + bw / 2;
    const tipY = by - 4;
    const shaftTopY = tipY - 36;
    const halfBase = 12;

    // Shadow
    ctx.shadowColor = "rgba(0,0,0,0.5)";
    ctx.shadowBlur = 8;
    ctx.fillStyle = "rgba(0,0,0,0.35)";
    ctx.beginPath();
    ctx.moveTo(cx, tipY + 2);
    ctx.lineTo(cx - halfBase, shaftTopY + 2);
    ctx.lineTo(cx + halfBase, shaftTopY + 2);
    ctx.closePath();
    ctx.fill();

    // Arrow body with glow
    ctx.shadowColor = theme.color;
    ctx.shadowBlur = 6 + pulse * 12;
    ctx.fillStyle = theme.color;
    ctx.beginPath();
    ctx.moveTo(cx, tipY);
    ctx.lineTo(cx - halfBase, shaftTopY);
    ctx.lineTo(cx + halfBase, shaftTopY);
    ctx.closePath();
    ctx.fill();
    ctx.shadowBlur = 0;
  }

  // Draw subtitle strip confined to a single screen.
  // Strip width fits the text content rather than spanning the full screen.
  function drawSubtitle(
    ctx: CanvasRenderingContext2D,
    canvasW: number, canvasH: number,
    ox: number, oy: number,
    activeScreen: Rect | null,
    text: string,
  ) {
    const sx = activeScreen ? (activeScreen.x - ox) : 0;
    const sy = activeScreen ? (activeScreen.y - oy) : 0;
    const sw = activeScreen ? activeScreen.width  : canvasW;
    const sh = activeScreen ? activeScreen.height : canvasH;

    const hPad = 22;
    const vPad = 12;
    const r = 10;
    const maxTextW = sw * 0.78;
    const cx = sx + sw / 2;

    // Measure text first so strip width can fit the content
    ctx.font = "bold 18px Inter, -apple-system, 'Segoe UI', sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    const words = text.split(" ");
    const lines: string[] = [];
    let line = "";
    for (const word of words) {
      const test = line ? `${line} ${word}` : word;
      if (ctx.measureText(test).width > maxTextW && line) { lines.push(line); line = word; }
      else { line = test; }
    }
    if (line) lines.push(line);

    const lineH = 22;
    const maxLineW = Math.max(...lines.map(l => ctx.measureText(l).width));
    const stripW = Math.min(maxLineW + hPad * 2, maxTextW + hPad * 2);
    const stripH = lines.length * lineH + vPad * 2;
    const left   = cx - stripW / 2;
    const right  = cx + stripW / 2;
    const stripY = sy + sh - stripH - 10;

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
   * Three-stage animation over APP_BOUNDARY_DURATION_MS (10 s):
   *   0..250ms  — flash in at full opacity with inner glow
   *   250..9000ms — hold at full opacity (solid outline)
   *   9000..10000ms — ease-out cubic fade to 0
   * A new capture replaces appBoundary immediately, resetting the timer.
   * Returns true while the animation is still running, false when complete.
   */
  function drawAppBoundary(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    age: number,
  ): boolean {
    if (age >= APP_BOUNDARY_DURATION_MS) return false;

    const flashEnd = 250;
    const fadeStart = APP_BOUNDARY_DURATION_MS - 1_000; // 9000ms
    let opacity = 1.0;
    if (age > fadeStart) {
      const fadeProgress = (age - fadeStart) / 1_000;
      // ease-out cubic
      const eased = 1 - Math.pow(1 - fadeProgress, 3);
      opacity = 1 - eased;
    }

    const [r, g, b] = hexToRgb(theme.color);
    const lw = Math.max(2, theme.thickness);

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

  function drawGrid(ctx: CanvasRenderingContext2D, g: GridUpdate, canvasW: number, canvasH: number) {
    const [ox, oy] = g.virtual_origin;
    const gx = g.capture_rect ? g.capture_rect.x - ox : 0;
    const gy = g.capture_rect ? g.capture_rect.y - oy : 0;
    const gw = g.capture_rect ? g.capture_rect.width  : canvasW;
    const gh = g.capture_rect ? g.capture_rect.height : canvasH;
    const cellW = gw / g.cols;
    const cellH = gh / g.rows;

    // Highlight selected cell
    if (g.highlighted_cell && g.highlighted_cell.length >= 2) {
      const row = g.highlighted_cell.charCodeAt(0) - "A".charCodeAt(0);
      const col = parseInt(g.highlighted_cell.slice(1), 10) - 1;
      if (row >= 0 && row < g.rows && col >= 0 && col < g.cols) {
        ctx.fillStyle = "rgba(255, 107, 53, 0.30)";
        ctx.fillRect(gx + col * cellW, gy + row * cellH, cellW, cellH);
        ctx.strokeStyle = "rgba(255, 107, 53, 0.85)";
        ctx.lineWidth = 2;
        ctx.setLineDash([]);
        ctx.strokeRect(gx + col * cellW + 1, gy + row * cellH + 1, cellW - 2, cellH - 2);
      }
    }

    // Grid lines (dashed white)
    ctx.strokeStyle = "rgba(255, 255, 255, 0.35)";
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 3]);
    for (let c = 1; c < g.cols; c++) {
      const x = gx + c * cellW;
      ctx.beginPath(); ctx.moveTo(x, gy); ctx.lineTo(x, gy + gh); ctx.stroke();
    }
    for (let r = 1; r < g.rows; r++) {
      const y = gy + r * cellH;
      ctx.beginPath(); ctx.moveTo(gx, y); ctx.lineTo(gx + gw, y); ctx.stroke();
    }
    ctx.setLineDash([]);

    // Cell labels (top-left corner of each cell)
    ctx.font = "bold 10px monospace";
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    for (let r = 0; r < g.rows; r++) {
      for (let c = 0; c < g.cols; c++) {
        const label = String.fromCharCode("A".charCodeAt(0) + r) + (c + 1);
        const lx = gx + c * cellW + 2;
        const ly = gy + r * cellH + 2;
        ctx.fillStyle = "rgba(0,0,0,0.55)";
        ctx.fillText(label, lx + 1, ly + 1);
        ctx.fillStyle = "rgba(255,255,255,0.75)";
        ctx.fillText(label, lx, ly);
      }
    }
  }

  function renderFrame(timestamp: number) {
    if (!canvas) return;
    const ctx = canvas.getContext("2d")!;

    // Pick a virtual_origin/size — prefer currentUpdate, fall back to appBoundary.
    const reference = currentUpdate ?? appBoundary;
    if (!reference) return;
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

    // Draw grid test overlay beneath other elements (drawn every frame so it
    // persists alongside animated arrows/boxes without a separate rAF loop).
    if (gridUpdate) drawGrid(ctx, gridUpdate, vw, vh);

    let needNextFrame = false;

    // Phase 0.2: app-boundary flash overlay — sits beneath the locator
    // highlight, auto-clears after APP_BOUNDARY_DURATION_MS.
    if (appBoundary && appBoundary.bbox) {
      const ageMs = timestamp - appBoundaryStart;
      const ab = appBoundary;
      const abx = ab.bbox.x - ox;
      const aby = ab.bbox.y - oy;
      const abw = ab.bbox.width;
      const abh = ab.bbox.height;
      const stillRunning = drawAppBoundary(ctx, abx, aby, abw, abh, ageMs);
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
        // No regular overlay — keep the rAF alive only if grid or boundary need redraw.
        if (gridUpdate || needNextFrame) animFrame = requestAnimationFrame(renderFrame);
        return;
      }

      // Subtitle is drawn alongside every overlay type (arrow, box, subtitle-only).
      // Rust always passes step.instruction as u.text.
      if (theme.subtitle_enabled && u.text) {
        drawSubtitle(ctx, vw, vh, ox, oy, u.active_screen, u.text);
      }

      // Subtitle-only step — no bbox to locate, no animation loop needed.
      if (u.kind === "subtitle") {
        if (needNextFrame) animFrame = requestAnimationFrame(renderFrame);
        return;
      }

      if (u.bbox) {
        const padding = 12;
        const bx = u.bbox.x - ox - padding;
        const by = u.bbox.y - oy - padding;
        const bw = u.bbox.width + padding * 2;
        const bh = u.bbox.height + padding * 2;

        if (u.kind === "arrow") drawArrow(ctx, bx, by, bw, bh, t);
        else if (u.kind !== "app_boundary") drawBox(ctx, bx, by, bw, bh, t);
        animFrame = requestAnimationFrame(renderFrame);
        return;
      }
    }

    if (needNextFrame) animFrame = requestAnimationFrame(renderFrame);
  }

  function startAnimation(update: OverlayUpdate) {
    if (animFrame !== null) { cancelAnimationFrame(animFrame); animFrame = null; }
    currentUpdate = update;

    if (update.kind === "none") {
      if (canvas) {
        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.clearRect(0, 0, canvas.width, canvas.height);
          // Re-draw the grid even when the regular overlay is cleared.
          if (gridUpdate) {
            const [vw, vh] = gridUpdate.virtual_size;
            if (canvas.width !== vw || canvas.height !== vh) { canvas.width = vw; canvas.height = vh; }
            drawGrid(ctx, gridUpdate, vw, vh);
          }
        }
      }
      return;
    }

    animStart = performance.now();
    animFrame = requestAnimationFrame(renderFrame);
  }

  onMount(async () => {
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
    });
    await listen<GridUpdate>("overlay:grid", (event) => {
      gridUpdate = event.payload;
      // If no animation loop is running, draw the grid immediately.
      if (animFrame === null && canvas) {
        const ctx = canvas.getContext("2d");
        if (ctx) {
          const [vw, vh] = gridUpdate.virtual_size;
          if (canvas.width !== vw || canvas.height !== vh) { canvas.width = vw; canvas.height = vh; }
          ctx.clearRect(0, 0, vw, vh);
          drawGrid(ctx, gridUpdate, vw, vh);
        }
      }
    });
    await listen<void>("overlay:grid_clear", () => {
      gridUpdate = null;
      if (animFrame === null && canvas) {
        const ctx = canvas.getContext("2d");
        if (ctx) ctx.clearRect(0, 0, canvas.width, canvas.height);
      }
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
