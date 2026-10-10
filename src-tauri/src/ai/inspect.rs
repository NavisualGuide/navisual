//! H2 `inspect` (navisual-internal harness-plan.md): a tool the model may call before it answers,
//! returning one region of the CURRENT capture at native resolution. No new capture is taken --
//! it is the same frame the AI image was made from, with the same masking.
//!
//! Why only sometimes: the AI image is fitted inside 1536x768 (`capture::MAX_CAP_W/H`). On a 4K
//! screen that is 36%, and a 2026-10-09 probe had two models misread a 12-px word count and an
//! italic paragraph there that they read correctly once zoomed; on a 1080p screen (71%) the same
//! models needed no help. So the tool is offered only when the capture was reduced below
//! `MIN_SCALE`, and a 1080p request is unchanged.

use std::sync::Arc;

use image::RgbImage;
use serde_json::{json, Value};

pub const NAME: &str = "inspect";
/// Zooms allowed per answer; after the last one the model must answer with navigate_step.
pub const MAX_INSPECTS: usize = 2;
/// Offered only when the AI image is smaller than this fraction of the capture.
pub const MIN_SCALE: f32 = 0.6;
/// A crop larger than this on its long side is reduced, so one zoom cannot cost a full 4K frame.
const MAX_CROP_SIDE: u32 = 2048;
const AI_MAX_W: f32 = 1536.0;
const AI_MAX_H: f32 = 768.0;

const DESCRIPTION: &str = "Look at one region of the current screenshot at full resolution \
before you answer. Use it when a detail you need -- small text, the state of a small control, \
which of several similar icons -- is too small to read with confidence in the screenshot. \
Coordinates are 0-1000 across and down the screenshot. You may call it at most twice per answer; \
then answer with navigate_step.";

/// Said only in `Mode::Note`: a fact about the image, never a size or coordinate of the user's
/// screen (design decision 1's corollary). A 2026-10-09 smoke run had luna, on a 4K frame,
/// answer "the paragraph is now bold" with confidence and never zoom -- a model cannot tell from
/// the image alone that it is reduced.
const REDUCED_NOTE: &str = " The screenshot you were given was reduced to fit, so small text and \
small controls in it can be unreadable or misread.";

/// When the model may zoom. `Choice`: offered, the model decides. `Note`: the same, with the
/// description saying the image was reduced. `First`: the first round must zoom (the model still
/// picks the region), later rounds are its choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Off,
    Choice,
    Note,
    First,
}

impl Mode {
    /// `INSPECT=` in the `.env`: `choice` (or any true word), `note`, `first`; anything else off.
    pub fn parse(v: &str) -> Mode {
        match v.trim().to_ascii_lowercase().as_str() {
            "choice" | "true" | "1" | "yes" | "on" => Mode::Choice,
            "note" => Mode::Note,
            "first" => Mode::First,
            _ => Mode::Off,
        }
    }
}

pub fn description(mode: Mode) -> String {
    if mode == Mode::Note {
        format!("{DESCRIPTION}{REDUCED_NOTE}")
    } else {
        DESCRIPTION.to_string()
    }
}

pub fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "x0": {"type": "number", "description": "left edge, 0-1000 across the screenshot"},
            "y0": {"type": "number", "description": "top edge, 0-1000 down the screenshot"},
            "x1": {"type": "number", "description": "right edge, 0-1000"},
            "y1": {"type": "number", "description": "bottom edge, 0-1000"}
        },
        "required": ["x0", "y0", "x1", "y1"]
    })
}

/// The frame a request may zoom into, or `None` when the AI image is large enough already.
pub fn offered(frame: &Option<Arc<RgbImage>>) -> Option<Arc<RgbImage>> {
    let f = frame.as_ref()?;
    let (w, h) = f.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (AI_MAX_W / w as f32).min(AI_MAX_H / h as f32);
    (scale < MIN_SCALE).then(|| f.clone())
}

/// The region `args` names ({x0, y0, x1, y1}, 0-1000) as a JPEG in base64, with its pixel box.
/// `None` for a region under 16 px on a side -- nothing to read there.
pub fn crop(frame: &RgbImage, args: &Value) -> Option<(String, [u32; 4])> {
    let (w, h) = frame.dimensions();
    let at = |key: &str, n: u32, default: f64| -> u32 {
        let v = args.get(key).and_then(Value::as_f64).unwrap_or(default);
        ((v / 1000.0 * n as f64).round().max(0.0) as u32).min(n)
    };
    let (mut x0, mut x1) = (at("x0", w, 0.0), at("x1", w, 1000.0));
    let (mut y0, mut y1) = (at("y0", h, 0.0), at("y1", h, 1000.0));
    if x0 > x1 {
        std::mem::swap(&mut x0, &mut x1);
    }
    if y0 > y1 {
        std::mem::swap(&mut y0, &mut y1);
    }
    if x1 - x0 < 16 || y1 - y0 < 16 {
        return None;
    }
    let mut region = image::imageops::crop_imm(frame, x0, y0, x1 - x0, y1 - y0).to_image();
    let long = region.width().max(region.height());
    if long > MAX_CROP_SIDE {
        let s = MAX_CROP_SIDE as f32 / long as f32;
        region = image::imageops::resize(
            &region,
            ((region.width() as f32 * s).round() as u32).max(1),
            ((region.height() as f32 * s).round() as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&region)
        .ok()?;
    Some((crate::capture::to_base64(&out), [x0, y0, x1, y1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_only_when_the_ai_image_is_much_smaller_than_the_screen() {
        let frame = |w, h| Some(Arc::new(RgbImage::new(w, h)));
        assert!(offered(&frame(1920, 1040)).is_none(), "1080p reaches the model at 74%");
        assert!(offered(&frame(2560, 1400)).is_some(), "1440p: 55%");
        assert!(offered(&frame(3840, 2120)).is_some(), "4K: 36%");
        assert!(offered(&None).is_none());
    }

    #[test]
    fn mode_parses_and_only_note_mentions_the_reduction() {
        assert_eq!(Mode::parse("first"), Mode::First);
        assert_eq!(Mode::parse(" Note "), Mode::Note);
        assert_eq!(Mode::parse("true"), Mode::Choice);
        assert_eq!(Mode::parse(""), Mode::Off);
        assert!(description(Mode::Note).contains("was reduced to fit"));
        assert!(!description(Mode::Choice).contains("reduced"));
        assert!(!description(Mode::Note).chars().any(|c| c.is_ascii_digit() && c != '0' && c != '1'),
            "no size or coordinate of the user's screen in the text");
    }

    #[test]
    fn crop_maps_thousandths_to_pixels_and_refuses_slivers() {
        let f = RgbImage::new(3840, 2120);
        let (_, b) = crop(&f, &json!({"x0": 0, "y0": 980, "x1": 250, "y1": 1000})).unwrap();
        assert_eq!(b, [0, 2078, 960, 2120]);
        let (_, b) = crop(&f, &json!({"x0": 500, "y0": 500, "x1": 100, "y1": 100})).unwrap();
        assert_eq!(b, [384, 212, 1920, 1060], "reversed corners are swapped");
        assert!(crop(&f, &json!({"x0": 10, "y0": 10, "x1": 11, "y1": 11})).is_none());
    }
}
