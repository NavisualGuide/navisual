//! Windows.Media.Ocr wrapper + `find_text` matcher.
//!
//! Port of `legacy/src/locator/ocr_engine.py`'s Windows backend and matcher
//! (PaddleOCR fallback is not ported — Windows is the MVP target). Runs
//! OCR synchronously on a blocking tokio task; the WinRT async operations
//! are awaited via `windows_future::Async::join()`.
//!
//! OcrResult bboxes are in **image pixels** (relative to the JPEG passed in
//! by the caller). The orchestrator is responsible for adding any crop
//! origin back on before producing a virtual-desktop rect.

use anyhow::{anyhow, Context, Result};
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
// `IAsyncOperation<T>::join()` is exposed directly (via windows_future::join).

use super::trace::OcrCandidate;

/// A single OCR detection — line-level or word-level.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OcrResult {
    pub text: String,
    /// (x, y, w, h) in image-pixel coords.
    pub bbox: (i32, i32, u32, u32),
    pub confidence: f32,
}

fn point_in_bbox(x: i32, y: i32, b: &(i32, i32, u32, u32)) -> bool {
    x >= b.0 && x < b.0 + b.2 as i32 && y >= b.1 && y < b.1 + b.3 as i32
}

/// Fraction of the winner's containing OCR line that `target` occupies, plus that line's
/// character length. A real control label fills its line (~1.0); content text embeds the
/// word in a long line (low ratio). Used by the locator's corroboration gate. Coords are
/// image-pixel space (same as `results`).
pub fn isolation_for(
    winner_bbox: &(i32, i32, u32, u32),
    target: &str,
    results: &[OcrResult],
) -> (f32, usize) {
    let wcx = winner_bbox.0 + winner_bbox.2 as i32 / 2;
    let wcy = winner_bbox.1 + winner_bbox.3 as i32 / 2;
    let line_text = results
        .iter()
        .filter(|r| r.confidence >= 1.0) // line-level results
        .find(|r| point_in_bbox(wcx, wcy, &r.bbox))
        .map(|r| r.text.as_str())
        .unwrap_or(target); // no containing line → treat as fully isolated
    let line_len = line_text.chars().count().max(1);
    let ratio = (target.chars().count() as f32 / line_len as f32).min(1.0);
    (ratio, line_len)
}

/// True when `anchor` text appears in the OCR results within ~1/4 image-diagonal of the
/// winner — a soft corroborator (the AI's `nearby_text` label sits next to the real target).
///
/// The winner itself and any line containing it are excluded: a result that contains the
/// winner's centre also contains the winner's own text, so "anchoring" on it is the locator
/// agreeing with itself (observed when a model sets nearby_text equal to target_text — the
/// wrong "Auto" corroborated itself). A genuinely adjacent anchor word survives because OCR
/// also emits word-level results, which sit beside — not around — the winner.
pub fn anchor_near(
    winner_bbox: &(i32, i32, u32, u32),
    anchor: &str,
    results: &[OcrResult],
    img_w: u32,
    img_h: u32,
) -> bool {
    let anchor_l = anchor.trim().to_ascii_lowercase();
    if anchor_l.chars().count() < 2 {
        return false;
    }
    let wcx_i = winner_bbox.0 + winner_bbox.2 as i32 / 2;
    let wcy_i = winner_bbox.1 + winner_bbox.3 as i32 / 2;
    let wcx = wcx_i as f32;
    let wcy = wcy_i as f32;
    let thresh = ((img_w as f32).powi(2) + (img_h as f32).powi(2)).sqrt() * 0.25;
    results.iter().any(|r| {
        if point_in_bbox(wcx_i, wcy_i, &r.bbox) {
            return false; // the winner itself / its containing line — not independent evidence
        }
        if !r.text.to_ascii_lowercase().contains(&anchor_l) {
            return false;
        }
        let acx = r.bbox.0 as f32 + r.bbox.2 as f32 / 2.0;
        let acy = r.bbox.1 as f32 + r.bbox.3 as f32 / 2.0;
        ((acx - wcx).powi(2) + (acy - wcy).powi(2)).sqrt() <= thresh
    })
}

