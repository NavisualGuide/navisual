use serde::{Deserialize, Serialize};

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
    pub target_text: Option<String>,
    pub target_role: Option<TargetRole>,
    pub target_region: Option<TargetRegion>,
    pub target_nearby_text: Option<String>,
    #[serde(default = "default_overlay")]
    pub overlay_type: OverlayType,
    pub clipboard: Option<String>,
    #[serde(default = "default_true")]
    pub checkpoint: bool,
    /// Cell label (e.g. "D7") for the target element: row A–I (top→bottom),
    /// col 1–16 (left→right). Used by the locator as the zone hint.
    pub grid_cell: Option<String>,
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
