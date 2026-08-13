//! Bake a loaded VRMA clip into a pose-library JSON animation and optionally
//! install it as the base layer in the animation stack.

use std::collections::HashMap;
use std::path::Path;

use bevy::animation::{AnimationPlayer, RepeatAnimation};
use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy_vrm1::prelude::{
    ChildSearcher, Initialized, PlayVrma, Vrm, Vrma, VrmaDuration, VrmaPath,
};

use crate::avatar_defaults::{AvatarDefaultsFile, save_avatar_defaults};
use crate::config::Settings;
use crate::pose_library::{AnimationFile, AnimationFrame, BoneRotation, slugify};

use super::anim_layers::{install_animation_per_bone_layers, LayerStackHandle};
use super::avatar_defaults::{install_idle_clip_layer, snapshot_expressions, AvatarDefaultsStatus};
use super::pose_driver::{publish_bone_snapshot, BoneSnapshotHandle};
use super::pose_library_assets::PoseLibraryAssets;

pub struct VrmaClipImportPlugin;

impl Plugin for VrmaClipImportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VrmaClipImportState>()
            .add_message::<StartVrmaClipImport>()
            .add_systems(
                Update,
                begin_vrma_clip_import.run_if(|s: Res<VrmaClipImportState>| s.job.is_none()),
            )
            .add_systems(
                PostUpdate,
                (
                    vrma_import_seek.before(AnimationSystems),
                    vrma_import_capture.after(publish_bone_snapshot),
                )
                    .run_if(|s: Res<VrmaClipImportState>| s.job.is_some()),
            );
    }
}

#[derive(Message, Clone, Debug)]
pub struct StartVrmaClipImport {
    /// Asset-relative path (e.g. `models/idle_loop.vrma`). Empty = `[avatar].idle_vrma_path`.
    pub vrma_path: String,
    /// Output animation name / filename stem. Empty = VRMA file stem.
    pub output_name: String,
    pub sample_fps: f32,
    pub add_as_base_layer: bool,
    pub save_to_defaults: bool,
    pub use_layer_stack_for_idle: bool,
    /// When true, install one layer-stack layer per animated bone instead of a single clip layer.
    pub per_bone_layers: bool,
}

impl Default for StartVrmaClipImport {
    fn default() -> Self {
        Self {
            vrma_path: String::new(),
            output_name: String::new(),
            sample_fps: 10.0,
            add_as_base_layer: true,
            save_to_defaults: true,
            use_layer_stack_for_idle: true,
            per_bone_layers: true,
        }
    }
}

#[derive(Resource, Default)]
pub struct VrmaClipImportState {
    pub status: Option<String>,
    pub error: Option<String>,
    pub job: Option<VrmaClipImportJob>,
}

struct VrmaClipImportJob {
    vrma_entity: Entity,
    vrm_entity: Entity,
    duration: f32,
    sample_fps: f32,
    sample_dt: f32,
    next_time: f32,
    frames: Vec<AnimationFrame>,
    output_name: String,
    output_filename: String,
    vrma_asset_path: String,
    add_as_base_layer: bool,
    save_to_defaults: bool,
    use_layer_stack_for_idle: bool,
    per_bone_layers: bool,
    saved_master_enabled: bool,
    started_playback: bool,
}

pub fn idle_vrma_asset_path(settings: &Settings) -> String {
    settings.avatar.idle_vrma_path.trim().to_string()
}

