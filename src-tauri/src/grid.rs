//! 16×9 Set-of-Marks grid helpers for AI locator test mode.
//!
//! When GRID_TEST_ENABLED=true, each screenshot sent to the AI has a 16-column
//! × 9-row grid drawn over it with column numbers (1–16) in a top margin strip
//! and row letters (A–I) in a left margin strip.  The AI reads the visible
//! labels and fills `grid_cell` with the cell containing the target element
//! (e.g. "D7").  The locator uses `grid_cell` as a zone filter for OCR.

use font8x8::UnicodeFonts;
use image::{codecs::jpeg::JpegEncoder, ColorType};

pub const COLS: u32 = 16;
pub const ROWS: u32 = 9;

/// Width of the left (row-label) strip and height of the top (col-label) strip.
const MARGIN: u32 = 24;

/// Strip background — near-white so labels stand out against any content.
const STRIP_BG: [u8; 3] = [240, 240, 240];
/// Label foreground — dark grey, readable on the light strip.
const LABEL_FG: [u8; 3] = [50, 50, 50];
/// Grid line colour blended over the content at 55 % opacity.
const GRID_LINE: [u8; 3] = [190, 190, 190];
const GRID_ALPHA: f32 = 0.55;

/// Parse a cell label like "D7" into (row_index, col_index), both 0-based.
/// Row A=0 … I=8; col 1→0 … 16→15. Returns None for out-of-range labels.
pub fn parse_cell(label: &str) -> Option<(u32, u32)> {
    let s = label.trim().to_ascii_uppercase();
    let mut chars = s.chars();
    let row_ch = chars.next()?;
    if !row_ch.is_ascii_alphabetic() {
        return None;
    }
    let col_str: String = chars.collect();
    let row = (row_ch as u32).checked_sub('A' as u32)?;
    let col = col_str.trim().parse::<u32>().ok()?.checked_sub(1)?;
    if row < ROWS && col < COLS {
        Some((row, col))
    } else {
        None
    }
}

/// Overlay a 16×9 Set-of-Marks grid onto a JPEG image and return new JPEG bytes.
///
/// Layout:
/// ```
///      1    2   …  16      ← top strip (MARGIN px tall), col numbers
///  A ┌────┬────┬────┐
///    │    │    │    │      ← content area with faint grid lines
///  B ├────┼────┼────┤
///    │    │    │    │
///  ⋮   ⋮    ⋮    ⋮
/// ```
/// The left strip (MARGIN px wide) shows row letters A–I centred in each row.
/// The top strip shows column numbers 1–16 centred in each column.
/// Grid lines are blended at 55 % opacity so the underlying UI stays readable.
pub fn overlay_grid_on_jpeg(jpeg_bytes: &[u8], quality: u8) -> Vec<u8> {
    let img = match image::load_from_memory(jpeg_bytes) {
        Ok(i) => i,
        Err(_) => return jpeg_bytes.to_vec(),
    };
    let content = img.to_rgb8();
    let cw = content.width();
    let ch = content.height();
    let fw = cw + MARGIN; // full canvas width  (content + left strip)
    let fh = ch + MARGIN; // full canvas height (content + top strip)

    // Canvas filled with strip background; content is pasted at (MARGIN, MARGIN).
    let mut canvas = image::RgbImage::from_pixel(fw, fh, image::Rgb(STRIP_BG));
    image::imageops::overlay(&mut canvas, &content, MARGIN as i64, MARGIN as i64);

    // --- Grid lines in content area only ---
    for col in 1..COLS {
        let x = MARGIN + col * cw / COLS;
        for y in MARGIN..fh {
            blend_px(canvas.get_pixel_mut(x, y), GRID_LINE, GRID_ALPHA);
        }
    }
    for row in 1..ROWS {
        let y = MARGIN + row * ch / ROWS;
        for x in MARGIN..fw {
            blend_px(canvas.get_pixel_mut(x, y), GRID_LINE, GRID_ALPHA);
        }
    }

    // --- Column numbers in top strip (1–16) ---
    for col in 0..COLS {
        let cell_x = MARGIN + col * cw / COLS;
        let cell_w = cw / COLS;
        let label = format!("{}", col + 1);
        // Each font glyph is 8 px wide; multi-char labels stack horizontally.
        let text_w = label.len() as u32 * 8;
        let x = cell_x as i32 + (cell_w as i32 - text_w as i32) / 2;
        let y = (MARGIN as i32 - 8) / 2; // vertically centred in top strip
        draw_str(&mut canvas, &label, x, y);
    }

    // --- Row letters in left strip (A–I) ---
    for row in 0..ROWS {
        let cell_y = MARGIN + row * ch / ROWS;
        let cell_h = ch / ROWS;
        let label = (b'A' + row as u8) as char;
        let x = (MARGIN as i32 - 8) / 2; // horizontally centred in left strip
        let y = cell_y as i32 + (cell_h as i32 - 8) / 2; // vertically centred
        draw_char(&mut canvas, label, x, y);
    }

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    let _ = encoder.encode(canvas.as_raw(), fw, fh, ColorType::Rgb8.into());
    if out.is_empty() {
        jpeg_bytes.to_vec()
    } else {
        out
    }
}

// ── Font rendering ────────────────────────────────────────────────────────────

/// Render a string of characters starting at pixel (x, y).
/// Each glyph occupies 8 px horizontally (font8x8 standard width).
fn draw_str(canvas: &mut image::RgbImage, s: &str, x: i32, y: i32) {
    let mut cx = x;
    for ch in s.chars() {
        draw_char(canvas, ch, cx, y);
        cx += 8;
    }
}

/// Render one 8×8 glyph from the font8x8 BASIC_FONTS set at pixel (x, y).
fn draw_char(canvas: &mut image::RgbImage, ch: char, x: i32, y: i32) {
    let Some(glyph) = font8x8::BASIC_FONTS.get(ch) else { return };
    for (row, &bits) in glyph.iter().enumerate() {
        for col in 0..8u32 {
            if (bits >> col) & 1 != 0 {
                let px = x + col as i32;
                let py = y + row as i32;
                if px >= 0 && py >= 0 {
                    let (px, py) = (px as u32, py as u32);
                    if px < canvas.width() && py < canvas.height() {
                        canvas.put_pixel(px, py, image::Rgb(LABEL_FG));
                    }
                }
            }
        }
    }
}

// ── Pixel helpers ─────────────────────────────────────────────────────────────

fn blend_px(px: &mut image::Rgb<u8>, [r, g, b]: [u8; 3], alpha: f32) {
    let bl = |base: u8, over: u8| -> u8 {
        ((base as f32) * (1.0 - alpha) + (over as f32) * alpha) as u8
    };
    px[0] = bl(px[0], r);
    px[1] = bl(px[1], g);
    px[2] = bl(px[2], b);
}
