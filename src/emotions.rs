//! Emotion-to-avatar binding table.
//!
//! The assistant emits ACT tokens like `[ACT emotion="sensual"]`. We parse
//! the label out in [`crate::act`] and then look it up here: each entry
//! describes *what the avatar should do* when that emotion fires — a VRM
//! expression preset to set, an animation file from the pose library to
//! play, and how long to hold before decaying back.
//!
//! The table is persisted separately from `config/user.toml` because the
//! keys are dynamic (user-defined emotion strings). We keep it in
//! `config/emotions.json` alongside the MToon sidecar — easy to hand-edit,
//! easy to ship seeded with sensible defaults.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default relative path for the persisted emotion mapping file. Resolved
/// against the launcher's current directory the same way
/// `config/user.toml` is.
pub const DEFAULT_EMOTIONS_PATH: &str = "config/emotions.json";

/// One emotion → avatar action binding.
///
/// Every field is optional so a mapping can be "just an expression",
/// "just an animation", or both. The dispatcher short-circuits cleanly
/// when a field is missing.
///
/// Facial output is merged from [`Self::expression_blend`] (per-VRM-preset
/// weights) and the primary [`Self::expression`] / [`Self::expression_weight`]
/// pair; the primary pair wins on duplicate preset names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmotionBinding {
    /// Filename of the animation JSON in the pose library (e.g.
    /// `curious_tilt.json`). `None` = don't touch the animation layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<String>,
    /// VRM expression preset name (`happy`, `angry`, `thinking`, …).
    /// `None` = don't touch expressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    /// Weight 0..=1 used when `expression` is set. Defaults to 1.0.
    #[serde(default = "default_weight")]
    pub expression_weight: f32,
    /// Optional multi-preset mix (e.g. `aa` + `happy` + `blinkLeft`).
    /// Merged with [`Self::expression`] in [`Self::merged_expression_weights`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub expression_blend: HashMap<String, f32>,
    /// Whether the animation should loop. Falls back to the animation's
    /// own `looping` metadata when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub looping: Option<bool>,
    /// How long (seconds) to hold the face before letting the decay system
    /// return to neutral. Used whenever [`EmotionBinding::drives_expressions`]
    /// is true.
    #[serde(default = "default_hold")]
    pub hold_seconds: f32,
    /// Free-form note for the UI (why this mapping exists, which lines
    /// the assistant uses it on, etc.). Never read programmatically.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

fn default_weight() -> f32 {
    1.0
}
fn default_hold() -> f32 {
    2.5
}

impl EmotionBinding {
    /// Preset name → weight for VRM expressions. Starts from
    /// [`Self::expression_blend`], then applies [`Self::expression`] /
    /// [`Self::expression_weight`] so the primary column overrides the same key.
    pub fn merged_expression_weights(&self) -> HashMap<String, f32> {
        let mut m = self.expression_blend.clone();
        if let Some(ref name) = self.expression {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                m.insert(
                    trimmed.to_string(),
                    self.expression_weight.clamp(0.0, 1.0),
                );
            }
        }
        m
    }

    /// Whether ACT dispatch should drive the face for this binding.
    pub fn drives_expressions(&self) -> bool {
        !self.merged_expression_weights().is_empty()
    }
}

/// Serializable on-disk layout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmotionMapFile {
    /// Emotion label → binding. Keys are expected to be lower-cased —
    /// [`EmotionMap::resolve`] lowercases the lookup key as a safety net.
    #[serde(default)]
    pub mappings: HashMap<String, EmotionBinding>,
}

/// In-memory copy of the emotion map. Stored as a Bevy resource so the
/// dispatcher + debug UI both touch the same state.
#[derive(Debug, Clone, Default)]
pub struct EmotionMap {
    pub mappings: HashMap<String, EmotionBinding>,
    pub path: PathBuf,
    /// Last disk error for the UI to surface; cleared on successful IO.
    pub last_error: Option<String>,
}

