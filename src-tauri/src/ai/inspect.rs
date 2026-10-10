//! H2 `inspect` (navisual-internal harness-plan.md): a tool the model may call before it answers,
//! returning one region of the CURRENT capture at native resolution. No new capture is taken --
//! it is the same frame the AI image was made from, with the same masking.
//!
//! Why only sometimes: the AI image is fitted inside 1536x768 (`capture::MAX_CAP_W/H`). On a 4K
//! screen that is 36%, and a 2026-10-09 probe had two models misread a 12-px word count and an
//! italic paragraph there that they read correctly once zoomed; on a 1080p screen (71%) the same
//! models needed no help. So the tool is offered only when the capture was reduced below
//! `MIN_SCALE`, and a 1080p request is unchanged.
//!
//! H4 `read_text` rides the same loop: Windows.Media.Ocr on a region of the same frame, answered
//! as text with boxes in screenshot space. No image goes back, so it costs a round trip and a few
//! hundred input tokens rather than an image. It reads characters only -- a 2026-10-10 probe on the
//! 4K renders read the 12-px status bar exactly at 2x and nearly at 1x, and of course nothing
//! about bold, italic or size. `Tools` picks which of the two are offered.
//!
//! H6 `recall` rides it too: an earlier turn's screenshot, exactly as it was sent then. S3 found
//! the state summary keeps what the user did and almost never what the screen showed, and a fact
//! that did not matter when it was on screen is written down by nobody (a date seen once: 0 of 18
//! under either rule 16). The model picks the turn from `Session::earlier_screens`, the list of
//! earlier screens in the window context.

use std::sync::Arc;

use image::RgbImage;
use serde_json::{json, Value};

pub const NAME: &str = "inspect";
pub const READ_TEXT: &str = "read_text";
pub const RECALL: &str = "recall";
/// Look-tool calls allowed per answer, both tools together; after the last one the model must
/// answer with navigate_step.
pub const MAX_INSPECTS: usize = 2;
/// Lines `read_text` returns at most; the rest are counted, never silently dropped.
const MAX_TEXT_LINES: usize = 80;
/// Offered only when the AI image is smaller than this fraction of the capture.
pub const MIN_SCALE: f32 = 0.6;
/// A crop larger than this on its long side is reduced, so one zoom cannot cost a full 4K frame.
const MAX_CROP_SIDE: u32 = 2048;
const AI_MAX_W: f32 = 1536.0;
const AI_MAX_H: f32 = 768.0;

const DESCRIPTION: &str = "Look at one region of the current screenshot at full resolution \
before you answer. Use it when a detail you need -- small text, the state of a small control, \
which of several similar icons -- is too small to read with confidence in the screenshot. \
Coordinates are 0-1000 across and down the screenshot.";

const READ_TEXT_DESCRIPTION: &str = "Read the exact text in one region of the current screenshot \
before you answer. The text is recognised from the full-resolution screen, so it is exact where \
the screenshot is too small to read: a status bar, a small label, a number, a file name. Returns \
each line of text with its box. It reads characters only -- not bold or italic, size, colour, or \
which control is selected or checked. Coordinates are 0-1000 across and down the screenshot.";

const RECALL_DESCRIPTION: &str = "Look again at the screenshot from an earlier turn of this \
session, exactly as it was sent then. Use it when you need something that was on an earlier \
screen and is neither on the current screen nor in your summary. The earlier screens are listed \
by turn in the message.";

/// The same, when one call may return several screens (`Sources::recall_max` above 1).
const RECALL_MANY_DESCRIPTION: &str = "Look again at screenshots from earlier turns of this \
session, exactly as they were sent then. Use it when you need something that was on earlier \
screens and is neither on the current screen nor in your summary -- including what changed \
between them. The earlier screens are listed by turn in the message.";

/// Said with every recalled screen: one look is enough if what was needed is written down.
const RECALL_KEEP_HINT: &str = " If you will need something from it again, put it in keep.";

const LIMIT_ONE: &str = " You may call it at most twice per answer; then answer with navigate_step.";

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

/// Which look tools are offered. `Zoom` is H2 alone, `Text` H4 alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tools {
    #[default]
    Zoom,
    Text,
    Both,
}

impl Tools {
    /// `INSPECT_TOOLS=` in the `.env`: `text`, `both`; anything else is the zoom alone.
    pub fn parse(v: &str) -> Tools {
        match v.trim().to_ascii_lowercase().as_str() {
            "text" | "read_text" | "ocr" => Tools::Text,
            "both" => Tools::Both,
            _ => Tools::Zoom,
        }
    }

