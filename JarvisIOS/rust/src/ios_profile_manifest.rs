//! Parse `JARVIS_PROFILE_MANIFEST` (JSON from desktop `GET /jarvis-ios/v1/manifest`) into [`IosAvatarSettings`].
//!
//! Desktop embeds the full `[avatar]` table (`AvatarSettings`); we read only the fields iOS uses so
//! extra keys (`window_width`, `auto_load_spring_preset`, …) never break deserialization.
//!
//! **Note:** iOS staticlib builds often use a stub `tracing-oslog` layer that drops `tracing!` output.
//! Critical lines go through `jarvis_ios_line!` (in-app log + stderr).

use bevy::prelude::*;
use serde_json::Value;

/// Runtime avatar + scene tuning for the embedded iOS viewer (from hub manifest or defaults).
#[derive(Resource, Clone)]
pub struct IosAvatarSettings {
    pub model_path: String,
    pub idle_vrma_path: String,
    pub world_position: Vec3,
    pub uniform_scale: f32,
    pub lock_root_xz: bool,
    pub lock_root_y: bool,
    pub lock_vrm_root_y: bool,
    pub background_color: Color,
}

impl Default for IosAvatarSettings {
    fn default() -> Self {
        Self {
            model_path: "models/airi.vrm".into(),
            idle_vrma_path: String::new(),
            world_position: Vec3::ZERO,
            uniform_scale: 1.0,
            lock_root_xz: true,
            lock_root_y: true,
            lock_vrm_root_y: true,
            background_color: Color::linear_rgba(0.05, 0.05, 0.08, 1.0),
        }
    }
}

impl IosAvatarSettings {
    /// Load from `JARVIS_PROFILE_MANIFEST` path when set and valid; otherwise [`Default`].
    pub fn from_env_manifest_or_default() -> Self {
        try_from_manifest_env().unwrap_or_else(|| {
            let s = Self::default();
            crate::jarvis_ios_line!(
                "[JarvisIOS] manifest: using DEFAULT settings model_path={} (manifest missing or invalid)",
                s.model_path
            );
            s
        })
    }
}

fn try_from_manifest_env() -> Option<IosAvatarSettings> {
    let path = match std::env::var("JARVIS_PROFILE_MANIFEST") {
        Ok(p) => p,
        Err(_) => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: JARVIS_PROFILE_MANIFEST unset");
            return None;
        }
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: read failed path={path} err={e}");
            return None;
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: JSON parse failed err={e}");
            return None;
        }
    };
    let schema = match v.get("schema").and_then(|x| x.as_str()) {
        Some(s) => s,
        None => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: missing schema field");
            return None;
        }
    };
    if schema != "jarvis-ios.profile.v1" {
        crate::jarvis_ios_line!("[JarvisIOS] manifest: bad schema={schema:?}");
        warn!(
            "JarvisIOS: manifest schema {:?} is not jarvis-ios.profile.v1 — ignoring",
            schema
        );
        return None;
    }
    let av = match v.get("avatar") {
        Some(a) => a,
        None => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: missing avatar object");
            return None;
        }
    };
    match ios_avatar_from_manifest_value(av) {
        Some(s) => {
            crate::jarvis_ios_line!(
                "[JarvisIOS] manifest: OK model_path={} idle_vrma_path={}",
                s.model_path,
                s.idle_vrma_path
            );
            Some(s)
        }
        None => {
            crate::jarvis_ios_line!("[JarvisIOS] manifest: avatar object parse failed (model_path / types?)");
            None
        }
    }
}

/// Pull iOS-relevant fields from the desktop `avatar` object without requiring a 1:1 struct match.
fn ios_avatar_from_manifest_value(av: &Value) -> Option<IosAvatarSettings> {
    let model_path = av.get("model_path")?.as_str()?.to_owned();
    let idle_vrma_path = av
        .get("idle_vrma_path")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_owned();
    let world_position = f32_array3_or_default(av.get("world_position"), [0.0, 0.0, 0.0]);
    let uniform_scale = av
        .get("uniform_scale")
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .filter(|x| x.is_finite())
        .unwrap_or(1.0)
        .max(0.001);
    let lock_root_xz = av
        .get("lock_root_xz")
        .and_then(|x| x.as_bool())
        .unwrap_or(true);
    let lock_root_y = av
        .get("lock_root_y")
        .and_then(|x| x.as_bool())
        .unwrap_or(true);
    let lock_vrm_root_y = av
        .get("lock_vrm_root_y")
        .and_then(|x| x.as_bool())
        .unwrap_or(true);
    let bg_default = [0.05_f32, 0.05, 0.08, 1.0];
    let bc = f32_array4_or_default(av.get("background_color"), bg_default);
    Some(IosAvatarSettings {
        model_path,
        idle_vrma_path,
        world_position: Vec3::from_array(world_position),
        uniform_scale,
        lock_root_xz,
        lock_root_y,
        lock_vrm_root_y,
        background_color: Color::linear_rgba(bc[0], bc[1], bc[2], bc[3]),
    })
}

fn f32_array3_or_default(v: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    let Some(Value::Array(arr)) = v else {
        return default;
    };
    if arr.len() != 3 {
        return default;
    }
    let mut out = default;
    for (i, x) in arr.iter().enumerate().take(3) {
        out[i] = x.as_f64().unwrap_or(0.0) as f32;
    }
    out
}

fn f32_array4_or_default(v: Option<&Value>, default: [f32; 4]) -> [f32; 4] {
    let Some(Value::Array(arr)) = v else {
        return default;
    };
    if arr.len() != 4 {
        return default;
    }
    let mut out = default;
    for (i, x) in arr.iter().enumerate().take(4) {
        out[i] = x.as_f64().unwrap_or(0.0) as f32;
    }
    out
}