impl EmotionMap {
    /// Load from `path`, falling back to the seed defaults if the file
    /// doesn't exist yet. IO errors are captured on `last_error` so the
    /// UI can surface them without panicking.
    pub fn load_or_default(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut out = Self {
            mappings: seed_defaults(),
            path: path.clone(),
            last_error: None,
        };
        if !path.exists() {
            return out;
        }
        match fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<EmotionMapFile>(&body) {
                Ok(file) => {
                    if !file.mappings.is_empty() {
                        out.mappings = file.mappings;
                    }
                }
                Err(e) => out.last_error = Some(format!("parse: {e}")),
            },
            Err(e) => out.last_error = Some(format!("read: {e}")),
        }
        out
    }

    /// Persist to [`Self::path`]. Creates the parent dir if missing.
    pub fn save(&mut self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let file = EmotionMapFile {
            mappings: self.mappings.clone(),
        };
        let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        match fs::write(&self.path, body) {
            Ok(()) => {
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.last_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Case-insensitive lookup. `None` = no binding; the caller should
    /// then fall back to the legacy `Emotion` enum if it wants to.
    pub fn resolve(&self, label: &str) -> Option<&EmotionBinding> {
        self.mappings.get(&label.trim().to_ascii_lowercase())
    }

    pub fn insert(&mut self, label: impl Into<String>, binding: EmotionBinding) {
        self.mappings
            .insert(label.into().trim().to_ascii_lowercase(), binding);
    }

    pub fn remove(&mut self, label: &str) {
        self.mappings.remove(&label.trim().to_ascii_lowercase());
    }

    /// Sorted key list, handy for the egui table.
    pub fn sorted_labels(&self) -> Vec<String> {
        let mut out: Vec<String> = self.mappings.keys().cloned().collect();
        out.sort();
        out
    }
}

/// Seed defaults that cover the AIRI built-in emotion set. Users can
/// overwrite any of these from the debug UI; the file replaces seeds
/// entirely once written.
fn seed_defaults() -> HashMap<String, EmotionBinding> {
    let mut m = HashMap::new();
    let expr = |name: &str| EmotionBinding {
        expression: Some(name.to_string()),
        expression_weight: 1.0,
        hold_seconds: 2.5,
        ..Default::default()
    };
    m.insert("happy".into(), expr("happy"));
    m.insert("sad".into(), expr("sad"));
    m.insert("angry".into(), expr("angry"));
    m.insert("surprised".into(), expr("surprised"));
    m.insert("curious".into(), expr("thinking"));
    m.insert("think".into(), expr("thinking"));
    m.insert("question".into(), expr("thinking"));
    m.insert("awkward".into(), expr("neutral"));
    m.insert("neutral".into(), expr("neutral"));
    // User-visible defaults for the emotions IronClaw actually emits today
    // but we have no canonical VRM preset for — leaves animation empty so
    // the user can wire a mapping in the UI.
    m.insert(
        "sensual".into(),
        EmotionBinding {
            expression: Some("happy".into()),
            expression_weight: 0.6,
            hold_seconds: 3.5,
            notes: "seeded default — configure animation to taste".into(),
            ..Default::default()
        },
    );
    m.insert(
        "flirty".into(),
        EmotionBinding {
            expression: Some("happy".into()),
            expression_weight: 0.7,
            hold_seconds: 2.8,
            notes: "seeded default".into(),
            ..Default::default()
        },
    );
    m
}

/// Helper so callers don't import `paths.rs` just to handle `~/…` in a
/// user-configured emotions path.
pub fn resolve_emotions_path(raw: impl AsRef<Path>) -> PathBuf {
    crate::paths::expand_home(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seed_defaults_contain_core() {
        let m = seed_defaults();
        assert!(m.contains_key("happy"));
        assert!(m.contains_key("curious"));
        assert!(m.contains_key("sensual"));
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("emotions.json");
        let mut map = EmotionMap::load_or_default(&path);
        map.insert(
            "playful",
            EmotionBinding {
                expression: Some("happy".into()),
                expression_weight: 0.9,
                expression_blend: HashMap::from([
                    ("aa".into(), 0.4_f32),
                    ("blinkLeft".into(), 0.15_f32),
                ]),
                animation: Some("bounce.json".into()),
                hold_seconds: 2.0,
                looping: Some(false),
                notes: "rt".into(),
            },
        );
        map.save().unwrap();
        let loaded = EmotionMap::load_or_default(&path);
        let b = loaded.resolve("playful").unwrap();
        assert_eq!(b.expression.as_deref(), Some("happy"));
        assert_eq!(b.animation.as_deref(), Some("bounce.json"));
        let merged = b.merged_expression_weights();
        assert!((merged["happy"] - 0.9).abs() < 1e-6);
        assert!((merged["aa"] - 0.4).abs() < 1e-6);
        assert!((merged["blinkLeft"] - 0.15).abs() < 1e-6);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let map = EmotionMap {
            mappings: seed_defaults(),
            ..Default::default()
        };
        assert!(map.resolve("HAPPY").is_some());
        assert!(map.resolve(" Curious ").is_some());
    }
}
