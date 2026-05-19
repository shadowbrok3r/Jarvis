//! Device motion from CoreMotion → VRMC spring bones only (gravity direction + shake power).
//!
//! Humanoid / animation bones are never touched — only [`SpringJointProps`] on spring joints.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_vrm1::prelude::{Initialized, SpringJointProps, Vrm, VrmSystemSets};

const WORLD_DOWN: Vec3 = Vec3::new(0.0, -1.0, 0.0);

pub struct IosDeviceMotionPlugin;

impl Plugin for IosDeviceMotionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<IosDeviceMotionInput>()
            .init_resource::<IosDeviceMotionTuning>()
            .init_resource::<IosSpringMotionBaselines>()
            .add_systems(
                PostUpdate,
                (
                    ios_capture_spring_motion_baselines,
                    ios_apply_device_motion_to_springs
                        .after(ios_capture_spring_motion_baselines)
                        .before(VrmSystemSets::SpringBone),
                ),
            );
    }
}

/// Runtime tuning (Swift Motion panel sliders).
#[derive(Resource, Clone, Debug)]
pub struct IosDeviceMotionTuning {
    /// Extra `gravity_power` per m/s² of user acceleration (shake only).
    pub shake_power_per_ms2: f32,
    pub max_power_mult: f32,
    /// Ignore table vibration / sensor noise below this (m/s²).
    pub shake_deadzone_ms2: f32,
    /// Blend toward phone gravity (0 = preset only, 1 = full phone tilt).
    pub phone_gravity_blend: f32,
    /// Max tilt of spring gravity away from world down (radians).
    pub max_tilt_from_down_rad: f32,
}

impl Default for IosDeviceMotionTuning {
    fn default() -> Self {
        Self {
            shake_power_per_ms2: 0.18,
            max_power_mult: 3.0,
            shake_deadzone_ms2: 0.12,
            phone_gravity_blend: 0.72,
            max_tilt_from_down_rad: 1.15,
        }
    }
}

/// Gravity + user acceleration in Bevy world space (Y-up).
#[derive(Resource)]
pub struct IosDeviceMotionInput {
    pub enabled: bool,
    pub gravity_dir: Vec3,
    /// Linear acceleration with gravity removed (m/s²), device shake component.
    pub user_accel: Vec3,
}

impl Default for IosDeviceMotionInput {
    fn default() -> Self {
        Self {
            enabled: false,
            gravity_dir: WORLD_DOWN,
            user_accel: Vec3::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SpringMotionBaseline {
    pub gravity_dir: Vec3,
    pub gravity_power: f32,
}

#[derive(Resource, Default)]
pub struct IosSpringMotionBaselines {
    pub joints: HashMap<Entity, SpringMotionBaseline>,
}

impl IosSpringMotionBaselines {
    pub fn clear(&mut self) {
        self.joints.clear();
    }

    pub fn capture_from_springs(
        &mut self,
        springs: impl Iterator<Item = (Entity, SpringMotionBaseline)>,
    ) {
        self.clear();
        for (e, b) in springs {
            self.joints.insert(e, b);
        }
    }
}

/// Snapshot per-joint spring params after VRM load.
pub fn ios_capture_spring_motion_baselines(
    vrm_ready: Query<(), (With<Vrm>, Added<Initialized>)>,
    springs: Query<(Entity, &SpringJointProps)>,
    mut baselines: ResMut<IosSpringMotionBaselines>,
) {
    if vrm_ready.is_empty() {
        return;
    }
    baselines.capture_from_springs(springs.iter().map(|(e, p)| {
        (
            e,
            SpringMotionBaseline {
                gravity_dir: p.gravity_dir,
                gravity_power: p.gravity_power,
            },
        )
    }));
    crate::jarvis_ios_line!(
        "[JarvisIOS] device motion: captured {} spring baselines",
        baselines.joints.len()
    );
}

fn clamp_tilt_from_down(dir: Vec3, max_tilt_rad: f32) -> Vec3 {
    let d = if dir.length_squared() > 1e-8 {
        dir.normalize()
    } else {
        WORLD_DOWN
    };
    let cos_max = max_tilt_rad.cos();
    let dot = d.dot(WORLD_DOWN).clamp(-1.0, 1.0);
    if dot >= cos_max {
        return d;
    }
    let axis = WORLD_DOWN.cross(d);
    if axis.length_squared() < 1e-8 {
        return WORLD_DOWN;
    }
    Quat::from_axis_angle(axis.normalize(), max_tilt_rad) * WORLD_DOWN
}

fn ios_apply_device_motion_to_springs(
    motion: Res<IosDeviceMotionInput>,
    tuning: Res<IosDeviceMotionTuning>,
    baselines: Res<IosSpringMotionBaselines>,
    mut springs: Query<(Entity, &mut SpringJointProps)>,
) {
    if springs.is_empty() {
        return;
    }
    if motion.enabled {
        let max_tilt = tuning.max_tilt_from_down_rad.max(0.05);
        let blend = tuning.phone_gravity_blend.clamp(0.0, 1.0);
        let phone_dir = clamp_tilt_from_down(motion.gravity_dir, max_tilt);
        let shake = (motion.user_accel.length() - tuning.shake_deadzone_ms2.max(0.0)).max(0.0);
        let power_mult = (1.0 + shake * tuning.shake_power_per_ms2.max(0.0))
            .clamp(1.0, tuning.max_power_mult.max(1.0));
        for (e, mut p) in springs.iter_mut() {
            if let Some(base) = baselines.joints.get(&e) {
                let blended = base
                    .gravity_dir
                    .lerp(phone_dir, blend)
                    .normalize_or_zero();
                p.gravity_dir = if blended.length_squared() > 1e-8 {
                    clamp_tilt_from_down(blended, max_tilt)
                } else {
                    phone_dir
                };
                p.gravity_power = base.gravity_power * power_mult;
            } else {
                p.gravity_dir = phone_dir;
            }
        }
    } else {
        for (e, mut p) in springs.iter_mut() {
            if let Some(base) = baselines.joints.get(&e) {
                p.gravity_dir = base.gravity_dir;
                p.gravity_power = base.gravity_power;
            }
        }
    }
}