fn begin_vrma_clip_import(
    mut messages: MessageReader<StartVrmaClipImport>,
    mut state: ResMut<VrmaClipImportState>,
    settings: Res<Settings>,
    stack: Option<Res<LayerStackHandle>>,
    vrmas: Query<(Entity, &ChildOf, &VrmaDuration, &VrmaPath), With<Vrma>>,
    vrms: Query<(), (With<Vrm>, With<Initialized>)>,
    mut commands: Commands,
) {
    for ev in messages.read() {
        state.error = None;
        state.status = None;
        let asset_path = if ev.vrma_path.trim().is_empty() {
            idle_vrma_asset_path(&settings)
        } else {
            ev.vrma_path.trim().to_string()
        };
        if asset_path.is_empty() {
            state.error = Some("idle VRMA path is empty — set [avatar].idle_vrma_path".into());
            continue;
        }
        if vrms.is_empty() {
            state.error = Some("VRM not ready — wait for Initialized".into());
            continue;
        }
        let Some((vrma_entity, vrm_entity, duration, _path)) =
            find_vrma_for_asset(&vrmas, &asset_path)
        else {
            state.error = Some(format!(
                "no loaded VRMA matching '{asset_path}' — ensure idle VRMA is spawned"
            ));
            continue;
        };
        let dur = duration.0.as_secs_f32();
        if dur <= f32::EPSILON {
            state.error = Some(format!("VRMA '{asset_path}' has zero duration"));
            continue;
        }
        let stem = Path::new(&asset_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("idle");
        let output_name = if ev.output_name.trim().is_empty() {
            stem.to_string()
        } else {
            ev.output_name.trim().to_string()
        };
        let sample_fps = ev.sample_fps.clamp(4.0, 30.0);
        let saved_master_enabled = stack
            .as_ref()
            .map(|s| s.with_read(|st| st.master_enabled))
            .unwrap_or(true);
        if let Some(stack) = stack.as_ref() {
            stack.with_write(|s| s.master_enabled = false);
        }
        commands.trigger(PlayVrma {
            vrma: vrma_entity,
            repeat: RepeatAnimation::Never,
            transition_duration: std::time::Duration::from_millis(0),
            reset_spring_bones: false,
        });
        state.job = Some(VrmaClipImportJob {
            vrma_entity,
            vrm_entity,
            duration: dur,
            sample_fps,
            sample_dt: 1.0 / sample_fps,
            next_time: 0.0,
            frames: Vec::new(),
            output_name: output_name.clone(),
            output_filename: format!("{}.json", slugify(&output_name)),
            vrma_asset_path: asset_path.clone(),
            add_as_base_layer: ev.add_as_base_layer,
            save_to_defaults: ev.save_to_defaults,
            use_layer_stack_for_idle: ev.use_layer_stack_for_idle,
            per_bone_layers: ev.per_bone_layers,
            saved_master_enabled,
            started_playback: false,
        });
        state.status = Some(format!(
            "importing '{asset_path}' → {output_name} @ {sample_fps:.0} fps"
        ));
    }
}

fn find_vrma_for_asset<'a>(
    vrmas: &'a Query<(Entity, &ChildOf, &VrmaDuration, &VrmaPath), With<Vrma>>,
    asset_path: &str,
) -> Option<(Entity, Entity, &'a VrmaDuration, &'a VrmaPath)> {
    let want = Path::new(asset_path);
    let want_name = want.file_name()?.to_str()?;
    vrmas.iter().find_map(|(e, child, dur, path)| {
        let p = &path.0;
        let matches = p.ends_with(asset_path)
            || p.file_name().and_then(|s| s.to_str()) == Some(want_name)
            || p.to_string_lossy().ends_with(asset_path);
        if matches {
            Some((e, child.parent(), dur, path))
        } else {
            None
        }
    })
}

fn vrma_import_seek(
    mut state: ResMut<VrmaClipImportState>,
    child_searcher: ChildSearcher,
    mut players: Query<&mut AnimationPlayer>,
) {
    let Some(job) = state.job.as_mut() else {
        return;
    };
    let Some(root_bone) = child_searcher.find_root_bone(job.vrm_entity) else {
        return;
    };
    let Ok(mut player) = players.get_mut(root_bone) else {
        return;
    };
    player.pause_all();
    for (_, anim) in player.playing_animations_mut() {
        anim.set_speed(0.0);
        anim.set_seek_time(job.next_time.min(job.duration));
    }
    job.started_playback = true;
}

