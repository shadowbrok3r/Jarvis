//! TOML spring preset format aligned with desktop `src/plugins/spring_preset.rs` (subset for iOS).
//!
//! Parsed from the hub manifest `spring_preset.toml` string when inlined by the desktop hub.

use bevy::prelude::*;
use bevy_vrm1::prelude::{ColliderShape, SpringJointProps};
use serde::{Deserialize, Serialize};

pub const PRESET_FORMAT_VERSION: u32 = 1;

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

fn apply_collider_shape(dst: &mut ColliderShape, src: &PresetColliderShapeV1) -> bool {
    match (dst, src) {
        (ColliderShape::Sphere(d), PresetColliderShapeV1::Sphere { offset, radius }) => {
            d.offset = *offset;
            d.radius = *radius;
            true
        }
        (ColliderShape::Capsule(d), PresetColliderShapeV1::Capsule { offset, tail, radius }) => {
            d.offset = *offset;
            d.tail = *tail;
            d.radius = *radius;
            true
        }
        _ => false,
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

pub fn parse_preset_toml(raw: &str) -> Result<SpringPresetFile, String> {
    toml::from_str(raw).map_err(|e| e.to_string())
}