    pub fn names(self) -> &'static [&'static str] {
        match self {
            Tools::Zoom => &[NAME],
            Tools::Text => &[READ_TEXT],
            Tools::Both => &[NAME, READ_TEXT],
        }
    }
}

/// The zoom's description as H2 measured it: with `Tools::Zoom` this is byte-identical to the
/// 2026-10-09 runs. The tests pin it there.
#[cfg(test)]
pub fn description(mode: Mode) -> String {
    describe(DESCRIPTION, mode, LIMIT_ONE)
}

fn describe(base: &str, mode: Mode, limit: &str) -> String {
    let note = if mode == Mode::Note { REDUCED_NOTE } else { "" };
    format!("{base}{limit}{note}")
}

/// One earlier screen `recall` can return: the turn it was taken on, and the JPEG exactly as it
/// was sent then.
#[derive(Debug, Clone)]
pub struct EarlierScreen {
    pub turn: usize,
    pub jpeg_b64: String,
}

/// What one request may look at: the current frame (zoom and text, H2/H4) and the earlier screens
/// (recall, H6). Either may be absent; with neither there is no loop.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub frame: Option<Arc<RgbImage>>,
    pub earlier: Option<Arc<Vec<EarlierScreen>>>,
    /// Screens one recall may return; 0 or 1 is one, as H6 was measured.
    pub recall_max: usize,
}

impl Sources {
    /// The tools these sources make possible, frame tools first.
    pub fn names(&self, tools: Tools) -> Vec<&'static str> {
        let mut n: Vec<&'static str> = Vec::new();
        if self.frame.is_some() {
            n.extend_from_slice(tools.names());
        }
        if self.earlier.as_ref().is_some_and(|e| !e.is_empty()) {
            n.push(RECALL);
        }
        n
    }
}

/// Whether a function name is one of the look tools (no caption to stream for it).
pub fn is_look(name: &str) -> bool {
    [NAME, READ_TEXT, RECALL].contains(&name)
}

/// (name, description, parameters) of every tool in `names`. One tool keeps the exact wording H2
/// was measured with; several share one sentence naming the joint budget. `recall_max` above 1
/// lets one recall name several turns.
pub fn declarations(mode: Mode, names: &[&'static str], recall_max: usize) -> Vec<(&'static str, String, Value)> {
    let limit = if names.len() > 1 {
        let (last, rest) = names.split_last().expect("non-empty");
        format!(
            " You may call {} and {last} at most twice in total per answer; then answer with navigate_step.",
            rest.join(", ")
        )
    } else {
        LIMIT_ONE.to_string()
    };
    names
        .iter()
        .map(|&n| match n {
            NAME => (n, describe(DESCRIPTION, mode, &limit), parameters()),
            READ_TEXT => (n, describe(READ_TEXT_DESCRIPTION, mode, &limit), parameters()),
            // A fact about the current frame does not belong on recall: no reduction note.
            _ if recall_max > 1 => (
                n,
                format!("{RECALL_MANY_DESCRIPTION} Name at most {recall_max} turns in one call.{limit}"),
                recall_many_parameters(recall_max),
            ),
            _ => (n, format!("{RECALL_DESCRIPTION}{limit}"), recall_parameters()),
        })
        .collect()
}

fn recall_many_parameters(max: usize) -> Value {
    json!({
        "type": "object",
        "properties": {
            "turns": {
                "type": "array",
                "items": {"type": "integer"},
                "description": format!("turn numbers from the list of earlier screens, at most {max}")
            }
        },
        "required": ["turns"]
    })
}

fn recall_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "turn": {"type": "integer", "description": "the turn number from the list of earlier screens"}
        },
        "required": ["turn"]
    })
}



/// One look-tool call as the harness records it: which tool, the pixel box, for `read_text` the
/// text that went back, and for `recall` the turn.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Look {
    pub tool: &'static str,
    pub region: [u32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<usize>,
}

