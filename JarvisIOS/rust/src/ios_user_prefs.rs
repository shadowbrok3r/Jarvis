//! On-device user preferences (Application Support), keyed by VRM model stem.
//!
//! Swift sets `JARVIS_IOS_USER_PREFS_DIR` before Bevy boots. Hub manifest values apply
//! first; saved prefs in this directory override them for the active model.

use std::path::{Path, PathBuf};

use crate::ios_material_visibility::IosMaterialVisibilityStore;

const DEFAULT_LOG_VERBOSITY_FILE: &str = "default_log_verbosity.txt";

fn prefs_dir() -> Option<PathBuf> {
    std::env::var("JARVIS_IOS_USER_PREFS_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn model_stem(model_path: &str) -> String {
    Path::new(model_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string()
}

fn material_visibility_path(model_path: &str) -> Option<PathBuf> {
    let dir = prefs_dir()?.join("material_visibility");
    Some(dir.join(format!("{}.json", model_stem(model_path))))
}

/// Saved hidden-material JSON for `model_path`, if present.
pub fn load_material_visibility_json(model_path: &str) -> Option<String> {
    let path = material_visibility_path(model_path)?;
    std::fs::read_to_string(path).ok()
}

pub fn save_material_visibility(model_path: &str, store: &IosMaterialVisibilityStore) -> bool {
    let Some(path) = material_visibility_path(model_path) else {
        crate::jarvis_ios_line!("[JarvisIOS] prefs: JARVIS_IOS_USER_PREFS_DIR unset — cannot save material visibility");
        return false;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            crate::jarvis_ios_line!("[JarvisIOS] prefs: mkdir {} failed: {e}", parent.display());
            return false;
        }
    }
    let body = store.to_json_string();
    match std::fs::write(&path, body) {
        Ok(()) => {
            crate::jarvis_ios_line!(
                "[JarvisIOS] prefs: saved material visibility → {}",
                path.display()
            );
            true
        }
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] prefs: save material visibility failed: {e}");
            false
        }
    }
}

/// Boot default log level (`0`–`3`) when env `JARVIS_IOS_LOG_VERBOSITY` is unset.
pub fn load_default_log_verbosity() -> Option<u8> {
    let path = prefs_dir()?.join(DEFAULT_LOG_VERBOSITY_FILE);
    let raw = std::fs::read_to_string(path).ok()?;
    parse_log_verbosity_token(raw.trim())
}

pub fn save_default_log_verbosity(level: u8) -> bool {
    let Some(dir) = prefs_dir() else {
        crate::jarvis_ios_line!("[JarvisIOS] prefs: JARVIS_IOS_USER_PREFS_DIR unset — cannot save log default");
        return false;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::jarvis_ios_line!("[JarvisIOS] prefs: mkdir {} failed: {e}", dir.display());
        return false;
    }
    let path = dir.join(DEFAULT_LOG_VERBOSITY_FILE);
    match std::fs::write(&path, level.to_string()) {
        Ok(()) => {
            crate::jarvis_ios_line!("[JarvisIOS] prefs: saved default log verbosity={level}");
            true
        }
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] prefs: save log default failed: {e}");
            false
        }
    }
}

fn parse_log_verbosity_token(t: &str) -> Option<u8> {
    if let Ok(n) = t.parse::<u8>() {
        return (n <= 3).then_some(n);
    }
    Some(match t.to_ascii_lowercase().as_str() {
        "off" => 0,
        "quiet" => 1,
        "normal" => 2,
        "debug" => 3,
        _ => return None,
    })
}

/// Hub manifest first, then on-device override for `model_path`.
pub fn material_visibility_store_for_model(
    manifest_json: Option<&str>,
    model_path: &str,
) -> IosMaterialVisibilityStore {
    let mut store = IosMaterialVisibilityStore::from_json(manifest_json);
    if let Some(saved) = load_material_visibility_json(model_path) {
        store.load_from_json_str(&saved);
    }
    store
}
