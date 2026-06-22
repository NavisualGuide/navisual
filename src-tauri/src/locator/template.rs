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
use imageproc::template_matching::{find_extremes, match_template, MatchTemplateMethod};

/// DPI-derived scale sweep applied to the template before matching. Covers the common ratios
/// between a pack crop's authored DPI and the live screen's scaling (100/125/150/175/200 %),
/// in both directions. Kept small — each scale is a full correlation pass.
pub const DEFAULT_SCALES: &[f32] = &[1.0, 1.25, 1.5, 0.8, 2.0, 0.67];

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
        let result = match_template(
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

/// Decode image bytes (PNG/JPEG) to grayscale — used for both the captured haystack and a
/// pack's icon-crop file.
pub fn load_gray_from_bytes(bytes: &[u8]) -> Result<GrayImage> {
    Ok(image::load_from_memory(bytes)
        .context("decode image for template matching")?
        .to_luma8())
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
    fn oversized_template_is_skipped_not_panic() {
        let hay = structured(60, 60);
        let tmpl = structured(100, 100); // larger than haystack on every scale
        // Must not panic (match_template panics on a non-smaller template) — just no match.
        assert!(match_icon(&hay, &tmpl, DEFAULT_SCALES, 0.5).is_none());
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
