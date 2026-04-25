//! Phase 3: drive `bevy_vrm1` `LookAt` from `vrm:set-look-at` envelopes (or fall back to cursor).
//!
//! Strategy: spawn a single invisible `LookAtTarget` entity. When the VRM finishes loading we
//! attach `LookAt::Target(target_entity)`. Incoming [`LookAtRequestMessage`]s move the target
//! in *local* space (parent = VRM root), so it tracks her regardless of `world_position`.
//! A `None` target reverts to the mouse cursor.
//!
//! `bevy_vrm1` only implements **bone** look-at. VRMs with `lookAt.type: "expression"` hit an
//! internal `todo!` if we insert [`LookAt`], so we parent the target entity but skip the driver.

use bevy::prelude::*;
use bevy_vrm1::prelude::*;

use jarvis_avatar::config::Settings;

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
                    decay_look_at_to_idle,
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

#[derive(Resource)]
struct LookAtRuntime {
    target: Option<Entity>,
    /// After the VRM root exists: parent gaze target here; only `true` when bone look-at is safe.
    target_parented: bool,
    bevy_look_at_enabled: bool,
    active_until: Option<std::time::Instant>,
}

impl Default for LookAtRuntime {
    fn default() -> Self {
        Self {
            target: None,
            target_parented: false,
            bevy_look_at_enabled: true,
            active_until: None,
        }
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
        commands
            .entity(vrm_entity)
            .insert(LookAt::Target(target));
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
    mut tf_q: Query<&mut Transform, With<LookAtTarget>>,
) {
    for msg in reader.read() {
        let Some(target_entity) = runtime.target else {
            continue;
        };
        match msg.local_target {
            Some(pos) => {
                if let Ok(mut tf) = tf_q.get_mut(target_entity) {
                    tf.translation = pos;
                }
                if runtime.bevy_look_at_enabled {
                    if let Ok(vrm) = vrm_q.single() {
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

fn decay_look_at_to_idle(
    mut runtime: ResMut<LookAtRuntime>,
    settings: Res<Settings>,
    time: Res<Time>,
    mut tf_q: Query<&mut Transform, With<LookAtTarget>>,
) {
    let Some(until) = runtime.active_until else {
        return;
    };
    if std::time::Instant::now() < until {
        return;
    }
    let Some(target) = runtime.target else {
        runtime.active_until = None;
        return;
    };
    let Ok(mut tf) = tf_q.get_mut(target) else {
        return;
    };
    // Ease back to the idle pose (just in front of the rig, at eye height).
    let idle = Vec3::new(0.0, 1.4, 1.0);
    let speed = settings.look_at.idle_return_speed.max(0.0);
    let t = (time.delta_secs() * speed).clamp(0.0, 1.0);
    tf.translation = tf.translation.lerp(idle, t);
    if tf.translation.distance(idle) < 0.005 {
        runtime.active_until = None;
    }
}
