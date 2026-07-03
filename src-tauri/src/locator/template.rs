//! Template matching — v0.6 Workstream B.
//!
//! The locator's Pass 3: when A11y and OCR both miss (icon-only buttons in sparse-A11y apps —
//! Electron toolbars, Blender, Photoshop), match a pack-supplied icon crop against the
//! captured window via normalized cross-correlation (NCC). Pure-Rust `imageproc`, no OpenCV
//! (Design Decision #7) — runs in tens of ms over one ~2-MP screenshot with a handful of
//! templates.
//!
//! UI icons are pixel-stable at a known DPI (no rotation/scale invariance needed), but the
//! live screen's DPI scaling may differ from the DPI the pack's crops were authored at, so we
//! sweep a few scales and keep the best. Matches are accepted only above a high NCC threshold
//! ("no pointer beats wrong pointer").
//!
//! Known limits (documented; pack-author guidance): templates are theme-specific (a dark-mode
//! crop won't match a light-mode UI — ship per-theme crops or rely on the threshold to reject);
//! repeated motifs (identical icons) can false-match; cap the template count per pack for latency.

use anyhow::{Context, Result};
use image::{imageops::FilterType, GrayImage};
use imageproc::template_matching::{find_extremes, match_template_parallel, MatchTemplateMethod};

/// DPI-derived scale sweep applied to the template before matching. Each scale is a full
/// correlation pass, so this is deliberately short and ordered most-likely-first (native
/// scale, then ±25 %, then 1.5×/0.67×) — covers the common pack-author-vs-screen DPI ratios
/// without the cost of a wide sweep. Matching is region-restricted to the AI bbox, so even a
/// few scales stay cheap.
pub const DEFAULT_SCALES: &[f32] = &[1.0, 1.25, 0.8, 1.5, 0.67];

/// Minimum NCC score to accept a match. NCC is in [-1, 1]; UI icons match near 1.0 when the
/// theme/DPI line up, so a high floor rejects the near-misses that would place a wrong pointer.
pub const DEFAULT_MIN_SCORE: f32 = 0.9;

/// Floor for a match at a physically implausible scale (see [`scale_near_prior`]) — near
/// certainty required, because a high-scoring look-alike at the wrong physical size is exactly
/// how a 200 % screen's false positive looked (live: "overlays" accepted a 0.925 gizmos-glyph
/// hit at scale 1.34 on a prior-2.0 monitor, 150 px from the real icon).
pub const OFF_SCALE_MIN_SCORE: f32 = 0.95;

/// Floor for the trusted-bbox rescue of a borderline match at the expected scale (see
/// [`template_match_accept`]). Spurious peaks at the expected scale measured ≤ 0.81; a true
/// icon under version drift measured 0.898 (Blender 5.1 rotate vs the 3.6-authored pack).
pub const RESCUE_MIN_SCORE: f32 = 0.85;

/// A match's scale is physically constrained: the OS renders app chrome at the monitor's DPI
/// scale (DWM stretches DPI-unaware apps to the same size), so a true icon appears at
/// ~`prior` × its authored size. Ratios inside this window are physically plausible; outside
/// it, only an app-internal UI scale (Blender's Resolution Scale) or a pack icon whose crop
/// canvas differs from the on-screen instance can be legitimate — rare, so off-window matches
/// must clear [`OFF_SCALE_MIN_SCORE`] instead of being rejected outright.
pub fn scale_near_prior(scale: f32, prior: f32) -> bool {
    let p = if prior.is_finite() && prior > 0.0 { prior } else { 1.0 };
    let r = scale / p;
    (0.75..=1.34).contains(&r)
}

/// Template-match acceptance: score threshold conditioned on how well the match agrees with
/// the *independent* evidence — physical scale plausibility and the pack's region hint — with
/// a trusted-bbox rescue for borderline true icons.
///
/// | scale ([`scale_near_prior`]) | region (hint) | requirement |
/// |---|---|---|
/// | expected | in / no hint | ≥ [`DEFAULT_MIN_SCORE`], or ≥ [`RESCUE_MIN_SCORE`] under a tight trusted bbox |
/// | expected | out          | ≥ [`OFF_SCALE_MIN_SCORE`] (a moved panel is possible, but demand near-certainty) |
/// | off      | in / no hint | ≥ [`OFF_SCALE_MIN_SCORE`] (odd crop canvas / app-internal UI scale) |
/// | off      | out          | **never** — two independent disagreements is a look-alike, full stop |
///
/// Live cases that pin each row: Blender 5.1 rotate 0.898 @ expected scale, in the `left`
/// region, 4 px from the bbox → rescued (icon drift near-miss). "Show Overlays" 0.90 on a
/// right-panel tab (expected scale, out of `top`) → rejected. Gizmos 0.9836 @ 0.67 on a 100 %
/// screen (off scale, in `top`) → passes the high bar. The overlays glyph's 13-px circle
/// look-alikes on a 1× screen (0.96, off scale, mid-screen) → rejected outright — degenerate
/// small glyphs (a pair of circles) score high against any small round thing, so matching
/// alone can't separate them; the stacked independent evidence can. The rescue also requires
/// the region to agree: a borderline score at a location the pack says is wrong stays
/// rejected even under the bbox (weak models ground look-alikes — that combination is exactly
/// the false-positive signature).
pub fn template_match_accept(
    score: f32,
    scale: f32,
    prior: f32,
    near_trusted_bbox: bool,
    region_ok: bool,
) -> bool {
    match (scale_near_prior(scale, prior), region_ok) {
        (true, true) => {
            score >= DEFAULT_MIN_SCORE || (near_trusted_bbox && score >= RESCUE_MIN_SCORE)
        }
        (true, false) | (false, true) => score >= OFF_SCALE_MIN_SCORE,
        (false, false) => false,
    }
}

/// A located template instance, in **haystack pixel coordinates** (the caller maps these back
/// to virtual-desktop coords the same way the OCR path does).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateMatch {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Best NCC score in [-1, 1] (higher = better).
    pub score: f32,
    /// Template scale factor that produced this match.
    pub scale: f32,
}

/// Per-scale needle preprocessing applied AFTER the resize (e.g. [`to_edges`]). Order is
/// load-bearing for cross-DPI matching: resizing an already-edged template stretches its
/// dilated ~3 px edge bands to ~6 px at a 2× scale, while the live 2×-rendered icon in the
/// (natively-edged) haystack keeps ~3 px bands — the width mismatch caps NCC at ~0.86, under
/// the 0.9 accept threshold (measured, Blender at 200 % DPI). Resizing the *grayscale* icon
/// first and edging at the target scale keeps both sides' edge widths comparable.
pub type NeedlePrep = Option<fn(&GrayImage) -> GrayImage>;

/// Minimum needle dimension for the fine/direct match. Small edge maps are degenerate — a
/// downscaled two-circle or line-box glyph NCC-matches empty viewport grid at ≥0.98 (measured
/// at FHD: an 11 px "gizmos" needle scored 0.9902 and a 16 px "collection" needle 0.9893 on
/// bare grid lines — above every acceptance bar). Scales that would shrink the needle under
/// this floor are skipped; a small icon on a low-DPI screen then falls back to OCR / AI-bbox
/// instead of gambling on a degenerate match ("no pointer beats wrong pointer"). 14 px keeps
/// FHD's 16 px true matches alive (modifiers scored 0.9996 there) while cutting the hopeless
/// 11–13 px range; the faint-structure impostors that DO clear this floor (grid at 16–20 px)
/// are killed by the absolute-contrast gate ([`contrast_plausible`]), not the size floor.
/// (The coarse pass keeps its own 4 px floor — it only localizes, the fine pass re-scores.)
const MIN_NEEDLE_PX: u32 = 14;

