<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  type Rect = { x: number; y: number; width: number; height: number };
  type OverlayUpdate = {
    kind: "arrow" | "box" | "subtitle" | "none";
    bbox: Rect | null;
    text: string | null;
    virtual_origin: [number, number];
    virtual_size: [number, number];
  };

  let canvas: HTMLCanvasElement;
  let currentUpdate: OverlayUpdate | null = null;
  let animFrame: number | null = null;
  let animStart = 0;

  // Draw the highlight box with a pulsing glow.
  // t = milliseconds elapsed since the box first appeared.
  function drawBox(
    ctx: CanvasRenderingContext2D,
    bx: number, by: number, bw: number, bh: number,
    t: number,
  ) {
    const pulse = (Math.sin(t / 700) + 1) / 2; // 0→1, ~4.4 s period

    // Subtle orange fill
    ctx.fillStyle = `rgba(255, 107, 53, ${0.07 + pulse * 0.07})`;
    ctx.fillRect(bx, by, bw, bh);

    // Dark shadow outline — ensures contrast on any background color
    ctx.shadowBlur = 0;
    ctx.strokeStyle = "rgba(0, 0, 0, 0.7)";
    ctx.lineWidth = 9;
    ctx.lineJoin = "round";
    ctx.strokeRect(bx, by, bw, bh);

    // Pulsing orange border
    ctx.shadowColor = "#FF6B35";
    ctx.shadowBlur = 8 + pulse * 18;
    ctx.strokeStyle = "#FF6B35";
    ctx.lineWidth = 4;
    ctx.strokeRect(bx, by, bw, bh);
    ctx.shadowBlur = 0;

    // Corner L-marks: white base layer + orange glow layer
    const tick = Math.min(24, Math.max(12, Math.min(bw * 0.35, bh * 0.35)));
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    function corners(color: string, width: number, glow: number) {
      ctx.strokeStyle = color;
      ctx.lineWidth = width;
      ctx.shadowColor = glow ? "#FF6B35" : "transparent";
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
    corners("#FF6B35", 2.5, 4 + pulse * 10);
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
    ctx.fillStyle = "#CC4410";
    ctx.beginPath();
    ctx.moveTo(cx, tipY + 2);
    ctx.lineTo(cx - halfBase, shaftTopY + 2);
    ctx.lineTo(cx + halfBase, shaftTopY + 2);
    ctx.closePath();
    ctx.fill();

    // Arrow body with glow
    ctx.shadowColor = "#FF6B35";
    ctx.shadowBlur = 6 + pulse * 12;
    ctx.fillStyle = "#FF6B35";
    ctx.beginPath();
    ctx.moveTo(cx, tipY);
    ctx.lineTo(cx - halfBase, shaftTopY);
    ctx.lineTo(cx + halfBase, shaftTopY);
    ctx.closePath();
    ctx.fill();
    ctx.shadowBlur = 0;
  }

  function drawSubtitle(ctx: CanvasRenderingContext2D, cw: number, ch: number, text: string) {
    const stripH = 72;
    const stripY = ch - stripH - 8;
    const pad = 16;
    const r = 10;

    ctx.fillStyle = "rgba(0,0,0,0.78)";
    ctx.beginPath();
    ctx.moveTo(pad + r, stripY);
    ctx.lineTo(cw - pad - r, stripY);
    ctx.quadraticCurveTo(cw - pad, stripY, cw - pad, stripY + r);
    ctx.lineTo(cw - pad, stripY + stripH - r);
    ctx.quadraticCurveTo(cw - pad, stripY + stripH, cw - pad - r, stripY + stripH);
    ctx.lineTo(pad + r, stripY + stripH);
    ctx.quadraticCurveTo(pad, stripY + stripH, pad, stripY + stripH - r);
    ctx.lineTo(pad, stripY + r);
    ctx.quadraticCurveTo(pad, stripY, pad + r, stripY);
    ctx.closePath();
    ctx.fill();

    ctx.fillStyle = "#FFFFFF";
    ctx.font = "bold 18px Inter, -apple-system, 'Segoe UI', sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    const maxWidth = cw * 0.8;
    const words = text.split(" ");
    const lines: string[] = [];
    let line = "";
    for (const word of words) {
      const test = line ? `${line} ${word}` : word;
      if (ctx.measureText(test).width > maxWidth && line) { lines.push(line); line = word; }
      else { line = test; }
    }
    if (line) lines.push(line);

    const lineH = 22;
    const totalH = lines.length * lineH;
    const startY = stripY + stripH / 2 - totalH / 2 + lineH / 2;
    for (let i = 0; i < lines.length; i++) {
      ctx.fillText(lines[i], cw / 2, startY + i * lineH, maxWidth);
    }
  }

  function renderFrame(timestamp: number) {
    if (!canvas || !currentUpdate) return;
    const ctx = canvas.getContext("2d")!;
    const u = currentUpdate;
    const [ox, oy] = u.virtual_origin;
    const [vw, vh] = u.virtual_size;

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

    const t = timestamp - animStart;

    if (u.kind === "none") return; // canvas cleared, stop

    if (u.kind === "subtitle") {
      drawSubtitle(ctx, vw, vh, u.text ?? "");
      // Subtitles are static — no need to keep animating
      return;
    }

    if (!u.bbox) return;
    const padding = 6;
    const bx = u.bbox.x - ox - padding;
    const by = u.bbox.y - oy - padding;
    const bw = u.bbox.width + padding * 2;
    const bh = u.bbox.height + padding * 2;

    if (u.kind === "arrow") drawArrow(ctx, bx, by, bw, bh, t);
    else drawBox(ctx, bx, by, bw, bh, t);

    animFrame = requestAnimationFrame(renderFrame);
  }

  function startAnimation(update: OverlayUpdate) {
    if (animFrame !== null) { cancelAnimationFrame(animFrame); animFrame = null; }
    currentUpdate = update;

    if (update.kind === "none") {
      // Just clear the canvas — no animation needed
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
    await listen<OverlayUpdate>("overlay:update", (event) => {
      startAnimation(event.payload);
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
