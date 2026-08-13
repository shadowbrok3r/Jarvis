//! Phase 3: drive `bevy_vrm1` `LookAt` from `vrm:set-look-at` envelopes (or fall back to cursor).
//!
//! Strategy: spawn a single invisible `LookAtTarget` entity. When the VRM finishes loading we
//! attach `LookAt::Target(target_entity)`. Incoming [`LookAtRequestMessage`]s move the target
//! in *local* space (parent = VRM root), so the target follows the rig regardless of `world_position`.
//! A `None` target reverts to the mouse cursor.
//!
//! `bevy_vrm1` only implements **bone** look-at. VRMs with `lookAt.type: "expression"` hit an
//! internal `todo!` if we insert [`LookAt`], so we parent the target entity but skip the driver.

use bevy::prelude::*;
use bevy_vrm1::prelude::*;
use rand::RngExt;

use crate::config::Settings;
use crate::plugins::anim_layers::{
    request_blink, set_look_around_external, smooth_alpha, LayerStackHandle,
};

use super::vrm_eye_debug::update_vrm_eye_lookat_debug;

use super::channel_server::LookAtRequestMessage;

pub struct LookAtPlugin;

impl Plugin for LookAtPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LookAtRuntime>()
            .init_resource::<super::VrmEyeLookatDebug>()
            .add_systems(Startup, spawn_look_target)
            .add_systems(
                Update,
                (
                    attach_lookat_to_vrm,
                    handle_look_at_requests,
                    drive_gaze_target.after(handle_look_at_requests),
                ),
            )
            // After VRM look-at, expressions, and the expression propagate step — then sample eye bones.
            .add_systems(
                PostUpdate,
                update_vrm_eye_lookat_debug
                    .after(VrmSystemSets::PropagateAfterExpressions)
                    .before(VrmSystemSets::SpringBone),
            );
    }
}

#[derive(Component)]
pub struct LookAtTarget;

/// Approximate head pivot in rig-local space (saccade angles are measured
/// from here).
const HEAD_PIVOT: Vec3 = Vec3::new(0.0, 1.45, 0.0);
/// Resting gaze point when nothing is being tracked.
const IDLE_GAZE: Vec3 = Vec3::new(0.0, 1.4, 1.0);

#[derive(Resource)]
pub struct LookAtRuntime {
    target: Option<Entity>,
    /// After the VRM root exists: parent gaze target here; only `true` when bone look-at is safe.
    target_parented: bool,
    bevy_look_at_enabled: bool,
    active_until: Option<std::time::Instant>,
    /// Requested gaze goal (rig-local); the saccade model chases it.
    goal: Vec3,
    /// Smoothed position written to the `LookAtTarget` entity each frame.
    current: Vec3,
    /// Center the micro-saccade jitter orbits during fixation.
    fixation_center: Vec3,
    next_microsaccade_in: f32,
    /// Pending head-follow event: (fires_in_secs, yaw_rad, pitch_rad).
    head_follow: Option<(f32, f32, f32)>,
}

impl LookAtRuntime {
    /// `true` while the avatar is actively tracking a gaze target (e.g. the user's face via the
    /// Home Assistant vision pipeline). The ambient `LookAround` head driver reads this to damp its
    /// motion so the head stays oriented toward the tracked face instead of wandering.
    pub fn gaze_active(&self) -> bool {
        self.active_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }
}

impl Default for LookAtRuntime {
    fn default() -> Self {
        Self {
            target: None,
            target_parented: false,
            bevy_look_at_enabled: true,
            active_until: None,
            goal: IDLE_GAZE,
            current: IDLE_GAZE,
            fixation_center: IDLE_GAZE,
            next_microsaccade_in: 0.6,
            head_follow: None,
        }
    }
}

/// Detach the shared [`LookAtTarget`] from the VRM before the avatar root is despawned (MCP model swap).
/// Preserves world-space gaze position via `remove_parent_in_place`. Clears `target_parented` so
/// [`attach_lookat_to_vrm`] runs again once the new `Vrm` exists.
pub fn detach_look_at_target_for_vrm_hot_swap(world: &mut World) {
    use bevy::transform::commands::BuildChildrenTransformExt;

    let (target, parented) = world
        .get_resource::<LookAtRuntime>()
        .map(|r| (r.target, r.target_parented))
        .unwrap_or((None, false));
    let Some(target) = target else {
        return;
    };
    if parented {
        if let Ok(mut em) = world.get_entity_mut(target) {
            em.remove_parent_in_place();
        }
    }
    if let Some(mut runtime) = world.get_resource_mut::<LookAtRuntime>() {
        runtime.target_parented = false;
    }
}

fn spawn_look_target(mut commands: Commands, mut runtime: ResMut<LookAtRuntime>) {
    // Default gaze target sits ~1 m in front of the rig at eye height. Parent is set to the
    // VRM root as soon as the VRM loads so the offset stays rig-local.
    let e = commands
        .spawn((
            Transform::from_xyz(0.0, 1.4, 1.0),
            GlobalTransform::default(),
            LookAtTarget,
        ))
        .id();
    runtime.target = Some(e);
}

