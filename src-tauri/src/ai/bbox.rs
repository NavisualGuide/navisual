//! AI-returned bounding-box coordinate-system conversion.
//!
//! All providers are instructed to return spatial coordinates as normalized
//! 0–1000 in [ymin, xmin, ymax, xmax] (Gemini's native object-detection format).
//! Normalized coordinates are resolution-independent, so the AI-image downscale
//! factor cannot corrupt them — absolute pixels proved unreliable for
//! non-grounding models (GPT/Claude/Qwen reported coordinates in inconsistent
//! scales, often exceeding the downscaled image size).
//!
//! `ai_bbox_to_screen_rect` takes whatever the AI returned and the active
//! provider name, plus the actual AI-image dimensions and the screen rect the
//! image represents, and produces a [`VdRect`] in virtual-desktop physical
//! pixels that the overlay can draw directly. The conversion itself is the
//! typed-space pipeline in [`crate::ai::spaces`] (NormBox → AiRect → VdRect);
//! this module adds the provider-format dispatch and the whole-frame policy.

use crate::ai::spaces::{AiRect, NormBox, VdRect};
use crate::capture::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BboxFormat {
    /// Coordinates are normalized to 0–1000 (Gemini's native object-detection scale).
    Normalized1000,
    /// Absolute pixels of the AI-image (post `cap_size` downscale). Reserved: no
    /// provider uses this now (all normalized — see `bbox_format_for_provider`);
    /// kept so a single provider can be reverted to the pixel contract.
    #[allow(dead_code)]
    Pixel,
}

/// All providers are instructed to return normalized 0–1000 coordinates
/// (Gemini's convention) — resolution-independent, so the downscale factor
/// cannot corrupt them. Absolute pixels proved unreliable for non-grounding
/// models. To revert one provider to the pixel contract, match it here and
/// return `BboxFormat::Pixel` (and change its prompt text back to pixels).
pub fn bbox_format_for_provider(_provider: &str) -> BboxFormat {
    BboxFormat::Normalized1000
}

/// Whether this model's `target_bbox` is trusted to *corroborate* (rescue) a borderline
/// OCR match in the locator's corroboration gate.
///
/// **Trust is default-ON.** A model qualifies UNLESS its name contains a substring from
/// `distrust_csv` (comma-separated, case-insensitive). This is a denylist, not an
/// allowlist, precisely so model churn doesn't require a code change: frontier models
/// (Gemini 3+, GPT-5.x, Claude Opus, Qwen-omni — and whatever ships next) are trusted
/// without edits, and a model that proves bad at grounding is muted by adding it to
/// `BBOX_DISTRUST_MODELS` in `.env`. The default list is the managed free-tier chain
/// (Nemotron / Gemma / Kimi), which emit inconsistent or degenerate bboxes
/// (model-comparison.md). An empty `distrust_csv` trusts every model.
pub fn bbox_is_decisive(model: &str, distrust_csv: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !distrust_csv
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .any(|bad| m.contains(&bad))
}

