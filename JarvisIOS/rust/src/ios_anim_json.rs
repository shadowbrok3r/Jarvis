//! Minimal pose-library JSON playback for JarvisIOS (Kimodo / MCP export format).
//! Applies each keyframe by matching bone `Name` under the avatar root (case-insensitive) and
//! triggers `SetExpressions` on the `Vrm` entity when keyframes carry `expressions`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_vrm1::prelude::*;
use serde::Deserialize;

use crate::ios_bevy::JarvisIosAvatarRoot;
use crate::ios_profile_manifest::IosAvatarSettings;

#[derive(Resource, Default)]
pub struct IosJsonAnimPlayback {
    inner: Option<ActiveJsonClip>,
}

pub(crate) struct ActiveJsonClip {
    animation: IosAnimFile,
    elapsed: f32,
    frame_duration_secs: f32,
    looping: bool,
    hold_duration_secs: f32,
    holding_elapsed: f32,
    finished_timeline: bool,
    last_applied_frame: Option<usize>,
    bone_lower_to_entity: HashMap<String, Entity>,
    vrm_entity: Entity,
    /// Idle VRMA entities we [`StopVrma`] so JSON can own bone transforms; replay on clip end.
    pub(crate) stopped_idle_vrma: Vec<Entity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IosAnimFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    fps: f64,
    #[serde(default)]
    frames: Vec<IosAnimFrame>,
    #[serde(default)]
    looping: Option<bool>,
    #[serde(default)]
    hold_duration: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IosAnimFrame {
    #[serde(default)]
    bones: HashMap<String, IosBoneRot>,
    #[serde(default)]
    expressions: HashMap<String, f32>,
}

#[derive(Debug, Deserialize)]
struct IosBoneRot {
    rotation: [f32; 4],
}

impl IosJsonAnimPlayback {
    pub fn stop(&mut self) {
        self.inner = None;
    }

    /// Replace any active clip with a new one built from disk + current scene.
    pub fn replace_with_clip(&mut self, clip: Option<ActiveJsonClip>) {
        self.inner = clip;
    }

    /// Idle VRMA entities paused for the current JSON clip (for supersede / replay before a new clip).
    pub(crate) fn supersede_stopped_idle_snapshot(&self) -> Vec<Entity> {
        self.inner
            .as_ref()
            .map(|c| c.stopped_idle_vrma.clone())
            .unwrap_or_default()
    }
}

/// Load JSON from `JARVIS_ASSET_ROOT` / `rel_path` and snapshot bone entities under the avatar root.
pub(crate) fn try_build_clip(rel_path: &str, world: &mut World) -> Option<ActiveJsonClip> {
    if !crate::ios_bevy::is_safe_asset_rel(rel_path) {
        crate::jarvis_ios_line!("[JarvisIOS] json anim: rejected unsafe path {rel_path:?}");
        return None;
    }
    let root = std::env::var("JARVIS_ASSET_ROOT").unwrap_or_else(|_| "assets".to_string());
    let abs = Path::new(&root).join(rel_path);
    let raw = std::fs::read_to_string(&abs).ok()?;
    let animation: IosAnimFile = serde_json::from_str(&raw).ok()?;
    if animation.frames.is_empty() {
        crate::jarvis_ios_line!("[JarvisIOS] json anim: no frames in {}", abs.display());
        return None;
    }
    let avatar_root = world
        .query_filtered::<Entity, With<JarvisIosAvatarRoot>>()
        .iter(world)
        .next()?;
    let vrm_entity = world
        .query_filtered::<Entity, With<Vrm>>()
        .iter(world)
        .next()?;
    let bone_lower_to_entity = build_bone_name_map(world, avatar_root);
    let fps = if animation.fps > 0.0 {
        animation.fps as f32
    } else {
        30.0
    };
    let frame_duration_secs = (1.0 / fps).max(1.0 / 240.0);
    let looping = animation.looping.unwrap_or(false);
    let hold_duration_secs = animation.hold_duration.unwrap_or(0.35).max(0.05);

    crate::jarvis_ios_line!(
        "[JarvisIOS] json anim: start {} frames={} fps={} loop={} bone_name_index={}",
        animation.name,
        animation.frames.len(),
        fps,
        looping,
        bone_lower_to_entity.len()
    );

    Some(ActiveJsonClip {
        animation,
        elapsed: 0.0,
        frame_duration_secs,
        looping,
        hold_duration_secs,
        holding_elapsed: 0.0,
        finished_timeline: false,
        last_applied_frame: None,
        bone_lower_to_entity,
        vrm_entity,
        stopped_idle_vrma: Vec::new(),
    })
}

/// BFS descendants including `root`.
fn collect_descendants(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        out.push(e);
        if let Some(ch) = world.get::<Children>(e) {
            for c in ch.iter() {
                stack.push(c);
            }
        }
    }
    out
}

