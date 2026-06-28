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

/// Slide `template` over `haystack` at each scale in `scales`, returning the single best match
/// whose NCC score ≥ `min_score`, or `None`. Scales that would make the template ≥ the
/// haystack (or smaller than 4 px) are skipped — `imageproc::match_template` panics if the
/// template isn't strictly smaller, so the guard is load-bearing, not just an optimization.
pub fn match_icon(
    haystack: &GrayImage,
    template: &GrayImage,
    scales: &[f32],
    min_score: f32,
) -> Option<TemplateMatch> {
    let (hw, hh) = haystack.dimensions();
    let mut best: Option<TemplateMatch> = None;
    for &s in scales {
        let tw = ((template.width() as f32) * s).round() as u32;
        let th = ((template.height() as f32) * s).round() as u32;
        if tw < 4 || th < 4 || tw >= hw || th >= hh {
            continue;
        }
        let scaled;
        let needle: &GrayImage = if (s - 1.0).abs() < f32::EPSILON {
            template
        } else {
            scaled = image::imageops::resize(template, tw, th, FilterType::Lanczos3);
            &scaled
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

/// For the best-scoring scale, return (scaled template w, h, the NCC result map). Used by the
/// pyramid coarse pass so we can extract multiple peaks, not just the single global best.
fn best_scale_map(haystack: &GrayImage, template: &GrayImage, scales: &[f32]) -> Option<(u32, u32, NccMap)> {
    let (hw, hh) = haystack.dimensions();
    let mut best: Option<(f32, u32, u32, NccMap)> = None;
    for &s in scales {
        let tw = ((template.width() as f32) * s).round() as u32;
        let th = ((template.height() as f32) * s).round() as u32;
        if tw < 4 || th < 4 || tw >= hw || th >= hh {
            continue;
        }
        let scaled = if (s - 1.0).abs() < f32::EPSILON {
            template.clone()
        } else {
            image::imageops::resize(template, tw, th, FilterType::Lanczos3)
        };
        let map = match_template_parallel(haystack, &scaled, MatchTemplateMethod::CrossCorrelationNormalized);
        let max = find_extremes(&map).max_value;
        if best.as_ref().map(|(b, ..)| max > *b).unwrap_or(true) {
            best = Some((max, tw, th, map));
        }
    }
    best.map(|(_, tw, th, map)| (tw, th, map))
}

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
pub fn match_icon_pyramid(haystack: &GrayImage, template: &GrayImage, min_score: f32) -> Vec<TemplateMatch> {
    let (hw, hh) = haystack.dimensions();
    let (tw, th) = template.dimensions();
    let cf = (COARSE_ICON_PX / tw.max(th) as f32).clamp(0.1, 1.0);
    // Template already small → a direct full match (pyramid wouldn't save anything).
    if cf >= 0.9 {
        return match_icon(haystack, template, DEFAULT_SCALES, min_score)
            .into_iter()
            .collect();
    }
    let (chw, chh) = (((hw as f32) * cf) as u32, ((hh as f32) * cf) as u32);
    let (ctw, cth) = (
        ((tw as f32) * cf).round().max(4.0) as u32,
        ((th as f32) * cf).round().max(4.0) as u32,
    );
    if ctw >= chw || cth >= chh {
        return match_icon(haystack, template, DEFAULT_SCALES, min_score)
            .into_iter()
            .collect();
    }
    let chay = image::imageops::resize(haystack, chw, chh, FilterType::Lanczos3);
    let ctmpl = image::imageops::resize(template, ctw, cth, FilterType::Lanczos3);
    let Some((cmw, cmh, cmap)) = best_scale_map(&chay, &ctmpl, COARSE_SCALES) else {
        return Vec::new();
    };
    let mw = (tw as f32 * 1.5 + 24.0) as i32;
    let mh = (th as f32 * 1.5 + 24.0) as i32;
    let mut out: Vec<TemplateMatch> = Vec::new();
    for (px, py, _cv) in topk_peaks(&cmap, cmw, cmh, MAX_PEAKS) {
        // coarse peak centre → native centre.
        let nx = (px as f32 + cmw as f32 / 2.0) / cf;
        let ny = (py as f32 + cmh as f32 / 2.0) / cf;
        let x0 = (nx as i32 - mw).max(0);
        let y0 = (ny as i32 - mh).max(0);
        let x1 = (nx as i32 + mw).min(hw as i32);
        let y1 = (ny as i32 + mh).min(hh as i32);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let fwin = image::imageops::crop_imm(haystack, x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
            .to_image();
        if let Some(fine) = match_icon(&fwin, template, DEFAULT_SCALES, min_score) {
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
        let m = match_icon(&hay, &tmpl, &[1.0], 0.99).expect("exact crop should match");
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
        assert!(match_icon(&hay, &tmpl, &[1.0], 1.01).is_none());
        // Sanity: the same crop is accepted under a sane floor (mirrors the exact-match test).
        assert!(match_icon(&hay, &tmpl, &[1.0], DEFAULT_MIN_SCORE).is_some());
    }

    #[test]
    fn finds_scaled_crop_via_scale_sweep() {
        let hay = structured(200, 160);
        let tmpl = crop(&hay, 50, 40, 48, 40);
        // Author the template at 1.25× so only the 0.8 scale brings it back to native size.
        let bigger = image::imageops::resize(&tmpl, 60, 50, FilterType::Lanczos3);
        let m = match_icon(&hay, &bigger, DEFAULT_SCALES, 0.9).expect("scaled crop should match");
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
        let hits = match_icon_pyramid(&hay, &tmpl, DEFAULT_MIN_SCORE);
        assert!(!hits.is_empty(), "pyramid should find the crop");
        let m = hits[0];
        assert!((m.x - 50).abs() <= 4 && (m.y - 40).abs() <= 4, "near true location: {m:?}");
        assert!(m.score >= DEFAULT_MIN_SCORE, "fine score should clear threshold: {}", m.score);
    }

    #[test]
    fn oversized_template_is_skipped_not_panic() {
        let hay = structured(60, 60);
        let tmpl = structured(100, 100); // larger than haystack on every scale
        // Must not panic (match_template panics on a non-smaller template) — just no match.
        assert!(match_icon(&hay, &tmpl, DEFAULT_SCALES, 0.5).is_none());
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
        // EDGES=1 → Sobel-edge preprocess both (the theme-robust production path that
        // `try_template_pass` uses); else raw intensity.
        if std::env::var("EDGES").is_ok() {
            hay = to_edges(&hay);
            tmpl = to_edges(&tmpl);
            eprintln!("EDGES: matching on Sobel gradient magnitude");
        }
        eprintln!("haystack {}x{}, template {}x{}, {} scales", hay.width(), hay.height(), tmpl.width(), tmpl.height(), DEFAULT_SCALES.len());
        // PYRAMID=1 → full-screen coarse-to-fine top-K (the production path); else single full match.
        if std::env::var("PYRAMID").is_ok() {
            let t = std::time::Instant::now();
            let hits = match_icon_pyramid(&hay, &tmpl, -1.0);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("pyramid full-screen: {} match(es) in {:.1} ms", hits.len(), ms);
            for m in &hits {
                eprintln!("  pos=({},{}) {}x{} score={:.4} scale={} accepted={}", m.x, m.y, m.width, m.height, m.score, m.scale, m.score >= DEFAULT_MIN_SCORE);
            }
            return;
        }
        let t = std::time::Instant::now();
        let res = match_icon(&hay, &tmpl, DEFAULT_SCALES, -1.0);
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
            match_icon(h, t, DEFAULT_SCALES, -1.0)
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
            let m_edge = match_icon(&hay_edge, &to_edges(&tmpl), DEFAULT_SCALES, -1.0);
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
        let rect = crate::capture::Rect { x: cx + 8, y: cy - 20, width: 480, height: 160 };
        let Ok(raw) = crate::capture::capture_region_raw(rect, &[]) else {
            return Vec::new();
        };
        // Upscale 6×: tooltip text is small + dim. 3× reads the brighter Object-mode tips but
        // loses the name/first line of denser Edit/Sculpt tooltips (Loop Cut's "Loop Cut" only
        // appears ≥~6×). `TIP_DIR=…` saves each crop for inspection (NOT `OUT_DIR` — cargo reserves
        // that for build scripts, so it gets clobbered to the build `out/` dir).
        let up = image::imageops::resize(&raw, raw.width() * 6, raw.height() * 6, FilterType::Lanczos3);
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
        let thr = (maxr / 6).max(1); // a row is "content" above ~1/6 of the busiest row
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
                if gap > 2 {
                    // bridge ≤2 px dips inside a glyph; a bigger gap ends the icon (keeps adjacent
                    // icons — esp. the active highlighted tool + its neighbour — from merging)
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
                (12..=44).contains(&hh) // an icon glyph, not noise or a merged run
            })
            .map(|(a, b)| (y0 + (a + b) / 2, b - a))
            .collect()
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

    // OCR the workspace-tab strip (top of the window) → each tab's text + screen x-centre, so the
    // sweep can click them. Upscaled 3× (small tab font).
    fn find_workspace_tabs() -> Vec<(String, i32)> {
        let region = crate::capture::Rect { x: 0, y: 26, width: 1180, height: 22 };
        let up = 4i32; // the inactive tabs are dim grey — 4× reads them, 3× misses
        crate::capture::capture_region_raw(region, &[])
            .ok()
            .map(|raw| image::imageops::resize(&raw, raw.width() * up as u32, raw.height() * up as u32, FilterType::Lanczos3))
            .and_then(|u| crate::capture::encode_png_for_ocr(&u).ok())
            .and_then(|png| crate::locator::ocr::run_ocr(&png).ok())
            .unwrap_or_default()
            .iter()
            .filter(|r| r.confidence >= 1.0) // each tab is its own line-level result; x-centre below
            .map(|r| (r.text.clone(), region.x + (r.bbox.0 + r.bbox.2 as i32 / 2) / up))
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
            tabs.iter().map(|(t, x)| format!("{t}@{x}")).collect::<Vec<_>>().join("  ")
        );
        for want in ["Layout", "Modeling", "Sculpting"] {
            let Some((_, tx)) = tabs.iter().find(|(t, _)| t.eq_ignore_ascii_case(want)) else {
                eprintln!("[{want}] tab not found — skipped");
                continue;
            };
            click_at(*tx, 37);
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
        if let Some((_, tx)) = tabs.iter().find(|(t, _)| t.eq_ignore_ascii_case("Layout")) {
            click_at(*tx, 37); // restore to Layout
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
        let tabs = find_workspace_tabs();
        // Fail fast if we're not actually looking at Blender. The sweep reads FIXED primary-monitor
        // regions, so Blender must be maximized + foreground there; otherwise another window (even one
        // whose title contains "blender", e.g. a File Explorer folder) is captured → 0 tools, silently.
        if !tabs.iter().any(|(t, _)| ["Layout", "Modeling", "Sculpting"].iter().any(|e| t.eq_ignore_ascii_case(e))) {
            eprintln!(
                "ABORT: no Blender workspace tabs found (read: {:?}). Maximize Blender on the PRIMARY \
                 monitor (toolbar at the left edge, Layout/Modeling/Sculpting tabs along the top) and re-run.",
                tabs.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
            );
            return;
        }
        // (idx, workspace, ocr_name, shortcut, slug) — idx matches the TIP_DIR crop order.
        let mut manifest: Vec<(usize, String, String, String, String)> = Vec::new();
        let mut idx = 0usize;
        for ws in ["Layout", "Modeling", "Sculpting"] {
            let Some((_, tx)) = tabs.iter().find(|(t, _)| t.eq_ignore_ascii_case(ws)) else {
                continue;
            };
            click_at(*tx, 37);
            std::thread::sleep(std::time::Duration::from_millis(700));
            scroll_at(28, 300, 10); // toolbar → top
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut seen_glyphs: Vec<image::GrayImage> = Vec::new(); // per-workspace dedup
            let mut empty = 0;
            for _scroll in 0..14 {
                unsafe {
                    let _ = SetCursorPos(960, 540); // park off the toolbar for a clean capture
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
                let Ok(cap) = crate::capture::capture_region_raw(crate::capture::Rect { x: 0, y: 0, width: 64, height: 900 }, &[]) else {
                    break;
                };
                let icons = detect_toolbar_icons(&cap, 4, 44, 100, 880);
                let mut new_here = 0;
                for (yc, hh) in &icons {
                    let region = (4i64, *yc as i64 - *hh as i64 / 2 - 4, 40i64, *hh as i64 + 8);
                    let Some((ax, ay, aw, ah)) = autocrop_glyph(&cap, region, 2, 45) else {
                        continue;
                    };
                    let rgba = image::imageops::crop_imm(&cap, ax, ay, aw, ah).to_image();
                    let g = image::DynamicImage::ImageRgba8(rgba.clone()).to_luma8();
                    if seen_glyphs.iter().any(|s| ncc_eq(&g, s) > 0.9) {
                        continue; // already captured this glyph (a prior scroll) — don't re-harvest
                    }
                    let lines = harvest_tooltip(28, *yc as i32);
                    let name = lines.first().cloned().unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let sc = lines
                        .iter()
                        .find(|l| l.contains("Shortcut"))
                        .and_then(|l| l.rsplit(',').next())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    seen_glyphs.push(g);
                    new_here += 1;
                    let slug = slugify(&name);
                    // Tight glyph crop (icon-only) — the bright-pixel bbox, no neighbours, no cell
                    // padding. Clean + robust to toolbar rearrangement; the matcher is scale-invariant
                    // so the slightly-varying size is fine. (A uniform 39px cell baked in neighbours.)
                    let _ = rgba.save(out.join("icons").join(format!("{slug}.png")));
                    manifest.push((idx, ws.to_string(), name, sc, slug));
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
                scroll_at(28, 300, -3); // scroll down
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
        if let Some((_, tx)) = tabs.iter().find(|(t, _)| t.eq_ignore_ascii_case("Layout")) {
            click_at(*tx, 37);
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
        let drop_names = ["result.", "User Perspective"];
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
