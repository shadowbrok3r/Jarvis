//! Shared library surface for `jarvis-avatar` (config, protocol types, ACT parsing,
//! and the Bevy plugin stack shared by the desktop binary and the Android app).

pub mod a2f;
pub mod act;
pub mod arkit;
pub mod avatar_defaults;
pub mod config;
pub mod egui_theme;
pub mod emotions;
pub mod home_assistant;
pub mod ironclaw;
pub mod kokoro_http;
pub mod zeroclaw;
pub mod model_catalog;
pub mod paths;
pub mod pose_library;
pub mod egui_widgets;
pub mod icons;
pub mod theme;

// Bevy layer. `mcp` and `kimodo` are cross-referenced by `plugins`, so the three
// move together. Android drops `mcp` (rmcp server) — see `plugins/mod.rs` gates.
pub mod kimodo;
#[cfg(not(target_os = "android"))]
pub mod mcp;
pub mod plugins;