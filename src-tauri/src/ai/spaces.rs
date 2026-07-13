//! Typed pixel-coordinate spaces (design improvement #1, 2026-07-13).
//!
//! Four coordinate spaces flow through the core, and every historical
//! wrong-pointer bug class was a mixup between two of them (pre-normalization
//! scale bugs, DPR caption misdraws, the OCR wrong-screen arrows):
//!
//! 1. **Normalized 0–1000** ([`NormBox`]) — the provider contract: `target_bbox`
//!    as `[ymin, xmin, ymax, xmax]`, resolution-independent.
//! 2. **AI-image pixels** ([`AiRect`]) — the downscaled JPEG the model actually
//!    saw (≤ the `cap_size` bound). Carries its own image dimensions so the
//!    un-downscale factor can never be paired with the wrong image.
//! 3. **Capture-relative pixels** — offsets inside the captured region. Only an
//!    intermediate here; [`AiRect::to_virtual_desktop`] passes through it.
//! 4. **Virtual-desktop physical pixels** ([`VdRect`]) — the OS space the
//!    overlay draws in and `capture::Rect` values from Win32 live in.
//!
//! Converting between spaces goes through the methods on these types — each
//! hop demands exactly the context it needs (image dims, capture rect), so a
//! future mixup is a compile error instead of a live wrong-pointer.
//!
//! **Adoption is incremental** (per the design note): this starts at bbox.rs's
//! boundaries — `ai_bbox_to_screen_rect` is now a composition of these types
//! and returns [`VdRect`]. `ocr.rs`'s crop-origin translation and the overlay's
//! CSS/DPR conversion are natural next adopters.

use crate::capture::Rect;

/// A `target_bbox` in the normalized 0–1000 space of the provider contract:
/// `[ymin, xmin, ymax, xmax]`, 0 = top/left edge, 1000 = bottom/right edge of
/// the AI image, regardless of its pixel size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormBox {
    pub ymin: f64,
    pub xmin: f64,
    pub ymax: f64,
    pub xmax: f64,
}

impl NormBox {
    /// Validate a raw provider `[ymin, xmin, ymax, xmax]` into the 0–1000 space.
    /// Rejects NaN/inf and non-positive extents. Auto-detects the 0–1 variant
    /// some models emit (all values ≤ 1.001) and scales it up ×1000, so the
    /// resulting box is ALWAYS in 0–1000 regardless of what the model did.
    pub fn from_raw(raw: [f64; 4]) -> Option<Self> {
        let [ymin, xmin, ymax, xmax] = raw;
        if !ymin.is_finite() || !xmin.is_finite() || !ymax.is_finite() || !xmax.is_finite() {
            return None;
        }
        if ymax <= ymin || xmax <= xmin {
            return None;
        }
        let max_val = ymin.max(xmin).max(ymax).max(xmax);
        let scale = if max_val <= 1.001 { 1000.0 } else { 1.0 };
        Some(Self {
            ymin: ymin * scale,
            xmin: xmin * scale,
            ymax: ymax * scale,
            xmax: xmax * scale,
        })
    }

    /// Project into AI-image pixel space. Clamps overshoot to the image bounds
    /// (models overshoot routinely) and rejects a sub-pixel result. The returned
    /// [`AiRect`] remembers `(ai_w, ai_h)` so the later un-downscale can't be
    /// computed against the wrong image size.
    pub fn to_ai_rect(self, ai_w: u32, ai_h: u32) -> Option<AiRect> {
        if ai_w == 0 || ai_h == 0 {
            return None;
        }
        let x0 = (self.xmin / 1000.0 * ai_w as f64).clamp(0.0, ai_w as f64);
        let x1 = (self.xmax / 1000.0 * ai_w as f64).clamp(0.0, ai_w as f64);
        let y0 = (self.ymin / 1000.0 * ai_h as f64).clamp(0.0, ai_h as f64);
        let y1 = (self.ymax / 1000.0 * ai_h as f64).clamp(0.0, ai_h as f64);
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            return None;
        }
        Some(AiRect {
            x0,
            y0,
            x1,
            y1,
            img_w: ai_w,
            img_h: ai_h,
        })
    }
}

/// A rect in AI-IMAGE pixels — the (usually downscaled) JPEG the model saw.
/// `img_w`/`img_h` are that image's dimensions, carried along so conversions
/// out of this space always use the right scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AiRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    img_w: u32,
    img_h: u32,
}

