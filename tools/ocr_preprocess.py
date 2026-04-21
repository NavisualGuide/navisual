"""Tier 1 OCR preprocessing for the AI-bbox spike — Approach C-preprocessed.

Standalone prototype of the preprocessing that ``src-tauri/src/locator/ocr.rs``
would add if we decide to ship Tier 1. Mirrors the spec from
``docs/ocr-improvements-plan.md`` (and §Step 2 of the spike plan):

- 2× Lanczos upscale.
- Mild contrast boost (~ +20, i.e. ``ImageEnhance.Contrast(1.2)``).

After running OCR on the preprocessed image, the caller **must** rescale
returned bounding boxes back to source-image coordinates by dividing all
four bbox fields by :data:`SCALE`.

Library API
-----------
``preprocess(img: PIL.Image.Image) -> PIL.Image.Image``
    Returns a new RGB image upscaled by ``SCALE`` with contrast boosted.

``SCALE``
    The constant factor by which bboxes must be rescaled to return them
    to source-image pixels.
"""

from __future__ import annotations

import argparse
import os
import sys
from typing import Optional

from PIL import Image, ImageEnhance

# 2× upscale — big enough to clear the Windows.Media.Ocr reliability floor
# (~28–32 px cap height) for 10–15 px source text, without doubling OCR
# runtime more than necessary.
SCALE: float = 2.0

# Mild contrast boost — matches ImageEnhance.Contrast(CONTRAST_FACTOR) in Pillow.
# +20 on a 0..100 scale ≈ factor 1.2.
CONTRAST_FACTOR: float = 1.2


def preprocess(img: Image.Image) -> Image.Image:
    """Apply the Tier 1 preprocessing pipeline.

    Pipeline:
        1. Convert to RGB if not already.
        2. Lanczos resample to ``SCALE``× dimensions.
        3. Boost contrast by ``CONTRAST_FACTOR``.
    """
    rgb = img.convert("RGB")
    new_w = int(round(rgb.width * SCALE))
    new_h = int(round(rgb.height * SCALE))
    upscaled = rgb.resize((new_w, new_h), Image.Resampling.LANCZOS)
    boosted = ImageEnhance.Contrast(upscaled).enhance(CONTRAST_FACTOR)
    return boosted


def rescale_bbox_back(bbox: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    """Divide a bbox returned by OCR on the preprocessed image by ``SCALE``.

    Returns the equivalent bbox in source-image coordinates, rounded to int.
    """
    x, y, w, h = bbox
    return (
        int(round(x / SCALE)),
        int(round(y / SCALE)),
        int(round(w / SCALE)),
        int(round(h / SCALE)),
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description=(
            "Apply Tier 1 OCR preprocessing (2× Lanczos upscale + contrast boost)."
        ),
    )
    p.add_argument("--input", required=True, help="Source image.")
    p.add_argument("--output", required=True, help="Where to write the preprocessed image.")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = _build_parser().parse_args(argv)
    img = Image.open(args.input)
    out = preprocess(img)
    os.makedirs(os.path.dirname(os.path.abspath(args.output)) or ".", exist_ok=True)
    out.save(args.output)
    print(
        f"wrote {args.output}  ({img.size[0]}×{img.size[1]} → {out.size[0]}×{out.size[1]},"
        f" SCALE={SCALE})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
