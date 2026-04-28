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
// Port of `legacy/src/locator/ocr_engine.py::OCREngine.find_text`, preserving:
// - Exact-match strategy with role-aware size preference.
// - Substring match with MIN_SUBSTR_LEN guard.
// - Fuzzy SequenceMatcher fallback (> 0.7 ratio).
// - Leading/trailing punctuation strip (curly quotes, apostrophes).
// - 4%-screen-height button cap (reject headings), MAX_LABEL_LEN=60.
// - Optional nearby_text anchor + 16×9 zone filter.

const MAX_LABEL_LEN: usize = 60;
const MIN_SUBSTR_LEN: usize = 8;

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

/// Find the best match for `target_text` in `results`. Returns the winning
/// OcrResult or None.
pub fn find_text<'a>(
    target_text: &str,
    results: &'a [OcrResult],
    opts: &FindOptions<'_>,
) -> Option<&'a OcrResult> {
    if target_text.is_empty() || results.is_empty() {
        return None;
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

    // Strategy 1: exact match (case-insensitive, punctuation-stripped).
    let exact: Vec<&OcrResult> = candidates
        .iter()
        .copied()
        .filter(|r| strip_punct(&r.text).to_ascii_lowercase() == target_lower)
        .collect();
    if let Some(r) = pick_best(&exact) {
        return Some(r);
    }

    // Strategy 2: substring match (either direction). Protect against short
    // OCR tokens ("in", "for") matching as substrings of the target.
    let substr: Vec<&OcrResult> = candidates
        .iter()
        .copied()
        .filter(|r| {
            let rc = strip_punct(&r.text).to_ascii_lowercase();
            if rc.is_empty() {
                return false;
            }
            // Target contains OCR text (OCR is a substring of target)
            let target_contains_rc = target_lower.contains(&rc) && rc.chars().count() >= MIN_SUBSTR_LEN;
            // OCR text contains target
            let rc_contains_target = if rc.contains(&target_lower) {
                if target_lower.chars().count() >= 4 {
                    true
                } else {
                    // If target is very short (e.g. "no"), require it to be a distinct word
                    // so it doesn't match "notice".
                    let is_word = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&target_lower)))
                        .map(|re| re.is_match(&rc))
                        .unwrap_or(false);
                    is_word
                }
            } else {
                false
            };
            target_contains_rc || rc_contains_target
        })
        .collect();
    if let Some(r) = pick_best(&substr) {
        return Some(r);
    }

    // Strategy 3: fuzzy SequenceMatcher > 0.85.
    // 0.7 was too loose — "Status" matched "Startup" at 0.77.
    let mut best: Option<&OcrResult> = None;
    let mut best_ratio = 0.85f32;
    for r in &candidates {
        let rc = strip_punct(&r.text).to_ascii_lowercase();
        if rc.is_empty() {
            continue;
        }
        let ratio = sequence_ratio(&target_lower, &rc);
        if ratio > best_ratio {
            best_ratio = ratio;
            best = Some(*r);
        }
    }
    best
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
