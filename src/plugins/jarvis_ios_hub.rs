//! HTTP surface for **JarvisIOS** ↔ desktop profile sync on the channel hub (same port as `/ws`).
//!
//! Routes live under **`/jarvis-ios/v1/`**. If `[ironclaw].auth_token` is non-empty, clients must
//! send **`Authorization: Bearer <token>`** (same value as WS `module:authenticate`). Empty token
//! keeps routes open for local dev (matches hub WS behaviour).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use jarvis_avatar::config::{AvatarSettings, CameraSettings, Settings};

use super::spring_preset::{default_preset_path_for_logical_path, vrm_preset_key};

/// Snapshot served over HTTP until Bevy can bump revisions when settings change.
#[derive(Debug, Clone)]
pub struct JarvisIosHubProfile {
    /// Bumped when desktop settings change (hub thread will refresh manifest in a later iteration).
    #[allow(dead_code)]
    pub revision: u64,
    manifest: Value,
}

impl JarvisIosHubProfile {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            revision: 1,
            manifest: build_manifest_value(settings, 1),
        }
    }

    pub fn manifest_json(&self) -> Value {
        self.manifest.clone()
    }
}

/// Subset of `[graphics]` safe to mirror on a phone (no secrets).
#[derive(Debug, Clone, Serialize)]
pub struct JarvisIosGraphicsLite {
    pub msaa_samples: u32,
    pub present_mode: String,
    pub hdr: bool,
    pub exposure_ev100: f32,
    pub ambient_brightness: f32,
    pub ambient_color: [f32; 4],
    pub directional_illuminance: f32,
    pub directional_shadows: bool,
    pub directional_position: [f32; 3],
    pub show_ground_plane: bool,
    pub ground_size: f32,
    pub ground_base_color: [f32; 3],
}

#[derive(Debug, Clone, Serialize)]
pub struct JarvisIosAssetRef {
    /// e.g. `vrm`, `idle_vrma`
    pub role: String,
    /// Path relative to the repo `assets/` root (same as `AssetServer` / `[avatar].model_path`).
    pub path: String,
    /// HTTP path on the hub (GET with `Authorization` when token is set).
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JarvisIosSpringPresetRef {
    pub vrm_key: String,
    pub logical_vrm_path: String,
    /// Basename `xxxxxxxxxxxxxxxx.toml` under `config/spring_presets/`.
    pub filename: String,
    pub url: String,
    /// Inlined TOML when the file exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toml: Option<String>,
}

fn build_manifest_value(settings: &Settings, revision: u64) -> Value {
    let logical = settings.avatar.model_path.clone();
    let vrm_key = vrm_preset_key(&logical);
    let profile_id = format!("vrm:{vrm_key}");

    let mut assets: Vec<JarvisIosAssetRef> = vec![JarvisIosAssetRef {
        role: "vrm".into(),
        path: settings.avatar.model_path.clone(),
        url: format!("/jarvis-ios/v1/asset/{}", settings.avatar.model_path),
    }];

    if !settings.avatar.idle_vrma_path.trim().is_empty() {
        assets.push(JarvisIosAssetRef {
            role: "idle_vrma".into(),
            path: settings.avatar.idle_vrma_path.clone(),
            url: format!("/jarvis-ios/v1/asset/{}", settings.avatar.idle_vrma_path),
        });
    }

    let root = assets_root();
    const MAX_SYNC_JSON: usize = 500;
    for rel in collect_json_assets_under(&root.join("animations"), &root, MAX_SYNC_JSON) {
        assets.push(JarvisIosAssetRef {
            role: "anim_json".into(),
            path: rel.clone(),
            url: format!("/jarvis-ios/v1/asset/{rel}"),
        });
    }
    for rel in collect_json_assets_under(&root.join("poses"), &root, MAX_SYNC_JSON) {
        assets.push(JarvisIosAssetRef {
            role: "pose_json".into(),
            path: rel.clone(),
            url: format!("/jarvis-ios/v1/asset/{rel}"),
        });
    }
    if resolve_asset_file("config/emotions.json").is_some() {
        assets.push(JarvisIosAssetRef {
            role: "emotions".into(),
            path: "config/emotions.json".into(),
            url: "/jarvis-ios/v1/asset/config/emotions.json".into(),
        });
    }

    let preset_path = default_preset_path_for_logical_path(None, &settings.avatar.model_path);
    let spring = preset_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|_| preset_path.is_file())
        .map(|filename| {
            let toml = std::fs::read_to_string(&preset_path).ok();
            JarvisIosSpringPresetRef {
                vrm_key: vrm_key.clone(),
                logical_vrm_path: logical.clone(),
                filename: filename.to_string(),
                url: format!("/jarvis-ios/v1/config/spring-presets/{filename}"),
                toml,
            }
        });

