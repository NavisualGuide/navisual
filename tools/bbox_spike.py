"""AI-bbox spike runner — compares raw AI bbox (A), SoM grid labelling (B)
and Windows.Media.Ocr (C) on a shared fixture set.

Run ``--help`` for usage. See ``docs/ai-bbox-spike-plan.md`` for the spec
and ``tools/README.md`` for step-by-step instructions.

Output
------
- ``tools/spike_output/results.csv`` — one row per prediction (6 per target).
- ``tools/spike_output/raw/<fixture>_<target>_<approach>.json`` — raw
  provider response for postmortem / debugging.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import csv
import io
import json
import logging
import os
import re
import sys
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

# Repo-root / tools paths — make sure we can always import the sibling modules
# whether the script is run via ``python tools/bbox_spike.py`` or from anywhere.
TOOLS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOLS_DIR.parent
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

from _spike_metrics import (  # type: ignore  # noqa: E402
    CSV_HEADER,
    Prediction,
    area_fraction,
    bbox_to_cell,
    cell_neighbours,
    claude_bbox_to_xywh,
    gemini_bbox_to_xywh,
    grid_cell_rect,
    iou,
    size_bucket,
)
from grid_draw import draw_grid  # type: ignore  # noqa: E402
import ocr_preprocess  # type: ignore  # noqa: E402
from _spike_ocr import (  # type: ignore  # noqa: E402
    OCRResult,
    WindowsOCRUnavailable,
    find_text,
    run_windows_ocr,
)

try:
    from dotenv import load_dotenv  # type: ignore
except ImportError:  # pragma: no cover
    load_dotenv = None  # type: ignore

try:
    from PIL import Image  # noqa: F401  — used in helpers
except ImportError:
    print("error: Pillow is required. pip install -r tools/requirements.txt", file=sys.stderr)
    raise


logger = logging.getLogger("bbox_spike")

FIXTURES_DIR = REPO_ROOT / "fixtures" / "bboxes"
OUTPUT_DIR = TOOLS_DIR / "spike_output"
RAW_DIR = OUTPUT_DIR / "raw"
CSV_PATH = OUTPUT_DIR / "results.csv"

CLAUDE_MODEL_DEFAULT = "claude-sonnet-4-6"
GEMINI_MODEL_DEFAULT = "gemini-2.5-flash"

# Valid grid-cell labels: A..J (rows) + 1..10 (cols).
CELL_RE = re.compile(r"[A-J](?:10|[1-9])")


# ---------------------------------------------------------------------------
# Fixture loading
# ---------------------------------------------------------------------------

@dataclass
class Target:
    text: str
    bbox: tuple[int, int, int, int]
    cap_height_px: Optional[float]
    role: Optional[str]


@dataclass
class Fixture:
    path: Path                    # .json sidecar path
    image_path: Path
    source_width: int
    source_height: int
    targets: list[Target] = field(default_factory=list)

    @property
    def stem(self) -> str:
        return self.path.stem

    def load_image_bytes(self, fmt: str = "JPEG") -> bytes:
        """Return the image re-encoded as ``fmt`` bytes (Pillow)."""
        img = Image.open(self.image_path)
        buf = io.BytesIO()
        img.convert("RGB").save(buf, format=fmt, quality=92)
        return buf.getvalue()


def load_fixtures(fixtures_dir: Path) -> list[Fixture]:
    """Discover and validate every ``*.json`` sidecar in ``fixtures_dir``."""
    fixtures: list[Fixture] = []
    if not fixtures_dir.is_dir():
        logger.error("no fixtures directory at %s", fixtures_dir)
        return fixtures

    for sidecar in sorted(fixtures_dir.glob("*.json")):
        try:
            data = json.loads(sidecar.read_text(encoding="utf-8"))
        except Exception as e:
            logger.error("skipping %s: cannot parse JSON (%s)", sidecar.name, e)
            continue

        image_field = data.get("image")
        if not image_field:
            logger.error("skipping %s: missing 'image' field", sidecar.name)
            continue
        image_path = (sidecar.parent / image_field).resolve()
        if not image_path.is_file():
            logger.error("skipping %s: image %s not found", sidecar.name, image_path)
            continue

        try:
            with Image.open(image_path) as im:
                actual_w, actual_h = im.size
        except Exception as e:
            logger.error("skipping %s: cannot open image (%s)", sidecar.name, e)
            continue

        src_w = int(data.get("source_width", actual_w))
        src_h = int(data.get("source_height", actual_h))
        if src_w != actual_w or src_h != actual_h:
            logger.warning(
                "%s: sidecar source dims %d×%d don't match image %d×%d — using image dims",
                sidecar.name, src_w, src_h, actual_w, actual_h,
            )
            src_w, src_h = actual_w, actual_h

        targets: list[Target] = []
        for t in data.get("targets", []):
            text = t.get("text")
            bbox = t.get("bbox")
            if not text or not isinstance(bbox, list) or len(bbox) != 4:
                logger.error("%s: skipping target with bad schema: %r", sidecar.name, t)
                continue
            cap = t.get("cap_height_px")
            targets.append(Target(
                text=str(text),
                bbox=tuple(int(v) for v in bbox),  # type: ignore
                cap_height_px=float(cap) if cap is not None else None,
                role=t.get("role"),
            ))

        if not targets:
            logger.warning("%s: no targets, skipping", sidecar.name)
            continue

        fixtures.append(Fixture(
            path=sidecar,
            image_path=image_path,
            source_width=src_w,
            source_height=src_h,
            targets=targets,
        ))

    return fixtures


# ---------------------------------------------------------------------------
# Raw-response persistence
# ---------------------------------------------------------------------------

def _safe_name(s: str) -> str:
    return re.sub(r"[^A-Za-z0-9_-]+", "_", s.strip())[:40] or "x"


def save_raw(fixture: Fixture, target: Target, approach_id: str, data: Any) -> None:
    """Write a raw provider response as JSON for postmortem. Never raises."""
    RAW_DIR.mkdir(parents=True, exist_ok=True)
    fname = f"{fixture.stem}__{_safe_name(target.text)}__{approach_id}.json"
    try:
        RAW_DIR.joinpath(fname).write_text(
            json.dumps(data, indent=2, default=str, ensure_ascii=False),
            encoding="utf-8",
        )
    except Exception as e:  # pragma: no cover
        logger.warning("could not write raw response %s: %s", fname, e)


# ---------------------------------------------------------------------------
# Approach A — raw AI bbox
# ---------------------------------------------------------------------------

CLAUDE_BBOX_TOOL = {
    "name": "report_bbox",
    "description": (
        "Return the pixel bounding box [x0, y0, x1, y1] of the requested UI element "
        "in the source image. Coordinates are absolute pixels, integers."
    ),
    "input_schema": {
        "type": "object",
        "properties": {
            "bbox": {
                "type": "array",
                "items": {"type": "integer"},
                "minItems": 4,
                "maxItems": 4,
                "description": "[x0, y0, x1, y1] in source-image pixels.",
            },
            "found": {
                "type": "boolean",
                "description": "false if the element is not visible in the image.",
            },
        },
        "required": ["bbox"],
    },
}

CLAUDE_GRID_TOOL = {
    "name": "report_cell",
    "description": (
        "Return the 2-character grid cell (e.g. 'D4') that contains the centre of "
        "the requested element. Rows are A–J top-to-bottom; columns are 1–10 "
        "left-to-right."
    ),
    "input_schema": {
        "type": "object",
        "properties": {
            "cell": {
                "type": "string",
                "pattern": "^[A-J](?:10|[1-9])$",
            },
            "found": {
                "type": "boolean",
                "description": "false if the element is not visible.",
            },
        },
        "required": ["cell"],
    },
}


async def _claude_call(
    *,
    api_key: str,
    model: str,
    image_bytes: bytes,
    prompt: str,
    tool: dict,
    timeout_sec: int = 60,
) -> dict:
    """One-shot Claude Messages call with tool_use forced to ``tool``.

    Returns the parsed ``input`` dict from the first matching tool_use block,
    plus the raw response JSON under key ``__raw`` for persistence.
    """
    try:
        import httpx  # type: ignore
    except ImportError as e:
        raise RuntimeError("httpx is required for Claude API calls") from e

    url = "https://api.anthropic.com/v1/messages"
    headers = {
        "x-api-key": api_key,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
    }
    payload = {
        "model": model,
        "max_tokens": 256,
        "tools": [tool],
        "tool_choice": {"type": "tool", "name": tool["name"]},
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/jpeg",
                        "data": base64.b64encode(image_bytes).decode("ascii"),
                    },
                },
                {"type": "text", "text": prompt},
            ],
        }],
    }
    async with httpx.AsyncClient(timeout=timeout_sec) as client:
        resp = await client.post(url, headers=headers, json=payload)
        resp.raise_for_status()
        data = resp.json()

    # Find the first tool_use block with the right name.
    for block in data.get("content", []):
        if block.get("type") == "tool_use" and block.get("name") == tool["name"]:
            return {"input": block.get("input", {}), "__raw": data}
    return {"input": {}, "__raw": data}


async def _gemini_call(
    *,
    api_key: str,
    model: str,
    image_bytes: bytes,
    prompt: str,
    tool: Optional[dict] = None,
    timeout_sec: int = 60,
) -> dict:
    """One-shot Gemini generateContent. If ``tool`` given, forces function call.

    Returns ``{"input": <args-or-{text: ...}>, "__raw": <full response>}``.
    """
    try:
        import httpx  # type: ignore
    except ImportError as e:
        raise RuntimeError("httpx is required for Gemini API calls") from e

    url = (
        f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        f"?key={api_key}"
    )
    body: dict = {
        "contents": [{
            "role": "user",
            "parts": [
                {
                    "inlineData": {
                        "mimeType": "image/jpeg",
                        "data": base64.b64encode(image_bytes).decode("ascii"),
                    },
                },
                {"text": prompt},
            ],
        }],
    }
    if tool is not None:
        body["tools"] = [{"function_declarations": [tool]}]
        body["toolConfig"] = {
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [tool["name"]],
            },
        }

    async with httpx.AsyncClient(timeout=timeout_sec) as client:
        resp = await client.post(url, json=body)
        resp.raise_for_status()
        data = resp.json()

    out: dict = {"input": {}, "__raw": data}
    candidates = data.get("candidates", []) or []
    for cand in candidates:
        parts = cand.get("content", {}).get("parts", []) or []
        for part in parts:
            if "functionCall" in part and (tool is None or part["functionCall"].get("name") == tool["name"]):
                out["input"] = part["functionCall"].get("args", {})
                return out
            if "text" in part and not out["input"]:
                out["input"] = {"text": part["text"]}
    return out


GEMINI_BBOX_FUNCTION = {
    "name": "report_bbox",
    "description": (
        "Return the normalised bounding box of the requested UI element. "
        "Gemini's native [ymin, xmin, ymax, xmax] format in 0–1000."
    ),
    "parameters": {
        "type": "object",
        "required": ["bbox"],
        "properties": {
            "bbox": {
                "type": "array",
                "items": {"type": "integer"},
                "minItems": 4,
                "maxItems": 4,
                "description": "[ymin, xmin, ymax, xmax] normalised to 0..1000.",
            },
            "found": {
                "type": "boolean",
                "description": "false if the element is not visible.",
            },
        },
    },
}


def _prompt_for_bbox(target_text: str, image_width: int, image_height: int) -> str:
    return (
        f"Locate the UI element labelled '{target_text}' in this screenshot "
        f"(image is {image_width}×{image_height} pixels). "
        f"Return its bounding box via the report_bbox tool."
    )


def _prompt_for_cell(target_text: str) -> str:
    return (
        f"This image has a 10×10 grid drawn on it. Rows are labelled A to J "
        f"from top to bottom; columns are labelled 1 to 10 from left to right. "
        f"Find the UI element labelled '{target_text}' and report the single "
        f"grid cell (e.g. 'D4') that contains the element's centre."
    )


def _parse_gemini_cell_text(text: str) -> Optional[str]:
    """Pull the first cell label out of a free-text Gemini response."""
    m = CELL_RE.search(text.upper())
    return m.group(0) if m else None


# ---------------------------------------------------------------------------
# Predictions per approach
# ---------------------------------------------------------------------------

async def predict_A_claude(
    fixture: Fixture,
    target: Target,
    api_key: str,
    model: str,
) -> Prediction:
    image_bytes = fixture.load_image_bytes("JPEG")
    prompt = _prompt_for_bbox(target.text, fixture.source_width, fixture.source_height)
    try:
        result = await _claude_call(
            api_key=api_key, model=model,
            image_bytes=image_bytes, prompt=prompt, tool=CLAUDE_BBOX_TOOL,
        )
    except Exception as e:
        save_raw(fixture, target, "A-claude-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "A", "claude", notes=f"error: {e!r}")

    save_raw(fixture, target, "A-claude", result)
    args = result.get("input", {})
    box = args.get("bbox")
    found_flag = args.get("found", True) if isinstance(args, dict) else True
    if not box or found_flag is False:
        return _unfound(fixture, target, "A", "claude", notes="no bbox returned")
    try:
        bbox = claude_bbox_to_xywh(box, fixture.source_width, fixture.source_height)
    except Exception as e:
        return _unfound(fixture, target, "A", "claude", notes=f"parse: {e}")
    return _scored(fixture, target, "A", "claude", bbox, notes="")


async def predict_A_gemini(
    fixture: Fixture,
    target: Target,
    api_key: str,
    model: str,
) -> Prediction:
    image_bytes = fixture.load_image_bytes("JPEG")
    prompt = _prompt_for_bbox(target.text, fixture.source_width, fixture.source_height)
    try:
        result = await _gemini_call(
            api_key=api_key, model=model,
            image_bytes=image_bytes, prompt=prompt, tool=GEMINI_BBOX_FUNCTION,
        )
    except Exception as e:
        save_raw(fixture, target, "A-gemini-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "A", "gemini", notes=f"error: {e!r}")

    save_raw(fixture, target, "A-gemini", result)
    args = result.get("input", {})
    box = args.get("bbox") if isinstance(args, dict) else None
    if not box:
        return _unfound(fixture, target, "A", "gemini", notes="no bbox returned")
    try:
        bbox = gemini_bbox_to_xywh(box, fixture.source_width, fixture.source_height)
    except Exception as e:
        return _unfound(fixture, target, "A", "gemini", notes=f"parse: {e}")
    return _scored(fixture, target, "A", "gemini", bbox, notes="")


async def predict_B_claude(
    fixture: Fixture,
    target: Target,
    api_key: str,
    model: str,
) -> Prediction:
    # Render grid to JPEG in-memory.
    img = Image.open(fixture.image_path)
    gridded = draw_grid(img)
    buf = io.BytesIO()
    gridded.save(buf, format="JPEG", quality=92)
    image_bytes = buf.getvalue()

    prompt = _prompt_for_cell(target.text)
    try:
        result = await _claude_call(
            api_key=api_key, model=model,
            image_bytes=image_bytes, prompt=prompt, tool=CLAUDE_GRID_TOOL,
        )
    except Exception as e:
        save_raw(fixture, target, "B-claude-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "B", "claude", notes=f"error: {e!r}")

    save_raw(fixture, target, "B-claude", result)
    args = result.get("input", {})
    cell = args.get("cell") if isinstance(args, dict) else None
    if not cell or not CELL_RE.fullmatch(str(cell).upper()):
        return _unfound(fixture, target, "B", "claude", notes=f"bad cell: {cell!r}")
    try:
        bbox = grid_cell_rect(cell, fixture.source_width, fixture.source_height)
    except Exception as e:
        return _unfound(fixture, target, "B", "claude", notes=f"cell parse: {e}")
    return _scored(fixture, target, "B", "claude", bbox, notes=f"cell={cell.upper()}")


async def predict_B_gemini(
    fixture: Fixture,
    target: Target,
    api_key: str,
    model: str,
) -> Prediction:
    img = Image.open(fixture.image_path)
    gridded = draw_grid(img)
    buf = io.BytesIO()
    gridded.save(buf, format="JPEG", quality=92)
    image_bytes = buf.getvalue()

    # For B-gemini the plan specifies plain text output parsed against the regex.
    prompt = _prompt_for_cell(target.text) + (
        " Respond with ONLY the cell label (e.g. 'D4'), no other text."
    )
    try:
        result = await _gemini_call(
            api_key=api_key, model=model,
            image_bytes=image_bytes, prompt=prompt, tool=None,
        )
    except Exception as e:
        save_raw(fixture, target, "B-gemini-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "B", "gemini", notes=f"error: {e!r}")

    save_raw(fixture, target, "B-gemini", result)
    args = result.get("input", {})
    cell: Optional[str] = None
    if isinstance(args, dict) and "text" in args:
        cell = _parse_gemini_cell_text(args["text"])
    if not cell:
        return _unfound(fixture, target, "B", "gemini", notes="no cell in text response")
    try:
        bbox = grid_cell_rect(cell, fixture.source_width, fixture.source_height)
    except Exception as e:
        return _unfound(fixture, target, "B", "gemini", notes=f"cell parse: {e}")
    return _scored(fixture, target, "B", "gemini", bbox, notes=f"cell={cell}")


def predict_C_baseline(fixture: Fixture, target: Target) -> Prediction:
    image_bytes = fixture.load_image_bytes("JPEG")
    try:
        results = run_windows_ocr(image_bytes)
    except WindowsOCRUnavailable as e:
        return _unfound(fixture, target, "C", "ocr-baseline", notes=str(e))
    except Exception as e:  # pragma: no cover
        save_raw(fixture, target, "C-baseline-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "C", "ocr-baseline", notes=f"error: {e!r}")

    save_raw(fixture, target, "C-baseline", {
        "results": [{"text": r.text, "bbox": r.bbox, "confidence": r.confidence} for r in results],
    })

    match = find_text(
        target.text,
        results,
        target_role=target.role,
        screen_width=fixture.source_width,
        screen_height=fixture.source_height,
    )
    if match is None:
        return _unfound(fixture, target, "C", "ocr-baseline", notes="not found")
    return _scored(fixture, target, "C", "ocr-baseline", match.bbox, notes=f"ocr='{match.text}'")


def predict_C_preprocessed(fixture: Fixture, target: Target) -> Prediction:
    img = Image.open(fixture.image_path)
    pre = ocr_preprocess.preprocess(img)
    buf = io.BytesIO()
    pre.save(buf, format="JPEG", quality=92)
    image_bytes = buf.getvalue()

    try:
        results = run_windows_ocr(image_bytes)
    except WindowsOCRUnavailable as e:
        return _unfound(fixture, target, "C", "ocr-preprocessed", notes=str(e))
    except Exception as e:
        save_raw(fixture, target, "C-preprocessed-error", {"error": repr(e), "trace": traceback.format_exc()})
        return _unfound(fixture, target, "C", "ocr-preprocessed", notes=f"error: {e!r}")

    # Bboxes are in preprocessed-image pixels → rescale to source image.
    rescaled = [
        OCRResult(
            text=r.text,
            bbox=ocr_preprocess.rescale_bbox_back(r.bbox),
            confidence=r.confidence,
        )
        for r in results
    ]
    save_raw(fixture, target, "C-preprocessed", {
        "results": [{"text": r.text, "bbox": r.bbox, "confidence": r.confidence} for r in rescaled],
    })

    match = find_text(
        target.text,
        rescaled,
        target_role=target.role,
        screen_width=fixture.source_width,
        screen_height=fixture.source_height,
    )
    if match is None:
        return _unfound(fixture, target, "C", "ocr-preprocessed", notes="not found")
    return _scored(fixture, target, "C", "ocr-preprocessed", match.bbox, notes=f"ocr='{match.text}'")


# ---------------------------------------------------------------------------
# Scoring / prediction helpers
# ---------------------------------------------------------------------------

def _scored(
    fixture: Fixture, target: Target,
    approach: str, provider: str,
    bbox: tuple[int, int, int, int],
    notes: str = "",
) -> Prediction:
    score = iou(bbox, target.bbox)
    return Prediction(
        fixture=fixture.stem,
        target_text=target.text,
        cap_height_px=target.cap_height_px,
        target_area_frac=area_fraction(target.bbox, fixture.source_width, fixture.source_height),
        approach=approach,
        provider=provider,
        found=True,
        bbox=bbox,
        iou_score=score,
        notes=notes,
    )


def _unfound(
    fixture: Fixture, target: Target,
    approach: str, provider: str,
    notes: str = "",
) -> Prediction:
    return Prediction(
        fixture=fixture.stem,
        target_text=target.text,
        cap_height_px=target.cap_height_px,
        target_area_frac=area_fraction(target.bbox, fixture.source_width, fixture.source_height),
        approach=approach,
        provider=provider,
        found=False,
        bbox=None,
        iou_score=0.0,
        notes=notes,
    )


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------

async def run_spike(
    fixtures: list[Fixture],
    *,
    only_approach: Optional[str],
    claude_api_key: Optional[str],
    gemini_api_key: Optional[str],
    claude_model: str,
    gemini_model: str,
) -> list[Prediction]:
    all_preds: list[Prediction] = []

    def want(approach: str) -> bool:
        return only_approach is None or only_approach == approach

    for fx in fixtures:
        for tgt in fx.targets:
            logger.info("→ %s / %r (bucket=%s)",
                        fx.stem, tgt.text,
                        size_bucket(tgt.bbox, fx.source_width, fx.source_height))

            if want("A"):
                if claude_api_key:
                    all_preds.append(await predict_A_claude(fx, tgt, claude_api_key, claude_model))
                else:
                    all_preds.append(_unfound(fx, tgt, "A", "claude", notes="ANTHROPIC_API_KEY not set"))
                if gemini_api_key:
                    all_preds.append(await predict_A_gemini(fx, tgt, gemini_api_key, gemini_model))
                else:
                    all_preds.append(_unfound(fx, tgt, "A", "gemini", notes="GEMINI_API_KEY not set"))

            if want("B"):
                if claude_api_key:
                    all_preds.append(await predict_B_claude(fx, tgt, claude_api_key, claude_model))
                else:
                    all_preds.append(_unfound(fx, tgt, "B", "claude", notes="ANTHROPIC_API_KEY not set"))
                if gemini_api_key:
                    all_preds.append(await predict_B_gemini(fx, tgt, gemini_api_key, gemini_model))
                else:
                    all_preds.append(_unfound(fx, tgt, "B", "gemini", notes="GEMINI_API_KEY not set"))

            if want("C"):
                all_preds.append(predict_C_baseline(fx, tgt))
                all_preds.append(predict_C_preprocessed(fx, tgt))

    return all_preds


def write_results(preds: list[Prediction], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(CSV_HEADER)
        for p in preds:
            w.writerow(p.as_csv_row())


# ---------------------------------------------------------------------------
# Dry-run / summary
# ---------------------------------------------------------------------------

def dry_run(fixtures: list[Fixture], only_approach: Optional[str]) -> None:
    if not fixtures:
        print("no fixtures found under fixtures/bboxes/")
        return

    approaches = [only_approach] if only_approach else ["A", "B", "C"]
    per_approach = {"A": 2, "B": 2, "C": 2}  # claude+gemini or baseline+preprocessed
    total_targets = sum(len(fx.targets) for fx in fixtures)
    total_preds = sum(per_approach[a] * total_targets for a in approaches)

    print(f"fixtures     : {len(fixtures)}")
    print(f"targets      : {total_targets}")
    print(f"approaches   : {','.join(approaches)}")
    print(f"predictions  : {total_preds}  (would be written to {CSV_PATH.relative_to(REPO_ROOT)})")
    print()
    for fx in fixtures:
        print(f"  [{fx.stem}]  {fx.image_path.name}  {fx.source_width}×{fx.source_height}")
        for tgt in fx.targets:
            bucket = size_bucket(tgt.bbox, fx.source_width, fx.source_height)
            cap = tgt.cap_height_px
            cap_s = f"{cap:.1f}px" if cap is not None else "?"
            print(f"     - {tgt.text!r:30s}  bbox={tgt.bbox}  bucket={bucket:6s} cap={cap_s}  role={tgt.role}")


def print_summary(preds: list[Prediction]) -> None:
    """Compact stdout summary after a real run."""
    from collections import defaultdict
    buckets: dict[tuple[str, str], list[Prediction]] = defaultdict(list)
    for p in preds:
        buckets[(p.approach, p.provider)].append(p)

    print()
    print(f"{'approach':<8} {'provider':<20} {'found/total':<12} {'iou>=0.5':<10} {'iou>=0.7':<10} {'mean_iou':<10}")
    print("-" * 80)
    for (approach, provider), plist in sorted(buckets.items()):
        total = len(plist)
        found = sum(1 for p in plist if p.found)
        ge50 = sum(1 for p in plist if p.iou_score >= 0.5)
        ge70 = sum(1 for p in plist if p.iou_score >= 0.7)
        mean_iou = sum(p.iou_score for p in plist) / total if total else 0.0
        print(f"{approach:<8} {provider:<20} {found}/{total:<10} "
              f"{ge50}/{total:<8} {ge70}/{total:<8} {mean_iou:.3f}")


# ---------------------------------------------------------------------------
# Self-test helpers (B-approach cell scoring sanity)
# ---------------------------------------------------------------------------

def _sanity_check_helpers() -> None:
    """Cheap checks — fail fast at startup if a helper is broken.

    Expected values account for the 5% left gutter in grid_draw.GUTTER_FRAC:
    on a 1000×1000 image gutter=50px, active_w=950px, cell_w=95px.
    """
    r = grid_cell_rect("A1", 1000, 1000)
    assert r == (50, 0, 95, 100), r          # col 1 starts after gutter
    r = grid_cell_rect("J10", 1000, 1000)
    assert r[0] + r[2] == 1000 and r[1] + r[3] == 1000, r
    # centre (505,505): cx_active=455, col=int(455*10/950)=4 → col5, row=5 → F
    assert bbox_to_cell((500, 500, 10, 10), 1000, 1000) == "F5", bbox_to_cell((500, 500, 10, 10), 1000, 1000)
    assert "A1" in cell_neighbours("A1")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Run the AI-bbox spike (A: raw bbox, B: SoM grid, C: OCR).",
    )
    p.add_argument(
        "--fixtures-dir",
        type=Path,
        default=FIXTURES_DIR,
        help=f"Where to find *.json sidecars (default: {FIXTURES_DIR})",
    )
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Parse fixtures and print what would run; no API calls.",
    )
    p.add_argument(
        "--only-approach",
        choices=["A", "B", "C"],
        default=None,
        help="Restrict the sweep to a single approach.",
    )
    p.add_argument(
        "--claude-model",
        default=os.environ.get("CLAUDE_MODEL", CLAUDE_MODEL_DEFAULT),
    )
    p.add_argument(
        "--gemini-model",
        default=os.environ.get("GEMINI_MODEL", GEMINI_MODEL_DEFAULT),
    )
    p.add_argument(
        "--log-level",
        default=os.environ.get("LOG_LEVEL", "INFO"),
    )
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = _build_parser().parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    _sanity_check_helpers()

    # Load .env from repo root if dotenv is available.
    if load_dotenv is not None:
        load_dotenv(dotenv_path=REPO_ROOT / ".env")

    fixtures = load_fixtures(args.fixtures_dir)
    if not fixtures:
        print(
            f"no usable fixtures found under {args.fixtures_dir}. "
            f"See fixtures/bboxes/README.md for the schema.",
            file=sys.stderr,
        )
        return 1 if not args.dry_run else 0

    if args.dry_run:
        dry_run(fixtures, args.only_approach)
        return 0

    claude_key = os.environ.get("ANTHROPIC_API_KEY")
    gemini_key = os.environ.get("GEMINI_API_KEY")
    if args.only_approach in (None, "A", "B") and not (claude_key or gemini_key):
        print(
            "warning: neither ANTHROPIC_API_KEY nor GEMINI_API_KEY is set. "
            "Approach A/B predictions will all be marked unfound.",
            file=sys.stderr,
        )

    preds = asyncio.run(run_spike(
        fixtures,
        only_approach=args.only_approach,
        claude_api_key=claude_key,
        gemini_api_key=gemini_key,
        claude_model=args.claude_model,
        gemini_model=args.gemini_model,
    ))
    write_results(preds, CSV_PATH)
    print_summary(preds)
    print(f"\nwrote {CSV_PATH} ({len(preds)} predictions)")
    print(f"raw responses in {RAW_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
