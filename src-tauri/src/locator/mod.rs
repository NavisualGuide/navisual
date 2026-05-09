//! Element locator — Phase C.2.
//!
//! Finds UI elements on the user's screen. Primary strategy is Windows UI
//! Automation (A11y tree, < 5ms for most apps). OCR fallback lands in C.3.
//!
//! Returns bounding boxes in **physical pixels, virtual-desktop coordinates**
//! — same coordinate system as `capture::Rect` so the overlay renderer can
//! consume either without translation.

#[cfg(windows)]
pub mod a11y;

#[cfg(windows)]
pub mod ocr;

#[cfg(windows)]
pub mod orchestrator;

#[cfg(windows)]
pub mod hit_test;

pub mod trace;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LocateResult {
    /// Bounding box in physical pixels, virtual-desktop coords.
    pub bbox: crate::capture::Rect,
    /// Accessible name of the located element (for debugging/logging).
    pub name: String,
    /// UIA control type (e.g. "Button", "Hyperlink").
    pub role: String,
    /// 1.0 for A11y hits, < 1.0 for OCR (later).
    pub confidence: f32,
}
