//! Minimal pose-library JSON playback for JarvisIOS (Kimodo / MCP export format).
//! Applies each keyframe by matching bone `Name` under the avatar root (case-insensitive) and
//! triggers `SetExpressions` on the `Vrm` entity when keyframes carry `expressions`.

use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use bevy_vrm1::prelude::*;
use serde::Deserialize;

use crate::ios_bevy::JarvisIosAvatarRoot;

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
        "[JarvisIOS] json anim: start {} frames={} fps={} loop={}",
        animation.name,
        animation.frames.len(),
        fps,
        looping
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
    })
}

fn build_bone_name_map(world: &mut World, root: Entity) -> HashMap<String, Entity> {
    let mut out = HashMap::new();
    visit_named_bones(&*world, root, &mut out);
    out
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
    mut transforms: Query<&mut Transform>,
    mut commands: Commands,
) {
    let Some(clip) = playback.inner.as_mut() else {
        return;
    };
    let total = clip.animation.frames.len();
    if total == 0 {
        playback.stop();
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
        playback.stop();
    }
}

fn apply_frame(
    frame: &IosAnimFrame,
    bone_map: &HashMap<String, Entity>,
    transforms: &mut Query<&mut Transform>,
    commands: &mut Commands,
    vrm_entity: Entity,
) {
    for (bone_name, rot) in &frame.bones {
        let key = bone_name.to_ascii_lowercase();
        let Some(&ent) = bone_map.get(&key) else {
            continue;
        };
        let Ok(mut tf) = transforms.get_mut(ent) else {
            continue;
        };
        let q = Quat::from_xyzw(
            rot.rotation[0],
            rot.rotation[1],
            rot.rotation[2],
            rot.rotation[3],
        );
        if q.x.is_finite() && q.y.is_finite() && q.z.is_finite() && q.w.is_finite() {
            tf.rotation = q.normalize();
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
    app.init_resource::<IosJsonAnimPlayback>()
        .add_systems(Update, ios_json_anim_tick);
}
