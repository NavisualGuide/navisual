"""Set-of-Marks grid overlay for the AI-bbox spike — Approach B input.

Draws a 10×10 grid (rows A..J, cols 1..10) on a source screenshot:

- Faint grid lines (alpha ~40 on black) so they don't hide UI text.
- Opaque white label tiles at the top (col numbers) and left (row letters)
  edges, each with black text, so labels remain readable against any
  backdrop.

Library API
-----------
``draw_grid(img: PIL.Image.Image, rows: int = 10, cols: int = 10) -> PIL.Image.Image``
    Returns a new RGB image with the grid drawn on top.

CLI
---
``python tools/grid_draw.py --input src.jpg --output grid.png``

A small smoke-test mode (``--smoke /tmp/grid_test.png``) renders a solid-
colour 768×432 image with the grid so you can eyeball the overlay.
"""

from __future__ import annotations

import argparse
import os
import sys
from typing import Optional

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError as exc:  # pragma: no cover — surfaced clearly at CLI
    print(
        "error: Pillow is required. Install with: pip install -r tools/requirements.txt",
        file=sys.stderr,
    )
    raise

ROW_LABELS = "ABCDEFGHIJ"  # 10 rows
LINE_ALPHA = 40            # out of 255 — faint but visible
LINE_COLOR_BLACK = (0, 0, 0, LINE_ALPHA)
LINE_COLOR_WHITE = (255, 255, 255, LINE_ALPHA)
TILE_BG = (255, 255, 255, 235)  # nearly opaque white
TILE_FG = (0, 0, 0, 255)         # black text

# Left gutter for row labels — keeps them out of column 1's content area so
# Claude doesn't count the label tile as a separate column.
GUTTER_FRAC: float = 0.05   # 5% of image width


def _load_font(pixels: int) -> ImageFont.ImageFont:
    """Try to load a TTF font at the requested pixel size; fall back to default.

    Pillow's default bitmap font is tiny and not scaleable — if we can find a
    TTF on the system we use it so the labels are legible.
    """
    candidates = [
        "arial.ttf",
        "Arial.ttf",
        "DejaVuSans-Bold.ttf",
        "DejaVuSans.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
    ]
    for path in candidates:
        try:
            return ImageFont.truetype(path, pixels)
        except (OSError, IOError):
            continue
    return ImageFont.load_default()


def draw_grid(
    img: Image.Image,
    rows: int = 10,
    cols: int = 10,
) -> Image.Image:
    """Overlay a faint 10×10 grid with row/column labels on ``img``.

    The returned image is RGB (no alpha) so it round-trips through JPEG.
    """
    if rows > len(ROW_LABELS):
        raise ValueError(f"rows ({rows}) exceeds available row labels ({len(ROW_LABELS)})")

    base = img.convert("RGBA")
    w, h = base.size
    overlay = Image.new("RGBA", base.size, (0, 0, 0, 0))
    drw = ImageDraw.Draw(overlay)

    # Left gutter — row labels live here so they don't overlap column 1.
    gutter = max(16, int(round(w * GUTTER_FRAC)))

    # --- opaque label tiles: size based on cell dims inside the active area.
    cell_w = (w - gutter) / cols
    cell_h = h / rows
    tile_w = max(14, int(round(cell_w * 0.30)))
    tile_h = max(12, int(round(cell_h * 0.30)))
    tile_w = min(tile_w, int(round(cell_w * 0.60)))
    tile_h = min(tile_h, int(round(cell_h * 0.60)))

    font_pixels = max(9, min(tile_h - 3, int(round(tile_h * 0.85))))
    font = _load_font(font_pixels)

    def _draw_tile(x: int, y: int, tw: int, th: int, text: str) -> None:
        drw.rectangle((x, y, x + tw, y + th), fill=TILE_BG)
        try:
            bbox = drw.textbbox((0, 0), text, font=font)
            text_w = bbox[2] - bbox[0]
            text_h = bbox[3] - bbox[1]
            offset_x = bbox[0]
            offset_y = bbox[1]
        except AttributeError:
            text_w, text_h = drw.textsize(text, font=font)
            offset_x = offset_y = 0
        tx = x + (tw - text_w) // 2 - offset_x
        ty = y + (th - text_h) // 2 - offset_y
        drw.text((tx, ty), text, fill=TILE_FG, font=font)

    # --- faint grid lines (columns start at gutter; rows span full width)
    for c in range(cols + 1):
        x = gutter + int(round(c * (w - gutter) / cols))
        drw.line([(x, 0), (x, h)], fill=LINE_COLOR_BLACK, width=1)
    for r in range(1, rows):
        y = int(round(r * h / rows))
        drw.line([(0, y), (w, y)], fill=LINE_COLOR_BLACK, width=1)

    # Row label tiles — in the left gutter, centred vertically per row
    for r_idx in range(rows):
        row_centre_y = int(round((r_idx + 0.5) * h / rows))
        ty = row_centre_y - tile_h // 2
        _draw_tile(0, ty, gutter - 2, tile_h, ROW_LABELS[r_idx])

    # Column label tiles — centred within each column (after gutter), top edge
    for c_idx in range(cols):
        col_left = gutter + int(round(c_idx * (w - gutter) / cols))
        col_right = gutter + int(round((c_idx + 1) * (w - gutter) / cols))
        col_centre_x = (col_left + col_right) // 2
        tx = col_centre_x - tile_w // 2
        _draw_tile(tx, 0, tile_w, tile_h, str(c_idx + 1))

    out = Image.alpha_composite(base, overlay).convert("RGB")
    return out


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Draw a 10×10 Set-of-Marks grid on an image.",
    )
    p.add_argument("--input", help="Source image (PNG/JPEG).")
    p.add_argument("--output", help="Where to write the gridded image.")
    p.add_argument("--rows", type=int, default=10)
    p.add_argument("--cols", type=int, default=10)
    p.add_argument(
        "--smoke",
        metavar="PATH",
        help="Generate a 768×432 solid-colour test image with the grid and exit.",
    )
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = _build_parser().parse_args(argv)

    if args.smoke:
        test = Image.new("RGB", (768, 432), (64, 96, 140))
        out = draw_grid(test, rows=args.rows, cols=args.cols)
        os.makedirs(os.path.dirname(os.path.abspath(args.smoke)) or ".", exist_ok=True)
        out.save(args.smoke)
        print(f"wrote smoke-test grid to {args.smoke}")
        return 0

    if not args.input or not args.output:
        print("error: --input and --output are required (or use --smoke)", file=sys.stderr)
        return 2

    img = Image.open(args.input)
    gridded = draw_grid(img, rows=args.rows, cols=args.cols)
    os.makedirs(os.path.dirname(os.path.abspath(args.output)) or ".", exist_ok=True)
    gridded.save(args.output)
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
