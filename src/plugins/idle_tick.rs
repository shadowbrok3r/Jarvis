//! Minimal local idle loop: periodically pick a random filtered pose or
//! animation from [`PoseLibraryAssets`] and apply it.
//!
//! Simple built-in idle: random pose or library clip on a timer. Disable it in
//! the Pose Controller UI if something else (hub agent, MCP, Kimodo) should own
//! idles instead.

use std::time::Duration;

use bevy::prelude::*;
use rand::seq::IndexedRandom;
use rand::RngExt;

use jarvis_avatar::config::Settings;

use crate::plugins::native_anim_player::ActiveNativeAnimation;
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};
use crate::plugins::pose_library_assets::PoseLibraryAssets;

pub struct IdleTickPlugin;

impl Plugin for IdleTickPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<IdleTickState>()
            .add_systems(Update, run_idle_tick);
    }
}

#[derive(Resource, Default)]
struct IdleTickState {
    next_pick_in: Option<Duration>,
    elapsed: Duration,
}

fn run_idle_tick(
    time: Res<Time>,
    settings: Res<Settings>,
    library: Option<Res<PoseLibraryAssets>>,
    sender: Option<Res<PoseCommandSender>>,
    mut active_anim: ResMut<ActiveNativeAnimation>,
    mut state: ResMut<IdleTickState>,
) {
    if !settings.pose_controller.idle_enabled {
        state.next_pick_in = None;
        state.elapsed = Duration::ZERO;
        return;
    }
    let Some(library) = library else {
        return;
    };
    let Some(sender) = sender else {
        return;
    };

    if state.next_pick_in.is_none() {
        state.next_pick_in = Some(sample_interval(&settings));
        state.elapsed = Duration::ZERO;
    }

    state.elapsed += Duration::from_secs_f32(time.delta_secs());
    let Some(target) = state.next_pick_in else {
        return;
    };
    if state.elapsed < target {
        return;
    }

    let category = settings.pose_controller.idle_category.trim().to_string();
    let mut rng = rand::rng();

    let poses = library.poses();
    let anims = library.animations();
    let filtered_poses: Vec<_> = poses
        .iter()
        .filter(|p| category.is_empty() || p.category.eq_ignore_ascii_case(&category))
        .collect();
    let filtered_anims: Vec<_> = anims
        .iter()
        .filter(|a| category.is_empty() || a.category.eq_ignore_ascii_case(&category))
        .collect();

    // Prefer animations when available and nothing is currently playing; else pose.
    if !active_anim.is_playing() && !filtered_anims.is_empty() && rng.random_bool(0.5) {
        if let Some(meta) = filtered_anims.choose(&mut rng) {
            match library.library.load_animation(&meta.filename) {
                Ok(anim) => active_anim.start(anim, meta.looping, meta.hold_duration),
                Err(e) => warn!("idle tick: load_animation({}) failed: {e}", meta.filename),
            }
        }
    } else if let Some(pose) = filtered_poses.choose(&mut rng) {
        let bones = pose
            .bones
            .iter()
            .map(|(k, v)| (k.clone(), v.rotation))
            .collect();
        sender.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: Some(pose.transition_duration),
        });
        if !pose.expressions.is_empty() {
            sender.send(PoseCommand::ApplyExpression {
                weights: pose.expressions.clone(),
            });
        }
    }

    state.next_pick_in = Some(sample_interval(&settings));
    state.elapsed = Duration::ZERO;
}

fn sample_interval(settings: &Settings) -> Duration {
    let min = settings.pose_controller.idle_interval_min_sec.max(1.0);
    let max = settings
        .pose_controller
        .idle_interval_max_sec
        .max(min + 0.5);
    let mut rng = rand::rng();
    let secs = rng.random_range(min..max);
    Duration::from_secs_f32(secs)
}
