use serde::{Deserialize, Serialize};

/// Lax `Option<T>` deserializer used on AI-response fields that models
/// commonly emit as `""` or as a non-enum string instead of omitting them.
///
/// Behaviour: JSON `null` → `None`; JSON `""` (empty / whitespace) → `None`;
/// any other value that fails to deserialize → `None` (swallowed, never
/// errors); anything that parses cleanly → `Some(T)`.
///
/// This stops a single malformed field (e.g. `"target_role": ""` instead of
/// a valid enum variant) from blowing up the whole response and dropping the
/// user into the raw-JSON-as-instruction fallback path.
fn lax_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(ref s) if s.trim().is_empty() => Ok(None),
        v => Ok(serde_json::from_value::<T>(v).ok()),
    }
}

/// Lax bbox deserializer. The prompt requests `[ymin, xmin, ymax, xmax]` (four
/// numbers), but some models (notably GPT) return a 4-corner polygon
/// `[[a,b],[a,b],[a,b],[a,b]]` instead. Accept either — normalising a polygon to
/// its bounding box while preserving the model's axis order — and fall back to
/// `None` on anything malformed, so a weird bbox never drops the whole response
/// into the raw-JSON-as-instruction fallback path (see [lax_option]).
fn lax_bbox<'de, D>(deserializer: D) -> Result<Option<[f64; 4]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(bbox_from_value(&value))
}

/// Best-effort `[f64; 4]` from arbitrary JSON; `None` (never an error) when the
/// value can't be read as a box.
fn bbox_from_value(value: &serde_json::Value) -> Option<[f64; 4]> {
    let arr = value.as_array()?;
    // Canonical form: four plain numbers, used as-is.
    if arr.len() == 4 && arr.iter().all(serde_json::Value::is_number) {
        let n: Vec<f64> = arr.iter().filter_map(serde_json::Value::as_f64).collect();
        if n.len() == 4 {
            return Some([n[0], n[1], n[2], n[3]]);
        }
    }
    // Polygon form: corner points `[a, b]` → bounding box. Each point uses the
    // same axis order as the requested format, so the box is just
    // [min a, min b, max a, max b].
    let mut a = Vec::with_capacity(arr.len());
    let mut b = Vec::with_capacity(arr.len());
    for pt in arr {
        let p = pt.as_array()?;
        if p.len() < 2 {
            return None;
        }
        a.push(p[0].as_f64()?);
        b.push(p[1].as_f64()?);
    }
    if a.is_empty() {
        return None;
    }
    let amin = a.iter().copied().fold(f64::INFINITY, f64::min);
    let amax = a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let bmin = b.iter().copied().fold(f64::INFINITY, f64::min);
    let bmax = b.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some([amin, bmin, amax, bmax])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayType {
    Arrow,
    Highlight,
    Circle,
    Subtitle,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetRole {
    Button,
    Tab,
    Link,
    Textbox,
    Menuitem,
    Checkbox,
    Radio,
    Combobox,
    Slider,
    Image,
    Heading,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetRegion {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidanceStep {
    pub instruction: String,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_text: Option<String>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_role: Option<TargetRole>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_region: Option<TargetRegion>,
    #[serde(default, deserialize_with = "lax_option")]
    pub target_nearby_text: Option<String>,
    #[serde(default = "default_overlay", deserialize_with = "lax_overlay")]
    pub overlay_type: OverlayType,
    #[serde(default, deserialize_with = "lax_option")]
    pub clipboard: Option<String>,
    #[serde(default = "default_true")]
    pub checkpoint: bool,
    /// Bounding box returned by the AI as `[ymin, xmin, ymax, xmax]`. Drives
    /// the locator (A11y proximity sort + OCR overlap filter) and the
    /// developer "show AI bbox" overlay.
    ///
    /// All providers are instructed to use normalized 0–1000 coordinates
    /// (resolution-independent). Converted to virtual-desktop screen coords by
    /// the backend before use.
    #[serde(default, deserialize_with = "lax_bbox")]
    pub target_bbox: Option<[f64; 4]>,
}

fn default_overlay() -> OverlayType {
    OverlayType::Arrow
}

/// Lax `OverlayType` deserializer: an unrecognised value (e.g. a model inventing
/// `"pointer"`) falls back to the default instead of failing the whole response
/// parse — which on the json-object providers (OpenAI/DeepSeek/Qwen) would drop
/// into the raw-JSON-as-instruction path. Mirrors [`lax_option`]; closes the
/// last un-lax field on [`GuidanceStep`].
fn lax_overlay<'de, D>(deserializer: D) -> Result<OverlayType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<OverlayType>(value).unwrap_or_else(|_| default_overlay()))
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigateStepResponse {
    pub steps: Vec<GuidanceStep>,
    #[serde(default)]
    pub state_summary: String,
    #[serde(default)]
    pub needs_input: bool,
    #[serde(default)]
    pub request_full_screen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(json: &str) -> GuidanceStep {
        serde_json::from_str(json).expect("GuidanceStep must deserialize")
    }

    #[test]
    fn minimal_step_gets_defaults() {
        let s = step(r#"{"instruction": "Click the button"}"#);
        assert!(s.checkpoint, "checkpoint defaults to true");
        assert!(matches!(s.overlay_type, OverlayType::Arrow));
        assert!(s.target_text.is_none());
        assert!(s.target_bbox.is_none());
    }

    #[test]
    fn invented_overlay_type_falls_back_to_arrow() {
        // A model inventing "pointer" must not fail the whole response parse.
        let s = step(r#"{"instruction": "x", "overlay_type": "pointer"}"#);
        assert!(matches!(s.overlay_type, OverlayType::Arrow));
    }

    #[test]
    fn empty_or_invalid_role_becomes_none() {
        let s = step(r#"{"instruction": "x", "target_role": ""}"#);
        assert!(s.target_role.is_none());
        let s = step(r#"{"instruction": "x", "target_role": "button-like-thing"}"#);
        assert!(s.target_role.is_none());
        let s = step(r#"{"instruction": "x", "target_role": "button"}"#);
        assert!(matches!(s.target_role, Some(TargetRole::Button)));
    }

    #[test]
    fn bbox_accepts_canonical_and_polygon_forms() {
        let s = step(r#"{"instruction": "x", "target_bbox": [80, 450, 110, 550]}"#);
        assert_eq!(s.target_bbox, Some([80.0, 450.0, 110.0, 550.0]));

        // GPT-style 4-corner polygon → normalized to its bounding box.
        let s = step(
            r#"{"instruction": "x", "target_bbox": [[80, 450], [80, 550], [110, 450], [110, 550]]}"#,
        );
        assert_eq!(s.target_bbox, Some([80.0, 450.0, 110.0, 550.0]));
    }

    #[test]
    fn malformed_bbox_becomes_none_not_error() {
        for bad in [
            r#"{"instruction": "x", "target_bbox": "top left"}"#,
            r#"{"instruction": "x", "target_bbox": [1, 2, 3]}"#,
            r#"{"instruction": "x", "target_bbox": null}"#,
        ] {
            let s = step(bad);
            assert!(s.target_bbox.is_none(), "should be None for: {bad}");
        }
    }

    #[test]
    fn response_optional_fields_default() {
        let r: NavigateStepResponse =
            serde_json::from_str(r#"{"steps": [{"instruction": "x"}]}"#).unwrap();
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.state_summary, "");
        assert!(!r.needs_input);
        assert!(!r.request_full_screen);
    }
}
