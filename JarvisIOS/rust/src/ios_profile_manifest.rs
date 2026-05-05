//! Parse `JARVIS_PROFILE_MANIFEST` (JSON from desktop `GET /jarvis-ios/v1/manifest`) into iOS runtime resources.
//!
//! Desktop embeds the full `[avatar]` table (`AvatarSettings`); we read only the fields iOS uses so
//! extra keys (`window_width`, `auto_load_spring_preset`, …) never break deserialization.
//! The `graphics` object mirrors the hub JSON struct `JarvisIosGraphicsLite` (`src/plugins/jarvis_ios_hub.rs` on desktop).
//!
//! **Note:** iOS staticlib builds often use a stub `tracing-oslog` layer that drops `tracing!` output.
//! Critical lines go through `jarvis_ios_line!` (in-app log + stderr).

use std::path::Path;

use bevy::prelude::*;
use serde_json::Value;

use crate::ios_graphics::{graphics_from_manifest_value, IosGraphicsSettings};

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
    /// When false, ignore inlined `spring_preset.toml` from the hub manifest (matches desktop `[avatar]`).
    pub auto_load_spring_preset: bool,
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
            auto_load_spring_preset: false,
        }
    }
}

/// Inlined `spring_preset.toml` from the hub manifest (desktop may embed full preset text).
#[derive(Resource, Clone, Default)]
pub struct IosSpringPresetToml(pub Option<String>);

/// Full hub profile: avatar, graphics snapshot, optional spring preset TOML to apply after VRM init.
///
/// After reading the manifest, applies optional `JARVIS_IOS_MODEL_PATH` / `JARVIS_IOS_IDLE_VRMA_PATH`
/// (relative paths under `JARVIS_ASSET_ROOT`, set from the iOS About screen) so the user can pick any
/// `.vrm` synced into the hub cache without editing the JSON on the server.
///
/// Returns `(avatar, graphics, spring_toml, mtoon_overrides_json)`.
pub fn load_ios_hub_profile_bundle_from_env() -> (IosAvatarSettings, IosGraphicsSettings, Option<String>, Option<String>) {
    let (avatar, graphics, spring, mtoon) = load_ios_hub_profile_bundle_from_env_inner();
    let avatar = apply_ios_env_model_overrides(avatar);
    let (avatar, graphics) = apply_ios_env_scene_env_overrides(avatar, graphics);
    (avatar, graphics, spring, mtoon)
}

fn load_ios_hub_profile_bundle_from_env_inner() -> (IosAvatarSettings, IosGraphicsSettings, Option<String>, Option<String>) {
    let Some(path) = std::env::var("JARVIS_PROFILE_MANIFEST").ok() else {
        crate::jarvis_ios_line!("[JarvisIOS] profile bundle: JARVIS_PROFILE_MANIFEST unset → defaults");
        return (
            IosAvatarSettings::default(),
            IosGraphicsSettings::default(),
            None,
            None,
        );
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] profile bundle: read failed path={path} err={e}");
            return (
                IosAvatarSettings::default(),
                IosGraphicsSettings::default(),
                None,
                None,
            );
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] profile bundle: JSON parse failed err={e}");
            return (
                IosAvatarSettings::default(),
                IosGraphicsSettings::default(),
                None,
                None,
            );
        }
    };
    if let Some((avatar, graphics, spring, mtoon)) = parse_profile_manifest_value(&v) {
        (avatar, graphics, spring, mtoon)
    } else {
        (
            IosAvatarSettings::default(),
            IosGraphicsSettings::default(),
            None,
            None,
        )
    }
}

fn ios_safe_rel_path(rel: &str) -> bool {
    let t = rel.trim();
    !t.is_empty() && !t.starts_with('/') && !t.contains("..")
}

