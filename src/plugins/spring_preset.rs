//! Per-VRM VRMC spring joint + collider overrides as TOML under `config/spring_presets/`.
//!
//! ## Stable VRM key (`vrm_key`, filename stem)
//! 16 hex chars = 64-bit **FNV-1a** over the UTF-8 logical path string:
//! - Prefer [`bevy_vrm1::prelude::VrmPath`] (resolved asset path) when the `Vrm` entity has it.
//! - Otherwise fall back to `[avatar].model_path` from settings (same string used to load the file).
//!
//! The key is **reproducible** (fixed algorithm, fixed width) and safe as a single path segment.
//! Each preset file also stores `vrm_path` and `vrm_display_name` for human audit.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy_vrm1::prelude::{
    ColliderShape, Initialized, SpringJointProps, SpringNodeRegistry, VrmPath,
};
use serde::{Deserialize, Serialize};

use jarvis_avatar::config::Settings;

/// Subdirectory (under cwd at launch) for preset files: `config/spring_presets/<vrm_key>.toml`.
pub const SPRING_PRESETS_DIR: &str = "config/spring_presets";

pub const PRESET_FORMAT_VERSION: u32 = 1;

const FNV_OFFSET: u64 = 14695981039346656037;
const FNV_PRIME: u64 = 1099511628211;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Hex key used for `config/spring_presets/<key>.toml`.
pub fn vrm_preset_key(path_logical: &str) -> String {
    format!("{:016x}", fnv1a64(path_logical.as_bytes()))
}

pub fn logical_vrm_path(vrm_path: Option<&Path>, model_config_path: &str) -> String {
    vrm_path
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| model_config_path.to_string())
}

