//! Load / save / apply per-VRM avatar defaults (expressions, pose, layers).

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_vrm1::prelude::Initialized;

use crate::avatar_defaults::{
    AvatarDefaultsFile, avatar_defaults_path, load_avatar_defaults, save_avatar_defaults,
};
use crate::config::Settings;
use crate::pose_library::PoseFile;

use super::anim_layer_sets::LayerSetsStore;
use super::anim_layers::LayerStackHandle;
use super::pose_driver::{BoneSnapshotHandle, IndexedBones, PoseCommand, PoseCommandSender};
use super::pose_library_assets::PoseLibraryAssets;

pub struct AvatarDefaultsPlugin;

impl Plugin for AvatarDefaultsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AvatarDefaultsStatus>()
            .init_resource::<PendingAvatarDefaults>()
            .add_systems(Startup, mark_avatar_defaults_pending)
            .add_systems(
                Update,
                (
                    mark_avatar_defaults_pending_on_model_change,
                    try_apply_avatar_defaults,
                ),
            );
    }
}

#[derive(Resource, Default, Clone)]
pub struct AvatarDefaultsStatus {
    pub last_path: Option<String>,
    pub last_message: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Resource, Default)]
struct PendingAvatarDefaults {
    model_path: String,
    needed: bool,
}

fn mark_avatar_defaults_pending(settings: Res<Settings>, mut pending: ResMut<PendingAvatarDefaults>) {
    pending.model_path = settings.avatar.model_path.clone();
    pending.needed = true;
}

fn mark_avatar_defaults_pending_on_model_change(
    settings: Res<Settings>,
    mut pending: ResMut<PendingAvatarDefaults>,
) {
    if pending.model_path != settings.avatar.model_path {
        pending.model_path = settings.avatar.model_path.clone();
        pending.needed = true;
    }
}

/// Re-queue defaults after MCP hot-swap or manual load.
pub fn request_apply_avatar_defaults(world: &mut World) {
    let model_path = world
        .get_resource::<Settings>()
        .map(|s| s.avatar.model_path.clone());
    if let Some(mut pending) = world.get_resource_mut::<PendingAvatarDefaults>() {
        if let Some(path) = model_path {
            pending.model_path = path;
        }
        pending.needed = true;
    }
}

pub fn save_avatar_defaults_from_snapshot(
    model_path: &str,
    snapshot: &crate::plugins::pose_driver::BoneSnapshot,
    rest_pose: Option<String>,
    layer_set: Option<String>,
    idle_clip: Option<String>,
    idle_use_layer_stack: bool,
) -> Result<std::path::PathBuf, String> {
    let file = AvatarDefaultsFile {
        expressions: snapshot.expressions.clone(),
        rest_pose,
        layer_set,
        idle_clip,
        idle_use_layer_stack,
        ..AvatarDefaultsFile::default()
    };
    save_avatar_defaults(model_path, &file)
}

pub fn apply_avatar_defaults_now(
    _settings: &Settings,
    defaults: &AvatarDefaultsFile,
    pose_tx: &PoseCommandSender,
    library: &PoseLibraryAssets,
    layer_sets: &LayerSetsStore,
    stack: &LayerStackHandle,
) -> Result<String, String> {
    let mut parts = Vec::new();

    if defaults.apply_expressions_on_load && !defaults.expressions.is_empty() {
        pose_tx.send(PoseCommand::SetExpression {
            weights: defaults.expressions.clone(),
        });
        parts.push(format!("{} expression(s)", defaults.expressions.len()));
    }

    if let Some(ref pose_name) = defaults.rest_pose {
        let pose = library
            .library
            .find_pose(pose_name)
            .map_err(|e| format!("rest_pose lookup: {e}"))?
            .ok_or_else(|| format!("rest_pose '{pose_name}' not found"))?;
        apply_pose_file(pose_tx, &pose);
        parts.push(format!("pose '{pose_name}'"));
    }

    if let Some(ref set_name) = defaults.layer_set {
        let n = stack.with_write(|s| layer_sets.load_into(set_name, s, &library.library, true))?;
        parts.push(format!("layer set '{set_name}' ({n} layers)"));
    } else if defaults.idle_use_layer_stack {
        if let Some(ref clip) = defaults.idle_clip {
            install_idle_clip_layer(stack, &library.library, clip, defaults.idle_clip_looping)?;
            parts.push(format!("idle clip '{clip}'"));
        }
    }

    if parts.is_empty() {
        Ok("avatar defaults file loaded but nothing to apply".into())
    } else {
        Ok(format!("applied: {}", parts.join(", ")))
    }
}

fn apply_pose_file(pose_tx: &PoseCommandSender, pose: &PoseFile) {
    let bones: HashMap<String, [f32; 4]> = pose
        .bones
        .iter()
        .map(|(k, v)| (k.clone(), v.rotation))
        .collect();
    if !bones.is_empty() {
        pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: Some(pose.transition_duration),
        });
    }
    if !pose.expressions.is_empty() {
        pose_tx.send(PoseCommand::ApplyExpression {
            weights: pose.expressions.clone(),
            cancel_expression_animation: true,
        });
    }
}

pub fn install_idle_clip_layer(
    stack: &LayerStackHandle,
    library: &crate::pose_library::PoseLibrary,
    filename: &str,
    looping: bool,
) -> Result<(), String> {
    let animation = library
        .load_animation(filename)
        .map_err(|e| format!("load_animation({filename}): {e}"))?;
    stack.with_write(|s| s.install_idle_clip_at_base(animation, looping));
    Ok(())
}

fn try_apply_avatar_defaults(
    settings: Res<Settings>,
    mut pending: ResMut<PendingAvatarDefaults>,
    mut status: ResMut<AvatarDefaultsStatus>,
    indexed: Option<Res<IndexedBones>>,
    vrm_q: Query<(), (With<bevy_vrm1::prelude::Vrm>, With<Initialized>)>,
    pose_tx: Option<Res<PoseCommandSender>>,
    library: Option<Res<PoseLibraryAssets>>,
    layer_sets: Option<Res<LayerSetsStore>>,
    stack: Option<Res<LayerStackHandle>>,
) {
    if !pending.needed || !settings.avatar.auto_apply_avatar_defaults {
        return;
    }
    if vrm_q.is_empty() {
        return;
    }
    let Some(indexed) = indexed else { return };
    if indexed.is_empty() {
        return;
    }
    let Some(defaults) = load_avatar_defaults(&settings.avatar.model_path) else {
        pending.needed = false;
        status.last_path =
            Some(avatar_defaults_path(&settings.avatar.model_path).display().to_string());
        status.last_message = Some("no avatar_defaults.json for this model".into());
        status.last_error = None;
        return;
    };
    let Some(pose_tx) = pose_tx else { return };
    let Some(library) = library else { return };
    let Some(layer_sets) = layer_sets else { return };
    let Some(stack) = stack else { return };

    match apply_avatar_defaults_now(
        &settings,
        &defaults,
        &pose_tx,
        &library,
        &layer_sets,
        &stack,
    ) {
        Ok(msg) => {
            info!("avatar defaults: {msg}");
            status.last_path =
                Some(avatar_defaults_path(&settings.avatar.model_path).display().to_string());
            status.last_message = Some(msg);
            status.last_error = None;
            pending.needed = false;
        }
        Err(e) => {
            warn!("avatar defaults: {e}");
            status.last_error = Some(e);
        }
    }
}

/// Capture current expression overrides for UI save actions.
pub fn snapshot_expressions(handle: &BoneSnapshotHandle) -> HashMap<String, f32> {
    handle.0.read().expressions.clone()
}
