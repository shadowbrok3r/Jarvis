//! Per-VRM avatar defaults persisted beside other ModelOverrides sidecars.
//!
//! Stored at `config/ModelOverrides/{stem}/avatar_defaults.json` where `{stem}`
//! is the VRM filename without extension (same rule as MToon / material overrides).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarDefaultsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Resting VRM expression weights applied on load (`SetExpressions` semantics).
    #[serde(default)]
    pub expressions: HashMap<String, f32>,
    /// Optional pose-library pose applied after expressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_pose: Option<String>,
    /// Named layer set from `config/anim_layer_sets.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_set: Option<String>,
    /// Animation-library JSON clip (e.g. `idle_loop.json`) used as the base
    /// body layer when `idle_use_layer_stack` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_clip: Option<String>,
    #[serde(default = "default_true")]
    pub idle_clip_looping: bool,
    /// When true, idle motion is driven by the layer stack clip instead of the
    /// VRMA `AnimationPlayer` child spawned from `[avatar].idle_vrma_path`.
    #[serde(default)]
    pub idle_use_layer_stack: bool,
    #[serde(default = "default_true")]
    pub apply_expressions_on_load: bool,
}

impl Default for AvatarDefaultsFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            expressions: HashMap::new(),
            rest_pose: None,
            layer_set: None,
            idle_clip: None,
            idle_clip_looping: true,
            idle_use_layer_stack: false,
            apply_expressions_on_load: true,
        }
    }
}

fn default_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// File stem for per-VRM override folders (`3` for both `models/3.vrm` and `models/3.ios.vrm`).
pub fn vrm_model_stem(model_path: &str) -> String {
    let stem = std::path::Path::new(model_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    if stem.ends_with(".ios") {
        stem[..stem.len().saturating_sub(4)].to_string()
    } else {
        stem.to_string()
    }
}

pub fn vrm_model_overrides_dir(model_path: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_default()
        .join("config")
        .join("ModelOverrides")
        .join(vrm_model_stem(model_path))
}

pub fn avatar_defaults_path(model_path: &str) -> PathBuf {
    vrm_model_overrides_dir(model_path).join("avatar_defaults.json")
}

pub fn load_avatar_defaults(model_path: &str) -> Option<AvatarDefaultsFile> {
    let path = avatar_defaults_path(model_path);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_avatar_defaults(model_path: &str, file: &AvatarDefaultsFile) -> Result<PathBuf, String> {
    let path = avatar_defaults_path(model_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body =
        serde_json::to_string_pretty(file).map_err(|e| format!("serialize avatar defaults: {e}"))?;
    fs::write(&path, &body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

pub fn delete_avatar_defaults(model_path: &str) -> Result<(), String> {
    let path = avatar_defaults_path(model_path);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("delete {}: {e}", path.display())),
    }
}
