"""Scoring primitives for the AI-bbox spike.

Kept in its own module so the IoU computation, size-bucket assignment and
cap-height binning can be unit-tested without touching any I/O / API code.

All bboxes are `(x, y, w, h)` in source-image pixels unless noted otherwise.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional


BBox = tuple[int, int, int, int]  # (x, y, w, h)


# ---------------------------------------------------------------------------
# IoU
# ---------------------------------------------------------------------------

def iou(a: BBox, b: BBox) -> float:
    """Intersection-over-union of two (x, y, w, h) boxes.

    Returns 0.0 if either box is degenerate or they do not overlap.
    """
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    if aw <= 0 or ah <= 0 or bw <= 0 or bh <= 0:
        return 0.0

    ax2, ay2 = ax + aw, ay + ah
    bx2, by2 = bx + bw, by + bh

    ix1 = max(ax, bx)
    iy1 = max(ay, by)
    ix2 = min(ax2, bx2)
    iy2 = min(ay2, by2)
    iw = ix2 - ix1
    ih = iy2 - iy1
    if iw <= 0 or ih <= 0:
        return 0.0

    inter = iw * ih
    union = aw * ah + bw * bh - inter
    if union <= 0:
        return 0.0
    return inter / union


def centre(b: BBox) -> tuple[float, float]:
    """Return the (cx, cy) centre of a (x, y, w, h) bbox."""
    x, y, w, h = b
    return x + w / 2.0, y + h / 2.0


def centre_error_px(pred: BBox, truth: BBox) -> float:
    """Euclidean distance between the centres of two boxes, in image pixels."""
    px, py = centre(pred)
    tx, ty = centre(truth)
    return ((px - tx) ** 2 + (py - ty) ** 2) ** 0.5


# ---------------------------------------------------------------------------
# Size buckets (§Step 3 of the spike plan)
# ---------------------------------------------------------------------------

# Large: > 3 % of image area, Medium: 0.3 – 3 %, Small: < 0.3 %.
LARGE_FRACTION = 0.03
MEDIUM_FRACTION = 0.003


def size_bucket(bbox: BBox, image_width: int, image_height: int) -> str:
    """Return one of {"large", "medium", "small"} for a ground-truth bbox."""
    if image_width <= 0 or image_height <= 0:
        return "small"
    area_frac = (bbox[2] * bbox[3]) / float(image_width * image_height)
    if area_frac > LARGE_FRACTION:
        return "large"
    if area_frac > MEDIUM_FRACTION:
        return "medium"
    return "small"


def area_fraction(bbox: BBox, image_width: int, image_height: int) -> float:
    """Fractional area of the bbox vs the image (0..1)."""
    if image_width <= 0 or image_height <= 0:
        return 0.0
    return (bbox[2] * bbox[3]) / float(image_width * image_height)


# ---------------------------------------------------------------------------
# Cap-height bins (§Step 3)
# ---------------------------------------------------------------------------

# Upper bounds — value falls in the first bin whose upper edge it is <= to.
CAP_HEIGHT_BIN_EDGES: list[tuple[float, str]] = [
    (6, "<6"),
    (9, "6-9"),
    (12, "9-12"),
    (18, "12-18"),
    (30, "18-30"),
    (float("inf"), ">=30"),
]


def cap_height_bin(cap_height_px: Optional[float]) -> str:
    """Return the cap-height bucket label for a measured cap height in pixels.

    ``None`` maps to ``"unknown"`` so fixtures missing the measurement don't
    silently collapse into the smallest bin.
    """
    if cap_height_px is None:
        return "unknown"
    for edge, label in CAP_HEIGHT_BIN_EDGES:
        if cap_height_px <= edge:
            return label
    return ">=30"


# ---------------------------------------------------------------------------
# Gemini bbox conversion
# ---------------------------------------------------------------------------

def gemini_bbox_to_xywh(
    gemini_box: list[float] | tuple[float, float, float, float],
    image_width: int,
    image_height: int,
) -> BBox:
    """Convert Gemini's native bbox format to our (x, y, w, h) pixel tuple.

    Gemini returns ``[ymin, xmin, ymax, xmax]`` normalised to the 0–1000
    range relative to the source image. We convert back to absolute pixels.
    """
    if len(gemini_box) != 4:
        raise ValueError(f"expected 4 numbers, got {len(gemini_box)}: {gemini_box}")
    ymin, xmin, ymax, xmax = [float(v) for v in gemini_box]
    # Guard against swapped order just in case the model inverts.
    if xmax < xmin:
        xmin, xmax = xmax, xmin
    if ymax < ymin:
        ymin, ymax = ymax, ymin
    x = int(round(xmin / 1000.0 * image_width))
    y = int(round(ymin / 1000.0 * image_height))
    w = int(round((xmax - xmin) / 1000.0 * image_width))
    h = int(round((ymax - ymin) / 1000.0 * image_height))
    # Clamp to image bounds
    x = max(0, min(image_width - 1, x))
    y = max(0, min(image_height - 1, y))
    w = max(0, min(image_width - x, w))
    h = max(0, min(image_height - y, h))
    return (x, y, w, h)


def claude_bbox_to_xywh(
    claude_box: list[int] | tuple[int, int, int, int],
    image_width: int,
    image_height: int,
) -> BBox:
    """Convert Claude's reported [x0, y0, x1, y1] pixel rect to (x, y, w, h).

    Defensive: clamps to image bounds and handles swapped corners.
    """
    if len(claude_box) != 4:
        raise ValueError(f"expected 4 numbers, got {len(claude_box)}: {claude_box}")
    x0, y0, x1, y1 = [int(v) for v in claude_box]
    if x1 < x0:
        x0, x1 = x1, x0
    if y1 < y0:
        y0, y1 = y1, y0
    x = max(0, min(image_width - 1, x0))
    y = max(0, min(image_height - 1, y0))
    w = max(0, min(image_width - x, x1 - x0))
    h = max(0, min(image_height - y, y1 - y0))
    return (x, y, w, h)


# ---------------------------------------------------------------------------
# Grid cell ↔ bbox (Approach B scoring helpers)
# ---------------------------------------------------------------------------

ROW_LABELS = "ABCDEFGHIJ"  # 10 rows
COL_LABELS = [str(i) for i in range(1, 11)]  # 10 cols


def grid_cell_rect(
    cell: str,
    image_width: int,
    image_height: int,
    rows: int = 10,
    cols: int = 10,
) -> BBox:
    """Convert a 2-char cell label like ``"D4"`` into its pixel rect.

    Rows are A..J (top→bottom); columns are 1..10 (left→right).
    """
    label = cell.strip().upper()
    if len(label) < 2 or label[0] not in ROW_LABELS:
        raise ValueError(f"bad cell label: {cell!r}")
    row_idx = ROW_LABELS.index(label[0])
    try:
        col_idx = int(label[1:]) - 1
    except ValueError as e:
        raise ValueError(f"bad cell column in {cell!r}") from e
    if not (0 <= row_idx < rows) or not (0 <= col_idx < cols):
        raise ValueError(f"cell out of range: {cell!r}")

    cell_w = image_width / cols
    cell_h = image_height / rows
    x = int(round(col_idx * cell_w))
    y = int(round(row_idx * cell_h))
    w = int(round((col_idx + 1) * cell_w)) - x
    h = int(round((row_idx + 1) * cell_h)) - y
    return (x, y, w, h)


def bbox_to_cell(
    bbox: BBox,
    image_width: int,
    image_height: int,
    rows: int = 10,
    cols: int = 10,
) -> str:
    """Return the grid cell label containing the bbox centre."""
    cx, cy = centre(bbox)
    col_idx = max(0, min(cols - 1, int(cx * cols / image_width)))
    row_idx = max(0, min(rows - 1, int(cy * rows / image_height)))
    return f"{ROW_LABELS[row_idx]}{col_idx + 1}"


def cell_neighbours(cell: str, rows: int = 10, cols: int = 10) -> list[str]:
    """Return the 3×3 neighbourhood of cells around ``cell`` (inclusive)."""
    label = cell.strip().upper()
    row_idx = ROW_LABELS.index(label[0])
    col_idx = int(label[1:]) - 1
    out: list[str] = []
    for dr in (-1, 0, 1):
        for dc in (-1, 0, 1):
            r = row_idx + dr
            c = col_idx + dc
            if 0 <= r < rows and 0 <= c < cols:
                out.append(f"{ROW_LABELS[r]}{c + 1}")
    return out


# ---------------------------------------------------------------------------
# Prediction outcome
# ---------------------------------------------------------------------------

@dataclass
class Prediction:
    """A single approach's prediction for one (image, target) pair."""
    fixture: str
    target_text: str
    cap_height_px: Optional[float]
    target_area_frac: float
    approach: str           # "A" | "B" | "C"
    provider: str           # "claude" | "gemini" | "ocr-baseline" | "ocr-preprocessed"
    found: bool
    bbox: Optional[BBox]    # None when found=False
    iou_score: float        # 0.0 when not found
    notes: str

    def as_csv_row(self) -> list:
        bx, by, bw, bh = self.bbox if self.bbox else ("", "", "", "")
        return [
            self.fixture,
            self.target_text,
            "" if self.cap_height_px is None else round(self.cap_height_px, 1),
            round(self.target_area_frac, 5),
            self.approach,
            self.provider,
            int(self.found),
            bx, by, bw, bh,
            round(self.iou_score, 3),
            self.notes,
        ]


CSV_HEADER = [
    "fixture", "target_text", "cap_height_px", "target_area_frac",
    "approach", "provider", "found",
    "bbox_x", "bbox_y", "bbox_w", "bbox_h",
    "iou", "notes",
]