    let graphics = JarvisIosGraphicsLite {
        msaa_samples: settings.graphics.msaa_samples,
        present_mode: settings.graphics.present_mode.clone(),
        hdr: settings.graphics.hdr,
        exposure_ev100: settings.graphics.exposure_ev100,
        ambient_brightness: settings.graphics.ambient_brightness,
        ambient_color: settings.graphics.ambient_color,
        directional_illuminance: settings.graphics.directional_illuminance,
        directional_shadows: settings.graphics.directional_shadows,
        directional_position: settings.graphics.directional_position,
        show_ground_plane: settings.graphics.show_ground_plane,
        ground_size: settings.graphics.ground_size,
        ground_base_color: settings.graphics.ground_base_color,
    };

    serde_json::to_value(ManifestDto {
        schema: "jarvis-ios.profile.v1",
        profile_id,
        revision,
        desktop_module: settings.ironclaw.module_name.clone(),
        avatar: settings.avatar.clone(),
        camera: settings.camera.clone(),
        graphics,
        assets,
        spring_preset: spring,
    })
    .unwrap_or_else(|_| json!({ "schema": "jarvis-ios.profile.v1", "error": "serialize_failed" }))
}

#[derive(Serialize)]
struct ManifestDto {
    schema: &'static str,
    profile_id: String,
    revision: u64,
    desktop_module: String,
    avatar: AvatarSettings,
    camera: CameraSettings,
    graphics: JarvisIosGraphicsLite,
    assets: Vec<JarvisIosAssetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spring_preset: Option<JarvisIosSpringPresetRef>,
}

/// `Authorization: Bearer …` must match `expected` when `expected` is non-empty.
pub fn http_authorized(expected: &str, auth_header: Option<&str>) -> bool {
    if expected.is_empty() {
        return true;
    }
    let Some(raw) = auth_header else {
        return false;
    };
    let Some(token) = raw.strip_prefix("Bearer ") else {
        return false;
    };
    token == expected
}

pub fn is_safe_assets_rel(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') {
        return false;
    }
    for c in Path::new(rel).components() {
        if matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::RootDir
        ) {
            return false;
        }
    }
    true
}

pub fn assets_root() -> PathBuf {
    std::env::current_dir().unwrap_or_default().join("assets")
}

pub fn resolve_asset_file(rel: &str) -> Option<PathBuf> {
    if !is_safe_assets_rel(rel) {
        return None;
    }
    // Allow syncing desktop `config/emotions.json` (not under `./assets/`).
    if rel == "config/emotions.json" {
        let p = std::env::current_dir().ok()?.join(rel);
        return p.is_file().then_some(p);
    }
    let p = assets_root().join(rel);
    p.is_file().then_some(p)
}

/// Collect JSON files under `dir` (recursive), returning paths relative to `assets_root()`.
fn collect_json_assets_under(dir: &Path, assets_root: &Path, max_files: usize) -> Vec<String> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if out.len() >= max_files {
                return out;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("json"))
            {
                if let Ok(rel) = p.strip_prefix(assets_root) {
                    let rel_s = rel.to_string_lossy().replace('\\', "/");
                    if is_safe_assets_rel(&rel_s) {
                        out.push(rel_s);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Only `xxxxxxxxxxxxxxxx.toml` (16 lowercase hex + `.toml`).
pub fn is_safe_spring_preset_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".toml") else {
        return false;
    };
    stem.len() == 16 && stem.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn resolve_spring_preset_file(name: &str) -> Option<PathBuf> {
    if !is_safe_spring_preset_filename(name) {
        return None;
    }
    let root = std::env::current_dir().ok()?;
    let p = root.join("config").join("spring_presets").join(name);
    p.is_file().then_some(p)
}

pub fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "vrm" | "glb" => "application/octet-stream",
        "gltf" | "json" => "application/json",
        "vrma" => "application/octet-stream",
        "toml" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}