fn vrma_import_capture(
    mut state: ResMut<VrmaClipImportState>,
    mut settings: ResMut<Settings>,
    snapshot: Res<BoneSnapshotHandle>,
    library: Option<Res<PoseLibraryAssets>>,
    stack: Option<Res<LayerStackHandle>>,
    mut defaults_status: ResMut<AvatarDefaultsStatus>,
) {
    let Some(mut job) = state.job.take() else {
        return;
    };
    if !job.started_playback {
        state.job = Some(job);
        return;
    }

    let snap = snapshot.0.read().clone();
    let mut bones = HashMap::with_capacity(snap.bones.len());
    for (name, entry) in &snap.bones {
        bones.insert(
            name.clone(),
            BoneRotation {
                rotation: entry.rotation,
            },
        );
    }
    job.frames.push(AnimationFrame {
        bones,
        duration_ms: Some((1000.0 / job.sample_fps as f64).max(1.0)),
        expressions: HashMap::new(),
        root_position: None,
    });

    job.next_time += job.sample_dt;
    if job.next_time <= job.duration + f32::EPSILON {
        state.job = Some(job);
        return;
    }

    let Some(library) = library else {
        state.error = Some("pose library unavailable".into());
        restore_stack_master(&stack, job.saved_master_enabled);
        return;
    };

    let anim = AnimationFile {
        name: job.output_name.clone(),
        prompt: format!("Imported from {}", job.vrma_asset_path),
        fps: job.sample_fps as f64,
        frame_count: job.frames.len(),
        frames: job.frames,
        category: Some("idle".into()),
        looping: Some(true),
        hold_duration: None,
    };

    match library.library.save_animation(&anim) {
        Ok(path) => {
            let msg = format!(
                "saved {} ({} frames @ {:.0} fps)",
                path.display(),
                anim.frame_count,
                anim.fps
            );
            info!("VRMA import: {msg}");
            state.status = Some(msg.clone());
            state.error = None;

            if job.add_as_base_layer {
                if let Some(stack) = stack.as_ref() {
                    if job.per_bone_layers {
                        stack.with_write(|s| {
                            install_animation_per_bone_layers(s, anim.clone(), true);
                            s.master_enabled = job.saved_master_enabled;
                        });
                    } else if let Err(e) = install_idle_clip_layer(
                        stack,
                        &library.library,
                        &job.output_filename,
                        true,
                    ) {
                        state.error = Some(format!("layer install failed: {e}"));
                    } else {
                        stack.with_write(|s| s.master_enabled = job.saved_master_enabled);
                    }
                }
            } else {
                restore_stack_master(&stack, job.saved_master_enabled);
            }

            if job.save_to_defaults {
                let expressions = snapshot_expressions(&snapshot);
                let file = AvatarDefaultsFile {
                    expressions,
                    idle_clip: Some(job.output_filename.clone()),
                    idle_clip_looping: true,
                    idle_use_layer_stack: job.use_layer_stack_for_idle,
                    ..AvatarDefaultsFile::default()
                };
                match save_avatar_defaults(&settings.avatar.model_path, &file) {
                    Ok(p) => {
                        defaults_status.last_path = Some(p.display().to_string());
                        defaults_status.last_message =
                            Some("avatar defaults updated with idle clip".into());
                    }
                    Err(e) => state.error = Some(e),
                }
            }

            if job.use_layer_stack_for_idle {
                settings.avatar.idle_use_layer_stack = true;
            }
        }
        Err(e) => {
            state.error = Some(format!("save animation failed: {e}"));
            restore_stack_master(&stack, job.saved_master_enabled);
        }
    }
}

fn restore_stack_master(stack: &Option<Res<LayerStackHandle>>, enabled: bool) {
    if let Some(stack) = stack {
        stack.with_write(|s| s.master_enabled = enabled);
    }
}
