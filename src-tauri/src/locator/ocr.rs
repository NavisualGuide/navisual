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

/// Run Windows.Media.Ocr on the given image bytes (JPEG or PNG). Returns
/// line-level results plus word-level results (so single-word targets can
/// still get a tight bbox instead of the whole line's span).
pub fn run_ocr(image_bytes: &[u8]) -> Result<Vec<OcrResult>> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| anyhow!("OCR engine init: {e}"))?;

    // Wrap the bytes in an InMemoryRandomAccessStream via a DataWriter.
    let stream =
        InMemoryRandomAccessStream::new().map_err(|e| anyhow!("create stream: {e}"))?;
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
// 16×9 zone filter, role-aware size preference.

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
    matches!(role.map(|s| s.to_ascii_lowercase()).as_deref(), Some("link"))
}

#[derive(Debug, Clone, Default)]
pub struct FindOptions<'a> {
    pub role: Option<&'a str>,
    pub nearby_text: Option<&'a str>,
    pub screen_width: u32,
    pub screen_height: u32,
    /// 16×9 zone filter cell coordinates (0..16, 0..9), or None.
    pub zone: Option<(u32, u32)>,
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
    let target_lower = target_text.trim().to_ascii_lowercase();

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

    // 16×9 zone filter — keep candidates whose centre falls within ±1 cell
    // of the AI-reported zone. Fall back to full pool if nothing survives.
    if let Some((zx, zy)) = opts.zone {
        if opts.screen_width > 0 && opts.screen_height > 0 {
            let cw = opts.screen_width as f32 / 16.0;
            let ch = opts.screen_height as f32 / 9.0;
            let x0 = 0f32.max((zx as f32 - 1.0) * cw);
            let x1 = (opts.screen_width as f32).min((zx as f32 + 2.0) * cw);
            let y0 = 0f32.max((zy as f32 - 1.0) * ch);
            let y1 = (opts.screen_height as f32).min((zy as f32 + 2.0) * ch);
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
        let mut best_ratio = 0.5f32;
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

    let pick_best = |pool: &[&'a OcrResult]| -> Option<&'a OcrResult> {
        if pool.is_empty() {
            return None;
        }
        if let Some((ax, ay)) = anchor {
            return pool
                .iter()
                .copied()
                .min_by(|a, b| {
                    let da = proximity_sq(a, ax, ay);
                    let db = proximity_sq(b, ax, ay);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
        }
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

    let make_candidate = |r: &OcrResult, strategy: &str, score: Option<f32>, selected: bool, reject: Option<&str>| OcrCandidate {
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
        .filter(|r| strip_punct(&r.text).to_ascii_lowercase() == target_lower)
        .collect();
    if let Some(winner) = pick_best(&exact) {
        for r in &exact {
            outcome.candidates.push(make_candidate(
                r,
                "exact",
                None,
                std::ptr::eq(*r, winner),
                if std::ptr::eq(*r, winner) { None } else { Some("not preferred size") },
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
    let target_wb_re = regex::Regex::new(
        &format!(r"(?i)\b{}\b", regex::escape(&target_lower))
    ).ok();
    let substr: Vec<&OcrResult> = candidates
        .iter()
        .copied()
        .filter(|r| {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            if rc.is_empty() {
                return false;
            }
            // Direction A: OCR token as whole word inside target.
            let target_contains_rc = rc.chars().count() >= 2 && {
                let rc_word_count = rc.split_whitespace().count();
                // Word-count guard: OCR token must cover more than half the target's words.
                rc_word_count * 2 > target_word_count &&
                regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&rc)))
                    .map(|re| re.is_match(&target_lower))
                    .unwrap_or(false)
            };
            // Direction B: target as whole word inside OCR line.
            let rc_contains_target = target_wb_re
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
                if std::ptr::eq(*r, winner) { None } else { Some("not preferred size") },
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
    // Tier 3 (fuzzy-t3): ≥ 0.65 — last resort; role-size preference is dropped,
    //                    highest scorer wins directly.
    const FUZZY_TIERS: &[(f32, &str)] = &[
        (0.85, "fuzzy-t1"),
        (0.75, "fuzzy-t2"),
        (0.65, "fuzzy-t3"),
    ];

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
        // Tier 3 ignores role-size preference — take the highest scorer directly.
        let chosen = if label == "fuzzy-t3" {
            pool.into_iter().next()
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
    let tier_label = if winning_tier.is_empty() { "fuzzy" } else { winning_tier };
    for (r, score) in scored.iter().take(5) {
        let selected = winner.map(|w| std::ptr::eq(*r, w)).unwrap_or(false);
        let reject = if selected {
            None
        } else if *score < 0.65 {
            Some("score below 0.65")
        } else {
            Some("not chosen")
        };
        outcome.candidates.push(make_candidate(r, tier_label, Some(*score), selected, reject));
    }
    if winner.is_some() {
        outcome.strategy_used = Some(winning_tier.to_string());
    }
    outcome.winner = winner;

    // Zone-filter fallback: if a zone was active and no strategy found a winner,
    // the AI's grid_cell estimate may be wrong (e.g. nav-sidebar items are in
    // col 1 but AI reports col 4). Retry on the full unfiltered pool so the
    // correct element is not silently excluded.
    if outcome.winner.is_none() && opts.zone.is_some() {
        let no_zone = FindOptions { zone: None, ..*opts };
        let mut fallback = find_text(target_text, results, &no_zone);
        // Prefix strategy with "nz-" so the debug drawer shows the retry.
        if let Some(ref s) = fallback.strategy_used.clone() {
            fallback.strategy_used = Some(format!("nz-{s}"));
        }
        for c in &mut fallback.candidates {
            c.strategy = format!("nz-{}", c.strategy);
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