/// Run Windows.Media.Ocr on the given image bytes (JPEG or PNG). Returns
/// line-level results plus word-level results (so single-word targets can
/// still get a tight bbox instead of the whole line's span).
pub fn run_ocr(image_bytes: &[u8]) -> Result<Vec<OcrResult>> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| anyhow!("OCR engine init: {e}"))?;

    // Wrap the bytes in an InMemoryRandomAccessStream via a DataWriter.
    let stream = InMemoryRandomAccessStream::new().map_err(|e| anyhow!("create stream: {e}"))?;
    let output_stream = stream
        .GetOutputStreamAt(0)
        .map_err(|e| anyhow!("get output stream: {e}"))?;
    let writer = DataWriter::CreateDataWriter(&output_stream)
        .map_err(|e| anyhow!("create datawriter: {e}"))?;
    writer
        .WriteBytes(image_bytes)
        .map_err(|e| anyhow!("write bytes: {e}"))?;
    writer
        .StoreAsync()
        .map_err(|e| anyhow!("StoreAsync: {e}"))?
        .join()
        .map_err(|e| anyhow!("StoreAsync await: {e}"))?;
    writer
        .FlushAsync()
        .map_err(|e| anyhow!("FlushAsync: {e}"))?
        .join()
        .map_err(|e| anyhow!("FlushAsync await: {e}"))?;
    writer
        .DetachStream()
        .map_err(|e| anyhow!("DetachStream: {e}"))?;

    // Rewind the backing stream so the decoder reads from the start.
    stream.Seek(0).context("stream seek")?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|e| anyhow!("BitmapDecoder::CreateAsync: {e}"))?
        .join()
        .map_err(|e| anyhow!("BitmapDecoder await: {e}"))?;

    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| anyhow!("GetSoftwareBitmapAsync: {e}"))?
        .join()
        .map_err(|e| anyhow!("GetSoftwareBitmap await: {e}"))?;

    let ocr = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| anyhow!("RecognizeAsync: {e}"))?
        .join()
        .map_err(|e| anyhow!("RecognizeAsync await: {e}"))?;

    let mut out = Vec::new();
    let lines = ocr.Lines().map_err(|e| anyhow!("Lines: {e}"))?;
    for line in lines {
        let words = line.Words().map_err(|e| anyhow!("Words: {e}"))?;
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut parts: Vec<String> = Vec::new();
        for word in &words {
            let r = match word.BoundingRect() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let x = r.X;
            let y = r.Y;
            let w = r.Width;
            let h = r.Height;
            if x < min_x {
                min_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if x + w > max_x {
                max_x = x + w;
            }
            if y + h > max_y {
                max_y = y + h;
            }
            if let Ok(t) = word.Text() {
                let ts = t.to_string();
                if !ts.is_empty() {
                    parts.push(ts);
                }
            }
        }
        if parts.is_empty() || !min_x.is_finite() {
            continue;
        }
        let line_text = parts.join(" ");
        let line_bbox = (
            min_x as i32,
            min_y as i32,
            (max_x - min_x).max(0.0) as u32,
            (max_y - min_y).max(0.0) as u32,
        );
        if line_bbox.2 > 0 && line_bbox.3 > 0 {
            out.push(OcrResult {
                text: line_text.clone(),
                bbox: line_bbox,
                confidence: 1.0,
            });
        }

        // Also emit individual words so single-word searches get a tight bbox.
        for word in &words {
            let r = match word.BoundingRect() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let wt = match word.Text() {
                Ok(t) => t.to_string(),
                Err(_) => continue,
            };
            let wt = wt.trim().to_string();
            if wt.is_empty() || wt == line_text {
                continue;
            }
            let w = r.Width.max(0.0) as u32;
            let h = r.Height.max(0.0) as u32;
            if w == 0 || h == 0 {
                continue;
            }
            out.push(OcrResult {
                text: wt,
                bbox: (r.X as i32, r.Y as i32, w, h),
                confidence: 0.95,
            });
        }
    }

    Ok(out)
}

// ---------- `find_text` matcher ----------
//
// Three strategies in priority order:
// 1. Exact match (punctuation-stripped, case-insensitive).
// 2. Word-boundary substring (G2): target or OCR token must sit at \b..\b
//    within the other string. Any length allowed — short labels ("OK", "Save")
//    now work; bare prefix matches ("Insert" in "InsertedText") are rejected.
// 3. Fuzzy SequenceMatcher fallback (> 0.85 ratio).
//
// Supporting: 4%-height button cap, MAX_LABEL_LEN=60, nearby_text anchor,
// AI-bbox proximity filter (±300%), role-aware size preference.

const MAX_LABEL_LEN: usize = 60;

fn strip_punct(s: &str) -> String {
    let start = s
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start);
    s[start..end].trim().to_string()
}

/// Roles whose real elements are visually larger than surrounding body text.
fn prefer_largest(role: Option<&str>) -> bool {
    matches!(
        role.map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("button") | Some("tab") | Some("menuitem") | Some("checkbox") | Some("radio")
    )
}

/// Roles whose real elements are smaller-font than headings that may share the word.
fn prefer_smallest(role: Option<&str>) -> bool {
    matches!(
        role.map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("link")
    )
}

#[derive(Debug, Clone, Default)]
pub struct FindOptions<'a> {
    pub role: Option<&'a str>,
    pub nearby_text: Option<&'a str>,
    pub screen_width: u32,
    pub screen_height: u32,
    /// AI-predicted target bbox in **OCR-image-pixel space**: `(x, y, w, h)`.
    /// Candidates whose centre falls inside this bbox expanded ±300% on each
    /// side are kept. If none survive the filter, the matcher falls back to
    /// the full pool (see `nb-*` retry in `find_text`).
    pub ai_bbox: Option<(i32, i32, u32, u32)>,
    /// "Wrong spot" memory, in OCR-image-pixel space: every bbox the locator
    /// pointed at that the user rejected this step (accumulates across retries —
    /// B5). Candidates whose centre falls inside any of them are excluded so the
    /// retry surfaces the next-best match instead of repeating a rejected pick.
    pub avoid_bboxes: Vec<(i32, i32, u32, u32)>,
    pub min_confidence: f32,
}

/// Output of `find_text` — the matched OcrResult plus debug info.
#[derive(Debug, Clone, Default)]
pub struct FindOutcome<'a> {
    pub winner: Option<&'a OcrResult>,
    pub strategy_used: Option<String>,
    pub candidates: Vec<OcrCandidate>,
}