fn vrma_path_matches_idle(vp: &Path, idle_rel: &str) -> bool {
    let idle_trim = idle_rel.trim();
    if idle_trim.is_empty() {
        return false;
    }
    let idle_norm = idle_trim.replace('\\', "/");
    let s = vp.to_string_lossy().replace('\\', "/");
    if s.ends_with(&idle_norm) {
        return true;
    }
    let idle_file = Path::new(idle_trim).file_name();
    let vp_file = vp.file_name();
    idle_file.is_some() && idle_file == vp_file
}

/// Stop the configured idle VRMA so pose JSON can drive bones without `AnimationPlayer` fighting us.
pub(crate) fn pause_matching_idle_vrma(
    world: &mut World,
    avatar_root: Entity,
    settings: &IosAvatarSettings,
) -> Vec<Entity> {
    let idle = settings.idle_vrma_path.trim();
    if idle.is_empty() {
        return Vec::new();
    }
    let mut stopped = Vec::new();
    for e in collect_descendants(world, avatar_root) {
        if world.get::<Vrma>(e).is_none() {
            continue;
        }
        let Some(vp) = world.get::<VrmaPath>(e) else {
            continue;
        };
        if !vrma_path_matches_idle(&vp.0, idle) {
            continue;
        }
        world
            .entity_mut(e)
            .trigger(|ent| StopVrma { entity: ent });
        stopped.push(e);
    }
    if !stopped.is_empty() {
        crate::jarvis_ios_line!(
            "[JarvisIOS] json anim: paused idle VRMA ({} target(s)) for pose JSON",
            stopped.len()
        );
    }
    stopped
}

fn resume_idle_vrmas(commands: &mut Commands, stopped: &[Entity]) {
    for &vrma_e in stopped {
        commands.entity(vrma_e).trigger(|e| PlayVrma {
            repeat: RepeatAnimation::Forever,
            transition_duration: Duration::ZERO,
            vrma: e,
            reset_spring_bones: false,
        });
    }
    if !stopped.is_empty() {
        crate::jarvis_ios_line!(
            "[JarvisIOS] json anim: resumed {} idle VRMA target(s)",
            stopped.len()
        );
    }
}

/// When queueing a new clip while one is active, the old clip never ran its “finished” path —
/// replay idle before we [`StopVrma`] again for the new clip.
pub(crate) fn resume_idle_vrmas_on_world(world: &mut World, stopped: &[Entity]) {
    for &e in stopped {
        world.entity_mut(e).trigger(|ent| PlayVrma {
            repeat: RepeatAnimation::Forever,
            transition_duration: Duration::ZERO,
            vrma: ent,
            reset_spring_bones: false,
        });
    }
    if !stopped.is_empty() {
        crate::jarvis_ios_line!(
            "[JarvisIOS] json anim: supersede — replayed {} idle VRMA before new JSON",
            stopped.len()
        );
    }
}

fn build_bone_name_map(world: &mut World, root: Entity) -> HashMap<String, Entity> {
    let mut out = HashMap::new();
    visit_named_bones(&*world, root, &mut out);
    out
}

/// Same as desktop `pose_driver::local_from_normalized`: Kimodo / pose-library JSON stores
/// **normalized humanoid** quaternions, not raw rig `Transform.rotation`.
#[inline]
fn local_from_normalized(rest_local: Quat, rest_world: Quat, pose_q: Quat) -> Quat {
    rest_local * rest_world.inverse() * pose_q * rest_world
}

fn visit_named_bones(world: &World, e: Entity, out: &mut HashMap<String, Entity>) {
    let Ok(er) = world.get_entity(e) else {
        return;
    };
    if let Some(n) = er.get::<Name>() {
        out.insert(n.as_str().to_ascii_lowercase(), e);
    }
    if let Some(ch) = er.get::<Children>() {
        for &child in ch {
            visit_named_bones(world, child, out);
        }
    }
}

