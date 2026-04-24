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
  let clearTimer: ReturnType<typeof setTimeout> | null = null;

  function drawBox(
    ctx: CanvasRenderingContext2D,
    bx: number,
    by: number,
    bw: number,
    bh: number,
  ) {
    ctx.strokeStyle = "rgba(0,0,0,0.75)";
    ctx.lineWidth = 6;
    ctx.lineJoin = "round";
    ctx.strokeRect(bx - 3, by - 3, bw + 6, bh + 6);

    ctx.strokeStyle = "#FF6B35";
    ctx.lineWidth = 3;
    ctx.lineJoin = "round";
    ctx.strokeRect(bx, by, bw, bh);

    const tick = Math.min(14, Math.max(6, bw * 0.3), Math.max(6, bh * 0.3));
    ctx.strokeStyle = "#FF6B35";
    ctx.lineWidth = 2;
    ctx.beginPath(); ctx.moveTo(bx, by + tick); ctx.lineTo(bx, by); ctx.lineTo(bx + tick, by); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(bx + bw - tick, by); ctx.lineTo(bx + bw, by); ctx.lineTo(bx + bw, by + tick); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(bx, by + bh - tick); ctx.lineTo(bx, by + bh); ctx.lineTo(bx + tick, by + bh); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(bx + bw - tick, by + bh); ctx.lineTo(bx + bw, by + bh); ctx.lineTo(bx + bw - tick, by + bh); ctx.stroke();
  }

  function drawArrow(
    ctx: CanvasRenderingContext2D,
    bx: number,
    by: number,
    bw: number,
    bh: number,
  ) {
    drawBox(ctx, bx, by, bw, bh);

    // Downward-pointing triangle above the bbox top edge.
    // Tip touches bbox top; shaft extends 30px upward.
    const cx = bx + bw / 2;
    const tipY = by;
    const shaftTopY = tipY - 30;
    const halfBase = 10;

    ctx.shadowColor = "rgba(0,0,0,0.6)";
    ctx.shadowBlur = 6;

    ctx.fillStyle = "#FF6B35";
    ctx.beginPath();
    ctx.moveTo(cx, tipY);
    ctx.lineTo(cx - halfBase, shaftTopY);
    ctx.lineTo(cx + halfBase, shaftTopY);
    ctx.closePath();
    ctx.fill();

    ctx.shadowBlur = 0;
  }

  function drawSubtitle(
    ctx: CanvasRenderingContext2D,
    cw: number,
    ch: number,
    text: string,
  ) {
    const stripH = 72;
    const stripY = ch - stripH - 8;
    const pad = 16;

    ctx.fillStyle = "rgba(0,0,0,0.72)";
    ctx.beginPath();
    const r = 10;
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
      if (ctx.measureText(test).width > maxWidth && line) {
        lines.push(line);
        line = word;
      } else {
        line = test;
      }
    }
    if (line) lines.push(line);

    const lineH = 22;
    const totalH = lines.length * lineH;
    const startY = stripY + stripH / 2 - totalH / 2 + lineH / 2;
    for (let i = 0; i < lines.length; i++) {
      ctx.fillText(lines[i], cw / 2, startY + i * lineH, maxWidth);
    }
  }

  function draw(update: OverlayUpdate) {
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const [ox, oy] = update.virtual_origin;
    const [vw, vh] = update.virtual_size;

    canvas.width = vw;
    canvas.height = vh;
    ctx.clearRect(0, 0, vw, vh);

    if (update.kind === "none") return;

    if (update.kind === "subtitle") {
      drawSubtitle(ctx, vw, vh, update.text ?? "");
      return;
    }

    if (!update.bbox) return;

    const bx = update.bbox.x - ox;
    const by = update.bbox.y - oy;
    const bw = update.bbox.width;
    const bh = update.bbox.height;

    if (update.kind === "arrow") {
      drawArrow(ctx, bx, by, bw, bh);
    } else {
      drawBox(ctx, bx, by, bw, bh);
    }
  }

  onMount(async () => {
    await listen<OverlayUpdate>("overlay:update", (event) => {
      const update = event.payload;

      if (clearTimer !== null) {
        clearTimeout(clearTimer);
        clearTimer = null;
      }

      draw(update);

      if (update.kind === "subtitle") {
        // Subtitles persist until explicitly cleared (kind=none).
        return;
      }

      if (update.kind !== "none") {
        clearTimer = setTimeout(() => {
          if (canvas) {
            const ctx = canvas.getContext("2d");
            ctx?.clearRect(0, 0, canvas.width, canvas.height);
          }
          clearTimer = null;
        }, 6000);
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