fn attach_lookat_to_vrm(
    mut commands: Commands,
    mut runtime: ResMut<LookAtRuntime>,
    vrm_q: Query<(Entity, Option<&LookAtProperties>), With<Vrm>>,
    settings: Res<Settings>,
) {
    if runtime.target_parented {
        return;
    }
    let Some(target) = runtime.target else {
        return;
    };
    let Ok((vrm_entity, look_at_props)) = vrm_q.single() else {
        return;
    };

    commands.entity(target).insert(ChildOf(vrm_entity));

    let expression_type = matches!(
        look_at_props,
        Some(p) if p.r#type == LookAtType::Expression
    );
    if expression_type {
        runtime.bevy_look_at_enabled = false;
        warn!(
            "look-at: VRM uses expression look-at; bevy_vrm1 only supports bone look-at — gaze driver disabled (re-export the model with bone look-at or use a bone-type VRM)"
        );
    } else {
        runtime.bevy_look_at_enabled = true;
        commands.entity(vrm_entity).insert(LookAt::Target(target));
        info!(
            "look-at: attached target to VRM (idle_return_speed {:.1})",
            settings.look_at.idle_return_speed
        );
    }

    runtime.target_parented = true;
}

fn handle_look_at_requests(
    mut reader: MessageReader<LookAtRequestMessage>,
    mut runtime: ResMut<LookAtRuntime>,
    mut commands: Commands,
    vrm_q: Query<Entity, With<Vrm>>,
) {
    for msg in reader.read() {
        if runtime.target.is_none() {
            continue;
        }
        match msg.local_target {
            Some(pos) => {
                // Only the goal moves here — the saccade controller in
                // `drive_gaze_target` chases it. A large shift schedules a
                // delayed head-follow (eyes lead, head lags).
                let prev = runtime.goal - HEAD_PIVOT;
                let next = pos - HEAD_PIVOT;
                let shift = if prev.length_squared() > 1e-6 && next.length_squared() > 1e-6 {
                    prev.angle_between(next).to_degrees()
                } else {
                    0.0
                };
                runtime.goal = pos;
                if shift > 8.0 {
                    let mut rng = rand::rng();
                    let yaw = next.x.atan2(next.z);
                    let pitch = -next.y.atan2((next.x * next.x + next.z * next.z).sqrt());
                    runtime.head_follow = Some((
                        rng.random_range(0.10_f32..0.25),
                        yaw * 0.6,
                        pitch * 0.6,
                    ));
                }
                if runtime.bevy_look_at_enabled {
                    if let (Ok(vrm), Some(target_entity)) = (vrm_q.single(), runtime.target) {
                        commands.entity(vrm).insert(LookAt::Target(target_entity));
                    }
                }
                runtime.active_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
            }
            None => {
                if runtime.bevy_look_at_enabled {
                    if let Ok(vrm) = vrm_q.single() {
                        commands.entity(vrm).insert(LookAt::Cursor);
                    }
                }
                runtime.active_until = None;
            }
        }
    }
}

/// Saccade-model gaze mover. Large goal shifts flick with a ~60 ms time
/// constant (and force a reflex blink past 20°); during fixation the gaze
/// micro-saccades around the fixation center and slowly drifts back to it.
/// When tracking expires the whole model eases home to [`IDLE_GAZE`].
fn drive_gaze_target(
    time: Res<Time>,
    settings: Res<Settings>,
    mut runtime: ResMut<LookAtRuntime>,
    layers: Option<Res<LayerStackHandle>>,
    mut tf_q: Query<&mut Transform, With<LookAtTarget>>,
) {
    let dt = time.delta_secs();
    let Some(target) = runtime.target else {
        return;
    };
    let Ok(mut tf) = tf_q.get_mut(target) else {
        return;
    };

    // Head-follow fires after its reaction lag.
    if let Some((mut t, yaw, pitch)) = runtime.head_follow.take() {
        t -= dt;
        if t <= 0.0 {
            if let Some(layers) = layers.as_ref() {
                layers.with_write(|s| set_look_around_external(s, yaw, pitch));
            }
        } else {
            runtime.head_follow = Some((t, yaw, pitch));
        }
    }

    // Tracking expired: drift home at the configured return speed.
    if !runtime.gaze_active() {
        let speed = settings.look_at.idle_return_speed.max(0.1);
        let a = smooth_alpha(dt, 1.0 / speed);
        runtime.goal = runtime.goal.lerp(IDLE_GAZE, a);
        runtime.fixation_center = runtime.fixation_center.lerp(IDLE_GAZE, a);
    }

    let from = runtime.current - HEAD_PIVOT;
    let to = runtime.goal - HEAD_PIVOT;
    let angle = if from.length_squared() > 1e-6 && to.length_squared() > 1e-6 {
        from.angle_between(to).to_degrees()
    } else {
        0.0
    };

    if angle > 3.0 {
        // Saccade: fast flick toward the goal; big shifts blink reflexively.
        if angle > 20.0 {
            if let Some(layers) = layers.as_ref() {
                layers.with_write(|s| request_blink(s));
            }
        }
        runtime.fixation_center = runtime.goal;
        let a = smooth_alpha(dt, 0.06);
        runtime.current = runtime.current.lerp(runtime.goal, a);
        let mut rng = rand::rng();
        runtime.next_microsaccade_in = rng.random_range(0.3_f32..1.2);
    } else {
        // Fixation: micro-saccade jitter around the center, slow drift back.
        runtime.next_microsaccade_in -= dt;
        if runtime.next_microsaccade_in <= 0.0 {
            let mut rng = rand::rng();
            let off = Vec3::new(
                rng.random_range(-1.0_f32..1.0),
                rng.random_range(-1.0_f32..1.0),
                0.0,
            ) * rng.random_range(0.006_f32..0.02);
            runtime.goal = runtime.fixation_center + off;
            runtime.next_microsaccade_in = rng.random_range(0.3_f32..1.2);
        }
        let a = smooth_alpha(dt, 0.05);
        runtime.current = runtime.current.lerp(runtime.goal, a);
        let drift = smooth_alpha(dt, 1.5);
        runtime.goal = runtime.goal.lerp(runtime.fixation_center, drift);
    }
    tf.translation = runtime.current;
}
