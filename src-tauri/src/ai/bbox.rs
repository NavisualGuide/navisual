//! AI-returned bounding-box coordinate-system conversion.
//!
//! Different vision models use different conventions for spatial coordinates:
//! - Gemini: normalized 0–1000 in [ymin, xmin, ymax, xmax] (native object-detection format)
//! - Most others: absolute pixel coordinates of the image they were shown
//!
//! `ai_bbox_to_screen_rect` takes whatever the AI returned and the active
//! provider name, plus the actual AI-image dimensions and the screen rect the
//! image represents, and produces a `Rect` in virtual-desktop physical pixels
//! that the overlay can draw directly.

use crate::capture::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BboxFormat {
    /// Coordinates are normalized to 0–1000 (Gemini's native object-detection scale).
    Normalized1000,
    /// Coordinates are absolute pixels of the AI-image (post `cap_size` downscale).
    Pixel,
}

/// Hardcoded per-provider format. Conservative defaults; can be overridden via
/// auto-detection if the values look out of range (see `decode_bbox`).
pub fn bbox_format_for_provider(provider: &str) -> BboxFormat {
    match provider {
        "gemini" => BboxFormat::Normalized1000,
        // Anthropic, OpenAI, DeepSeek, Qwen, Ollama, managed — instructed to
        // use absolute pixels in the system prompt.
        _ => BboxFormat::Pixel,
    }
}

/// Convert an AI-returned `[ymin, xmin, ymax, xmax]` to a screen Rect.
///
/// Auto-corrects format mismatch: if all four values are ≤ 1.001 we treat
/// it as 0–1 normalized; if all are ≤ 1000.0 but `format=Pixel` and the
/// AI-image is much larger than 1000 px, we still trust the declared format.
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
) -> Option<Rect> {
    let [ymin, xmin, ymax, xmax] = raw;

    // Sanity checks — any NaN or negative-extent box is bogus.
    if !ymin.is_finite() || !xmin.is_finite() || !ymax.is_finite() || !xmax.is_finite() {
        return None;
    }
    if ymax <= ymin || xmax <= xmin {
        return None;
    }
    if ai_w == 0 || ai_h == 0 {
        return None;
    }

    // Auto-detect 0–1 normalized (some models do this; rare but cheap to catch).
    let max_val = ymin.max(xmin).max(ymax).max(xmax);
    let effective_format = if max_val <= 1.001 {
        BboxFormat::Normalized1000 // we'll scale by 1000 below — but max is 1.0
    } else {
        format
    };

    // Convert raw → pixel coords in the AI-image.
    let (ai_ymin, ai_xmin, ai_ymax, ai_xmax) = match effective_format {
        BboxFormat::Normalized1000 => {
            // Distinguish 0–1 vs 0–1000: scale up either way.
            let scale = if max_val <= 1.001 { 1.0 } else { 1000.0 };
            (
                (ymin / scale) * ai_h as f64,
                (xmin / scale) * ai_w as f64,
                (ymax / scale) * ai_h as f64,
                (xmax / scale) * ai_w as f64,
            )
        }
        BboxFormat::Pixel => (ymin, xmin, ymax, xmax),
    };

    // Clamp to AI-image bounds (model may overshoot).
    let ai_xmin = ai_xmin.clamp(0.0, ai_w as f64);
    let ai_xmax = ai_xmax.clamp(0.0, ai_w as f64);
    let ai_ymin = ai_ymin.clamp(0.0, ai_h as f64);
    let ai_ymax = ai_ymax.clamp(0.0, ai_h as f64);
    if ai_xmax - ai_xmin < 1.0 || ai_ymax - ai_ymin < 1.0 {
        return None;
    }

    // Scale AI-image pixels → capture-rect pixels (un-downscale).
    let sx = capture_rect.width as f64 / ai_w as f64;
    let sy = capture_rect.height as f64 / ai_h as f64;

    let screen_x = capture_rect.x as f64 + ai_xmin * sx;
    let screen_y = capture_rect.y as f64 + ai_ymin * sy;
    let screen_w = (ai_xmax - ai_xmin) * sx;
    let screen_h = (ai_ymax - ai_ymin) * sy;

    Some(Rect {
        x: screen_x.round() as i32,
        y: screen_y.round() as i32,
        width: screen_w.round().max(1.0) as u32,
        height: screen_h.round().max(1.0) as u32,
    })
}