fn apply_ios_env_model_overrides(mut avatar: IosAvatarSettings) -> IosAvatarSettings {
    let asset_root = std::env::var("JARVIS_ASSET_ROOT").unwrap_or_default();
    if let Ok(p) = std::env::var("JARVIS_IOS_MODEL_PATH") {
        let t = p.trim();
        if ios_safe_rel_path(t) {
            let disk = Path::new(asset_root.trim()).join(t);
            if disk.is_file() {
                crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_MODEL_PATH override → {t}");
                avatar.model_path = t.to_string();
            } else {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] env JARVIS_IOS_MODEL_PATH ignored (file missing): {}",
                    disk.display()
                );
            }
        } else if !t.is_empty() {
            crate::jarvis_ios_line!(
                "[JarvisIOS] env JARVIS_IOS_MODEL_PATH ignored (unsafe path): {t:?}"
            );
        }
    }
    if let Ok(p) = std::env::var("JARVIS_IOS_IDLE_VRMA_PATH") {
        let t = p.trim();
        if t.is_empty() {
            avatar.idle_vrma_path.clear();
            crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_IDLE_VRMA_PATH override → (cleared)");
        } else if ios_safe_rel_path(t) {
            let disk = Path::new(asset_root.trim()).join(t);
            if disk.is_file() {
                crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_IDLE_VRMA_PATH override → {t}");
                avatar.idle_vrma_path = t.to_string();
            } else {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] env JARVIS_IOS_IDLE_VRMA_PATH ignored (file missing): {}",
                    disk.display()
                );
            }
        } else {
            crate::jarvis_ios_line!(
                "[JarvisIOS] env JARVIS_IOS_IDLE_VRMA_PATH ignored (unsafe path): {t:?}"
            );
        }
    }
    avatar
}

/// Optional `JARVIS_IOS_SHOW_GROUND` (`0`/`1`/`true`/`false`) and `JARVIS_IOS_BACKGROUND_LINEAR` (`r,g,b,a` linear floats).
fn apply_ios_env_scene_env_overrides(
    mut avatar: IosAvatarSettings,
    mut graphics: IosGraphicsSettings,
) -> (IosAvatarSettings, IosGraphicsSettings) {
    if let Ok(v) = std::env::var("JARVIS_IOS_SHOW_GROUND") {
        let t = v.trim();
        if t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("on") {
            graphics.show_ground_plane = true;
            crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_SHOW_GROUND → force show");
        } else if t == "0" || t.eq_ignore_ascii_case("false") || t.eq_ignore_ascii_case("off") {
            graphics.show_ground_plane = false;
            crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_SHOW_GROUND → force hide");
        }
    }
    if let Ok(v) = std::env::var("JARVIS_IOS_BACKGROUND_LINEAR") {
        let t = v.trim();
        if let Some(c) = parse_linear_rgba_csv(t) {
            avatar.background_color = c;
            crate::jarvis_ios_line!("[JarvisIOS] env JARVIS_IOS_BACKGROUND_LINEAR override");
        } else if !t.is_empty() {
            crate::jarvis_ios_line!(
                "[JarvisIOS] env JARVIS_IOS_BACKGROUND_LINEAR ignored (expected r,g,b,a linear floats): {t:?}"
            );
        }
    }
    (avatar, graphics)
}

fn parse_linear_rgba_csv(s: &str) -> Option<Color> {
    let parts: Vec<&str> = s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
    if parts.len() != 4 {
        return None;
    }
    let mut c = [0.0_f32; 4];
    for (i, p) in parts.iter().take(4).enumerate() {
        c[i] = p.parse().ok()?;
    }
    if !c.iter().all(|x| x.is_finite()) {
        return None;
    }
    Some(Color::linear_rgba(c[0], c[1], c[2], c[3]))
}

fn parse_profile_manifest_value(
    v: &Value,
) -> Option<(IosAvatarSettings, IosGraphicsSettings, Option<String>, Option<String>)> {
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
    let avatar = ios_avatar_from_manifest_value(av)?;
    let graphics = graphics_from_manifest_value(v.get("graphics"));
    let mut spring = v
        .get("spring_preset")
        .and_then(|sp| sp.get("toml"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_owned());
    if !avatar.auto_load_spring_preset {
        spring = None;
    }
    let mtoon_overrides_json = v
        .get("mtoon_overrides_json")
        .and_then(|x| x.as_str())
        .map(|s| s.to_owned());
    crate::jarvis_ios_line!(
        "[JarvisIOS] manifest: OK model_path={} idle_vrma_path={} msaa={} spring_toml={} auto_load_spring={} mtoon_overrides={}",
        avatar.model_path,
        avatar.idle_vrma_path,
        graphics.msaa_samples,
        if spring.is_some() { "yes" } else { "no" },
        avatar.auto_load_spring_preset,
        if mtoon_overrides_json.is_some() { "yes" } else { "no" },
    );
    Some((avatar, graphics, spring, mtoon_overrides_json))
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
    let auto_load_spring_preset = av
        .get("auto_load_spring_preset")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
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
        auto_load_spring_preset,
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
