//! 16×9 Set-of-Marks grid helpers for AI locator test mode.
//!
//! When GRID_TEST_ENABLED=true, each screenshot sent to the AI has a faint
//! 16-column × 9-row grid drawn over it. The AI is asked to return the cell
//! label (e.g. "D7") that contains the target element. This lets testers
//! compare AI-reported cells against the visible grid on the overlay.

use image::{codecs::jpeg::JpegEncoder, ColorType};

pub const COLS: u32 = 16;
pub const ROWS: u32 = 9;

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

/// Overlay a 16×9 grid onto a JPEG image and return new JPEG bytes.
/// Lines are light grey at ~55% opacity so the underlying UI remains readable.
pub fn overlay_grid_on_jpeg(jpeg_bytes: &[u8], quality: u8) -> Vec<u8> {
    let img = match image::load_from_memory(jpeg_bytes) {
        Ok(i) => i,
        Err(_) => return jpeg_bytes.to_vec(),
    };
    let mut rgb = img.to_rgb8();
    let w = rgb.width();
    let h = rgb.height();

    // Vertical column separators
    for col in 1..COLS {
        let x = (col * w / COLS).min(w - 1);
        for y in 0..h {
            blend_px(rgb.get_pixel_mut(x, y), [190, 190, 190], 0.55);
        }
    }
    // Horizontal row separators
    for row in 1..ROWS {
        let y = (row * h / ROWS).min(h - 1);
        for x in 0..w {
            blend_px(rgb.get_pixel_mut(x, y), [190, 190, 190], 0.55);
        }
    }

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    let _ = encoder.encode(rgb.as_raw(), w, h, ColorType::Rgb8.into());
    if out.is_empty() {
        jpeg_bytes.to_vec()
    } else {
        out
    }
}

fn blend_px(px: &mut image::Rgb<u8>, [r, g, b]: [u8; 3], alpha: f32) {
    let bl = |base: u8, over: u8| -> u8 {
        ((base as f32) * (1.0 - alpha) + (over as f32) * alpha) as u8
    };
    px[0] = bl(px[0], r);
    px[1] = bl(px[1], g);
    px[2] = bl(px[2], b);
}