/// What goes back to the model for one call. Images carry the sentence that names them; a zoom is
/// one image, a recall one or more.
pub enum Reply {
    Images(Vec<String>, String),
    Text(String),
    Refused(&'static str),
}

/// Answer one look-tool call. OCR blocks (WinRT), so it runs off the async thread. Returns the
/// reply and, when something was looked at, the record of it.
pub async fn answer(name: &str, sources: &Sources, args: Value) -> (Reply, Option<Look>) {
    if name == RECALL {
        // `turns` when several are allowed, `turn` as H6 was measured; either is read.
        let mut asked: Vec<usize> = args
            .get("turns")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).map(|t| t as usize).collect())
            .unwrap_or_default();
        if let Some(t) = args.get("turn").and_then(Value::as_u64) {
            asked.push(t as usize);
        }
        asked.dedup();
        asked.truncate(sources.recall_max.max(1));
        let found: Vec<&EarlierScreen> = asked
            .iter()
            .filter_map(|t| sources.earlier.as_ref().and_then(|e| e.iter().find(|s| s.turn == *t)))
            .collect();
        if found.is_empty() {
            return (Reply::Refused("there is no earlier screen with that turn number"), None);
        }
        let turns: Vec<usize> = found.iter().map(|s| s.turn).collect();
        let caption = match turns.as_slice() {
            [one] => format!("the screenshot from turn {one}.{RECALL_KEEP_HINT}"),
            many => format!(
                "the screenshots from turns {}, in that order.{RECALL_KEEP_HINT}",
                many.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
            ),
        };
        return (
            Reply::Images(found.iter().map(|s| s.jpeg_b64.clone()).collect(), caption),
            Some(Look { tool: RECALL, region: [0; 4], text: None, turns }),
        );
    }
    let Some(frame) = sources.frame.clone() else {
        return (Reply::Refused("that tool is not available for this screen"), None);
    };
    if name == READ_TEXT {
        let out = tokio::task::spawn_blocking(move || read_text(&frame, &args)).await;
        return match out {
            Ok(Some((text, region))) => (
                Reply::Text(text.clone()),
                Some(Look { tool: READ_TEXT, region, text: Some(text), turns: Vec::new() }),
            ),
            Ok(None) => (Reply::Refused("that region is too small; give a larger one"), None),
            Err(_) => (Reply::Refused("the text could not be read"), None),
        };
    }
    match crop(&frame, &args) {
        Some((b64, region)) => (
            Reply::Images(vec![b64], "the region at full resolution".to_string()),
            Some(Look { tool: NAME, region, text: None, turns: Vec::new() }),
        ),
        None => (Reply::Refused("that region is too small; give a larger one"), None),
    }
}

/// The pixel box `args` names ({x0, y0, x1, y1}, 0-1000), or `None` under 16 px on a side.
fn region_of(frame: &RgbImage, args: &Value) -> Option<[u32; 4]> {
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
    (x1 - x0 >= 16 && y1 - y0 >= 16).then_some([x0, y0, x1, y1])
}

