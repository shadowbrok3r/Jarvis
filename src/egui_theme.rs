//! Serialized [`egui::Style`] for desktop (`debug_ui`) and JarvisIOS (embedded `bevy_egui`).
//! Source of truth: `assets/egui_jarvis_theme.json` at the workspace root.

pub const STYLE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/egui_jarvis_theme.json"));