/// Convert an AI-returned `[ymin, xmin, ymax, xmax]` to a screen rect in
/// VIRTUAL-DESKTOP PHYSICAL pixels ([`VdRect`] — unwrap at the consuming
/// boundary only).
///
/// The space hops are the typed pipeline in [`crate::ai::spaces`]:
/// `NormBox::from_raw` (validation + 0–1 auto-detect) → `to_ai_rect` (AI-image
/// pixels, clamped) → `to_virtual_desktop` (un-downscale + capture origin).
/// This function adds what ISN'T space conversion: provider-format dispatch
/// (the reserved `Pixel` contract enters the pipeline at [`AiRect`]) and the
/// whole-frame rejection policy.
///
/// `capture_rect` is the original window/desktop region in virtual-desktop
/// physical pixels (what `capture_active_window_jpeg` returned).
/// `(ai_w, ai_h)` is the dimensions of the JPEG the AI actually saw.
pub fn ai_bbox_to_screen_rect(
    raw: [f64; 4],
    format: BboxFormat,
    ai_w: u32,
    ai_h: u32,
    capture_rect: Rect,
) -> Option<VdRect> {
    let ai_rect = match format {
        // NormBox::from_raw auto-detects the 0–1 variant, so a model emitting
        // fractions still lands in the right place.
        BboxFormat::Normalized1000 => NormBox::from_raw(raw)?.to_ai_rect(ai_w, ai_h)?,
        BboxFormat::Pixel => {
            // Auto-detect 0–1 normalized even under the pixel contract (some
            // models do this; rare but cheap to catch — pre-existing behavior).
            let max_val = raw.iter().cloned().fold(f64::MIN, f64::max);
            if max_val <= 1.001 {
                NormBox::from_raw(raw)?.to_ai_rect(ai_w, ai_h)?
            } else {
                AiRect::from_ai_pixels(raw, ai_w, ai_h)?
            }
        }
    };

    // Reject a box that spans almost the whole frame: it carries no localization
    // signal (the model failed to point at anything specific), and as a locator
    // filter / proximity centre it's pure noise that drags the pick toward screen
    // centre. Weak grounders (Nemotron especially) emit these. Falling back to
    // text-only locating is strictly better than steering on a whole-screen box.
    let (cover_x, cover_y) = ai_rect.coverage();
    if cover_x >= 0.85 && cover_y >= 0.85 {
        return None;
    }

    Some(ai_rect.to_virtual_desktop(capture_rect))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn normalized_1000_converts_and_undownscales() {
        // AI image 1000×500, capture rect 2000×1000 → scale ×2 each axis.
        let r = ai_bbox_to_screen_rect(
            [100.0, 200.0, 300.0, 400.0],
            BboxFormat::Normalized1000,
            1000,
            500,
            capture(0, 0, 2000, 1000),
        )
        .unwrap()
        .into_inner();
        assert_eq!((r.x, r.y, r.width, r.height), (400, 100, 400, 200));
    }

    #[test]
    fn zero_to_one_normalized_is_autodetected() {
        // [0.1, 0.2, 0.3, 0.4] must land identically to [100, 200, 300, 400].
        let a = ai_bbox_to_screen_rect(
            [0.1, 0.2, 0.3, 0.4],
            BboxFormat::Normalized1000,
            1000,
            500,
            capture(0, 0, 2000, 1000),
        )
        .unwrap()
        .into_inner();
        assert_eq!((a.x, a.y, a.width, a.height), (400, 100, 400, 200));
    }

    #[test]
    fn capture_origin_offset_is_applied() {
        // 60% box anchored at the top-left of a left-secondary monitor — the capture
        // origin (-1920, 50) must be added. (Not whole-frame, so not rejected.)
        let r = ai_bbox_to_screen_rect(
            [0.0, 0.0, 600.0, 600.0],
            BboxFormat::Normalized1000,
            1000,
            500,
            capture(-1920, 50, 1000, 500),
        )
        .unwrap()
        .into_inner();
        assert_eq!((r.x, r.y, r.width, r.height), (-1920, 50, 600, 300));
    }

    #[test]
    fn overshoot_is_clamped_to_image() {
        // Right edge overshoots (xmax 2000 → clamped to image width 1000); the box is
        // full-width but only 70% tall, so it's a legitimate target, not whole-frame.
        let r = ai_bbox_to_screen_rect(
            [0.0, 0.0, 700.0, 2000.0],
            BboxFormat::Normalized1000,
            1000,
            500,
            capture(0, 0, 2000, 1000),
        )
        .unwrap()
        .into_inner();
        assert_eq!((r.x, r.y, r.width, r.height), (0, 0, 2000, 700));
    }

    #[test]
    fn bogus_boxes_return_none() {
        let cap = capture(0, 0, 2000, 1000);
        // Inverted extent.
        assert!(
            ai_bbox_to_screen_rect([300.0, 400.0, 100.0, 200.0], BboxFormat::Normalized1000, 1000, 500, cap)
                .is_none()
        );
        // NaN.
        assert!(
            ai_bbox_to_screen_rect([f64::NAN, 0.0, 10.0, 10.0], BboxFormat::Normalized1000, 1000, 500, cap)
                .is_none()
        );
        // Degenerate AI image.
        assert!(
            ai_bbox_to_screen_rect([0.0, 0.0, 10.0, 10.0], BboxFormat::Normalized1000, 0, 0, cap)
                .is_none()
        );
    }

    #[test]
    fn whole_frame_box_is_rejected() {
        let cap = capture(0, 0, 2000, 1000);
        // ~the entire image (0–1000 on both axes) → no localization signal.
        assert!(
            ai_bbox_to_screen_rect([0.0, 0.0, 1000.0, 1000.0], BboxFormat::Normalized1000, 1000, 500, cap)
                .is_none()
        );
        // 90% of both axes also rejected.
        assert!(
            ai_bbox_to_screen_rect([50.0, 50.0, 950.0, 950.0], BboxFormat::Normalized1000, 1000, 500, cap)
                .is_none()
        );
        // A large-but-real dialog (70% wide, 60% tall) still passes.
        assert!(
            ai_bbox_to_screen_rect([200.0, 150.0, 800.0, 850.0], BboxFormat::Normalized1000, 1000, 500, cap)
                .is_some()
        );
    }

    #[test]
    fn bbox_trust_classifier() {
        const DEFAULT: &str = "nemotron,gemma,kimi";
        // Trusted by default — frontier models (incl. ones the original allowlist excluded).
        assert!(bbox_is_decisive("gemini-3-flash", DEFAULT));
        assert!(bbox_is_decisive("qwen3.5-omni-plus", DEFAULT));
        assert!(bbox_is_decisive("gpt-5.5", DEFAULT));
        assert!(bbox_is_decisive("claude-opus-4-7", DEFAULT));
        assert!(bbox_is_decisive("some-future-model-x9", DEFAULT));
        // Muted: the managed free-tier chain.
        assert!(!bbox_is_decisive("nvidia/nemotron-nano-12b-v2-vl", DEFAULT));
        assert!(!bbox_is_decisive("google/gemma-4-26b-a4b-it", DEFAULT));
        assert!(!bbox_is_decisive("moonshotai/kimi-k2.6", DEFAULT));
        // Empty distrust list trusts everything.
        assert!(bbox_is_decisive("nvidia/nemotron-nano", ""));
        // Whitespace / casing in the list are tolerated.
        assert!(!bbox_is_decisive("GPT-5.5", " gpt-5.5 , foo "));
    }
}