/// OCR of the region `args` names, as the lines the model reads: `[x0,y0,x1,y1] text`, boxes in
/// 0-1000 of the whole screenshot (decision 1's corollary: nothing in the user's pixels). The
/// region is upscaled 2x first -- the probe's sweet spot; 3x read no better at three times the
/// time -- unless that would pass 8000 px on a side or 16 MP, when it is read as it is: a full-
/// width 4K strip still gets 2x (at 1x the first smoke run lost "File" from a menu bar), a whole
/// 4K screen does not.
pub fn read_text(frame: &RgbImage, args: &Value) -> Option<(String, [u32; 4])> {
    let [x0, y0, x1, y1] = region_of(frame, args)?;
    let (w, h) = frame.dimensions();
    let region = image::imageops::crop_imm(frame, x0, y0, x1 - x0, y1 - y0).to_image();
    let (rw, rh) = region.dimensions();
    let up = if rw.max(rh) * 2 <= 8000 && rw as u64 * rh as u64 * 4 <= 16_000_000 { 2 } else { 1 };
    let region = if up == 1 {
        region
    } else {
        image::imageops::resize(
            &region,
            region.width() * up,
            region.height() * up,
            image::imageops::FilterType::Lanczos3,
        )
    };
    let mut png = std::io::Cursor::new(Vec::new());
    region.write_to(&mut png, image::ImageFormat::Png).ok()?;
    let results = crate::locator::ocr::run_ocr(png.get_ref()).ok()?;
    // run_ocr gives each line (confidence 1.0) followed by its words; the lines are the reading.
    let lines: Vec<String> = results
        .iter()
        .filter(|r| r.confidence >= 1.0)
        .map(|r| {
            let k = |px: i32, origin: u32, n: u32| -> u32 {
                let v = origin as f64 + px.max(0) as f64 / up as f64;
                ((v / n as f64 * 1000.0).round() as u32).min(1000)
            };
            let (bx, by) = (r.bbox.0, r.bbox.1);
            let (ex, ey) = (bx + r.bbox.2 as i32, by + r.bbox.3 as i32);
            format!("[{},{},{},{}] {}", k(bx, x0, w), k(by, y0, h), k(ex, x0, w), k(ey, y0, h), r.text)
        })
        .collect();
    let text = if lines.is_empty() {
        "No text was found in that region.".to_string()
    } else if lines.len() > MAX_TEXT_LINES {
        format!(
            "{}\n({} more lines not shown; read a smaller region)",
            lines[..MAX_TEXT_LINES].join("\n"),
            lines.len() - MAX_TEXT_LINES
        )
    } else {
        lines.join("\n")
    };
    Some((text, [x0, y0, x1, y1]))
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
    let [x0, y0, x1, y1] = region_of(frame, args)?;
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
    fn tools_offer_what_they_name_and_the_zoom_alone_reads_as_measured() {
        assert_eq!(Tools::parse("text"), Tools::Text);
        assert_eq!(Tools::parse("both"), Tools::Both);
        assert_eq!(Tools::parse(""), Tools::Zoom);
        let zoom = declarations(Mode::Choice, Tools::Zoom.names(), 1);
        assert_eq!(zoom.len(), 1);
        assert_eq!(zoom[0].1, description(Mode::Choice));
        assert!(zoom[0].1.ends_with("at most twice per answer; then answer with navigate_step."));
        let both = declarations(Mode::Note, Tools::Both.names(), 1);
        assert_eq!(both.iter().map(|d| d.0).collect::<Vec<_>>(), vec![NAME, READ_TEXT]);
        assert!(both.iter().all(|d| d.1.contains("inspect and read_text at most twice in total")));
        assert!(both.iter().all(|d| d.1.contains("reduced to fit")));
        assert!(declarations(Mode::First, Tools::Text.names(), 1)[0].1.contains("characters only"));
    }

    #[test]
    fn recall_is_offered_only_with_earlier_screens_and_finds_them_by_turn() {
        let none = Sources::default();
        assert!(none.names(Tools::Zoom).is_empty());
        let screens = vec![
            EarlierScreen { turn: 2, jpeg_b64: "AAA".into() },
            EarlierScreen { turn: 5, jpeg_b64: "BBB".into() },
        ];
        let earlier = Sources { frame: None, earlier: Some(Arc::new(screens)), recall_max: 1 };
        assert_eq!(earlier.names(Tools::Zoom), vec![RECALL], "no frame, no zoom");
        let d = declarations(Mode::Note, &[RECALL], 1);
        assert!(d[0].1.contains("earlier turn") && !d[0].1.contains("reduced"), "{}", d[0].1);
        assert!(d[0].2["properties"].get("turn").is_some(), "one screen per call, as measured");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (reply, look) = rt.block_on(answer(RECALL, &earlier, json!({"turn": 2})));
        assert!(matches!(reply, Reply::Images(ref b, ref c) if b == &["AAA"] && c.contains("turn 2") && c.contains("keep")));
        assert_eq!(look.unwrap().turns, vec![2]);
        let (_, look) = rt.block_on(answer(RECALL, &earlier, json!({"turns": [5, 2]})));
        assert_eq!(look.unwrap().turns, vec![5], "one allowed: the first named");
        let (reply, look) = rt.block_on(answer(RECALL, &earlier, json!({"turn": 9})));
        assert!(matches!(reply, Reply::Refused(_)) && look.is_none());
        let many = Sources { recall_max: 3, ..earlier.clone() };
        let d = declarations(Mode::Choice, &[RECALL], 3);
        assert!(d[0].1.contains("at most 3 turns") && d[0].2["properties"].get("turns").is_some());
        let (reply, look) = rt.block_on(answer(RECALL, &many, json!({"turns": [5, 2, 9]})));
        assert!(matches!(reply, Reply::Images(ref b, ref c) if b.len() == 2 && c.contains("turns 5, 2")));
        assert_eq!(look.unwrap().turns, vec![5, 2], "an unknown turn is skipped");

    }

    #[test]
    fn crop_maps_thousandths_to_pixels_and_refuses_slivers() {
        let f = RgbImage::new(3840, 2120);
        let (_, b) = crop(&f, &json!({"x0": 0, "y0": 980, "x1": 250, "y1": 1000})).unwrap();
        assert_eq!(b, [0, 2078, 960, 2120]);
        let (_, b) = crop(&f, &json!({"x0": 500, "y0": 500, "x1": 100, "y1": 100})).unwrap();
        assert_eq!(b, [384, 212, 1920, 1060], "reversed corners are swapped");
        assert!(crop(&f, &json!({"x0": 10, "y0": 10, "x1": 11, "y1": 11})).is_none());
        assert!(read_text(&f, &json!({"x0": 10, "y0": 10, "x1": 11, "y1": 11})).is_none());
    }

    #[test]
    fn read_text_on_a_blank_region_says_so() {
        let f = RgbImage::from_pixel(800, 400, image::Rgb([255, 255, 255]));
        let (text, b) = read_text(&f, &json!({"x0": 0, "y0": 0, "x1": 500, "y1": 500})).unwrap();
        assert_eq!(b, [0, 0, 400, 200]);
        assert_eq!(text, "No text was found in that region.");
    }
}
