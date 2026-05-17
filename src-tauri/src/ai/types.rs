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
    #[serde(default = "default_overlay")]
    pub overlay_type: OverlayType,
    #[serde(default, deserialize_with = "lax_option")]
    pub clipboard: Option<String>,
    #[serde(default = "default_true")]
    pub checkpoint: bool,
    /// Bounding box returned by the AI as `[ymin, xmin, ymax, xmax]`. Drives
    /// the locator (A11y proximity sort + OCR overlap filter) and the
    /// developer "show AI bbox" overlay.
    ///
    /// Coordinate system depends on the provider:
    /// - Gemini: normalized 0–1000
    /// - Others: absolute pixels of the AI-image
    ///
    /// Converted to virtual-desktop screen coords by the backend before use.
    #[serde(default)]
    pub target_bbox: Option<[f64; 4]>,
}

fn default_overlay() -> OverlayType {
    OverlayType::Arrow
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
