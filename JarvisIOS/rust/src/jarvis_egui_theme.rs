//! Same JSON as desktop `jarvis_avatar::egui_theme` / `assets/egui_jarvis_theme.json`.

pub const STYLE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/egui_jarvis_theme.json"
));