/// Find the best match for `target_text` in `results`. Returns the winning
/// OcrResult plus a list of candidates considered for debug tracing.
pub fn find_text<'a>(
    target_text: &str,
    results: &'a [OcrResult],
    opts: &FindOptions<'_>,
) -> FindOutcome<'a> {
    let mut outcome = FindOutcome::default();
    if target_text.is_empty() || results.is_empty() {
        return outcome;
    }
    // Strip a trailing ellipsis the model copied from a clipped label, then match
    // on the de-truncated form so the word-boundary substring strategy matches
    // the full text (e.g. "Sum of Output USD per…" → "Sum of Output USD per 1M").
    let (target_core, _truncated) = super::strip_trailing_ellipsis(target_text);
    let target_text = target_core.as_str();
    let target_lower = target_text.trim().to_ascii_lowercase();
    // Punct-stripped + digit-only views of the target, for numeric matching:
    // slider/spinner values arrive as "+0.17" / "−14" and OCR reads them with
    // separated signs ("+ 0.17") or as bare fragments ("17").
    let target_stripped = strip_punct(&target_lower);
    let target_digits: String = target_lower.chars().filter(|c| c.is_ascii_digit()).collect();

    let min_conf = if opts.min_confidence <= 0.0 {
        0.5
    } else {
        opts.min_confidence
    };

    // Filter by confidence + label length.
    let mut candidates: Vec<&OcrResult> = results
        .iter()
        .filter(|r| r.confidence >= min_conf && r.text.trim().chars().count() <= MAX_LABEL_LEN)
        .collect();

    // "Wrong spot" memory: hard-exclude candidates at every previously-rejected
    // location so the retry can pick the next-best match (the user already
    // told us those exact spots are wrong).
    if !opts.avoid_bboxes.is_empty() {
        candidates.retain(|r| {
            let cx = r.bbox.0 + r.bbox.2 as i32 / 2;
            let cy = r.bbox.1 + r.bbox.3 as i32 / 2;
            !opts.avoid_bboxes.iter().any(|av| point_in_bbox(cx, cy, av))
        });
    }

    // AI-bbox proximity filter — keep candidates whose centre falls inside the
    // AI bbox expanded ±300% on each side (final keep-rect is 7× the AI bbox
    // width and height, centred on the AI bbox). Generous enough to forgive
    // significant AI imprecision while still excluding the unrelated half of
    // the screen. Fall back to full pool if nothing survives.
    if let Some((ax, ay, aw, ah)) = opts.ai_bbox {
        if aw > 0 && ah > 0 && opts.screen_width > 0 && opts.screen_height > 0 {
            let pad_x = aw as f32 * 3.0;
            let pad_y = ah as f32 * 3.0;
            let x0 = 0f32.max(ax as f32 - pad_x);
            let y0 = 0f32.max(ay as f32 - pad_y);
            let x1 = (opts.screen_width as f32).min((ax + aw as i32) as f32 + pad_x);
            let y1 = (opts.screen_height as f32).min((ay + ah as i32) as f32 + pad_y);
            let filtered: Vec<&OcrResult> = candidates
                .iter()
                .copied()
                .filter(|r| {
                    let cx = r.bbox.0 as f32 + r.bbox.2 as f32 / 2.0;
                    let cy = r.bbox.1 as f32 + r.bbox.3 as f32 / 2.0;
                    cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1
                })
                .collect();
            if !filtered.is_empty() {
                candidates = filtered;
            }
        }
    }

    // Resolve nearby_text anchor (centre of the best-matching OCR result).
    let anchor = opts.nearby_text.and_then(|nt| {
        let nt_lower = nt.trim().to_ascii_lowercase();
        // A nearby_text identical to the target is a self-anchor: it resolves to
        // one of the target candidates themselves and then "confirms" whichever
        // one it landed on — actively steering the pick to the wrong duplicate
        // (observed: target "Auto", nearby "Auto" picked the wrong Auto while the
        // AI bbox pointed at the right one). Ignore it and let the AI-bbox
        // proximity sort drive instead.
        if nt_lower == target_lower {
            return None;
        }
        // 4.a: accept a slightly-misread nearby word as the anchor (was 0.5).
        let mut best_ratio = 0.4f32;
        let mut best_pt: Option<(f32, f32)> = None;
        for r in results {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            if rc.is_empty() {
                continue;
            }
            let ratio = if nt_lower.contains(&rc) || rc.contains(&nt_lower) {
                1.0
            } else {
                sequence_ratio(&nt_lower, &rc)
            };
            if ratio > best_ratio {
                best_ratio = ratio;
                let (x, y, w, h) = r.bbox;
                best_pt = Some((x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0));
            }
        }
        best_pt
    });

    // 4% screen-height button cap — reject heading-sized matches when the
    // target is a button-like role.
    let button_height_cap = (opts.screen_height as f32 * 0.04).max(40.0) as u32;
    let want_largest = prefer_largest(opts.role);
    let want_smallest = prefer_smallest(opts.role);

    // AI-bbox centre (OCR-image-pixel space). When set, used as the dominant
    // tie-breaker among matches — same idea as A11y's proximity sort.
    let ai_center: Option<(f32, f32)> = opts
        .ai_bbox
        .map(|(x, y, w, h)| (x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0));

    let pick_best = |pool: &[&'a OcrResult]| -> Option<&'a OcrResult> {
        if pool.is_empty() {
            return None;
        }
        // 1. nearby_text anchor — strongest signal, user-provided.
        if let Some((ax, ay)) = anchor {
            return pool.iter().copied().min_by(|a, b| {
                let da = proximity_sq(a, ax, ay);
                let db = proximity_sq(b, ax, ay);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        // 2. AI-bbox proximity — sort by distance to the AI's predicted centre.
        //    For button-like roles, apply the 4%-height plausibility filter
        //    first so a heading-sized match next to the AI bbox can't beat the
        //    real button that's slightly further away.
        if let Some((bx, by)) = ai_center {
            let plausible: Vec<&OcrResult> = if want_largest {
                let p: Vec<&OcrResult> = pool
                    .iter()
                    .copied()
                    .filter(|r| r.bbox.3 <= button_height_cap)
                    .collect();
                if !p.is_empty() {
                    p
                } else {
                    pool.to_vec()
                }
            } else {
                pool.to_vec()
            };
            return plausible.into_iter().min_by(|a, b| {
                let da = proximity_sq(a, bx, by);
                let db = proximity_sq(b, bx, by);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        // 3. Fallback — role-aware size preference (legacy behaviour when the
        //    AI did not return a target_bbox).
        if pool.len() > 1 {
            if want_largest {
                let plausible: Vec<&OcrResult> = pool
                    .iter()
                    .copied()
                    .filter(|r| r.bbox.3 <= button_height_cap)
                    .collect();
                let chosen = if !plausible.is_empty() {
                    plausible
                } else {
                    pool.to_vec()
                };
                return chosen
                    .into_iter()
                    .max_by_key(|r| (r.bbox.2 as u64) * (r.bbox.3 as u64));
            }
            if want_smallest {
                return pool
                    .iter()
                    .copied()
                    .min_by_key(|r| (r.bbox.2 as u64) * (r.bbox.3 as u64));
            }
        }
        Some(pool[0])
    };

    let make_candidate = |r: &OcrResult,
                          strategy: &str,
                          score: Option<f32>,
                          selected: bool,
                          reject: Option<&str>| OcrCandidate {
        text: r.text.clone(),
        bbox: r.bbox,
        confidence: r.confidence,
        strategy: strategy.to_string(),
        score,
        selected,
        reject_reason: reject.map(|s| s.to_string()),
    };

    // Strategy 1: exact match (case-insensitive, punctuation-stripped).
    let exact: Vec<&OcrResult> = candidates
        .iter()
        .copied()
        .filter(|r| {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            // Compare punct-stripped on BOTH sides: strip_punct removes the
            // leading "+" from the OCR text ("+ 0.17" → "0.17") but the raw
            // target keeps it ("+0.17"), so symmetric stripping is required
            // for exact to ever match signed numbers.
            rc == target_lower || (!target_stripped.is_empty() && rc == target_stripped)
        })
        .collect();
    if let Some(winner) = pick_best(&exact) {
        for r in &exact {
            outcome.candidates.push(make_candidate(
                r,
                "exact",
                None,
                std::ptr::eq(*r, winner),
                if std::ptr::eq(*r, winner) {
                    None
                } else {
                    Some("not preferred size")
                },
            ));
        }
        outcome.strategy_used = Some("exact".to_string());
        outcome.winner = Some(winner);
        return outcome;
    }

    // Strategy 2: word-boundary substring match (G2).
    //
    // Direction A — OCR token is a whole word within the target text:
    //   `\b<rc>\b` must match inside target_lower.
    //   Minimum 2 chars to suppress single-letter OCR noise.
    //   Word-count guard: OCR text must cover more than half of target's word
    //   count, so a single-word OCR token (e.g. "GPU") cannot match a multi-
    //   word target (e.g. "GPU 0") when many unrelated "GPU" elements exist on
    //   screen. Single-word targets are unaffected (1*2 > 1 passes).
    //
    // Direction B — target is a whole word within the OCR line:
    //   Pre-compiled `\b<target>\b` must match inside the OCR text.
    //   Example: target "Message" found inside OCR "Message (Ctrl+Enter to commit...)".
    //   Also correctly rejects: "Insert" in "InsertedText" (no boundary after "t").
    //   Note: "Insert" still matches "Insert Space" (space IS a boundary) — the
    //   exact-match strategy wins first when both words appear in the OCR pool.
    let target_word_count = target_lower.split_whitespace().count();
    // CJK has no space-separated words, and between two CJK chars there is NO `\b`
    // (they're all word characters), so the word-boundary regexes below can never
    // fire on a CJK target inside a longer CJK line ("保存" in "点击保存按钮").
    // Plain containment is the CJK analogue of the whole-word match — the glued-text
    // false-positive class this tier guards against ("Insert" ⊄ "InsertedText") has
    // no CJK equivalent, since unspaced adjacency IS how CJK words compose. Gated on
    // actual CJK content so ASCII behaviour is unchanged (C4 parity pass, 2026-07-13).
    let target_is_cjk = super::contains_cjk(&target_lower);
    let target_wb_re =
        regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&target_lower))).ok();
    // Second word-boundary regex from the punct-stripped target: `\b\+0\.17\b`
    // can never match (no word boundary before "+"), but `\b0\.17\b` finds the
    // value inside "Exposure + 0.17".
    let target_wb_re_stripped = (target_stripped != target_lower
        && target_stripped.chars().count() >= 2)
        .then(|| regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&target_stripped))).ok())
        .flatten();
    let substr: Vec<&OcrResult> = candidates
        .iter()
        .copied()
        .filter(|r| {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            if rc.is_empty() {
                return false;
            }
            // Numeric guard: a bare number must carry the target's FULL digit
            // sequence. "." is a word boundary, so without this "17" whole-word
            // matches inside "+0.17" and the pointer lands on the wrong slider
            // value (Lightroom: target "+0.17" matched the Vibrance "+ 17").
            let rc_is_bare_number = !rc.chars().any(|c| c.is_alphabetic());
            if rc_is_bare_number {
                let rc_digits: String = rc.chars().filter(|c| c.is_ascii_digit()).collect();
                if rc_digits.is_empty() || rc_digits != target_digits {
                    return false;
                }
            }
            // CJK: word boundaries don't exist between CJK chars — use plain
            // containment in both directions (see target_is_cjk above). The ≥2-char
            // guard suppresses single-character noise, same as the ASCII path.
            if target_is_cjk || super::contains_cjk(&rc) {
                let target_in_rc =
                    target_lower.chars().count() >= 2 && rc.contains(&target_lower);
                let rc_in_target = rc.chars().count() >= 2 && target_lower.contains(&rc);
                return target_in_rc || rc_in_target;
            }
            // Direction A: OCR token as whole word inside target.
            let target_contains_rc = rc.chars().count() >= 2 && {
                let rc_word_count = rc.split_whitespace().count();
                // Word-count guard: OCR token must cover more than half the target's words.
                rc_word_count * 2 > target_word_count
                    && regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&rc)))
                        .map(|re| re.is_match(&target_lower))
                        .unwrap_or(false)
            };
            // Direction B: target as whole word inside OCR line (raw or
            // punct-stripped form).
            let rc_contains_target = target_wb_re
                .as_ref()
                .map(|re| re.is_match(&rc))
                .unwrap_or(false)
                || target_wb_re_stripped
                    .as_ref()
                    .map(|re| re.is_match(&rc))
                    .unwrap_or(false);
            target_contains_rc || rc_contains_target
        })
        .collect();
    if let Some(winner) = pick_best(&substr) {
        for r in &substr {
            outcome.candidates.push(make_candidate(
                r,
                "substring",
                None,
                std::ptr::eq(*r, winner),
                if std::ptr::eq(*r, winner) {
                    None
                } else {
                    Some("not preferred size")
                },
            ));
        }
        outcome.strategy_used = Some("substring".to_string());
        outcome.winner = Some(winner);
        return outcome;
    }

    // Strategy 3: fuzzy cascade (D2) — three tiers with relaxed thresholds.
    //
    // Scores are computed once from the already-held OCR results (no recapture).
    // Each tier re-filters the scored pool; the first tier that yields a winner
    // stops the cascade.
    //
    // Tier 1 (fuzzy-t1): ≥ 0.85 — same strict bar as before.
    // Tier 2 (fuzzy-t2): ≥ 0.75 — catches OCR misreads ("Commit ✓", glyph noise).
    // Tier 3 (fuzzy-t3): ≥ 0.70 — last resort; role-size preference is dropped,
    //                    highest scorer wins directly. Floor raised from 0.65 to
    //                    prevent short-word false positives (e.g. "change" scoring
    //                    0.67 against target "manage" via shared LCS "ange").
    const FUZZY_TIERS: &[(f32, &str)] =
        &[(0.85, "fuzzy-t1"), (0.75, "fuzzy-t2"), (0.70, "fuzzy-t3")];

    // Score every candidate once, sorted best-first.
    // Length guard: OCR text shorter than 2/3 of the target gets a zero score
    // so it cannot sneak through any tier. "GPU" (3 chars) must not score 0.75
    // against "GPU 0" (5 chars) simply because it is a common substring.
    let target_len = target_lower.len();
    let mut scored: Vec<(&OcrResult, f32)> = candidates
        .iter()
        .map(|r| {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            let ratio = if rc.is_empty() || rc.len() * 3 < target_len * 2 {
                0.0
            } else {
                sequence_ratio(&target_lower, &rc)
            };
            (*r, ratio)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut winner: Option<&OcrResult> = None;
    let mut winning_tier: &str = "";

    'tiers: for &(threshold, label) in FUZZY_TIERS {
        let pool: Vec<&OcrResult> = scored
            .iter()
            .filter(|(_, s)| *s >= threshold)
            .map(|(r, _)| *r)
            .collect();
        if pool.is_empty() {
            continue;
        }
        // Tier 3 ignores role-size preference. When AI bbox is set, pick the
        // closest-to-AI-bbox among the top-scored to break ties; otherwise
        // take the highest scorer directly.
        let chosen = if label == "fuzzy-t3" {
            if let Some((bx, by)) = ai_center {
                pool.into_iter().min_by(|a, b| {
                    let da = proximity_sq(a, bx, by);
                    let db = proximity_sq(b, bx, by);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
            } else {
                pool.into_iter().next()
            }
        } else {
            pick_best(&pool)
        };
        if let Some(w) = chosen {
            winner = Some(w);
            winning_tier = label;
            break 'tiers;
        }
    }

    // Record top-5 for the debug drawer with the resolved tier label.
    let tier_label = if winning_tier.is_empty() {
        "fuzzy"
    } else {
        winning_tier
    };
    for (r, score) in scored.iter().take(5) {
        let selected = winner.map(|w| std::ptr::eq(*r, w)).unwrap_or(false);
        let reject = if selected {
            None
        } else if *score < 0.70 {
            Some("score below 0.70")
        } else {
            Some("not chosen")
        };
        outcome.candidates.push(make_candidate(
            r,
            tier_label,
            Some(*score),
            selected,
            reject,
        ));
    }
    if winner.is_some() {
        outcome.strategy_used = Some(winning_tier.to_string());
    }
    outcome.winner = winner;

    // AI-bbox fallback: if an AI bbox filter was active and no strategy found a
    // winner, the AI's predicted location may be off (rare but happens — e.g.
    // model confuses sibling controls). Retry on the full unfiltered pool so
    // the correct element is not silently excluded. The "nb-" prefix (no-bbox)
    // shows up in the debug drawer.
    if outcome.winner.is_none() && opts.ai_bbox.is_some() {
        let no_bbox = FindOptions {
            ai_bbox: None,
            avoid_bboxes: opts.avoid_bboxes.clone(),
            ..*opts
        };
        let mut fallback = find_text(target_text, results, &no_bbox);
        if let Some(ref s) = fallback.strategy_used.clone() {
            fallback.strategy_used = Some(format!("nb-{s}"));
        }
        for c in &mut fallback.candidates {
            c.strategy = format!("nb-{}", c.strategy);
        }
        return fallback;
    }

    outcome
}

fn proximity_sq(r: &OcrResult, ax: f32, ay: f32) -> f32 {
    let cx = r.bbox.0 as f32 + r.bbox.2 as f32 / 2.0;
    let cy = r.bbox.1 as f32 + r.bbox.3 as f32 / 2.0;
    let dx = cx - ax;
    let dy = cy - ay;
    dx * dx + dy * dy
}

/// SequenceMatcher-style ratio: 2 * matches / (len(a) + len(b)). Simplified
/// longest-common-subsequence-based scoring — close enough to Python's for
/// single-line label matching.
fn sequence_ratio(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let la = a.chars().count();
    let lb = b.chars().count();
    if la == 0 || lb == 0 {
        return 0.0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev = vec![0u32; lb + 1];
    let mut curr = vec![0u32; lb + 1];
    for i in 1..=la {
        for j in 1..=lb {
            if a_chars[i - 1] == b_chars[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = prev[j].max(curr[j - 1]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|v| *v = 0);
    }
    let lcs = prev[lb] as f32;
    2.0 * lcs / (la + lb) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    // E2 spike (measurement only — no production change): OCR a capture at several scales,
    // full-image or a cropped REGION, to see whether upscaling rescues compact text the native
    // pass misses, and at what cost. IN=capture.png; optional REGION=x,y,w,h; UPS="1,2,3".
    //   $env:IN="vscode_cap.png"; $env:UPS="1,2,3"; (optional $env:REGION="x,y,w,h")
    //   cargo test --lib ocr_spike -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_spike() {
        let img = image::open(std::env::var("IN").expect("set IN")).expect("open").to_rgba8();
        let sub = if let Ok(r) = std::env::var("REGION") {
            let p: Vec<u32> = r.split(',').map(|s| s.trim().parse().unwrap()).collect();
            image::imageops::crop_imm(&img, p[0], p[1], p[2], p[3]).to_image()
        } else {
            img
        };
        let ups: Vec<u32> = std::env::var("UPS")
            .unwrap_or_else(|_| "1,2,3".into())
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        eprintln!("base region {}x{}", sub.width(), sub.height());
        for up in ups {
            let scaled = if up == 1 {
                sub.clone()
            } else {
                image::imageops::resize(&sub, sub.width() * up, sub.height() * up, image::imageops::FilterType::Lanczos3)
            };
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(scaled.clone())
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let t = std::time::Instant::now();
            let res = run_ocr(png.get_ref()).expect("ocr");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let words = res.iter().filter(|r| r.confidence < 1.0).count();
            let lines = res.iter().filter(|r| r.confidence >= 1.0).count();
            let text: String = res
                .iter()
                .filter(|r| r.confidence >= 1.0)
                .map(|r| r.text.clone())
                .collect::<Vec<_>>()
                .join(" ");
            let sample: String = text.chars().take(220).collect();
            eprintln!(
                "up={up} {}x{}: {lines} lines / {words} words in {ms:.0} ms",
                scaled.width(),
                scaled.height()
            );
            eprintln!("   text: {sample}");
        }
    }

    fn word(text: &str, bbox: (i32, i32, u32, u32)) -> OcrResult {
        OcrResult {
            text: text.to_string(),
            bbox,
            confidence: 0.9,
        }
    }

    fn line(text: &str, bbox: (i32, i32, u32, u32)) -> OcrResult {
        OcrResult {
            text: text.to_string(),
            bbox,
            confidence: 1.0, // line-level results carry confidence 1.0
        }
    }

    fn opts(screen: (u32, u32)) -> FindOptions<'static> {
        FindOptions {
            screen_width: screen.0,
            screen_height: screen.1,
            ..Default::default()
        }
    }

    // --- sequence_ratio -------------------------------------------------

    #[test]
    fn sequence_ratio_basics() {
        assert_eq!(sequence_ratio("save", "save"), 1.0);
        assert_eq!(sequence_ratio("", ""), 1.0);
        assert_eq!(sequence_ratio("save", ""), 0.0);
        // The historic "Status" ↔ "Startup" false match must stay below the
        // 0.85 tier-1 threshold (LCS "statu" = 5 → 10/13 ≈ 0.77).
        assert!(sequence_ratio("status", "startup") < 0.85);
    }

    // --- find_text cascade ----------------------------------------------

    #[test]
    fn exact_match_wins_over_substring() {
        let results = vec![
            word("Save", (10, 10, 40, 20)),
            word("Save As Document", (100, 100, 200, 20)),
        ];
        let out = find_text("Save", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("exact"));
        assert_eq!(out.winner.unwrap().text, "Save");
    }

    #[test]
    fn word_boundary_rejects_glued_text() {
        // "Insert" must not match "InsertedText" (no word boundary), and the
        // fuzzy score (≈0.67) stays below the 0.70 floor.
        let results = vec![word("InsertedText", (10, 10, 80, 20))];
        let out = find_text("Insert", &results, &opts((1000, 600)));
        assert!(out.winner.is_none());
    }

    #[test]
    fn word_boundary_matches_target_inside_line() {
        let results = vec![word("Message (Ctrl+Enter)", (10, 10, 120, 20))];
        let out = find_text("Message", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("substring"));
        assert!(out.winner.is_some());
    }

    #[test]
    fn cjk_target_matches_inside_unspaced_line() {
        // CJK parity (2026-07-13): "保存" (Save) inside "点击保存按钮" (click the Save
        // button). `\b` never fires between CJK word characters, so before the CJK
        // containment path this could only miss.
        let results = vec![word("点击保存按钮", (10, 10, 90, 20))];
        let out = find_text("保存", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("substring"));
        assert!(out.winner.is_some());
    }

    #[test]
    fn cjk_ocr_token_matches_inside_target() {
        // Direction A: OCR read a shortened fragment of the target label.
        let results = vec![word("设置", (10, 10, 40, 20))];
        let out = find_text("打开设置", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("substring"));
        assert!(out.winner.is_some());
    }

    #[test]
    fn cjk_unrelated_labels_do_not_substring_match() {
        // No shared substring — must fall past the substring tier (and fuzzy won't
        // rescue two unrelated 2-char labels).
        let results = vec![word("印刷", (10, 10, 40, 20))];
        let out = find_text("保存", &results, &opts((1000, 600)));
        assert!(out.winner.is_none());
    }

    #[test]
    fn word_count_guard_blocks_partial_token() {
        // A lone "GPU" token must not match the multi-word target "GPU 0":
        // substring direction A is blocked by the word-count guard and the
        // fuzzy length guard zeroes the score.
        let results = vec![word("GPU", (10, 10, 30, 15))];
        let out = find_text("GPU 0", &results, &opts((1000, 600)));
        assert!(out.winner.is_none());
    }

    #[test]
    fn fuzzy_tier1_catches_ocr_misread() {
        // "Perfonmance" misread: LCS 10 of 11 → ratio ≈ 0.91 ≥ 0.85 (tier 1).
        let results = vec![word("Perfonmance", (10, 10, 90, 18))];
        let out = find_text("Performance", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("fuzzy-t1"));
        assert!(out.winner.is_some());
    }

    #[test]
    fn truncated_target_matches_full_line() {
        // Model copied a clipped "…" label; the de-truncated core must
        // whole-word match the full on-screen text.
        let results = vec![word("Sum of Output USD per 1M tokens", (5, 5, 300, 20))];
        let out = find_text("Sum of Output USD per…", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("substring"));
        assert!(out.winner.is_some());
    }

    #[test]
    fn nearby_text_anchor_disambiguates_duplicates() {
        // Two identical "Reply" labels; the anchor "Inbox" sits next to the
        // second — the anchored pick must choose it.
        let results = vec![
            word("Reply", (10, 10, 50, 20)),
            word("Reply", (500, 300, 50, 20)),
            word("Inbox", (470, 300, 40, 20)),
        ];
        let o = FindOptions {
            nearby_text: Some("Inbox"),
            ..opts((1000, 600))
        };
        let out = find_text("Reply", &results, &o);
        assert_eq!(out.winner.unwrap().bbox.0, 500);
    }

    #[test]
    fn ai_bbox_miss_falls_back_to_full_pool() {
        // The AI bbox filter keeps only "Cancel" (near the predicted spot);
        // nothing there matches "OK", so the nb- retry on the full pool must
        // find the real "OK" elsewhere on screen.
        let results = vec![
            word("Cancel", (905, 505, 40, 10)),
            word("OK", (10, 10, 20, 10)),
        ];
        let o = FindOptions {
            ai_bbox: Some((900, 500, 50, 20)),
            ..opts((1000, 600))
        };
        let out = find_text("OK", &results, &o);
        assert_eq!(out.strategy_used.as_deref(), Some("nb-exact"));
        assert_eq!(out.winner.unwrap().text, "OK");
    }

    // Live diagnostic: does upscaling actually help Windows.Media.Ocr, and by how
    // much? Sweeps OCR over 1×/1.5×/2×/3× on the full image and (optionally) a
    // region crop, reporting word count, target-found, and wall-clock time per
    // scale. This is the experiment behind the upscale-retry keep/replace/revert
    // decision — not a single anecdote.
    //
    // Run:
    //   $env:NAVISUAL_TEST_IMG="<png>"; $env:NAVISUAL_TEST_TARGET="Select and Mask"
    //   # optional region crop in image px: x,y,w,h
    //   $env:NAVISUAL_TEST_CROP="0,0,1920,70"
    //   cargo test --lib ocr_scale_sweep -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_scale_sweep() {
        use std::time::Instant;
        let path = std::env::var("NAVISUAL_TEST_IMG").expect("set NAVISUAL_TEST_IMG");
        let target = std::env::var("NAVISUAL_TEST_TARGET").unwrap_or_default();
        let bytes = std::fs::read(&path).expect("read image");
        let full = image::load_from_memory(&bytes).expect("decode").to_rgba8();

        let crop = std::env::var("NAVISUAL_TEST_CROP").ok().map(|s| {
            let n: Vec<u32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (n[0], n[1], n[2], n[3])
        });

        let region = if let Some((x, y, w, h)) = crop {
            image::imageops::crop_imm(&full, x, y, w, h).to_image()
        } else {
            full.clone()
        };

        let tlow = target.to_ascii_lowercase();
        let run_at = |scale: f32| -> (usize, bool, String, u128, u32, u32) {
            let (w, h) = (region.width(), region.height());
            let (nw, nh) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
            let scaled = if (scale - 1.0).abs() < 0.01 {
                region.clone()
            } else {
                image::imageops::resize(&region, nw, nh, image::imageops::FilterType::Lanczos3)
            };
            let mut buf = Vec::new();
            image::DynamicImage::ImageRgba8(scaled)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .expect("encode");
            let t0 = Instant::now();
            let rs = run_ocr(&buf).expect("ocr");
            let ms = t0.elapsed().as_millis();
            let words = rs.iter().filter(|r| r.confidence < 1.0).count();
            // Target found = any line-level result contains the target text.
            let hit_line = rs
                .iter()
                .filter(|r| r.confidence >= 1.0)
                .find(|r| !tlow.is_empty() && r.text.to_ascii_lowercase().contains(&tlow))
                .map(|r| r.text.clone())
                .unwrap_or_default();
            (words, !hit_line.is_empty(), hit_line, ms, nw, nh)
        };

        eprintln!(
            "\n=== {} ===  target={:?}  region={}x{}{}",
            std::path::Path::new(&path).file_name().unwrap().to_string_lossy(),
            target,
            region.width(),
            region.height(),
            crop.map(|_| " (cropped)").unwrap_or(""),
        );
        eprintln!("scale  dims          MP    ms     words  target?  line");
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            let (words, found, line, ms, nw, nh) = run_at(scale);
            let mp = (nw as f64 * nh as f64 / 1e6 * 10.0).round() / 10.0;
            eprintln!(
                "{:>4.1}x  {:>5}x{:<5}  {:>4}  {:>5}  {:>5}  {:>6}   {}",
                scale,
                nw,
                nh,
                mp,
                ms,
                words,
                if found { "YES" } else { "no" },
                if line.len() > 48 { &line[..48] } else { &line },
            );
        }
    }

    // --- corroboration helpers -------------------------------------------

    #[test]
    fn isolation_low_inside_long_content_line() {
        // "status" embedded in a long terminal line → low isolation ratio.
        let containing = line("git status shows your working tree", (0, 0, 400, 20));
        let target_word = word("status", (50, 0, 60, 20));
        let results = vec![containing, target_word.clone()];
        let (ratio, line_len) = isolation_for(&target_word.bbox, "status", &results);
        assert!(ratio < 0.3, "ratio {ratio} should be low for content text");
        assert!(line_len > 30);
    }

    #[test]
    fn isolation_full_when_no_containing_line() {
        let target_word = word("Save", (50, 0, 40, 20));
        let results = vec![target_word.clone()];
        let (ratio, _) = isolation_for(&target_word.bbox, "Save", &results);
        assert_eq!(ratio, 1.0);
    }

    #[test]
    fn anchor_near_respects_distance_threshold() {
        let winner = (100, 100, 50, 20);
        let near = vec![word("Run", (130, 100, 30, 20))];
        let far = vec![word("Run", (900, 560, 30, 20))];
        assert!(anchor_near(&winner, "Run", &near, 1000, 600));
        assert!(!anchor_near(&winner, "Run", &far, 1000, 600));
        // Single-character anchors are ignored entirely.
        assert!(!anchor_near(&winner, "R", &near, 1000, 600));
    }

    #[test]
    fn anchor_near_excludes_winner_and_containing_line() {
        // The winner's own text and the line containing it must not corroborate
        // the winner (self-anchoring — the Lightroom wrong-Auto case).
        let winner_bbox = (100, 100, 50, 20);
        let results = vec![
            word("Auto+:", winner_bbox),                  // the winner itself
            line("WB: Auto+: Tint", (90, 95, 200, 30)),   // its containing line
        ];
        assert!(!anchor_near(&winner_bbox, "Auto", &results, 1000, 600));
        // A genuinely separate nearby word still counts.
        let mut with_sibling = results.clone();
        with_sibling.push(word("Auto", (170, 100, 40, 20)));
        assert!(anchor_near(&winner_bbox, "Auto", &with_sibling, 1000, 600));
    }

    #[test]
    fn self_anchor_is_ignored_ai_bbox_drives_pick() {
        // nearby_text identical to the target used to resolve the anchor to the
        // FIRST duplicate and steer the pick there. With the self-anchor ignored,
        // the AI-bbox proximity sort picks the right duplicate.
        let results = vec![
            word("Auto", (700, 350, 40, 16)),
            word("Auto", (800, 400, 40, 16)),
        ];
        let o = FindOptions {
            nearby_text: Some("Auto"),
            ai_bbox: Some((790, 390, 40, 20)), // centred on the second Auto
            ..opts((1000, 600))
        };
        let out = find_text("Auto", &results, &o);
        assert_eq!(out.winner.unwrap().bbox.0, 800);
    }

    #[test]
    fn signed_decimal_matches_exact_after_symmetric_strip() {
        // Lightroom slider values: OCR separates the sign ("+ 0.17") while the
        // AI sends "+0.17". Exact must match via punct-stripped comparison on
        // BOTH sides, and must beat the wrong row's "+ 17".
        let results = vec![
            word("+ 17", (300, 455, 40, 14)), // Vibrance — the historic wrong pick
            word("17", (310, 455, 20, 14)),
            word("+ 0.17", (300, 222, 46, 14)), // Exposure — the right one
            word("Exposure", (100, 222, 60, 14)),
        ];
        let out = find_text("+0.17", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("exact"));
        assert_eq!(out.winner.unwrap().bbox.1, 222);
    }

    #[test]
    fn bare_number_fragment_cannot_match_decimal_target() {
        // "." is a word boundary, so "17" used to whole-word match inside
        // "+0.17" and the pointer landed on the wrong slider value.
        let results = vec![
            word("+ 17", (300, 455, 40, 14)),
            word("17", (310, 455, 20, 14)),
        ];
        let out = find_text("+0.17", &results, &opts((1000, 600)));
        assert!(out.winner.is_none());
    }

    #[test]
    fn stripped_target_matches_inside_line() {
        // Direction B with the punct-stripped form: `\b\+0\.17\b` can never
        // match (no word boundary before "+"), but `\b0\.17\b` finds the value
        // inside the row line.
        let results = vec![word("Exposure + 0.17", (100, 222, 250, 16))];
        let out = find_text("+0.17", &results, &opts((1000, 600)));
        assert_eq!(out.strategy_used.as_deref(), Some("substring"));
    }

    #[test]
    fn avoid_bbox_excludes_rejected_spot() {
        // "Wrong spot" memory: the previously-pointed bbox is excluded, so the
        // retry surfaces the other duplicate instead of repeating the pick.
        let results = vec![word("OK", (10, 10, 20, 10)), word("OK", (300, 200, 20, 10))];
        let o = FindOptions {
            avoid_bboxes: vec![(0, 0, 60, 40)], // where the first OK was pointed
            ..opts((1000, 600))
        };
        let out = find_text("OK", &results, &o);
        assert_eq!(out.strategy_used.as_deref(), Some("exact"));
        assert_eq!(out.winner.unwrap().bbox.0, 300);
    }

    #[test]
    fn avoid_list_accumulates_across_retries() {
        // B5: two rejected spots exclude BOTH duplicates; the third instance wins.
        let results = vec![
            word("OK", (10, 10, 20, 10)),
            word("OK", (300, 200, 20, 10)),
            word("OK", (600, 400, 20, 10)),
        ];
        let o = FindOptions {
            avoid_bboxes: vec![(0, 0, 60, 40), (290, 190, 60, 40)],
            ..opts((1000, 600))
        };
        let out = find_text("OK", &results, &o);
        assert_eq!(out.winner.unwrap().bbox.0, 600);
    }
}
