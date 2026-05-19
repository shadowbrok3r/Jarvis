//! Device motion from CoreMotion → VRMC spring bones (gravity direction + shake power).
//!
//! Scope is configurable: all spring joints (desktop parity) or secondary hair/cloth only.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_vrm1::prelude::{
    Initialized, SpringJointProps, SpringJointState, SpringRoot, Vrm, VrmSystemSets,
    reset_spring_velocities_recursive,
    reset_spring_velocities_recursive_world,
};

use crate::ios_bevy::IosAvatarRootEntity;

const WORLD_DOWN: Vec3 = Vec3::new(0.0, -1.0, 0.0);

/// Which spring joints receive phone tilt / shake (`0` = all, `1` = secondary only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IosSpringBoneScope {
    #[default]
    All = 0,
    SecondaryOnly = 1,
}

impl IosSpringBoneScope {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::SecondaryOnly,
            _ => Self::All,
        }
    }
}

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
    pub shake_power_per_ms2: f32,
    pub max_power_mult: f32,
    pub shake_deadzone_ms2: f32,
    pub phone_gravity_blend: f32,
    pub max_tilt_from_down_rad: f32,
    pub spring_scope: IosSpringBoneScope,
}

impl Default for IosDeviceMotionTuning {
    fn default() -> Self {
        Self {
            shake_power_per_ms2: 0.18,
            max_power_mult: 3.0,
            shake_deadzone_ms2: 0.12,
            phone_gravity_blend: 0.72,
            max_tilt_from_down_rad: 1.15,
            spring_scope: IosSpringBoneScope::All,
        }
    }
}

#[derive(Resource)]
pub struct IosDeviceMotionInput {
    pub enabled: bool,
    pub gravity_dir: Vec3,
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

fn spring_receives_phone_motion(scope: IosSpringBoneScope, name: Option<&Name>) -> bool {
    match scope {
        IosSpringBoneScope::All => true,
        IosSpringBoneScope::SecondaryOnly => is_secondary_spring_bone(name),
    }
}

/// Hair/cloth/accessory-style springs — excludes humanoid core name tokens.
fn is_secondary_spring_bone(name: Option<&Name>) -> bool {
    let Some(n) = name else {
        return false;
    };
    let s = n.as_str().to_ascii_lowercase();
    const CORE: &[&str] = &[
        "hips",
        "spine",
        "chest",
        "upperchest",
        "neck",
        "head",
        "shoulder",
        "upperleg",
        "lowerleg",
        "upperarm",
        "lowerarm",
        "foot",
        "toe",
        "hand",
        "thumb",
        "index",
        "middle",
        "ring",
        "little",
        "jaw",
        "eye",
    ];
    !CORE.iter().any(|token| s.contains(token))
}

fn snap_gravity_downward(dir: Vec3) -> Vec3 {
    let d = if dir.length_squared() > 1e-8 {
        dir.normalize()
    } else {
        WORLD_DOWN
    };
    if d.y > 0.0 { -d } else { d }
}

fn clamp_tilt_from_down(dir: Vec3, max_tilt_rad: f32) -> Vec3 {
    let d = snap_gravity_downward(dir);
    let cos_max = max_tilt_rad.cos();
    let dot = d.dot(WORLD_DOWN).clamp(-1.0, 1.0);
    if dot >= cos_max {
        return d;
    }
    let axis = WORLD_DOWN.cross(d);
    if axis.length_squared() < 1e-8 {
        return WORLD_DOWN;
    }
    snap_gravity_downward(Quat::from_axis_angle(axis.normalize(), max_tilt_rad) * WORLD_DOWN)
}

fn restore_spring_baselines(
    springs: &mut Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    baselines: &IosSpringMotionBaselines,
) {
    for (e, _, mut p) in springs.iter_mut() {
        if let Some(base) = baselines.joints.get(&e) {
            p.gravity_dir = base.gravity_dir;
            p.gravity_power = base.gravity_power;
        }
    }
}

fn ios_apply_device_motion_to_springs(
    motion: Res<IosDeviceMotionInput>,
    tuning: Res<IosDeviceMotionTuning>,
    baselines: Res<IosSpringMotionBaselines>,
    avatar: Res<IosAvatarRootEntity>,
    mut springs: Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    spring_roots: Query<&SpringRoot>,
    mut joint_states: Query<&mut SpringJointState>,
    children: Query<&Children>,
    mut was_enabled: Local<bool>,
) {
    if springs.is_empty() {
        return;
    }

    let enabled = motion.enabled;
    if !enabled {
        if *was_enabled {
            restore_spring_baselines(&mut springs, &baselines);
            if let Some(root) = avatar.0 {
                reset_spring_velocities_recursive(root, &spring_roots, &mut joint_states, &children);
            }
            crate::jarvis_ios_line!("[JarvisIOS] device motion: disabled — restored springs + reset velocities");
        }
        *was_enabled = false;
        return;
    }

    if !*was_enabled {
        crate::jarvis_ios_line!(
            "[JarvisIOS] device motion: enabled — spring scope {:?}",
            tuning.spring_scope
        );
    }
    *was_enabled = true;

    let max_tilt = tuning.max_tilt_from_down_rad.max(0.05);
    let blend = tuning.phone_gravity_blend.clamp(0.0, 1.0);
    let phone_dir = clamp_tilt_from_down(motion.gravity_dir, max_tilt);
    let shake = (motion.user_accel.length() - tuning.shake_deadzone_ms2.max(0.0)).max(0.0);
    let power_mult = (1.0 + shake * tuning.shake_power_per_ms2.max(0.0))
        .clamp(1.0, tuning.max_power_mult.max(1.0));

    for (e, name, mut p) in springs.iter_mut() {
        if !spring_receives_phone_motion(tuning.spring_scope, name) {
            continue;
        }
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
            p.gravity_power = p.gravity_power * power_mult;
        }
    }
}

/// Called from FFI when the user toggles phone motion off in Swift.
pub fn ios_reset_springs_after_device_motion_off(world: &mut World) {
    let baselines = world.resource::<IosSpringMotionBaselines>().joints.clone();
    let mut q = world.query::<(Entity, &mut SpringJointProps)>();
    for (e, mut p) in q.iter_mut(world) {
        if let Some(base) = baselines.get(&e) {
            p.gravity_dir = base.gravity_dir;
            p.gravity_power = base.gravity_power;
        }
    }
    let avatar = world.resource::<IosAvatarRootEntity>().0;
    if let Some(root) = avatar {
        reset_spring_velocities_recursive_world(world, root);
    }
}