fn ios_json_anim_tick(
    time: Res<Time>,
    mut playback: ResMut<IosJsonAnimPlayback>,
    mut transforms: Query<(
        &mut Transform,
        Option<&RestTransform>,
        Option<&RestGlobalTransform>,
    )>,
    mut commands: Commands,
) {
    let Some(clip) = playback.inner.as_mut() else {
        return;
    };
    let total = clip.animation.frames.len();
    if total == 0 {
        let stopped = clip.stopped_idle_vrma.clone();
        playback.stop();
        resume_idle_vrmas(&mut commands, &stopped);
        return;
    }

    if !clip.finished_timeline {
        clip.elapsed += time.delta_secs();
        let frame_idx_raw = (clip.elapsed / clip.frame_duration_secs) as i64;

        let (frame_idx, finished) = if frame_idx_raw < total as i64 {
            (frame_idx_raw as usize, false)
        } else if clip.looping {
            ((frame_idx_raw.rem_euclid(total as i64)) as usize, false)
        } else {
            (total - 1, true)
        };

        if Some(frame_idx) != clip.last_applied_frame {
            if let Some(frame) = clip.animation.frames.get(frame_idx) {
                apply_frame(frame, &clip.bone_lower_to_entity, &mut transforms, &mut commands, clip.vrm_entity);
            }
            clip.last_applied_frame = Some(frame_idx);
        }

        if finished {
            clip.finished_timeline = true;
            clip.holding_elapsed = 0.0;
        }
        return;
    }

    clip.holding_elapsed += time.delta_secs();
    if clip.holding_elapsed >= clip.hold_duration_secs {
        commands.trigger(ClearExpressions {
            entity: clip.vrm_entity,
        });
        crate::jarvis_ios_line!("[JarvisIOS] json anim: finished {}", clip.animation.name);
        let stopped = clip.stopped_idle_vrma.clone();
        playback.stop();
        resume_idle_vrmas(&mut commands, &stopped);
    }
}

fn apply_frame(
    frame: &IosAnimFrame,
    bone_map: &HashMap<String, Entity>,
    transforms: &mut Query<(
        &mut Transform,
        Option<&RestTransform>,
        Option<&RestGlobalTransform>,
    )>,
    commands: &mut Commands,
    vrm_entity: Entity,
) {
    for (bone_name, rot) in &frame.bones {
        let key = bone_name.to_ascii_lowercase();
        let Some(&ent) = bone_map.get(&key) else {
            continue;
        };
        let Ok((mut tf, rest, rest_world)) = transforms.get_mut(ent) else {
            continue;
        };
        let pose_q = Quat::from_xyzw(
            rot.rotation[0],
            rot.rotation[1],
            rot.rotation[2],
            rot.rotation[3],
        );
        if !(pose_q.x.is_finite()
            && pose_q.y.is_finite()
            && pose_q.z.is_finite()
            && pose_q.w.is_finite())
        {
            continue;
        }
        let pose_q = pose_q.normalize();
        let final_q = match (rest, rest_world) {
            (Some(rt), Some(rgt)) => {
                let rest_local = rt.0.rotation;
                let rw = rgt.0.rotation();
                local_from_normalized(rest_local, rw, pose_q)
            }
            // Skin extras or timing: fall back to legacy raw write (better than skipping).
            _ => pose_q,
        };
        if final_q.x.is_finite()
            && final_q.y.is_finite()
            && final_q.z.is_finite()
            && final_q.w.is_finite()
        {
            tf.rotation = final_q.normalize();
        }
    }

    if !frame.expressions.is_empty() {
        let weights: HashMap<VrmExpression, f32> = frame
            .expressions
            .iter()
            .filter_map(|(k, &w)| {
                let name = k.trim();
                if name.is_empty() {
                    return None;
                }
                Some((VrmExpression::from(name), w.clamp(0.0, 1.0)))
            })
            .collect();
        if !weights.is_empty() {
            commands.trigger(SetExpressions::from_iter(vrm_entity, weights));
        }
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<IosJsonAnimPlayback>().add_systems(
        PostUpdate,
        // 1) After VRMA sampling (`AnimationSystems`). 2) Before VRM roll / rotation constraints
        //    (same slot as desktop `apply_pose_commands`) so constraints do not stomp our pose.
        // 3) Before transform propagation. JSON rotations are **normalized humanoid** space; see
        //    `local_from_normalized` + `apply_frame`.
        ios_json_anim_tick
            .after(AnimationSystems)
            .before(VrmSystemSets::Constraints)
            .before(TransformSystems::Propagate),
    );
}