pub fn default_preset_path_for_logical_path(
    vrm_path: Option<&Path>,
    model_config_path: &str,
) -> PathBuf {
    let logical = logical_vrm_path(vrm_path, model_config_path);
    let key = vrm_preset_key(&logical);
    Path::new(SPRING_PRESETS_DIR).join(format!("{key}.toml"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpringPresetFile {
    pub preset_version: u32,
    pub vrm_key: String,
    pub vrm_path: String,
    #[serde(default)]
    pub vrm_display_name: String,
    pub joints: Vec<PresetJoint>,
    pub colliders: Vec<PresetCollider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetJoint {
    pub name: String,
    pub stiffness: f32,
    pub drag_force: f32,
    pub gravity_power: f32,
    pub hit_radius: f32,
    pub gravity_dir: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetCollider {
    pub name: String,
    #[serde(flatten)]
    pub shape: PresetColliderShapeV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PresetColliderShapeV1 {
    Sphere {
        offset: [f32; 3],
        radius: f32,
    },
    Capsule {
        offset: [f32; 3],
        tail: [f32; 3],
        radius: f32,
    },
}

impl From<&ColliderShape> for PresetColliderShapeV1 {
    fn from(value: &ColliderShape) -> Self {
        match value {
            ColliderShape::Sphere(s) => PresetColliderShapeV1::Sphere {
                offset: s.offset,
                radius: s.radius,
            },
            ColliderShape::Capsule(c) => PresetColliderShapeV1::Capsule {
                offset: c.offset,
                tail: c.tail,
                radius: c.radius,
            },
        }
    }
}

fn apply_collider_shape(dst: &mut ColliderShape, src: &PresetColliderShapeV1) -> bool {
    match (dst, src) {
        (ColliderShape::Sphere(d), PresetColliderShapeV1::Sphere { offset, radius }) => {
            d.offset = *offset;
            d.radius = *radius;
            true
        }
        (
            ColliderShape::Capsule(d),
            PresetColliderShapeV1::Capsule {
                offset,
                tail,
                radius,
            },
        ) => {
            d.offset = *offset;
            d.tail = *tail;
            d.radius = *radius;
            true
        }
        _ => false,
    }
}

/// Build a preset document from already-sampled joint/collider rows (see Rig editor export).
pub fn build_spring_preset_file(
    vrm_key: String,
    vrm_path: String,
    vrm_display_name: String,
    mut joints: Vec<PresetJoint>,
    mut colliders: Vec<PresetCollider>,
) -> SpringPresetFile {
    joints.sort_by(|a, b| a.name.cmp(&b.name));
    colliders.sort_by(|a, b| a.name.cmp(&b.name));

    SpringPresetFile {
        preset_version: PRESET_FORMAT_VERSION,
        vrm_key,
        vrm_path,
        vrm_display_name,
        joints,
        colliders,
    }
}

pub fn apply_spring_preset(
    preset: &SpringPresetFile,
    springs: &mut Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    colliders: &mut Query<(Entity, Option<&Name>, &mut ColliderShape)>,
) -> (usize, usize, usize, usize) {
    let mut joints_hit = 0usize;
    let mut joints_miss = 0usize;
    for j in &preset.joints {
        let mut found = false;
        for (_, name, mut p) in springs.iter_mut() {
            let Some(n) = name else { continue };
            if n.as_str() != j.name {
                continue;
            }
            p.stiffness = j.stiffness;
            p.drag_force = j.drag_force;
            p.gravity_power = j.gravity_power;
            p.hit_radius = j.hit_radius;
            p.gravity_dir = Vec3::from_array(j.gravity_dir);
            joints_hit += 1;
            found = true;
            break;
        }
        if !found {
            joints_miss += 1;
        }
    }

    let mut col_hit = 0usize;
    let mut col_miss = 0usize;
    for c in &preset.colliders {
        let mut found = false;
        for (_, name, mut shape) in colliders.iter_mut() {
            let Some(n) = name else { continue };
            if n.as_str() != c.name {
                continue;
            }
            if apply_collider_shape(&mut shape, &c.shape) {
                col_hit += 1;
            } else {
                col_miss += 1;
            }
            found = true;
            break;
        }
        if !found {
            col_miss += 1;
        }
    }

    (joints_hit, joints_miss, col_hit, col_miss)
}

pub fn load_preset_file(path: &Path) -> Result<SpringPresetFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

pub fn save_preset_file(path: &Path, preset: &SpringPresetFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir {}: {e}", parent.display()))?;
    }
    let s = toml::to_string_pretty(preset).map_err(|e| format!("serialize preset: {e}"))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

/// First path segment when splitting on `_` `.` `/` `:` — used for UI grouping.
pub fn bone_name_prefix(name: &str) -> String {
    let token = name
        .split(|c: char| c == '_' || c == '.' || c == '/' || c == ':')
        .next()
        .unwrap_or("");
    if token.is_empty() {
        "(no prefix)".to_string()
    } else {
        token.to_string()
    }
}

/// `joint_name` -> VRMC spring chain name (`SpringNode.name`).
pub fn joint_to_spring_chain(registry: &SpringNodeRegistry) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for node in registry.iter() {
        for jn in &node.joints {
            out.push((jn.as_str().to_string(), node.name.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Collider node name -> spring chain name (last writer wins if a name appears twice).
pub fn collider_to_spring_chain(registry: &SpringNodeRegistry) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for node in registry.iter() {
        for (cn, _) in &node.colliders {
            out.push((cn.as_str().to_string(), node.name.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// When `[avatar].auto_load_spring_preset` is true, load `config/spring_presets/<key>.toml`
/// once per `Initialized` VRM spawn (see `SpringBonePlugin`).
pub fn auto_load_spring_preset_system(
    settings: Res<Settings>,
    vrm_q: Query<
        (&VrmPath, &Name, Option<&SpringNodeRegistry>),
        (With<bevy_vrm1::prelude::Vrm>, Added<Initialized>),
    >,
    mut springs: Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    mut colliders: Query<(Entity, Option<&Name>, &mut ColliderShape)>,
) {
    if !settings.avatar.auto_load_spring_preset {
        return;
    }
    let Ok((path, vrm_name, _reg)) = vrm_q.single() else {
        return;
    };
    let logical = logical_vrm_path(Some(path.0.as_path()), settings.avatar.model_path.as_str());
    let key = vrm_preset_key(&logical);
    let file_path = Path::new(SPRING_PRESETS_DIR).join(format!("{key}.toml"));
    let Ok(preset) = load_preset_file(&file_path) else {
        return;
    };
    if preset.preset_version != PRESET_FORMAT_VERSION {
        tracing::warn!(
            "spring preset {:?}: expected preset_version {}, got {}",
            file_path,
            PRESET_FORMAT_VERSION,
            preset.preset_version
        );
    }
    if preset.vrm_key != key {
        tracing::warn!(
            "spring preset {:?}: file vrm_key {} != current {} — applying anyway",
            file_path,
            preset.vrm_key,
            key
        );
    }
    let (jh, jm, ch, cm) = apply_spring_preset(&preset, &mut springs, &mut colliders);
    tracing::info!(
        "auto-loaded spring preset {:?} for '{}' ({}): joints {}/{} ok, colliders {}/{} ok",
        file_path,
        vrm_name.as_str(),
        logical,
        jh,
        jh + jm,
        ch,
        ch + cm
    );
}