impl AiRect {
    /// Build directly from `[ymin, xmin, ymax, xmax]` ALREADY in AI-image pixels
    /// (the reserved `BboxFormat::Pixel` contract). Same clamping/degenerate
    /// rejection as [`NormBox::to_ai_rect`].
    pub fn from_ai_pixels(raw: [f64; 4], ai_w: u32, ai_h: u32) -> Option<Self> {
        let [ymin, xmin, ymax, xmax] = raw;
        if !ymin.is_finite() || !xmin.is_finite() || !ymax.is_finite() || !xmax.is_finite() {
            return None;
        }
        if ai_w == 0 || ai_h == 0 || ymax <= ymin || xmax <= xmin {
            return None;
        }
        let x0 = xmin.clamp(0.0, ai_w as f64);
        let x1 = xmax.clamp(0.0, ai_w as f64);
        let y0 = ymin.clamp(0.0, ai_h as f64);
        let y1 = ymax.clamp(0.0, ai_h as f64);
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            return None;
        }
        Some(Self {
            x0,
            y0,
            x1,
            y1,
            img_w: ai_w,
            img_h: ai_h,
        })
    }

    /// Fraction of the AI image this rect covers on each axis `(cover_x, cover_y)`.
    /// Used by policy checks (e.g. the whole-frame rejection) — policy stays with
    /// the caller; the space type just answers the geometric question.
    pub fn coverage(&self) -> (f64, f64) {
        (
            (self.x1 - self.x0) / self.img_w as f64,
            (self.y1 - self.y0) / self.img_h as f64,
        )
    }

    /// Un-downscale into VIRTUAL-DESKTOP PHYSICAL pixels: scale by
    /// capture-size ÷ AI-image-size (per axis), then add the capture origin.
    /// `capture_rect` is the captured region in virtual-desktop physical pixels
    /// (what `capture_active_window_jpeg` returned) — the intermediate
    /// capture-relative space exists only inside this method.
    pub fn to_virtual_desktop(self, capture_rect: Rect) -> VdRect {
        let sx = capture_rect.width as f64 / self.img_w as f64;
        let sy = capture_rect.height as f64 / self.img_h as f64;
        let x = capture_rect.x as f64 + self.x0 * sx;
        let y = capture_rect.y as f64 + self.y0 * sy;
        let w = (self.x1 - self.x0) * sx;
        let h = (self.y1 - self.y0) * sy;
        VdRect(Rect {
            x: x.round() as i32,
            y: y.round() as i32,
            width: w.round().max(1.0) as u32,
            height: h.round().max(1.0) as u32,
        })
    }
}

/// A rect in VIRTUAL-DESKTOP PHYSICAL pixels — the OS coordinate space the
/// overlay draws in and Win32 window rects live in. Unwrap with
/// [`VdRect::into_inner`] only at a boundary that genuinely consumes this
/// space (overlay emit, locator options); passing the inner `Rect` onward
/// loses the compile-time space tag.
#[derive(Debug, Clone, Copy)]
pub struct VdRect(pub Rect);

impl VdRect {
    pub fn into_inner(self) -> Rect {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_box_rejects_garbage_and_autodetects_0_1() {
        assert!(NormBox::from_raw([f64::NAN, 0.0, 10.0, 10.0]).is_none());
        assert!(NormBox::from_raw([300.0, 400.0, 100.0, 200.0]).is_none()); // inverted
        let b = NormBox::from_raw([0.1, 0.2, 0.3, 0.4]).unwrap();
        assert_eq!((b.ymin, b.xmin, b.ymax, b.xmax), (100.0, 200.0, 300.0, 400.0));
    }

    #[test]
    fn full_pipeline_matches_the_documented_conversion() {
        // AI image 1000×500, capture rect 2000×1000 at origin (-1920, 50):
        // norm [100,200,300,400] → ai (200,50)-(400,150) → vd offset+×2.
        let vd = NormBox::from_raw([100.0, 200.0, 300.0, 400.0])
            .unwrap()
            .to_ai_rect(1000, 500)
            .unwrap()
            .to_virtual_desktop(Rect {
                x: -1920,
                y: 50,
                width: 2000,
                height: 1000,
            })
            .into_inner();
        assert_eq!((vd.x, vd.y, vd.width, vd.height), (-1520, 150, 400, 200));
    }

    #[test]
    fn ai_rect_carries_its_image_dims() {
        let a = NormBox::from_raw([0.0, 0.0, 500.0, 500.0])
            .unwrap()
            .to_ai_rect(1000, 500)
            .unwrap();
        let (cx, cy) = a.coverage();
        assert!((cx - 0.5).abs() < 1e-9);
        assert!((cy - 0.5).abs() < 1e-9);
    }
}