/// Slide `template` over `haystack` at each scale in `scales`, returning the single best match
/// whose NCC score ≥ `min_score`, or `None`. Scales that would make the template ≥ the
/// haystack (or smaller than [`MIN_NEEDLE_PX`]) are skipped — `imageproc::match_template`
/// panics if the template isn't strictly smaller, so the upper guard is load-bearing.
/// With `prep` set, `template` must be the RAW grayscale icon; the prep runs after each
/// resize (see [`NeedlePrep`]). `None` matches `template` as given (raw-intensity tests).
pub fn match_icon(
    haystack: &GrayImage,
    template: &GrayImage,
    scales: &[f32],
    min_score: f32,
    prep: NeedlePrep,
) -> Option<TemplateMatch> {
    let (hw, hh) = haystack.dimensions();
    let mut best: Option<TemplateMatch> = None;
    for &s in scales {
        let tw = ((template.width() as f32) * s).round() as u32;
        let th = ((template.height() as f32) * s).round() as u32;
        if tw < MIN_NEEDLE_PX || th < MIN_NEEDLE_PX || tw >= hw || th >= hh {
            continue;
        }
        let scaled;
        let resized: &GrayImage = if (s - 1.0).abs() < f32::EPSILON {
            template
        } else {
            scaled = image::imageops::resize(template, tw, th, FilterType::Lanczos3);
            &scaled
        };
        let prepped;
        let needle: &GrayImage = match prep {
            Some(p) => {
                prepped = p(resized);
                &prepped
            }
            None => resized,
        };
        let result = match_template_parallel(
            haystack,
            needle,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let ext = find_extremes(&result);
        let score = ext.max_value;
        if best.map(|b| score > b.score).unwrap_or(true) {
            let (mx, my) = ext.max_value_location;
            best = Some(TemplateMatch {
                x: mx as i32,
                y: my as i32,
                width: tw,
                height: th,
                score,
                scale: s,
            });
        }
    }
    best.filter(|b| b.score >= min_score)
}

/// Target coarse icon size (px). The coarse pass downscales so the icon lands near this size —
/// small enough that full-screen NCC is cheap, large enough to still localize. Deriving the
/// factor from the icon size keeps coarse cost roughly constant across screen DPIs (UI icon px
/// scales with DPI, so the factor adapts).
const COARSE_ICON_PX: f32 = 12.0;

/// Max distinct on-screen instances the coarse pass reports (so callers can disambiguate
/// similar/repeated icons with a spatial prior). Bounds the fine-refine count.
const MAX_PEAKS: usize = 5;

/// Scales for the coarse *localization* pass — fewer than `DEFAULT_SCALES` (the full per-locate
/// cost driver), but still spanning the DPI range so a cross-DPI icon peaks in the right area.
/// The fine pass re-sweeps `DEFAULT_SCALES` to nail the exact scale, so coarse only needs to get
/// us to the right neighbourhood.
const COARSE_SCALES: &[f32] = &[0.67, 1.0, 1.5];

/// NCC result surface from `match_template` (one f32 score per candidate position).
type NccMap = image::ImageBuffer<image::Luma<f32>, Vec<f32>>;

// (`best_scale_map` — the single-best-scale coarse map — was removed when the coarse pass
// switched to pooling top-K peaks from EVERY scale's map; see `match_icon_pyramid`.)

/// Up to `k` well-separated peaks (x, y, score) in an NCC map via greedy non-max suppression
/// (suppress a ±(tw,th) window around each found peak so the next is a *different* location).
fn topk_peaks(map: &NccMap, tw: u32, th: u32, k: usize) -> Vec<(u32, u32, f32)> {
    let (w, h) = map.dimensions();
    let mut buf: Vec<f32> = map.pixels().map(|p| p.0[0]).collect();
    let mut peaks = Vec::new();
    for _ in 0..k {
        let mut bi = 0usize;
        let mut bv = f32::MIN;
        for (i, &v) in buf.iter().enumerate() {
            if v > bv {
                bv = v;
                bi = i;
            }
        }
        if bv <= f32::MIN {
            break;
        }
        let (px, py) = (bi as u32 % w, bi as u32 / w);
        peaks.push((px, py, bv));
        let (x0, y0) = (px.saturating_sub(tw), py.saturating_sub(th));
        let (x1, y1) = ((px + tw).min(w), (py + th).min(h));
        for yy in y0..y1 {
            for xx in x0..x1 {
                buf[(yy * w + xx) as usize] = f32::MIN;
            }
        }
    }
    peaks
}

/// Coarse-to-fine **full-screen** match returning **all** on-screen instances (top-K), so the
/// caller can disambiguate similar/repeated icons with a spatial prior (AI bbox / region).
/// *Coarse:* downscale haystack+template so the icon is ~`COARSE_ICON_PX`, one cheap full-frame
/// NCC, then NMS for up to `MAX_PEAKS` rough locations. *Fine:* re-match the **native** template
/// in a small window around each peak for pixel-precise, ~1.0-score hits. Returns matches in
/// **native (full-image) coords** with fine score ≥ `min_score`, sorted by score desc and
/// de-duplicated. Full-screen but ~0.3–0.5 s vs ~5.6 s for a naive native scan.
///
/// `scale_prior` centres the coarse+fine scale sweeps on an expected DPI ratio (target monitor
/// scale ÷ authoring scale); pass 1.0 when no DPI prior is known. With `prep` set, `template`
/// must be the RAW grayscale icon: the coarse pass edges it once at native scale (both coarse
/// sides then shrink identically), while each fine-pass scale resizes the grayscale first and
/// edges at the target scale (see [`NeedlePrep`] — the cross-DPI fix).
pub fn match_icon_pyramid(
    haystack: &GrayImage,
    template: &GrayImage,
    min_score: f32,
    scale_prior: f32,
    prep: NeedlePrep,
) -> Vec<TemplateMatch> {
    // DPI prior: the pack's crops are authored at one display scale; the live monitor may render
    // the app's icons larger/smaller. `scale_prior` = target-monitor-scale ÷ authoring-scale
    // *centres* the scale sweep on the expected ratio (e.g. a 200 %/100 % user → 2.0), so the
    // fixed ±range still brackets the true on-screen size without a wide, false-positive-prone
    // sweep. 1.0 (default / no DPI info) reproduces the pre-prior behaviour exactly.
    let prior = if scale_prior.is_finite() && scale_prior > 0.0 {
        scale_prior
    } else {
        1.0
    };
    let dscales: Vec<f32> = DEFAULT_SCALES.iter().map(|s| s * prior).collect();
    let cscales: Vec<f32> = COARSE_SCALES.iter().map(|s| s * prior).collect();
    let (hw, hh) = haystack.dimensions();
    let (tw, th) = template.dimensions();
    // The factor is template-based, NOT divided by the prior: a prior-aware `cf` was tried and
    // reverted — it shrinks the coarse TEMPLATE to ~6 px where the dilated edges vanish (thin
    // glyphs like move's arrows lost their coarse signature entirely). The on-screen icon lands
    // at ~COARSE_ICON_PX·prior in the coarse image instead; the cscales (×prior) bridge the gap.
    let cf = (COARSE_ICON_PX / tw.max(th) as f32).clamp(0.1, 1.0);
    // Template already small → a direct full match (pyramid wouldn't save anything).
    if cf >= 0.9 {
        return match_icon(haystack, template, &dscales, min_score, prep)
            .into_iter()
            .collect();
    }
    let (chw, chh) = (((hw as f32) * cf) as u32, ((hh as f32) * cf) as u32);
    let (ctw, cth) = (
        ((tw as f32) * cf).round().max(4.0) as u32,
        ((th as f32) * cf).round().max(4.0) as u32,
    );
    if ctw >= chw || cth >= chh {
        return match_icon(haystack, template, &dscales, min_score, prep)
            .into_iter()
            .collect();
    }
    // Coarse localizes on the SAME representation as the haystack (edged at native scale when
    // prep is set) so both sides' edges shrink identically through the cf downscale — the
    // dilation keeps them alive (see to_edges). Only the fine pass needs resize-then-prep.
    let tmpl_native;
    let coarse_src: &GrayImage = match prep {
        Some(p) => {
            tmpl_native = p(template);
            &tmpl_native
        }
        None => template,
    };
    let chay = image::imageops::resize(haystack, chw, chh, FilterType::Lanczos3);
    let ctmpl = image::imageops::resize(coarse_src, ctw, cth, FilterType::Lanczos3);
    // Pool top-K peaks from EVERY coarse scale's map — not just the single best-scoring map
    // (`best_scale_map`'s contract). With the DPI prior widening the swept range (1.34–3.0 at
    // prior 2), a spurious off-scale peak anywhere on screen can win the global max, and the
    // true icon's scale map would then never be peak-scanned at all (measured: Blender "scale"
    // at 200 % — the real toolbar icon was absent from the top-K while 1.34× look-alikes filled
    // it). The fine refine re-scores every pooled window at native res, so the extra coarse
    // candidates only cost a few small-window NCCs.
    let (chw2, chh2) = chay.dimensions();
    let mut coarse_centres: Vec<(f32, f32)> = Vec::new(); // native-res centres
    for &cs in &cscales {
        let stw = ((ctmpl.width() as f32) * cs).round() as u32;
        let sth = ((ctmpl.height() as f32) * cs).round() as u32;
        if stw < 4 || sth < 4 || stw >= chw2 || sth >= chh2 {
            continue;
        }
        let scaled = image::imageops::resize(&ctmpl, stw, sth, FilterType::Lanczos3);
        let map = match_template_parallel(
            &chay,
            &scaled,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        for (px, py, _cv) in topk_peaks(&map, stw, sth, MAX_PEAKS) {
            coarse_centres.push((
                (px as f32 + stw as f32 / 2.0) / cf,
                (py as f32 + sth as f32 / 2.0) / cf,
            ));
        }
    }
    if coarse_centres.is_empty() {
        return Vec::new();
    }
    // The same location usually peaks in several scale maps — dedupe near-identical centres
    // BEFORE the fine refine (each refine is a multi-scale small-window NCC, the cost driver).
    let (ddx, ddy) = (tw as f32 * prior / 2.0, th as f32 * prior / 2.0);
    let mut centres: Vec<(f32, f32)> = Vec::new();
    for (nx, ny) in coarse_centres {
        if !centres
            .iter()
            .any(|&(ex, ey)| (ex - nx).abs() < ddx && (ey - ny).abs() < ddy)
        {
            centres.push((nx, ny));
        }
    }
    // Widen the fine-refine window by the prior so a larger-than-authored on-screen icon still
    // fits comfortably around the coarse peak centre.
    let mw = (tw as f32 * prior * 1.5 + 24.0) as i32;
    let mh = (th as f32 * prior * 1.5 + 24.0) as i32;
    let mut out: Vec<TemplateMatch> = Vec::new();
    for (nx, ny) in centres {
        let x0 = (nx as i32 - mw).max(0);
        let y0 = (ny as i32 - mh).max(0);
        let x1 = (nx as i32 + mw).min(hw as i32);
        let y1 = (ny as i32 + mh).min(hh as i32);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let fwin = image::imageops::crop_imm(haystack, x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
            .to_image();
        if let Some(fine) = match_icon(&fwin, template, &dscales, min_score, prep) {
            let m = TemplateMatch {
                x: fine.x + x0,
                y: fine.y + y0,
                ..fine
            };
            // De-dup: two coarse peaks can refine to the same native spot.
            if !out
                .iter()
                .any(|e| (e.x - m.x).abs() < tw as i32 && (e.y - m.y).abs() < th as i32)
            {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Decode image bytes (PNG/JPEG) to grayscale — used for both the captured haystack and a
/// pack's icon-crop file.
pub fn load_gray_from_bytes(bytes: &[u8]) -> Result<GrayImage> {
    Ok(image::load_from_memory(bytes)
        .context("decode image for template matching")?
        .to_luma8())
}

/// Mean Sobel gradient magnitude of a grayscale image, UN-normalized — the absolute edge
/// energy. [`to_edges`] normalizes each image to its own max, which is what makes matching
/// theme-robust but also amplifies faint structure: Blender's grey-on-grey viewport grid edges
/// (≈15–40 grey levels) normalize to full range and NCC-match simple box/circle glyphs at
/// ≥0.97 (measured at FHD). The raw energy tells them apart: a real icon's strokes carry
/// ~5–10× the gradient energy of grid lines, on any theme (both sides are measured on the
/// actual images, so no absolute threshold is baked in).
pub fn mean_gradient(g: &GrayImage) -> f32 {
    let grad = imageproc::gradients::sobel_gradients(g);
    let n = (grad.width() * grad.height()).max(1) as f32;
    grad.pixels().map(|p| p.0[0] as f32).sum::<f32>() / n
}

/// Whether a fine match's window has plausible ABSOLUTE contrast for the icon it claims to be:
/// the raw (un-normalized) edge energy of the matched haystack window must be at least
/// `CONTRAST_MIN_RATIO` of the template's own energy at that scale. Rejects the degenerate
/// faint-structure matches (viewport grid ≈0.1–0.2×) while keeping true matches on any theme
/// (≈0.6–1.5×).
pub const CONTRAST_MIN_RATIO: f32 = 0.35;
pub fn contrast_plausible(hay_raw: &GrayImage, m: &TemplateMatch, needle_raw: &GrayImage) -> bool {
    let (hw, hh) = hay_raw.dimensions();
    if m.x < 0 || m.y < 0 || m.width == 0 || m.height == 0 {
        return true; // degenerate geometry — leave the decision to the score gates
    }
    let (x, y) = (m.x as u32, m.y as u32);
    if x + m.width > hw || y + m.height > hh {
        return true;
    }
    let win = image::imageops::crop_imm(hay_raw, x, y, m.width, m.height).to_image();
    let scaled = image::imageops::resize(needle_raw, m.width.max(1), m.height.max(1), FilterType::Lanczos3);
    let te = mean_gradient(&scaled);
    if te <= f32::EPSILON {
        return true;
    }
    mean_gradient(&win) >= te * CONTRAST_MIN_RATIO
}

/// Sobel gradient-magnitude edge map, normalized to 0–255. **Theme-robust matching preprocessing:**
/// `|∇|` is identical whether a glyph is dark-on-light or light-on-dark, so an icon cropped from one
/// theme still matches under a dark↔light (or grey/custom) flip — the icon's *shape* survives while
/// only its colour changes. Measured (Blender dark icons vs a captured White theme): raw-intensity
/// NCC collapses to 0.82–0.88 (below the 0.9 accept threshold → every icon misses), while edge NCC
/// holds at 0.94–1.00 and lands on the correct toolbar positions, with the same-theme baseline
/// ≥0.995 (no regression). The matcher (`match_icon` / `match_icon_pyramid`) is preprocessing-
/// agnostic — `try_template_pass` feeds it the edge maps (haystack edged once, each icon once).
pub fn to_edges(g: &GrayImage) -> GrayImage {
    let grad = imageproc::gradients::sobel_gradients(g);
    let max = grad.pixels().map(|p| p.0[0]).max().unwrap_or(1).max(1) as u32;
    let mut norm = GrayImage::new(grad.width(), grad.height());
    for (x, y, p) in grad.enumerate_pixels() {
        norm.put_pixel(x, y, image::Luma([(p.0[0] as u32 * 255 / max) as u8]));
    }
    // Thicken edges with a 3×3 dilation. A raw Sobel edge is ~1 px thin; the coarse pass downscales
    // the icon to ~12 px, where a 1 px line resamples to sub-pixel and disappears — so thin-edged
    // glyphs (move, add-cube) lose their signature and the coarse pass mis-localizes. Dilating to
    // ~3 px keeps the edge structure alive through the downscale.
    dilate3x3(&norm)
}

/// 3×3 max filter (grayscale dilation). Used to thicken edge maps so they survive the coarse-pass
/// downscale. O(9·N), a few tens of ms on a full screen — done once per locate on the haystack.
fn dilate3x3(img: &GrayImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dy in 0..3i32 {
                for dx in 0..3i32 {
                    let nx = x as i32 + dx - 1;
                    let ny = y as i32 + dy - 1;
                    if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                        m = m.max(img.get_pixel(nx as u32, ny as u32).0[0]);
                    }
                }
            }
            out.put_pixel(x, y, image::Luma([m]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, RgbaImage};

    /// Build a deterministic but non-uniform grayscale test image (a gradient with a few
    /// distinct blocks) so NCC has real structure to lock onto (a flat image is degenerate).
    fn structured(w: u32, h: u32) -> GrayImage {
        let mut img = GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 7 + y * 13) % 256) as u8;
                img.put_pixel(x, y, Luma([v]));
            }
        }
        // A couple of solid blocks to create a locally unique signature. Bounds-checked so the
        // helper is safe for any size (a small haystack just gets the clipped blocks).
        let mut block = |x0: u32, x1: u32, y0: u32, y1: u32, v: u8| {
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    img.put_pixel(x, y, Luma([v]));
                }
            }
        };
        block(50, 90, 40, 70, 240);
        block(20, 45, 80, 95, 10);
        img
    }

    fn crop(img: &GrayImage, x: u32, y: u32, w: u32, h: u32) -> GrayImage {
        image::imageops::crop_imm(img, x, y, w, h).to_image()
    }

    #[test]
    fn finds_exact_crop_at_its_location() {
        let hay = structured(200, 160);
        let tmpl = crop(&hay, 50, 40, 48, 40); // covers the bright block — locally unique
        let m = match_icon(&hay, &tmpl, &[1.0], 0.99, None).expect("exact crop should match");
        assert_eq!((m.x, m.y), (50, 40));
        assert_eq!((m.width, m.height), (48, 40));
        assert!(m.score > 0.99, "exact crop should score ~1.0, got {}", m.score);
    }

    #[test]
    fn rejects_when_below_threshold() {
        // The acceptance gate rejects whenever nothing clears the floor. NCC never exceeds 1.0,
        // so a threshold above 1.0 yields no match even for the exact crop (which otherwise
        // scores ~1.0) — a deterministic test of the min_score filter ("no pointer beats wrong
        // pointer"). The NCC of an *unrelated* template is data-dependent at these sizes, so the
        // threshold mechanism, not an absolute-rejection assumption, is what we verify.
        let hay = structured(200, 160);
        let tmpl = crop(&hay, 50, 40, 48, 40);
        assert!(match_icon(&hay, &tmpl, &[1.0], 1.01, None).is_none());
        // Sanity: the same crop is accepted under a sane floor (mirrors the exact-match test).
        assert!(match_icon(&hay, &tmpl, &[1.0], DEFAULT_MIN_SCORE, None).is_some());
    }

    #[test]
    fn finds_scaled_crop_via_scale_sweep() {
        let hay = structured(200, 160);
        let tmpl = crop(&hay, 50, 40, 48, 40);
        // Author the template at 1.25× so only the 0.8 scale brings it back to native size.
        let bigger = image::imageops::resize(&tmpl, 60, 50, FilterType::Lanczos3);
        let m = match_icon(&hay, &bigger, DEFAULT_SCALES, 0.9, None).expect("scaled crop should match");
        // The contract is "a down-scaling pass rescues an oversized template near the true
        // location" — the exact winning scale (≈0.8) is an implementation detail, so assert the
        // robust properties: the sweep had to shrink the 1.25×-authored crop (<1.0), and it
        // landed near the original footprint.
        assert!(m.scale < 1.0, "expected a down-scaling pass to win, got {}", m.scale);
        assert!((m.x - 50).abs() <= 4 && (m.y - 40).abs() <= 4, "near true location: {m:?}");
    }

    #[test]
    fn pyramid_finds_crop_full_screen() {
        // Coarse-to-fine over a larger image: the bright block is locally unique, so the pyramid
        // should return it at native precision near (50,40) with a high score.
        let hay = structured(400, 320);
        let tmpl = crop(&hay, 50, 40, 40, 30);
        let hits = match_icon_pyramid(&hay, &tmpl, DEFAULT_MIN_SCORE, 1.0, None);
        assert!(!hits.is_empty(), "pyramid should find the crop");
        let m = hits[0];
        assert!((m.x - 50).abs() <= 4 && (m.y - 40).abs() <= 4, "near true location: {m:?}");
        assert!(m.score >= DEFAULT_MIN_SCORE, "fine score should clear threshold: {}", m.score);
    }

    #[test]
    fn dpi_prior_rescues_upscaled_icon_via_2x_scale() {
        // Simulate a 200 %-DPI user: the on-screen icon is 2× the authored template. Crop the
        // template across the bright block *edge* (like the other tests) so NCC has real structure
        // — a crop fully inside the flat block would match flat-on-flat everywhere. Render the
        // "live screen" at 2× so the 48×40 feature at (50,40) becomes ~96×80 at ~(100,80).
        let base = structured(300, 240);
        let tmpl = crop(&base, 50, 40, 48, 40); // straddles the block edge → locally unique
        let hay = image::imageops::resize(&base, 600, 480, FilterType::Lanczos3);

        // Prior 1.0: the sweep tops out at 1.5×, so it physically cannot produce a ~2× match —
        // whatever it does (or doesn't) find, no hit can reach the enlarged scale.
        let no_prior = match_icon_pyramid(&hay, &tmpl, DEFAULT_MIN_SCORE, 1.0, None);
        assert!(
            no_prior.iter().all(|m| m.scale < 1.8),
            "1.0 prior sweep (max 1.5×) can't reach 2×, got {no_prior:?}"
        );

        // Prior 2.0: the sweep centres on 2×, so it locates the enlarged feature at the doubled
        // position, at ≈2× scale, covering the ~96 px footprint — none of which the 1.0 sweep can.
        let hits = match_icon_pyramid(&hay, &tmpl, DEFAULT_MIN_SCORE, 2.0, None);
        assert!(!hits.is_empty(), "2.0 prior should rescue the 2×-scaled icon");
        let m = hits[0];
        assert!(m.scale >= 1.8, "winning scale should be ≈2×, got {}", m.scale);
        assert!((m.x - 100).abs() <= 12 && (m.y - 80).abs() <= 12, "near 2× location: {m:?}");
        assert!(m.width >= 80, "match should cover the enlarged ~96 px footprint, got {}", m.width);
    }

    #[test]
    fn contrast_gate_rejects_faint_structure() {
        // A faint (low-contrast) copy of a glyph NCC-matches in NORMALIZED edge space — that is
        // exactly the measured viewport-grid failure. The absolute-contrast gate must reject the
        // faint window while accepting the true-contrast one.
        let mut tmpl = GrayImage::new(30, 30);
        for y in 8..22 {
            for x in 8..22 {
                // bright hollow box on dark bg (strong strokes)
                let edge = x == 8 || x == 21 || y == 8 || y == 21;
                tmpl.put_pixel(x, y, Luma([if edge { 230 } else { 40 }]));
            }
        }
        let mut hay = GrayImage::from_pixel(200, 80, Luma([40]));
        // True-contrast instance at (10,10); faint instance (strokes ~12 levels) at (120,10).
        for y in 8..22 {
            for x in 8..22 {
                let edge = x == 8 || x == 21 || y == 8 || y == 21;
                hay.put_pixel(x + 2, y + 2, Luma([if edge { 230 } else { 40 }]));
                hay.put_pixel(x + 112, y + 2, Luma([if edge { 52 } else { 40 }]));
            }
        }
        let strong = TemplateMatch { x: 2, y: 2, width: 30, height: 30, score: 1.0, scale: 1.0 };
        let faint = TemplateMatch { x: 112, y: 2, width: 30, height: 30, score: 1.0, scale: 1.0 };
        assert!(contrast_plausible(&hay, &strong, &tmpl), "true-contrast window must pass");
        assert!(!contrast_plausible(&hay, &faint, &tmpl), "faint window must be rejected");
    }

    #[test]
    fn non_finite_or_zero_prior_falls_back_to_1x() {
        // A bad prior (0, NaN, negative) must not break matching — it should behave as 1.0.
        let hay = structured(400, 320);
        let tmpl = crop(&hay, 50, 40, 40, 30);
        for bad in [0.0_f32, f32::NAN, -3.0, f32::INFINITY] {
            let hits = match_icon_pyramid(&hay, &tmpl, DEFAULT_MIN_SCORE, bad, None);
            assert!(!hits.is_empty(), "prior {bad} should fall back to 1.0 and still match");
            assert!((hits[0].x - 50).abs() <= 4 && (hits[0].y - 40).abs() <= 4);
        }
    }

    #[test]
    fn oversized_template_is_skipped_not_panic() {
        let hay = structured(60, 60);
        let tmpl = structured(100, 100); // larger than haystack on every scale
        // Must not panic (match_template panics on a non-smaller template) — just no match.
        assert!(match_icon(&hay, &tmpl, DEFAULT_SCALES, 0.5, None).is_none());
    }

    // Live helper for building icon packs: capture a window through Navisual's OWN capture
    // pipeline (the same per-monitor BitBlt + masking the locator's OCR path uses) and save it
    // as a native-resolution PNG. Icon crops taken from this file are in the exact pixel space
    // Pass-3 matches against at runtime, so they match cleanly. Pass the window handle in
    // NAVISUAL_TEST_HWND and optionally an output path in OUT. Run:
    //   $env:NAVISUAL_TEST_HWND=<hwnd>; $env:OUT="C:\path\cap.png";
    //   cargo test --lib capture_window_png -- --ignored --nocapture
    #[test]
    #[ignore]
    fn capture_window_png() {
        let hwnd: usize = std::env::var("NAVISUAL_TEST_HWND")
            .expect("set NAVISUAL_TEST_HWND to the target window handle")
            .parse()
            .expect("NAVISUAL_TEST_HWND must be a decimal handle");
        let out = std::env::var("OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("navisual_capture.png"));
        let (img, rect) =
            crate::capture::recapture_window_raw(hwnd, &[]).expect("capture failed");
        let png = crate::capture::encode_png_for_ocr(&img).expect("encode failed");
        std::fs::write(&out, &png).expect("write failed");
        eprintln!(
            "captured {}x{} (window rect {:?}) -> {}",
            img.width(),
            img.height(),
            rect,
            out.display()
        );
    }

    // Capture a screen REGION via capture_region_raw (BitBlt) — the path that works on OpenGL apps
    // like Blender (PrintWindow returns blank grey there). Produces a proper matching haystack.
    //   $env:OUT="cap.png"; $env:REGION="0,0,1920,1032"; cargo test --lib capture_screen_png -- --ignored --nocapture
    #[test]
    #[ignore]
    fn capture_screen_png() {
        let parts: Vec<i32> = std::env::var("REGION")
            .unwrap_or_else(|_| "0,0,1920,1032".into())
            .split(',')
            .map(|s| s.trim().parse().expect("REGION ints"))
            .collect();
        let rect = crate::capture::Rect { x: parts[0], y: parts[1], width: parts[2] as u32, height: parts[3] as u32 };
        let raw = crate::capture::capture_region_raw(rect, &[]).expect("capture failed");
        let png = crate::capture::encode_png_for_ocr(&raw).expect("encode failed");
        std::fs::write(std::env::var("OUT").expect("set OUT"), &png).expect("write failed");
        eprintln!("captured {}x{} (BitBlt) -> {}", raw.width(), raw.height(), std::env::var("OUT").unwrap());
    }

    // Diagnostic: click a workspace tab, park the cursor, BitBlt the screen. For inspecting one
    // workspace's toolbar (e.g. why a captured icon looks wrong).
    //   $env:WS="Modeling"; $env:OUT="modeling.png"; cargo test --lib diag_capture_workspace -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_capture_workspace() {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        let ws = std::env::var("WS").unwrap();
        let out = std::env::var("OUT").unwrap();
        if let Some((_, tx, ty)) = find_workspace_tabs().iter().find(|(t, _, _)| t.eq_ignore_ascii_case(&ws)) {
            click_at(*tx, *ty);
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        if let Some(n) = std::env::var("SCROLL").ok().and_then(|s| s.parse::<i32>().ok()) {
            if n != 0 {
                let us = ui_scale();
                scroll_at((28.0 * us) as i32, (300.0 * us) as i32, -n); // negative = toward lower tools
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
        unsafe {
            let _ = SetCursorPos(960, 540);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let raw = crate::capture::capture_region_raw(crate::capture::Rect { x: 0, y: 0, width: 1920, height: 1032 }, &[]).unwrap();
        std::fs::write(&out, crate::capture::encode_png_for_ocr(&raw).unwrap()).unwrap();
        eprintln!("captured {ws} -> {out}");
    }

    // Diagnostic: hover a specific toolbar Y in a workspace, print its tooltip, save a wide crop of
    // that slot — to check whether a captured icon's glyph matches its tooltip name + is centred.
    //   $env:WS="Modeling"; $env:Y="688"; $env:OUT="probe.png"; cargo test --lib diag_hover_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diag_hover_probe() {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        let ws = std::env::var("WS").unwrap();
        let y: i32 = std::env::var("Y").unwrap().parse().unwrap();
        if let Some((_, tx, ty)) = find_workspace_tabs().iter().find(|(t, _, _)| t.eq_ignore_ascii_case(&ws)) {
            click_at(*tx, *ty);
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
        let lines = harvest_tooltip((28.0 * ui_scale()) as i32, y);
        eprintln!("Y={y} tooltip: {lines:?}");
        unsafe {
            let _ = SetCursorPos(960, 540);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        if let Ok(cap) = crate::capture::capture_region_raw(crate::capture::Rect { x: 0, y: 0, width: 64, height: 900 }, &[]) {
            let c = image::imageops::crop_imm(&cap, 0, (y - 24).max(0) as u32, 54, 48).to_image();
            if let Ok(o) = std::env::var("OUT") {
                let big = image::imageops::resize(&c, 54 * 6, 48 * 6, FilterType::Nearest);
                let _ = big.save(o);
            }
        }
    }

    // Live helper: crop IN by CROP="x,y,w,h", optionally upscale by SCALE (integer, nearest —
    // for eyeballing tiny icons), write OUT. Used to extract an icon template from a capture.
    //   $env:IN="cap.png"; $env:CROP="0,58,26,170"; $env:SCALE="5"; $env:OUT="strip.png";
    //   cargo test --lib crop_png -- --ignored --nocapture
    #[test]
    #[ignore]
    fn crop_png() {
        let inp = std::env::var("IN").expect("set IN");
        let out = std::env::var("OUT").expect("set OUT");
        let parts: Vec<u32> = std::env::var("CROP")
            .expect("set CROP=x,y,w,h")
            .split(',')
            .map(|s| s.trim().parse().expect("CROP ints"))
            .collect();
        let [x, y, w, h] = parts[..] else {
            panic!("CROP must be x,y,w,h")
        };
        let scale: u32 = std::env::var("SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let img = image::open(&inp).expect("open IN").to_rgba8();
        let mut sub = image::imageops::crop_imm(&img, x, y, w, h).to_image();
        if scale > 1 {
            sub = image::imageops::resize(&sub, w * scale, h * scale, FilterType::Nearest);
        }
        sub.save(&out).expect("save OUT");
        eprintln!("cropped {w}x{h} @ ({x},{y}) scale {scale} -> {out}");
    }

    // Live helper: auto-tighten an icon crop. Given a ROUGH box around ONE icon (REGION=x,y,w,h),
    // find the bright glyph's bounding box (pixels brighter than the dark button background) and
    // crop the original tightly to it + PAD px. Produces consistently centred, tight templates
    // without eyeballing. Prints the resolved absolute box. Optional SCALE writes an upscaled
    // preview to OUT_PREVIEW. THRESH = luma over background (default 45).
    //   $env:IN="cap.png"; $env:REGION="2,184,28,32"; $env:OUT="move.png";
    //   cargo test --lib autocrop_icon -- --ignored --nocapture
    #[test]
    #[ignore]
    fn autocrop_icon() {
        let inp = std::env::var("IN").expect("set IN");
        let out = std::env::var("OUT").expect("set OUT");
        let parts: Vec<i64> = std::env::var("REGION")
            .expect("set REGION=x,y,w,h")
            .split(',')
            .map(|s| s.trim().parse().expect("REGION ints"))
            .collect();
        let [rx, ry, rw, rh] = parts[..] else {
            panic!("REGION must be x,y,w,h")
        };
        let pad: i64 = std::env::var("PAD").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        let thresh: u16 = std::env::var("THRESH").ok().and_then(|s| s.parse().ok()).unwrap_or(45);

        let img = image::open(&inp).expect("open IN").to_rgba8();
        let region = image::imageops::crop_imm(&img, rx as u32, ry as u32, rw as u32, rh as u32)
            .to_image();
        let gray = image::DynamicImage::ImageRgba8(region).to_luma8();
        // Background = median luma (the dark button dominates the rough box). Bright glyph
        // pixels exceed it by THRESH; their bounding box is the icon's true extent.
        let mut lumas: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
        lumas.sort_unstable();
        let bg = lumas[lumas.len() / 2] as u16;
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let mut bright = 0u32;
        for (px, py, p) in gray.enumerate_pixels() {
            if p.0[0] as u16 > bg + thresh {
                bright += 1;
                x0 = x0.min(px as i64);
                y0 = y0.min(py as i64);
                x1 = x1.max(px as i64);
                y1 = y1.max(py as i64);
            }
        }
        assert!(bright >= 8, "too few bright pixels ({bright}) — adjust REGION/THRESH");
        // Absolute, padded, clamped to the image.
        let (iw, ih) = (img.width() as i64, img.height() as i64);
        let ax0 = (rx + x0 - pad).clamp(0, iw - 1);
        let ay0 = (ry + y0 - pad).clamp(0, ih - 1);
        let ax1 = (rx + x1 + pad + 1).clamp(ax0 + 1, iw);
        let ay1 = (ry + y1 + pad + 1).clamp(ay0 + 1, ih);
        let (aw, ah) = ((ax1 - ax0) as u32, (ay1 - ay0) as u32);
        let tight = image::imageops::crop_imm(&img, ax0 as u32, ay0 as u32, aw, ah).to_image();
        tight.save(&out).expect("save OUT");
        eprintln!("autocrop: bg={bg} bright={bright} -> box ({ax0},{ay0}) {aw}x{ah} -> {out}");
        if let Ok(pv) = std::env::var("OUT_PREVIEW") {
            let s = std::env::var("SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(8u32);
            image::imageops::resize(&tight, aw * s, ah * s, FilterType::Nearest)
                .save(&pv)
                .expect("save preview");
            eprintln!("  preview {s}x -> {pv}");
        }
    }

    // Live: run the real engine — match TEMPLATE against HAYSTACK and print the best result.
    // Proves an icon crop is findable in a real capture before wiring up the full app test.
    //   $env:HAYSTACK="cap.png"; $env:TEMPLATE="move.png";
    //   cargo test --lib match_icon_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn match_icon_live() {
        let hay = load_gray_from_bytes(&std::fs::read(std::env::var("HAYSTACK").unwrap()).unwrap())
            .unwrap();
        let mut tmpl =
            load_gray_from_bytes(&std::fs::read(std::env::var("TEMPLATE").unwrap()).unwrap())
                .unwrap();
        let mut hay = hay;
        // CAP="WxH" simulates matching on the downscaled image we send the AI: fit the haystack
        // within W×H and shrink the template by the SAME factor (i.e. templates cropped at AI
        // scale), so a hit lands at sweep-scale ~1.0.
        if let Ok(cap) = std::env::var("CAP") {
            let dims: Vec<u32> = cap.split('x').filter_map(|s| s.trim().parse().ok()).collect();
            if let [cw, ch] = dims[..] {
                let f = (cw as f32 / hay.width() as f32)
                    .min(ch as f32 / hay.height() as f32)
                    .min(1.0);
                hay = image::imageops::resize(&hay, (hay.width() as f32 * f) as u32, (hay.height() as f32 * f) as u32, FilterType::Lanczos3);
                tmpl = image::imageops::resize(&tmpl, (tmpl.width() as f32 * f).round() as u32, (tmpl.height() as f32 * f).round() as u32, FilterType::Lanczos3);
                eprintln!("CAP {cw}x{ch}: downscale factor {f:.3}");
            }
        }
        // EDGES=1 → the theme-robust production path `try_template_pass` uses: haystack edged
        // once at native scale, template kept RAW and edged per-scale AFTER the resize
        // (NeedlePrep — the cross-DPI fix). Else raw intensity on both.
        let hay_raw = hay.clone();
        let tmpl_raw = tmpl.clone();
        let prep: NeedlePrep = if std::env::var("EDGES").is_ok() {
            hay = to_edges(&hay);
            eprintln!("EDGES: matching on Sobel gradient magnitude (needle edged after resize)");
            Some(to_edges)
        } else {
            None
        };
        eprintln!("haystack {}x{}, template {}x{}, {} scales", hay.width(), hay.height(), tmpl.width(), tmpl.height(), DEFAULT_SCALES.len());
        // PYRAMID=1 → full-screen coarse-to-fine top-K (the production path); else single full match.
        if std::env::var("PYRAMID").is_ok() {
            // PRIOR=<f32> exercises the DPI prior by hand (e.g. PRIOR=2.0 for a 200 % monitor).
            let prior: f32 = std::env::var("PRIOR").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let t = std::time::Instant::now();
            let hits = match_icon_pyramid(&hay, &tmpl, -1.0, prior, prep);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("pyramid full-screen: {} match(es) in {:.1} ms", hits.len(), ms);
            for m in &hits {
                // Production acceptance (scale-gated + contrast; no bbox rescue / region hint here).
                let contrast = contrast_plausible(&hay_raw, m, &tmpl_raw);
                let acc = template_match_accept(m.score, m.scale, prior, false, true) && contrast;
                eprintln!("  pos=({},{}) {}x{} score={:.4} scale={} contrast={} accepted={}", m.x, m.y, m.width, m.height, m.score, m.scale, contrast, acc);
            }
            return;
        }
        let t = std::time::Instant::now();
        let res = match_icon(&hay, &tmpl, DEFAULT_SCALES, -1.0, prep);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match res {
            Some(m) => eprintln!(
                "best: pos=({},{}) {}x{} score={:.4} scale={} accepted={} | match_icon took {:.1} ms",
                m.x, m.y, m.width, m.height, m.score, m.scale, m.score >= DEFAULT_MIN_SCORE, ms
            ),
            None => eprintln!("no match | match_icon took {ms:.1} ms"),
        }
    }

    // Capture a SPECIFIC app window by title (not the deduped picker list) — for grabbing the main
    // Blender window while its Preferences dialog is also open.
    //   $env:TITLE="blender"; $env:EXCLUDE="preferences"; $env:OUT="c:/Users/fujin/blender_light.png";
    //   cargo test --lib capture_main_window -- --ignored --nocapture
    #[test]
    #[ignore]
    fn capture_main_window() {
        let title = std::env::var("TITLE").unwrap_or_else(|_| "blender".into());
        let exclude = std::env::var("EXCLUDE").unwrap_or_default();
        let out = std::env::var("OUT").expect("set OUT");
        let hwnd = crate::capture::find_window_by_title(&title, &exclude).expect("no window found");
        let (img, rect) = crate::capture::recapture_window_raw(hwnd, &[]).expect("capture failed");
        let png = crate::capture::encode_png_for_ocr(&img).expect("encode failed");
        std::fs::write(&out, &png).expect("write failed");
        eprintln!(
            "captured '{title}' hwnd={hwnd} {}x{} rect={:?} -> {out}",
            img.width(),
            img.height(),
            rect
        );
    }

    // Theme-robustness eval: for each icon template, the best NCC against HAYSTACK using three
    // preprocessings — intensity (current), Sobel edges (proposed, theme-invariant), inverted
    // template (cheap dark↔light interim). Run with a same-theme capture (baseline, all should be
    // ~1.0) and a flipped-theme capture (where intensity collapses but edges should hold).
    //   $env:HAYSTACK="c:/Users/fujin/blender_light.png"; $env:ICONS="src-tauri/packs/blender/icons";
    //   cargo test --lib theme_match_eval -- --ignored --nocapture
    #[test]
    #[ignore]
    fn theme_match_eval() {
        let hay =
            load_gray_from_bytes(&std::fs::read(std::env::var("HAYSTACK").unwrap()).unwrap()).unwrap();
        let hay_edge = to_edges(&hay);
        let dir = std::env::var("ICONS").unwrap_or_else(|_| "src-tauri/packs/blender/icons".into());
        let score = |h: &GrayImage, t: &GrayImage| {
            match_icon(h, t, DEFAULT_SCALES, -1.0, None)
                .map(|m| m.score)
                .unwrap_or(f32::NAN)
        };
        eprintln!("haystack {}x{} | icons {dir}", hay.width(), hay.height());
        eprintln!("{:<12}{:>9} {:>9}  {:<13}{:>9}", "icon", "intens", "edge", "edge@(x,y)", "invert");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "png"))
            .collect();
        paths.sort();
        for p in paths {
            let tmpl = load_gray_from_bytes(&std::fs::read(&p).unwrap()).unwrap();
            let s_int = score(&hay, &tmpl);
            // Production edge path: raw template + prep-after-resize.
            let m_edge = match_icon(&hay_edge, &tmpl, DEFAULT_SCALES, -1.0, Some(to_edges));
            let (s_edge, ex, ey) = m_edge.map(|m| (m.score, m.x, m.y)).unwrap_or((f32::NAN, -1, -1));
            let mut inv = tmpl.clone();
            image::imageops::invert(&mut inv);
            let s_inv = score(&hay, &inv);
            eprintln!(
                "{:<12}{:>9.4} {:>9.4}  ({:>4},{:>4}){:>9.4}",
                p.file_stem().unwrap().to_string_lossy(),
                s_int,
                s_edge,
                ex,
                ey,
                s_inv
            );
        }
    }

    // P1 of the nav-pack auto-generation plan: hover each Object-Mode toolbar icon, OCR the tooltip
    // box beside it, and recover the name + shortcut. Proves the Path-B (hover + OCR) harvest on the
    // hardest case (Blender = pure OpenGL, no a11y). MOVES THE MOUSE — keep hands off ~15 s. Blender
    // should be in the **Layout** workspace / **Object Mode** (Object-Mode toolbar); other modes just
    // read whichever tools sit at those rows.
    //   cargo test --lib tooltip_sweep_blender -- --ignored --nocapture
    #[test]
    #[ignore]
    fn tooltip_sweep_blender() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let mut orig = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut orig);
        }
        // Toolbar icon centres (screen px; the Blender window sits at 0,0). x≈28 = column centre;
        // y from the locator eval (glyph top + ~12).
        let icons = [
            ("cursor", 147),
            ("move", 188),
            ("rotate", 224),
            ("scale", 260),
            ("transform", 293),
            ("annotate", 333),
            ("measure", 367),
            ("add_cube", 406),
        ];
        eprintln!("hovering toolbar; OCR of the tooltip box beside each icon:");
        for (name, y) in icons {
            let (cx, cy) = (28i32, y + 12);
            let lines = harvest_tooltip(cx, cy).join("  ⏎  ");
            eprintln!("  [{name:>9} @ {cx},{cy}]  {lines}");
        }
        unsafe {
            let _ = SetCursorPos(orig.x, orig.y);
        }
        eprintln!("(cursor restored to {},{})", orig.x, orig.y);
    }

    // Snap-to-glyph: given a rough icon cell (rx,ry,rw,rh), find the bright glyph's tight bbox
    // (pixels > median-background + thresh) in absolute image coords. The callable core of
    // `autocrop_icon`, reused by the pack generator. `None` if too few bright pixels.
    fn autocrop_glyph(
        img: &image::RgbaImage,
        region: (i64, i64, i64, i64),
        pad: i64,
        thresh: u16,
    ) -> Option<(u32, u32, u32, u32)> {
        let (rx, ry, rw, rh) = region;
        let sub = image::imageops::crop_imm(img, rx as u32, ry as u32, rw as u32, rh as u32).to_image();
        let gray = image::DynamicImage::ImageRgba8(sub).to_luma8();
        let mut lumas: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
        if lumas.is_empty() {
            return None;
        }
        lumas.sort_unstable();
        let bg = lumas[lumas.len() / 2] as u16;
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let mut bright = 0u32;
        for (px, py, p) in gray.enumerate_pixels() {
            if p.0[0] as u16 > bg + thresh {
                bright += 1;
                x0 = x0.min(px as i64);
                y0 = y0.min(py as i64);
                x1 = x1.max(px as i64);
                y1 = y1.max(py as i64);
            }
        }
        if bright < 8 {
            return None;
        }
        let (iw, ih) = (img.width() as i64, img.height() as i64);
        let ax0 = (rx + x0 - pad).clamp(0, iw - 1);
        let ay0 = (ry + y0 - pad).clamp(0, ih - 1);
        let ax1 = (rx + x1 + pad + 1).clamp(ax0 + 1, iw);
        let ay1 = (ry + y1 + pad + 1).clamp(ay0 + 1, ih);
        Some((ax0 as u32, ay0 as u32, (ax1 - ax0) as u32, (ay1 - ay0) as u32))
    }

    // Authoring-time UI scale (UI_SCALE env, default 1.0): the app under authoring may render at
    // 2× (a 200 % display, or Blender's Resolution Scale = 2.0 used to author high-fidelity
    // templates on a 100 % monitor — see nav-packs.md §6.1). Scales every layout constant in the
    // sweep helpers; 1.0 keeps the original 1× behaviour byte-identical.
    fn ui_scale() -> f32 {
        std::env::var("UI_SCALE").ok().and_then(|v| v.parse().ok()).filter(|s: &f32| s.is_finite() && *s > 0.0).unwrap_or(1.0)
    }

    // Hover an icon centre and OCR its tooltip (3× upscaled) → the text lines (line 1 = name,
    // a `Shortcut:` line = the key). Caller saves/restores the cursor around a sweep.
    fn harvest_tooltip(cx: i32, cy: i32) -> Vec<String> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        static N: AtomicUsize = AtomicUsize::new(0);
        unsafe {
            let _ = SetCursorPos(cx, cy);
        }
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let us = ui_scale();
        let rect = crate::capture::Rect {
            x: cx + (8.0 * us) as i32,
            y: cy - (20.0 * us) as i32,
            width: (480.0 * us) as u32,
            height: (160.0 * us) as u32,
        };
        let Ok(raw) = crate::capture::capture_region_raw(rect, &[]) else {
            return Vec::new();
        };
        // Upscale 6×: tooltip text is small + dim. 3× reads the brighter Object-mode tips but
        // loses the name/first line of denser Edit/Sculpt tooltips (Loop Cut's "Loop Cut" only
        // appears ≥~6×). `TIP_DIR=…` saves each crop for inspection (NOT `OUT_DIR` — cargo reserves
        // that for build scripts, so it gets clobbered to the build `out/` dir).
        // Text is already `us`× bigger at a higher UI scale — divide the upscale accordingly.
        let upf = ((6.0 / ui_scale()).ceil() as u32).max(2);
        let up = image::imageops::resize(&raw, raw.width() * upf, raw.height() * upf, FilterType::Lanczos3);
        if let Ok(dir) = std::env::var("TIP_DIR") {
            let n = N.fetch_add(1, Ordering::SeqCst);
            let _ = std::fs::create_dir_all(&dir);
            let _ = up.save(std::path::Path::new(&dir).join(format!("tip_{n:03}_y{cy}.png")));
        }
        crate::capture::encode_png_for_ocr(&up)
            .ok()
            .and_then(|png| crate::locator::ocr::run_ocr(&png).ok())
            .map(|res| {
                res.iter()
                    .filter(|r| r.confidence >= 1.0)
                    .map(|r| r.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    // Detect icon rows in a vertical toolbar (x0..x1, y0..y1) by edge-content banding: rows that
    // carry glyph edges are icon rows, flat gaps between icons carry none. Returns each icon's
    // (y_centre, height) top→bottom — so a sweep no longer needs hardcoded rows. Theme-independent
    // (edges, not colour).
    fn detect_toolbar_icons(cap: &image::RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32) -> Vec<(u32, u32)> {
        let gray = image::DynamicImage::ImageRgba8(
            image::imageops::crop_imm(cap, x0, y0, x1 - x0, y1 - y0).to_image(),
        )
        .to_luma8();
        let edges = imageproc::gradients::sobel_gradients(&gray);
        let h = edges.height();
        let mut row = vec![0u32; h as usize];
        for (_, yy, p) in edges.enumerate_pixels() {
            row[yy as usize] += p.0[0] as u32;
        }
        let maxr = *row.iter().max().unwrap_or(&1);
        let thr = (maxr / 12).max(1); // a row is "content" above ~1/12 of the busiest row (thin glyphs at 2× sit well under /6; the gaps are truly flat so low is safe)
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let (mut start, mut gap) = (None::<u32>, 0u32);
        for yy in 0..h {
            if row[yy as usize] >= thr {
                if start.is_none() {
                    start = Some(yy);
                }
                gap = 0;
            } else if let Some(s) = start {
                gap += 1;
                if gap > (2.0 * ui_scale()).round() as u32 {
                    // bridge ≤~2·scale px dips inside a glyph; a bigger gap ends the icon (keeps
                    // adjacent icons — esp. the active highlighted tool + its neighbour — from merging)
                    bands.push((s, yy - gap));
                    start = None;
                }
            }
        }
        if let Some(s) = start {
            bands.push((s, h - 1));
        }
        bands
            .into_iter()
            .filter(|(a, b)| {
                let hh = b - a;
                let us = ui_scale();
                let (lo, hi) = ((12.0 * us) as u32, (44.0 * us) as u32);
                (lo..=hi).contains(&hh) // an icon glyph, not noise or a merged run
            })
            .map(|(a, b)| (y0 + (a + b) / 2, b - a))
            .collect()
    }

    // Brightness-based icon detection for panel regions (tab columns / header rows), either
    // orientation. Unlike the edge-based left-toolbar detector, this catches FILLED coloured icons
    // (orange Object tab, blue Modifiers, red World) whose edges are weak: it bands the lines (rows
    // if vert, columns if horiz) that hold several glyph-bright pixels. cap IS the region; returns
    // (centre, size) in cap coords.
    fn detect_region_icons(cap: &image::RgbaImage, vert: bool) -> Vec<(u32, u32)> {
        let gray = image::DynamicImage::ImageRgba8(cap.clone()).to_luma8();
        let (w, h) = (gray.width(), gray.height());
        let mut sorted: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
        sorted.sort_unstable();
        let bg = sorted[sorted.len() / 3] as u16; // panel background
        let thr = bg + 22; // glyph pixels (white line-art OR a filled colour) exceed this
        let (n, cross) = if vert { (h, w) } else { (w, h) };
        let mut line = vec![0u32; n as usize];
        for (xx, yy, p) in gray.enumerate_pixels() {
            if p.0[0] as u16 > thr {
                line[(if vert { yy } else { xx }) as usize] += 1;
            }
        }
        let linethr = (cross / 5).max(1); // a line with >~20% glyph pixels is icon content
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let (mut start, mut gap) = (None::<u32>, 0u32);
        for i in 0..n {
            if line[i as usize] >= linethr {
                if start.is_none() {
                    start = Some(i);
                }
                gap = 0;
            } else if let Some(s) = start {
                gap += 1;
                if gap > 2 {
                    bands.push((s, i - gap));
                    start = None;
                }
            }
        }
        if let Some(s) = start {
            bands.push((s, n - 1));
        }
        bands
            .into_iter()
            .filter(|(a, b)| (8..=40).contains(&(b - a)))
            .map(|(a, b)| ((a + b) / 2, b - a))
            .collect()
    }

    // Detect the BUTTON BOX bounds (not the glyph). Blender's left-toolbar buttons are filled
    // rounded-rects slightly brighter than the dark gaps between them; band the rows where the box
    // fill spans >half the strip width. Returns (y_centre, box_height, box_left, box_right). The box
    // is uniform-sized and gapped, so cropping to it gives clean uniform squares (unlike the glyph
    // bbox, which varies and can touch a neighbour).
    fn detect_toolbar_boxes(cap: &image::RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32) -> Vec<(u32, u32, u32, u32)> {
        let gray = image::DynamicImage::ImageRgba8(
            image::imageops::crop_imm(cap, x0, y0, x1 - x0, y1 - y0).to_image(),
        )
        .to_luma8();
        let (w, h) = (gray.width(), gray.height());
        let mut sorted: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
        sorted.sort_unstable();
        let bg = sorted[sorted.len() / 10] as u16; // toolbar gap (the darkest 10%)
        let thr = bg + 10; // box fill + glyph exceed the gap by this
        let mut row_fill = vec![0u32; h as usize];
        for (_, yy, p) in gray.enumerate_pixels() {
            if p.0[0] as u16 > thr {
                row_fill[yy as usize] += 1;
            }
        }
        let rowthr = w / 2; // a box row fills more than half the strip width
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let (mut start, mut gap) = (None::<u32>, 0u32);
        for yy in 0..h {
            if row_fill[yy as usize] >= rowthr {
                if start.is_none() {
                    start = Some(yy);
                }
                gap = 0;
            } else if let Some(s) = start {
                gap += 1;
                if gap > 1 {
                    bands.push((s, yy - gap));
                    start = None;
                }
            }
        }
        if let Some(s) = start {
            bands.push((s, h - 1));
        }
        bands
            .into_iter()
            .filter(|(a, b)| (18..=46).contains(&(b - a)))
            .map(|(a, b)| {
                // box left/right: the column span where this band's rows are box-bg-or-brighter
                let (mut cmin, mut cmax) = (w, 0u32);
                for yy in a..=b {
                    for xx in 0..w {
                        if gray.get_pixel(xx, yy).0[0] as u16 > thr {
                            cmin = cmin.min(xx);
                            cmax = cmax.max(xx);
                        }
                    }
                }
                (y0 + (a + b) / 2, b - a, x0 + cmin, x0 + cmax)
            })
            .collect()
    }

    // Measure the button boxes on a capture: prints each box + the derived uniform size and spacing.
    //   $env:IN="c:/Users/fujin/blender_bitblt.png"; cargo test --lib measure_toolbar_boxes -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_toolbar_boxes() {
        let cap = image::open(std::env::var("IN").unwrap()).unwrap().to_rgba8();
        // Luma deciles of the strip — to see the gap/box-bg/glyph clusters.
        let strip = image::DynamicImage::ImageRgba8(image::imageops::crop_imm(&cap, 0, 100, 54, 780).to_image()).to_luma8();
        let mut ls: Vec<u8> = strip.pixels().map(|p| p.0[0]).collect();
        ls.sort_unstable();
        let dec: Vec<u8> = (0..=10).map(|i| ls[(ls.len() - 1) * i / 10]).collect();
        eprintln!("luma deciles: {dec:?}");
        let boxes = detect_toolbar_boxes(&cap, 0, 54, 100, 880);
        eprintln!("{} boxes:", boxes.len());
        let mut prev = 0u32;
        for (yc, hh, l, r) in &boxes {
            let sp = if prev == 0 { 0 } else { yc - prev };
            eprintln!("  y={yc:>3} h={hh:>2} w={:>2} (x {l}-{r})  spacing={sp}", r - l);
            prev = *yc;
        }
    }

    // Test the toolbar icon detector against a capture; prints detected centres for tuning.
    //   $env:IN="c:/Users/fujin/blender_current.png"; cargo test --lib detect_toolbar_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn detect_toolbar_live() {
        let cap = image::open(std::env::var("IN").unwrap()).unwrap().to_rgba8();
        let icons = detect_toolbar_icons(&cap, 4, 44, 100, 480);
        eprintln!("detected {} toolbar icons:", icons.len());
        for (yc, hh) in &icons {
            eprintln!("  y_centre={yc}  height={hh}");
        }
    }

    // P2 emit round-trip: sweep Blender's Object-Mode toolbar, autocrop each glyph, and write a
    // real generated pack (`OUT/pack.json` + `OUT/icons/<slug>.png`) from the harvested names +
    // shortcuts. MOVES THE MOUSE. Output goes to a separate dir so it can be compared to the
    // hand-made pack + run through the eval; nothing in the repo is overwritten.
    //   $env:OUT="c:/Users/fujin/blender_autogen"; cargo test --lib generate_blender_pack -- --ignored --nocapture
    #[test]
    #[ignore]
    fn generate_blender_pack() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let out = std::path::PathBuf::from(
            std::env::var("OUT").unwrap_or_else(|_| "c:/Users/fujin/blender_autogen".into()),
        );
        std::fs::create_dir_all(out.join("icons")).expect("mkdir");
        // Park the cursor off the toolbar + capture a clean toolbar column (no hover highlight).
        let mut orig = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut orig);
            let _ = SetCursorPos(960, 540);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let cap = crate::capture::capture_region_raw(
            crate::capture::Rect { x: 0, y: 0, width: 64, height: 520 },
            &[],
        )
        .expect("capture toolbar");
        // Object-Mode rows (y centres from the locator eval). Each cell ≈ x4..44.
        let rows = [147, 188, 224, 260, 293, 333, 367, 406];
        let mut entries: Vec<(String, String)> = Vec::new(); // (name, shortcut)
        for y in rows {
            let lines = harvest_tooltip(28, y + 12);
            let name = lines.first().cloned().unwrap_or_default();
            let shortcut = lines
                .iter()
                .find(|l| l.contains("Shortcut"))
                .and_then(|l| l.rsplit(',').next())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let slug: String = name
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_");
            if let Some((ax, ay, aw, ah)) = autocrop_glyph(&cap, (4, (y - 4) as i64, 40, 36), 2, 45) {
                let crop = image::imageops::crop_imm(&cap, ax, ay, aw, ah).to_image();
                let _ = crop.save(out.join("icons").join(format!("{slug}.png")));
            }
            entries.push((name, shortcut));
        }
        unsafe {
            let _ = SetCursorPos(orig.x, orig.y);
        }
        // Emit pack.json (matches the hand-made schema).
        let shortcuts = entries
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(n, s)| format!("    \"{n}\": \"{s}\""))
            .collect::<Vec<_>>()
            .join(",\n");
        let hints = entries
            .iter()
            .map(|(n, _)| format!("    {{ \"name\": \"{n}\", \"region\": \"left\", \"role\": \"button\" }}"))
            .collect::<Vec<_>>()
            .join(",\n");
        let pack = format!(
            "{{\n  \"id\": \"blender_autogen\",\n  \"name\": \"Blender Nav-Pack (auto-generated)\",\n  \"version\": \"1.0.0\",\n  \"min_app_version\": \"0.6.0\",\n  \"target_app\": \"Blender\",\n  \"window_title_pattern\": \"(?i)blender\",\n  \"system_prompt_injection\": \"The user is in Blender. Prefer keyboard shortcuts; the left Toolbar holds the active tools.\",\n  \"shortcuts\": {{\n{shortcuts}\n  }},\n  \"element_hints\": [\n{hints}\n  ]\n}}\n"
        );
        std::fs::write(out.join("pack.json"), &pack).expect("write pack.json");
        eprintln!("generated pack -> {}", out.display());
        for (n, s) in &entries {
            eprintln!("  {n:<12} shortcut={s}");
        }
    }

    // Synthesized wheel scroll at a point (clicks>0 = up, <0 = down). For reaching toolbar icons
    // below the fold (e.g. the Sculpt toolbar). Authoring-only.
    #[allow(deprecated)]
    fn scroll_at(x: i32, y: i32, clicks: i32) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_WHEEL};
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        unsafe {
            let _ = SetCursorPos(x, y);
            std::thread::sleep(std::time::Duration::from_millis(60));
            mouse_event(MOUSEEVENTF_WHEEL, 0, 0, clicks * 120, 0); // WHEEL_DELTA = 120/click
        }
    }

    fn slugify(name: &str) -> String {
        name.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    }

    // Normalized cross-correlation of two glyph crops (b resized to a's size). Used to dedup an
    // already-captured icon across scrolls WITHOUT re-harvesting its tooltip (the slow part).
    fn ncc_eq(a: &image::GrayImage, b: &image::GrayImage) -> f32 {
        let b = image::imageops::resize(b, a.width().max(1), a.height().max(1), FilterType::Triangle);
        let n = (a.width() * a.height()) as f32;
        if n < 1.0 {
            return 0.0;
        }
        let ma = a.pixels().map(|p| p.0[0] as f32).sum::<f32>() / n;
        let mb = b.pixels().map(|p| p.0[0] as f32).sum::<f32>() / n;
        let (mut num, mut da, mut db) = (0.0f32, 0.0f32, 0.0f32);
        for (pa, pb) in a.pixels().zip(b.pixels()) {
            let (va, vb) = (pa.0[0] as f32 - ma, pb.0[0] as f32 - mb);
            num += va * vb;
            da += va * va;
            db += vb * vb;
        }
        num / (da.sqrt() * db.sqrt() + 1e-6)
    }

    // Synthesized left click at a screen point — for the multi-state authoring sweep (clicking
    // workspace tabs). Authoring-only; the shipped app never actuates the UI.
    #[allow(deprecated)]
    fn click_at(x: i32, y: i32) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        };
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        unsafe {
            let _ = SetCursorPos(x, y);
            std::thread::sleep(std::time::Duration::from_millis(80));
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            std::thread::sleep(std::time::Duration::from_millis(40));
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        }
    }

    // Root window class at a screen point. Blender is "GHOST_WindowClass"; a File Explorer that's
    // overlapping the toolbar is "CabinetWClass". Lets the sweep skip rows covered by a foreign window.
    fn window_class_at(x: i32, y: i32) -> String {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetClassNameW, WindowFromPoint, GA_ROOT};
        unsafe {
            let hwnd = WindowFromPoint(POINT { x, y });
            if hwnd.0.is_null() {
                return String::new();
            }
            let root = GetAncestor(hwnd, GA_ROOT);
            let mut buf = [0u16; 256];
            let n = GetClassNameW(root, &mut buf);
            String::from_utf16_lossy(&buf[..n as usize])
        }
    }

    // OCR the workspace-tab strip (top of the window) → each tab's text + screen x-centre, so the
    // sweep can click them. Upscaled 3× (small tab font).
    fn find_workspace_tabs() -> Vec<(String, i32, i32)> {
        let us = ui_scale();
        // The ~24 px title row above the tab strip is OS-drawn and does NOT scale with the app's
        // ui_scale, so the strip's y can't be scaled linearly — scan a strip tall enough to hold
        // the tab row at any supported scale and return each tab's OCR-measured centre (x AND y),
        // so the caller clicks exactly what was read.
        let region = crate::capture::Rect {
            x: 0,
            y: 20,
            width: ((1180.0 * us) as u32).min(1920),
            height: (30.0 * us) as u32 + 20,
        };
        let up = ((4.0 / us).ceil() as i32).max(2); // the inactive tabs are dim grey — 4× reads them at 1×
        crate::capture::capture_region_raw(region, &[])
            .ok()
            .map(|raw| image::imageops::resize(&raw, raw.width() * up as u32, raw.height() * up as u32, FilterType::Lanczos3))
            .and_then(|u| crate::capture::encode_png_for_ocr(&u).ok())
            .and_then(|png| crate::locator::ocr::run_ocr(&png).ok())
            .unwrap_or_default()
            .iter()
            .filter(|r| r.confidence >= 1.0) // each tab is its own line-level result; centres below
            .map(|r| {
                (
                    r.text.clone(),
                    region.x + (r.bbox.0 + r.bbox.2 as i32 / 2) / up,
                    region.y + (r.bbox.1 + r.bbox.3 as i32 / 2) / up,
                )
            })
            .collect()
    }

    // Dump raw OCR results (order, bbox, confidence) for a saved image — to debug tooltip parsing.
    //   $env:IN="…/tip_020_y565.png"; cargo test --lib ocr_dump -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_dump() {
        let bytes = std::fs::read(std::env::var("IN").unwrap()).unwrap();
        let up: u32 = std::env::var("UP").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let png = if up > 1 {
            let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
            let big = image::imageops::resize(&img, img.width() * up, img.height() * up, FilterType::Lanczos3);
            crate::capture::encode_png_for_ocr(&big).unwrap()
        } else {
            bytes
        };
        eprintln!("UP={up}:");
        for r in crate::locator::ocr::run_ocr(&png).unwrap_or_default().iter().filter(|r| r.confidence >= 1.0) {
            eprintln!("  y={:>3} '{}'", r.bbox.1, r.text);
        }
    }

    // Iterate the tab OCR on a saved capture (no live click). IN=capture.png; UP=upscale.
    //   $env:IN="c:/Users/fujin/blender_current.png"; $env:UP="5"; cargo test --lib find_tabs_eval -- --ignored --nocapture
    #[test]
    #[ignore]
    fn find_tabs_eval() {
        let img = image::open(std::env::var("IN").unwrap()).unwrap().to_rgba8();
        let up: u32 = std::env::var("UP").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
        let sub = image::imageops::crop_imm(&img, 0, 26, 1180, 22).to_image();
        let big = image::imageops::resize(&sub, sub.width() * up, sub.height() * up, FilterType::Lanczos3);
        let png = crate::capture::encode_png_for_ocr(&big).unwrap();
        let res = crate::locator::ocr::run_ocr(&png).unwrap_or_default();
        eprintln!("UP={up} — {} results:", res.len());
        for r in &res {
            eprintln!("  '{}'  x≈{}  conf={:.2}", r.text, r.bbox.0 / up as i32, r.confidence);
        }
    }

    // P2 multi-state: autonomously walk Blender's workspace tabs and harvest each toolbar — proves
    // the tool can navigate the app itself (OCR tabs → click → detect icons → hover/OCR tooltips).
    // MOVES + CLICKS THE MOUSE (authoring-only). Keep hands off ~1–2 min.
    //   cargo test --lib walk_workspaces_blender -- --ignored --nocapture
    #[test]
    #[ignore]
    fn walk_workspaces_blender() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let mut orig = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut orig);
        }
        let tabs = find_workspace_tabs();
        eprintln!(
            "workspace tabs: {}",
            tabs.iter().map(|(t, x, y)| format!("{t}@{x},{y}")).collect::<Vec<_>>().join("  ")
        );
        for want in ["Layout", "Modeling", "Sculpting"] {
            let Some((_, tx, ty)) = tabs.iter().find(|(t, _, _)| t.eq_ignore_ascii_case(want)) else {
                eprintln!("[{want}] tab not found — skipped");
                continue;
            };
            click_at(*tx, *ty);
            std::thread::sleep(std::time::Duration::from_millis(700)); // workspace switch
            unsafe {
                let _ = SetCursorPos(960, 540); // park off the toolbar
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let icons = crate::capture::capture_region_raw(crate::capture::Rect { x: 0, y: 0, width: 64, height: 760 }, &[])
                .ok()
                .map(|c| detect_toolbar_icons(&c, 4, 44, 100, 740))
                .unwrap_or_default();
            eprintln!("[{want}] detected {} toolbar icons:", icons.len());
            for (yc, _h) in &icons {
                let lines = harvest_tooltip(28, *yc as i32);
                let name = lines.first().cloned().unwrap_or_default();
                let sc = lines
                    .iter()
                    .find(|l| l.contains("Shortcut"))
                    .and_then(|l| l.rsplit(',').next())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                eprintln!("    {name:<24} [{sc}]");
            }
        }
        if let Some((_, tx, ty)) = tabs.iter().find(|(t, _, _)| t.eq_ignore_ascii_case("Layout")) {
            click_at(*tx, *ty); // restore to Layout
        }
        unsafe {
            let _ = SetCursorPos(orig.x, orig.y);
        }
        eprintln!("(restored to Layout + cursor {},{})", orig.x, orig.y);
    }

    // The "real" generator: walk Blender's workspaces, SCROLL each toolbar to the bottom, autocrop
    // every glyph, save each tooltip crop (TIP_DIR) for me to read, and emit one combined pack.json
    // + icons/. Dedup by name (across scrolls + workspaces). 6× OCR drives detection/dedup; the
    // authoritative names come from me reading the saved crops afterward. MOVES + CLICKS + SCROLLS
    // the mouse (authoring-only). Keep hands off a few minutes.
    //   $env:OUT="c:/Users/fujin/blender_pack"; $env:TIP_DIR="c:/Users/fujin/blender_pack/tips";
    //   cargo test --lib capture_blender_pack -- --ignored --nocapture
    #[test]
    #[ignore]
    fn capture_blender_pack() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
        let out = std::path::PathBuf::from(std::env::var("OUT").unwrap_or_else(|_| "c:/Users/fujin/blender_pack".into()));
        let _ = std::fs::create_dir_all(out.join("icons"));
        let mut orig = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut orig);
        }
        // UI_SCALE (default 1): all layout constants below are authored at 1× and multiplied up —
        // authoring at 2× (Blender Resolution Scale 2.0) yields high-fidelity templates whose
        // downscale to 1× is information-preserving (measured; nav-packs.md §6.1).
        let us = ui_scale();
        let sc = |v: f32| (v * us).round() as i32;
        let tabs = find_workspace_tabs();
        // Fail fast if we're not actually looking at Blender. The sweep reads FIXED primary-monitor
        // regions, so Blender must be maximized + foreground there; otherwise another window (even one
        // whose title contains "blender", e.g. a File Explorer folder) is captured → 0 tools, silently.
        if !tabs.iter().any(|(t, _, _)| ["Layout", "Modeling", "Sculpting"].iter().any(|e| t.eq_ignore_ascii_case(e))) {
            eprintln!(
                "ABORT: no Blender workspace tabs found (read: {:?}). Maximize Blender on the PRIMARY \
                 monitor (toolbar at the left edge, Layout/Modeling/Sculpting tabs along the top) and re-run.",
                tabs.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>()
            );
            return;
        }
        // (idx, workspace, ocr_name, shortcut, slug) — idx matches the TIP_DIR crop order.
        let mut manifest: Vec<(usize, String, String, String, String)> = Vec::new();
        let mut idx = 0usize;
        for ws in ["Layout", "Modeling", "Sculpting"] {
            let Some((_, tx, ty)) = tabs.iter().find(|(t, _, _)| t.eq_ignore_ascii_case(ws)) else {
                continue;
            };
            click_at(*tx, *ty);
            std::thread::sleep(std::time::Duration::from_millis(700));
            scroll_at(sc(28.0), sc(300.0), 10); // toolbar → top
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut seen_glyphs: Vec<image::GrayImage> = Vec::new(); // per-workspace dedup
            let mut empty = 0;
            for _scroll in 0..14 {
                unsafe {
                    let _ = SetCursorPos(960, 540); // park off the toolbar for a clean capture
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
                let cap_h = ((900.0 * us) as u32).min(1044);
                let Ok(cap) = crate::capture::capture_region_raw(crate::capture::Rect { x: 0, y: 0, width: (64.0 * us) as u32, height: cap_h }, &[]) else {
                    break;
                };
                // x 10..47 (× scale) = the toolbar interior. Outside it (the window margins) the 3D
                // viewport shows through; its green axis-line is bright enough to wreck the autocrop bbox.
                let icons = detect_toolbar_icons(&cap, sc(10.0) as u32, sc(47.0) as u32, sc(100.0) as u32, cap_h.saturating_sub(sc(60.0) as u32));
                let mut new_here = 0;
                for (i, (yc, hh)) in icons.iter().enumerate() {
                    // Skip any row covered by a non-Blender window (e.g. a File Explorer sidebar
                    // overlapping the toolbar) — its glyphs would otherwise be harvested as "tools".
                    if !window_class_at(sc(28.0), *yc as i32).contains("GHOST") {
                        continue;
                    }
                    // Clamp the crop to this icon's half-slot (midpoints to the neighbours above/below)
                    // so the autocrop can't grab a sliver of an adjacent toolbar icon.
                    // Clamp to just past the neighbour's *actual edge* (its bottom / its top), not
                    // merely the midpoint, so a tall adjacent glyph can't leave a sliver in the crop.
                    let top_lim = if i > 0 {
                        let p = &icons[i - 1];
                        ((*yc as i64 + p.0 as i64) / 2 + 1).max(p.0 as i64 + p.1 as i64 / 2 + 2)
                    } else {
                        0
                    };
                    let bot_lim = if i + 1 < icons.len() {
                        let n = &icons[i + 1];
                        ((*yc as i64 + n.0 as i64) / 2 - 1).min(n.0 as i64 - n.1 as i64 / 2 - 2)
                    } else {
                        cap.height() as i64
                    };
                    let r_top = (*yc as i64 - *hh as i64 / 2 - sc(3.0) as i64).max(top_lim);
                    let r_bot = (*yc as i64 + *hh as i64 / 2 + sc(3.0) as i64).min(bot_lim);
                    let region = (sc(10.0) as i64, r_top, sc(37.0) as i64, (r_bot - r_top).max(1)); // toolbar interior (skip green margins)
                    let Some((ax, ay, aw, ah)) = autocrop_glyph(&cap, region, sc(2.0).max(2) as i64, 45) else {
                        continue;
                    };
                    let rgba = image::imageops::crop_imm(&cap, ax, ay, aw, ah).to_image();
                    let g = image::DynamicImage::ImageRgba8(rgba.clone()).to_luma8();
                    if seen_glyphs.iter().any(|s| ncc_eq(&g, s) > 0.9) {
                        continue; // already captured this glyph (a prior scroll) — don't re-harvest
                    }
                    let lines = harvest_tooltip(sc(28.0), *yc as i32);
                    let name = lines.first().cloned().unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let shortcut = lines
                        .iter()
                        .find(|l| l.contains("Shortcut"))
                        .and_then(|l| l.rsplit(',').next())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    seen_glyphs.push(g);
                    new_here += 1;
                    let slug = slugify(&name);
                    // Clean glyph (clamped, no neighbours) centred on a UNIFORM square, padded with
                    // the local toolbar background — every icon the same size, glyph-on-bg matching the
                    // live look. (The button fill is only subtly distinct from the bg, so this reads as
                    // the icon in its slot; the clamp keeps a neighbour from being baked in.)
                    let sq = sc(34.0) as u32;
                    let bgpx = *cap.get_pixel(sc(11.0) as u32, (ay + ah / 2).min(cap.height() - 1)); // toolbar bg, past the green margin
                    let mut canvas = image::RgbaImage::from_pixel(sq, sq, bgpx);
                    let ox = (sq.saturating_sub(aw) / 2) as i64;
                    let oy = (sq.saturating_sub(ah) / 2) as i64;
                    image::imageops::overlay(&mut canvas, &rgba, ox, oy);
                    let _ = canvas.save(out.join("icons").join(format!("{slug}.png")));
                    manifest.push((idx, ws.to_string(), name, shortcut, slug));
                    idx += 1;
                }
                eprintln!("[{ws}] scroll {_scroll}: {new_here} new (seen {})", seen_glyphs.len());
                if new_here == 0 {
                    empty += 1;
                    if empty >= 2 {
                        break;
                    }
                } else {
                    empty = 0;
                }
                scroll_at(sc(28.0), sc(300.0), -3); // scroll down
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
        if let Some((_, tx, ty)) = tabs.iter().find(|(t, _, _)| t.eq_ignore_ascii_case("Layout")) {
            click_at(*tx, *ty);
        }
        unsafe {
            let _ = SetCursorPos(orig.x, orig.y);
        }
        // Manifest (idx ↔ saved tooltip crop) + a first-pass pack from the OCR names.
        let man: String = manifest
            .iter()
            .map(|(i, ws, n, sc, slug)| format!("tip_{i:03}\t{ws}\t{n}\t[{sc}]\t{slug}.png"))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(out.join("manifest.tsv"), &man);
        eprintln!("\n=== {} unique tools across workspaces ===", manifest.len());
        for (i, ws, n, sc, _) in &manifest {
            eprintln!("  tip_{i:03} [{ws:<10}] {n:<24} [{sc}]");
        }
        eprintln!("manifest + icons -> {}", out.display());
    }

    // OFFLINE pack extraction from saved workspace captures: detect toolbar icon bands, clamp to
    // the half-gap, autocrop, pad to a uniform square — and NAME each crop by intensity-NCC
    // against a REFERENCE pack's icons (glyph designs are stable across Blender versions, so the
    // old pack names the new crops; cross-scale handled by resizing). No live app, no tooltips —
    // deterministic and re-runnable, unlike the hover-timing-sensitive live sweep (which produced
    // garbled OCR names at UI_SCALE=2). Unmatched crops save as unknown_N.png for visual naming.
    //   $env:INS="ws1.png;ws2.png"; $env:REF="…/packs/blender/icons"; $env:OUT="out";
    //   $env:UI_SCALE="2"; cargo test --lib extract_pack_offline -- --ignored --nocapture
    #[test]
    #[ignore]
    fn extract_pack_offline() {
        let us = ui_scale();
        let sc = |v: f32| (v * us).round() as i64;
        let out = std::path::PathBuf::from(std::env::var("OUT").expect("set OUT"));
        let _ = std::fs::create_dir_all(out.join("icons"));
        let mut seen: Vec<image::GrayImage> = Vec::new();
        let mut unknown = 0usize;
        for cap_path in std::env::var("INS").expect("set INS").split(';').filter(|s| !s.is_empty()) {
            let cap = image::open(cap_path).expect("capture").to_rgba8();
            let cap_h = cap.height();
            let icons = detect_toolbar_icons(
                &cap,
                sc(10.0) as u32,
                sc(47.0) as u32,
                sc(100.0) as u32,
                // Bottom bound is SCREEN-anchored: the timeline row sits right below the toolbar
                // on a fixed-height screen, so scaling 880 linearly would reach into it at 2×.
                cap_h.saturating_sub(sc(60.0) as u32),
            );
            eprintln!("{} → {} bands {:?}", cap_path, icons.len(), icons);
            for (i, (yc, hh)) in icons.iter().enumerate() {
                let top_lim = if i > 0 {
                    let p = &icons[i - 1];
                    ((*yc as i64 + p.0 as i64) / 2 + 1).max(p.0 as i64 + p.1 as i64 / 2 + 2)
                } else {
                    0
                };
                let bot_lim = if i + 1 < icons.len() {
                    let n = &icons[i + 1];
                    ((*yc as i64 + n.0 as i64) / 2 - 1).min(n.0 as i64 - n.1 as i64 / 2 - 2)
                } else {
                    cap_h as i64
                };
                let r_top = (*yc as i64 - *hh as i64 / 2 - sc(3.0)).max(top_lim);
                let r_bot = (*yc as i64 + *hh as i64 / 2 + sc(3.0)).min(bot_lim);
                let region = (sc(10.0), r_top, sc(37.0), (r_bot - r_top).max(1));
                let Some((ax, ay, aw, ah)) = autocrop_glyph(&cap, region, sc(2.0).max(2), 45) else {
                    continue;
                };
                let rgba = image::imageops::crop_imm(&cap, ax, ay, aw, ah).to_image();
                let g = image::DynamicImage::ImageRgba8(rgba.clone()).to_luma8();
                if seen.iter().any(|s| ncc_eq(&g, s) > 0.9) {
                    continue;
                }
                seen.push(g);
                // Pad to the uniform square first (same framing as the reference icons), then name.
                let sq = sc(34.0) as u32;
                let bgpx = *cap.get_pixel(sc(11.0) as u32, (ay + ah / 2).min(cap_h - 1));
                let mut canvas = image::RgbaImage::from_pixel(sq, sq, bgpx);
                image::imageops::overlay(&mut canvas, &rgba, (sq.saturating_sub(aw) / 2) as i64, (sq.saturating_sub(ah) / 2) as i64);
                unknown += 1;
                let fname = format!("crop_{unknown:03}.png");
                eprintln!("  y={yc:<4} {aw}x{ah} → {fname}");
                let _ = canvas.save(out.join("icons").join(fname));
            }
        }
        eprintln!("{} unique crops → {}", seen.len(), out.join("icons").display());
    }

    // Name a directory of icon crops by GREEDY-UNIQUE intensity-NCC against a reference pack:
    // score every (crop, ref) pair, assign the best pair first, remove both, repeat — so two
    // look-alike crops (rotate vs transform, both circular) can't claim the same name; the true
    // best keeps it and the runner-up falls to its next candidate. Crops from MULTIPLE input dirs
    // are cross-deduped (ncc > 0.9). Below MIN (default 0.5) → unknown_N.png for visual naming.
    //   $env:INS="dirA;dirB"; $env:REF="…/packs/blender/icons"; $env:OUT="named";
    //   cargo test --lib rename_crops_by_ref -- --ignored --nocapture
    #[test]
    #[ignore]
    fn rename_crops_by_ref() {
        let out = std::path::PathBuf::from(std::env::var("OUT").expect("set OUT"));
        let _ = std::fs::create_dir_all(&out);
        let min: f32 = std::env::var("MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5);
        let refs: Vec<(String, image::GrayImage)> = std::fs::read_dir(std::env::var("REF").expect("set REF"))
            .expect("REF dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "png"))
            .map(|p| (p.file_stem().unwrap().to_string_lossy().to_string(), image::open(&p).expect("ref").to_luma8()))
            .collect();
        // Load + cross-dedup the crops (multiple sources may hold the same glyph).
        let mut crops: Vec<(String, image::RgbaImage, image::GrayImage)> = Vec::new();
        for dir in std::env::var("INS").expect("set INS").split(';').filter(|s| !s.is_empty()) {
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .expect("INS dir")
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "png"))
                .collect();
            paths.sort();
            for p in paths {
                let rgba = image::open(&p).expect("crop").to_rgba8();
                let g = image::DynamicImage::ImageRgba8(rgba.clone()).to_luma8();
                if crops.iter().any(|(_, _, e)| ncc_eq(&g, e) > 0.9) {
                    continue;
                }
                crops.push((p.file_name().unwrap().to_string_lossy().to_string(), rgba, g));
            }
        }
        // Score matrix → greedy unique assignment.
        let mut pairs: Vec<(usize, usize, f32)> = Vec::new();
        for (ci, (_, _, g)) in crops.iter().enumerate() {
            for (ri, (_, r)) in refs.iter().enumerate() {
                pairs.push((ci, ri, ncc_eq(r, g)));
            }
        }
        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let mut crop_taken = vec![false; crops.len()];
        let mut ref_taken = vec![false; refs.len()];
        let mut named: Vec<(usize, String, f32)> = Vec::new();
        for (ci, ri, v) in pairs {
            if v < min || crop_taken[ci] || ref_taken[ri] {
                continue;
            }
            crop_taken[ci] = true;
            ref_taken[ri] = true;
            named.push((ci, refs[ri].0.clone(), v));
        }
        let mut unknown = 0usize;
        for (ci, (src, rgba, _)) in crops.iter().enumerate() {
            let (name, score) = named
                .iter()
                .find(|(i, _, _)| *i == ci)
                .map(|(_, n, v)| (n.clone(), *v))
                .unwrap_or_else(|| {
                    unknown += 1;
                    (format!("unknown_{unknown}"), -1.0)
                });
            eprintln!("  {src:<28} → {name:<24} ({score:.3})");
            let _ = rgba.save(out.join(format!("{name}.png")));
        }
        let missing: Vec<&str> = refs
            .iter()
            .enumerate()
            .filter(|(ri, _)| !ref_taken[*ri])
            .map(|(_, (n, _))| n.as_str())
            .collect();
        eprintln!("
{} crops named, {} unknown; reference names NOT matched ({}): {:?}", named.len(), unknown, missing.len(), missing);
    }

    // Generic region sweep (for the top header + right Properties tabs, not the left toolbar):
    // detect an icon row/column in REGION, hover each for its tooltip name, autocrop + pad to a
    // uniform SIZE square. ORIENT=v (vertical column, default) or h (horizontal row).
    //   $env:OUT="c:/Users/fujin/blender_right"; $env:REGION="1600,392,1624,720"; $env:ORIENT="v";
    //   $env:SIZE="26"; cargo test --lib capture_region_icons -- --ignored --nocapture
    #[test]
    #[ignore]
    fn capture_region_icons() {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        let out = std::path::PathBuf::from(std::env::var("OUT").expect("set OUT"));
        let _ = std::fs::create_dir_all(out.join("icons"));
        let p: Vec<i32> = std::env::var("REGION")
            .expect("set REGION=x0,y0,x1,y1")
            .split(',')
            .map(|s| s.trim().parse().expect("REGION ints"))
            .collect();
        let (x0, y0, x1, y1) = (p[0], p[1], p[2], p[3]);
        let vert = std::env::var("ORIENT").map(|s| s != "h").unwrap_or(true);
        let size: u32 = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(34);
        // Semi-manual override for dense/mixed regions: POSITIONS = comma-separated centres along the
        // sweep axis (skip detection); NAMES = comma-separated names in order (skip the tooltip hover).
        let positions: Vec<i32> = std::env::var("POSITIONS")
            .ok()
            .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_default();
        let names: Vec<String> = std::env::var("NAMES")
            .ok()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default();
        unsafe {
            let _ = SetCursorPos(960, 540);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rect = crate::capture::Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
        let Ok(cap) = crate::capture::capture_region_raw(rect, &[]) else {
            eprintln!("capture failed");
            return;
        };
        let (rw, rh) = (cap.width(), cap.height());
        let icons: Vec<(u32, u32)> = if positions.is_empty() {
            detect_region_icons(&cap, vert)
        } else {
            positions.iter().map(|&p| (p as u32, 18u32)).collect()
        };
        eprintln!("detected {} icons ({}): {:?}", icons.len(), if vert { "v" } else { "h" }, icons);
        let mut manifest: Vec<(usize, String, String, String)> = Vec::new();
        let mut idx = 0usize;
        for (i, (c, sz)) in icons.iter().enumerate() {
            let name = if names.is_empty() {
                let (sx, sy) = if vert {
                    (x0 + rw as i32 / 2, y0 + *c as i32)
                } else {
                    (x0 + *c as i32, y0 + rh as i32 / 2)
                };
                harvest_tooltip(sx, sy).first().cloned().unwrap_or_default()
            } else {
                names.get(i).cloned().unwrap_or_default()
            };
            if name.is_empty() {
                continue;
            }
            // Autocrop the glyph from the resting capture (taken before any hover), clamped to the
            // half-gap to the neighbours so touching buttons/tabs can't leak a sliver into the crop.
            let lo = if i > 0 { (icons[i - 1].0 + *c) / 2 + 1 } else { 0 };
            let hi = if i + 1 < icons.len() {
                (*c + icons[i + 1].0) / 2 - 1
            } else if vert {
                rh
            } else {
                rw
            };
            let a = (*c as i64 - *sz as i64 / 2 - 2).max(lo as i64);
            let b = (*c as i64 + *sz as i64 / 2 + 2).min(hi as i64);
            let region = if vert {
                (2i64, a, rw as i64 - 4, (b - a).max(1))
            } else {
                (a, 2i64, (b - a).max(1), rh as i64 - 4)
            };
            // On an autocrop miss (e.g. a glyph that fills its button, defeating the median-bg
            // estimate), fall back to the clamped region itself.
            let (ax, ay, aw, ah) = autocrop_glyph(&cap, region, 2, 40).unwrap_or((
                region.0.max(0) as u32,
                region.1.max(0) as u32,
                region.2.max(1) as u32,
                region.3.max(1) as u32,
            ));
            let rgba = image::imageops::crop_imm(&cap, ax, ay, aw, ah).to_image();
            let bgpx = *cap.get_pixel(1.min(rw - 1), (ay + ah / 2).min(rh - 1));
            let mut canvas = image::RgbaImage::from_pixel(size, size, bgpx);
            let ox = (size.saturating_sub(aw) / 2) as i64;
            let oy = (size.saturating_sub(ah) / 2) as i64;
            image::imageops::overlay(&mut canvas, &rgba, ox, oy);
            let slug = slugify(&name);
            let _ = canvas.save(out.join("icons").join(format!("{slug}.png")));
            eprintln!("  {name:<28} {aw}x{ah}");
            manifest.push((idx, name, String::new(), slug));
            idx += 1;
        }
        let m: String = manifest
            .iter()
            .map(|(i, n, s, sl)| format!("tip_{i:03}\tRegion\t{n}\t[{s}]\t{sl}.png"))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(out.join("manifest.tsv"), m);
        eprintln!("=== {} icons saved -> {} ===", manifest.len(), out.display());
    }

    // Re-crop saved icons in place to drop Blender's tool-group corner BADGE — a small bright blob
    // in the bottom-right, gapped from the glyph, that inflated the crop. Connected-components on
    // the bright mask; drop a small (<25% of the largest) component whose centre is bottom-right;
    // re-bbox the rest + 1px pad. Idempotent for icons without a badge.
    //   $env:DIR="c:/Users/fujin/blender_pack/icons"; cargo test --lib recrop_icons -- --ignored --nocapture
    #[test]
    #[ignore]
    fn recrop_icons() {
        let dir = std::env::var("DIR").unwrap();
        for p in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())) {
            if p.extension().is_none_or(|x| x != "png") {
                continue;
            }
            let img = image::open(&p).unwrap().to_rgba8();
            let (w, h) = img.dimensions();
            let gray = image::DynamicImage::ImageRgba8(img.clone()).to_luma8();
            let mut lumas: Vec<u8> = gray.pixels().map(|px| px.0[0]).collect();
            lumas.sort_unstable();
            let thr = lumas[lumas.len() / 2] as u16 + 30; // median bg + delta
            let bright: Vec<bool> = gray.pixels().map(|px| px.0[0] as u16 > thr).collect();
            // Flood-fill 8-connected components → (area, x0,y0,x1,y1).
            let mut label = vec![0u32; (w * h) as usize];
            let mut comps: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
            for s in 0..(w * h) as usize {
                if !bright[s] || label[s] != 0 {
                    continue;
                }
                let id = comps.len() as u32 + 1;
                let (mut a, mut x0, mut y0, mut x1, mut y1) = (0u32, w, h, 0u32, 0u32);
                let mut stack = vec![s];
                label[s] = id;
                while let Some(i) = stack.pop() {
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    a += 1;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                                continue;
                            }
                            let ni = (ny as u32 * w + nx as u32) as usize;
                            if bright[ni] && label[ni] == 0 {
                                label[ni] = id;
                                stack.push(ni);
                            }
                        }
                    }
                }
                comps.push((a, x0, y0, x1, y1));
            }
            if comps.is_empty() {
                continue;
            }
            let max_a = comps.iter().map(|c| c.0).max().unwrap();
            let keep: Vec<&(u32, u32, u32, u32, u32)> = comps
                .iter()
                .filter(|(a, x0, y0, x1, y1)| {
                    let small = (*a as f32) < 0.25 * max_a as f32;
                    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
                    let bottom_right = cx > w / 2 && cy > h / 2;
                    !(small && bottom_right) // drop the badge
                })
                .collect();
            let (mut bx0, mut by0, mut bx1, mut by1) = (w, h, 0u32, 0u32);
            for (_, x0, y0, x1, y1) in &keep {
                bx0 = bx0.min(*x0);
                by0 = by0.min(*y0);
                bx1 = bx1.max(*x1);
                by1 = by1.max(*y1);
            }
            let (nx0, ny0) = (bx0.saturating_sub(1), by0.saturating_sub(1));
            let (nx1, ny1) = ((bx1 + 2).min(w), (by1 + 2).min(h));
            if nx1 <= nx0 || ny1 <= ny0 {
                continue;
            }
            let (nw, nh) = (nx1 - nx0, ny1 - ny0);
            if nw == w && nh == h {
                continue; // no change
            }
            let crop = image::imageops::crop_imm(&img, nx0, ny0, nw, nh).to_image();
            let _ = crop.save(&p);
            eprintln!("{:<28} {w}x{h} -> {nw}x{nh}", p.file_name().unwrap().to_string_lossy());
        }
    }

    // Print icon dimensions (smallest→largest) to spot oversized crops (a corner badge + gap).
    //   $env:DIR="c:/Users/fujin/blender_pack/icons"; cargo test --lib icon_sizes -- --ignored --nocapture
    #[test]
    #[ignore]
    fn icon_sizes() {
        let dir = std::env::var("DIR").unwrap();
        let mut v: Vec<(String, u32, u32)> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "png"))
            .map(|p| {
                let (w, h) = image::image_dimensions(&p).unwrap_or((0, 0));
                (p.file_name().unwrap().to_string_lossy().into_owned(), w, h)
            })
            .collect();
        v.sort_by_key(|(_, w, h)| w * h);
        for (n, w, h) in &v {
            eprintln!("{w:>3} x {h:<3}  ({:>4}px)  {n}", w * h);
        }
    }

    // Assemble the final pack.json from a captured manifest.tsv — clean names/shortcuts, dedup by
    // slug, drop garbage rows, rename known OCR errors. Decoupled from capture so it's re-runnable.
    //   $env:OUT="c:/Users/fujin/blender_pack"; cargo test --lib emit_pack_from_manifest -- --ignored --nocapture
    #[test]
    #[ignore]
    fn emit_pack_from_manifest() {
        use std::collections::HashSet;
        let out = std::path::PathBuf::from(std::env::var("OUT").unwrap_or_else(|_| "c:/Users/fujin/blender_pack".into()));
        let man = std::fs::read_to_string(out.join("manifest.tsv")).expect("manifest.tsv");
        // Non-tool captures to drop: viewport orientation label ("User Perspective"/"User Pers"),
        // OCR fragments. (Foreign-window contamination — e.g. a File Explorer sidebar overlapping the
        // toolbar — is prevented at capture time by the GHOST_WindowClass guard in the sweep.)
        let drop_names = ["result.", "User Perspective", "User Pers"];
        let mut seen: HashSet<String> = HashSet::new();
        let mut tools: Vec<(String, String)> = Vec::new(); // (name, shortcut) — slug = slugify(name)
        for line in man.lines() {
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() < 5 {
                continue;
            }
            let mut name = c[2].to_string();
            // Known OCR-error fix (verified by reading the crop: a Sculpt tool, shortcut G = Grab).
            if name == "erab" {
                name = "Grab".into();
                let _ = std::fs::rename(out.join("icons/erab.png"), out.join("icons/grab.png"));
            }
            // Strip non-ASCII noise (e.g. the • in "Multi•plane Scrape").
            name = name.chars().filter(|ch| ch.is_ascii_graphic() || *ch == ' ').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
            if name.is_empty() || drop_names.contains(&name.as_str()) {
                let _ = std::fs::remove_file(out.join("icons").join(c[4].trim()));
                continue;
            }
            let sc = c[3]
                .trim_matches(|ch| ch == '[' || ch == ']')
                .trim()
                .trim_start_matches("Shortcut:")
                .trim()
                .to_string();
            let slug = slugify(&name);
            if seen.contains(&slug) {
                continue;
            }
            seen.insert(slug);
            tools.push((name, sc));
        }
        let shortcuts = tools
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(n, s)| format!("    {:?}: {:?}", n, s))
            .collect::<Vec<_>>()
            .join(",\n");
        let hints = tools
            .iter()
            .map(|(n, _)| format!("    {{ \"name\": {:?}, \"region\": \"left\", \"role\": \"button\" }}", n))
            .collect::<Vec<_>>()
            .join(",\n");
        let pack = format!(
            "{{\n  \"id\": \"blender\",\n  \"name\": \"Blender Nav-Pack (auto-generated)\",\n  \"version\": \"1.0.0\",\n  \"min_app_version\": \"0.6.0\",\n  \"target_app\": \"Blender\",\n  \"window_title_pattern\": \"(?i)blender\",\n  \"system_prompt_injection\": \"The user is in Blender. The left Toolbar holds the active tools (Object/Edit/Sculpt sets differ by workspace tab). Prefer the keyboard shortcut when one exists.\",\n  \"shortcuts\": {{\n{shortcuts}\n  }},\n  \"element_hints\": [\n{hints}\n  ]\n}}\n"
        );
        std::fs::write(out.join("pack.json"), &pack).expect("write pack.json");
        eprintln!("emitted pack.json with {} tools -> {}", tools.len(), out.display());
        for (n, s) in &tools {
            eprintln!("  {n:<28} {s}");
        }
    }

    #[test]
    fn load_gray_from_png_bytes_roundtrips() {
        let mut rgba = RgbaImage::new(8, 8);
        for p in rgba.pixels_mut() {
            *p = image::Rgba([120, 120, 120, 255]);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let gray = load_gray_from_bytes(png.get_ref()).unwrap();
        assert_eq!(gray.dimensions(), (8, 8));
    }
}
