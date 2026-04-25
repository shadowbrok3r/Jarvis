//! VRM 1.0 + default idle VRMA (`bevy_vrm1`).

use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy_vrm1::prelude::*;

use jarvis_avatar::config::Settings;

pub struct AvatarPlugin;

impl Plugin for AvatarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AvatarDebugStats>()
            .add_systems(Startup, spawn_scene)
            .add_systems(
                PostUpdate,
                (
                    lock_hips_root_motion,
                    clamp_vrm_root_y,
                    collect_avatar_debug_stats,
                )
                    .chain()
                    .after(AnimationSystems),
            );
    }
}

/// Live per-frame snapshot of Y-axis positions for the VRM root and hips bone,
/// plus the rest-pose baseline. The Avatar debug window renders this so we can
/// pinpoint which node is actually translating when the rig appears to slide.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct AvatarDebugStats {
    pub vrm_root_local_y: f32,
    pub vrm_root_world_y: f32,
    pub hips_local_y: f32,
    pub hips_world_y: f32,
    pub hips_rest_local_y: f32,
    pub has_vrm: bool,
    pub has_hips: bool,
}

fn spawn_scene(mut commands: Commands, asset_server: Res<AssetServer>, settings: Res<Settings>) {
    let [r, g, b, a] = settings.avatar.background_color;
    commands.insert_resource(ClearColor(Color::linear_rgba(r, g, b, a)));

    let vrm_path = settings.avatar.model_path.clone();
    let vrma_path = settings.avatar.idle_vrma_path.clone();

    info!("loading VRM from {vrm_path}");
    if !vrma_path.is_empty() {
        info!("default VRMA idle from {vrma_path}");
    }

    let pos = Vec3::from_array(settings.avatar.world_position);
    let scale = settings.avatar.uniform_scale.max(0.001);
    let mut vrm = commands.spawn((
        Transform {
            translation: pos,
            scale: Vec3::splat(scale),
            ..default()
        },
        GlobalTransform::default(),
        VrmHandle(asset_server.load(vrm_path)),
    ));

    if !vrma_path.trim().is_empty() {
        vrm.with_children(|parent| {
            parent
                .spawn(VrmaHandle(asset_server.load(vrma_path)))
                .observe(play_idle_when_vrma_loaded);
        });
    }
}

/// Fires when the VRMA clip is ready; loops forever (see `bevy_vrm1` `examples/vrma.rs`).
fn play_idle_when_vrma_loaded(
    trigger: On<LoadedVrma>,
    mut commands: Commands,
) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::ZERO,
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

/// After VRMA sampling, optionally snap hips **translation** back to the bind pose on selected
/// axes. VRMA retarget uses `RestTransform` + delta (`bevy_vrm1` `calc_hips_position`); writing
/// `0` here was wrong — it destroys the rig’s rest local offset and fights retargeting. We only
/// replace components with `bevy_vrm1::RestTransform` so we strip animated **delta** on that
/// axis, not the model’s natural hips offset.
fn lock_hips_root_motion(
    settings: Res<Settings>,
    mut hips_q: Query<(&mut Transform, &RestTransform), With<Hips>>,
) {
    let a = &settings.avatar;
    if !a.lock_root_xz && !a.lock_root_y {
        return;
    }
    for (mut tf, rest) in &mut hips_q {
        let r = rest.0.translation;
        if a.lock_root_xz {
            tf.translation.x = r.x;
            tf.translation.z = r.z;
        }
        if a.lock_root_y {
            tf.translation.y = r.y;
        }
    }
}

/// Hard-clamp the VRM root entity's local `Transform.translation.y` back to
/// `settings.avatar.world_position.y` after animation runs. This is the last line of defence
/// against "sliding" caused by *anything* translating the VRM scene root — transform
/// propagation bugs, VRMA clips that target the armature root instead of hips, etc.
fn clamp_vrm_root_y(settings: Res<Settings>, mut vrm_q: Query<&mut Transform, With<Vrm>>) {
    if !settings.avatar.lock_vrm_root_y {
        return;
    }
    let target_y = settings.avatar.world_position[1];
    for mut tf in &mut vrm_q {
        if (tf.translation.y - target_y).abs() > f32::EPSILON {
            tf.translation.y = target_y;
        }
    }
}

fn collect_avatar_debug_stats(
    mut stats: ResMut<AvatarDebugStats>,
    vrm_q: Query<(&Transform, &GlobalTransform), With<Vrm>>,
    hips_q: Query<(&Transform, &GlobalTransform, &RestTransform), With<Hips>>,
) {
    let mut next = AvatarDebugStats::default();
    if let Ok((tf, gtf)) = vrm_q.single() {
        next.has_vrm = true;
        next.vrm_root_local_y = tf.translation.y;
        next.vrm_root_world_y = gtf.translation().y;
    }
    if let Ok((tf, gtf, rest)) = hips_q.single() {
        next.has_hips = true;
        next.hips_local_y = tf.translation.y;
        next.hips_world_y = gtf.translation().y;
        next.hips_rest_local_y = rest.0.translation.y;
    }
    *stats = next;
}
