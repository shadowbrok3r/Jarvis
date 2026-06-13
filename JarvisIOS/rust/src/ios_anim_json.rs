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

/// Seconds of crossfade across a looping clip's wrap (last frame → first frame),
/// mirroring the desktop layer system's `loop_fade`. Clips baked seamless
/// (integer-cycle oscillators) pass through it unchanged; legacy clips stop popping.
const LOOP_FADE_SECS: f32 = 0.25;

/// Seconds to blend from the rig's CURRENT pose into the clip's first frames —
/// desktop `native_anim_player` `ENTRY_BLEND_SECS` parity. Kills the whole-body
/// snap when a clip starts from idle or hands off from a different clip.
const ENTRY_BLEND_SECS: f32 = 0.22;

#[inline]
fn smoothstep01(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// When the configured idle is a JSON clip (`idle_vrma_path` ends in `.json`),
/// this drives starting / restarting it: `pending` is set at spawn, on idle
/// re-enable, and when a one-shot JSON clip finishes; the exclusive restart
/// system then (re)builds the looping idle clip once the rig is ready.
#[derive(Resource, Default)]
pub struct IosJsonIdleState {
    pub rel_path: Option<String>,
    pub pending: bool,
}

/// Sequence several JSON clips back-to-back, optionally looping the list —
/// "play between several animations on a loop". Each item plays once
/// (non-looping); the finish path flags `advance_pending` and the exclusive
/// advance system starts the next item. Item handoffs are smoothed by the
/// `ENTRY_BLEND_SECS` entry blend (held end pose → next clip's first frames,
/// no rest-pose snap between items). A direct user clip / VRMA cancels it.
#[derive(Resource, Default)]
pub struct IosJsonPlaylist {
    pub items: Vec<String>,
    pub next_index: usize,
    pub looping: bool,
    pub active: bool,
    pub advance_pending: bool,
}

impl IosJsonPlaylist {
    fn has_next(&self) -> bool {
        self.active && (self.next_index < self.items.len() || self.looping)
    }
}

pub(crate) struct ActiveJsonClip {
    animation: IosAnimFile,
    /// Asset-relative path this clip was built from (idle detection / logs).
    pub(crate) rel_path: String,
    elapsed: f32,
    frame_duration_secs: f32,
    looping: bool,
    hold_duration_secs: f32,
    holding_elapsed: f32,
    finished_timeline: bool,
    bone_lower_to_entity: HashMap<String, Entity>,
    /// Resolved `hips` bone for the optional per-frame `rootPosition` channel.
    hips_entity: Option<Entity>,
    vrm_entity: Entity,
    /// Idle VRMA entities we [`StopVrma`] so JSON can own bone transforms; replay on clip end.
    pub(crate) stopped_idle_vrma: Vec<Entity>,
    /// Bone entities this clip actually writes (union of all frame bone keys).
    /// Restored to their `RestTransform` rotation when the clip finishes or is
    /// superseded so a clip's pose never persists as a stuck "override" into the
    /// next clip / idle (the bug that forced a manual "clear overrides").
    pub(crate) touched_bones: Vec<Entity>,
    /// Rig pose (local rotation + translation) at clip start; the first
    /// `ENTRY_BLEND_SECS` slerp from here into the sampled clip pose.
    entry_pose: HashMap<Entity, (Quat, Vec3)>,
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
    /// Root motion: hips translation delta from bind, meters (desktop
    /// `AnimationFrame.root_position`; camelCase JSON key `rootPosition`).
    #[serde(default, alias = "root_position")]
    root_position: Option<[f32; 3]>,
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

    /// Bones the currently-active clip has been driving — restore these to rest
    /// before a superseding clip starts so its pose doesn't leak through.
    pub(crate) fn supersede_touched_snapshot(&self) -> Vec<Entity> {
        self.inner
            .as_ref()
            .map(|c| c.touched_bones.clone())
            .unwrap_or_default()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// Asset-relative path of the active clip (idle-vs-user-clip detection).
    pub(crate) fn active_rel_path(&self) -> Option<&str> {
        self.inner.as_ref().map(|c| c.rel_path.as_str())
    }

    pub(crate) fn active_vrm_entity(&self) -> Option<Entity> {
        self.inner.as_ref().map(|c| c.vrm_entity)
    }
}

/// Restore a set of bone entities to their `RestTransform` rotation AND
/// translation (translation matters for `hips` after root motion). Used to
/// clear a finished/superseded clip's pose so it never persists as an override.
pub(crate) fn reset_bones_to_rest_on_world(world: &mut World, bones: &[Entity]) {
    for &ent in bones {
        let Some(rest) = world
            .get::<RestTransform>(ent)
            .map(|r| (r.0.rotation, r.0.translation))
        else {
            continue;
        };
        if let Some(mut tf) = world.get_mut::<Transform>(ent) {
            tf.rotation = rest.0;
            tf.translation = rest.1;
        }
    }
}

/// Single-frame pose format: top-level `bones` and `expressions` without a `frames` array.
/// Produced by the desktop pose-library export (e.g. `assets/poses/*.json`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IosPoseFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    bones: HashMap<String, IosBoneRot>,
    #[serde(default)]
    expressions: HashMap<String, f32>,
    /// Used as the hold duration so the pose stays visible for a moment before clearing.
    #[serde(default)]
    transition_duration: Option<f32>,
    #[serde(default)]
    hold_duration: Option<f32>,
}

/// Load JSON from `JARVIS_ASSET_ROOT` / `rel_path` and snapshot bone entities under the avatar root.
///
/// `loop_override` — when `Some`, wins over the file's `"looping"` field (Swift Play/Loop buttons).
pub(crate) fn try_build_clip(
    rel_path: &str,
    world: &mut World,
    loop_override: Option<bool>,
) -> Option<ActiveJsonClip> {
    if !crate::ios_bevy::is_safe_asset_rel(rel_path) {
        crate::jarvis_ios_line!("[JarvisIOS] json anim: rejected unsafe path {rel_path:?}");
        return None;
    }
    let root = std::env::var("JARVIS_ASSET_ROOT").unwrap_or_else(|_| "assets".to_string());
    let abs = Path::new(&root).join(rel_path);
    let raw = std::fs::read_to_string(&abs).ok()?;
    let animation: IosAnimFile = serde_json::from_str(&raw).ok()?;
    // If no `frames` array, check for a pose file with top-level `bones`.
    let animation = if animation.frames.is_empty() {
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if v.get("bones").is_some_and(|b| b.is_object()) {
            match serde_json::from_value::<IosPoseFile>(v) {
                Ok(pose) => {
                    let hold = pose.hold_duration
                        .or(pose.transition_duration)
                        .unwrap_or(1.0)
                        .max(0.05);
                    crate::jarvis_ios_line!(
                        "[JarvisIOS] json anim: pose file {} → single-frame hold={:.2}s",
                        abs.display(),
                        hold
                    );
                    IosAnimFile {
                        name: pose.name,
                        fps: 30.0,
                        frames: vec![IosAnimFrame {
                            bones: pose.bones,
                            expressions: pose.expressions,
                            root_position: None,
                        }],
                        looping: Some(false),
                        hold_duration: Some(hold),
                    }
                }
                Err(e) => {
                    crate::jarvis_ios_line!(
                        "[JarvisIOS] json anim: pose parse failed {}: {e}",
                        abs.display()
                    );
                    return None;
                }
            }
        } else {
            crate::jarvis_ios_line!("[JarvisIOS] json anim: no frames in {}", abs.display());
            return None;
        }
    } else {
        animation
    };
    let avatar_root = world
        .query_filtered::<Entity, With<JarvisIosAvatarRoot>>()
        .iter(world)
        .next()?;
    let vrm_entity = world
        .query_filtered::<Entity, With<Vrm>>()
        .iter(world)
        .next()?;
    let bone_lower_to_entity = build_bone_name_map(world, avatar_root);
    // Bones this clip writes → entities, so we can restore them to rest when it
    // ends / is superseded (no persistent overrides).
    let mut touched_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in &animation.frames {
        for k in f.bones.keys() {
            touched_keys.insert(k.to_ascii_lowercase());
        }
    }
    let touched_bones: Vec<Entity> = touched_keys
        .iter()
        .filter_map(|k| bone_lower_to_entity.get(k).copied())
        .collect();
    let has_root = animation.frames.iter().any(|f| f.root_position.is_some());
    let hips_entity = if has_root {
        bone_lower_to_entity.get("hips").copied()
    } else {
        None
    };
    // Snapshot the current rig pose for the entry blend (covers handoffs from
    // idle VRMA, a previous clip's held end pose, or manual posing).
    let mut entry_pose: HashMap<Entity, (Quat, Vec3)> =
        HashMap::with_capacity(touched_bones.len() + 1);
    for &ent in touched_bones.iter().chain(hips_entity.iter()) {
        if let Some(tf) = world.get::<Transform>(ent) {
            entry_pose.insert(ent, (tf.rotation, tf.translation));
        }
    }
    let fps = if animation.fps > 0.0 {
        animation.fps as f32
    } else {
        30.0
    };
    let frame_duration_secs = (1.0 / fps).max(1.0 / 240.0);
    let looping = loop_override.unwrap_or_else(|| animation.looping.unwrap_or(false));
    let hold_duration_secs = animation.hold_duration.unwrap_or(0.35).max(0.05);

    crate::jarvis_ios_line!(
        "[JarvisIOS] json anim: start {} frames={} fps={} loop={} root_motion={} bone_name_index={}",
        animation.name,
        animation.frames.len(),
        fps,
        looping,
        hips_entity.is_some(),
        bone_lower_to_entity.len()
    );

    Some(ActiveJsonClip {
        animation,
        rel_path: rel_path.to_string(),
        elapsed: 0.0,
        frame_duration_secs,
        looping,
        hold_duration_secs,
        holding_elapsed: 0.0,
        finished_timeline: false,
        bone_lower_to_entity,
        hips_entity,
        vrm_entity,
        stopped_idle_vrma: Vec::new(),
        touched_bones,
        entry_pose,
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
        // Stop self-loop re-triggering while paused, or it would resume itself.
        world
            .entity_mut(e)
            .remove::<crate::ios_bevy::IosVrmaSelfLoop>();
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

/// Start the configured idle VRMA loop (used when user re-enables idle after load).
pub(crate) fn start_matching_idle_vrma(
    world: &mut World,
    avatar_root: Entity,
    settings: &IosAvatarSettings,
) -> Vec<Entity> {
    let idle = settings.idle_vrma_path.trim();
    if idle.is_empty() {
        return Vec::new();
    }
    let mut started = Vec::new();
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
        // Self-crossfade loop (no hard wrap): single pass + IosVrmaSelfLoop re-trigger.
        world
            .entity_mut(e)
            .insert(crate::ios_bevy::IosVrmaSelfLoop::default());
        world.entity_mut(e).trigger(|ent| PlayVrma {
            repeat: RepeatAnimation::Never,
            transition_duration: Duration::ZERO,
            vrma: ent,
            reset_spring_bones: false,
        });
        started.push(e);
    }
    if !started.is_empty() {
        crate::jarvis_ios_line!(
            "[JarvisIOS] idle playback: started {} VRMA target(s)",
            started.len()
        );
    }
    started
}

fn resume_idle_vrmas(commands: &mut Commands, stopped: &[Entity]) {
    for &vrma_e in stopped {
        commands
            .entity(vrma_e)
            .insert(crate::ios_bevy::IosVrmaSelfLoop::default());
        commands.entity(vrma_e).trigger(|e| PlayVrma {
            repeat: RepeatAnimation::Never,
            transition_duration: Duration::from_secs_f32(crate::ios_bevy::VRMA_LOOP_FADE_SECS),
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
        world
            .entity_mut(e)
            .insert(crate::ios_bevy::IosVrmaSelfLoop::default());
        world.entity_mut(e).trigger(|ent| PlayVrma {
            repeat: RepeatAnimation::Never,
            transition_duration: Duration::from_secs_f32(crate::ios_bevy::VRMA_LOOP_FADE_SECS),
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

/// One continuously-sampled instant of a clip: SLERPed bone quats (still in
/// normalized-humanoid space), lerped expression weights, lerped root delta.
struct ClipSample {
    bones: HashMap<String, Quat>,
    expressions: HashMap<String, f32>,
    root: Option<Vec3>,
}

fn frame_quat(rot: &IosBoneRot) -> Option<Quat> {
    let q = Quat::from_xyzw(rot.rotation[0], rot.rotation[1], rot.rotation[2], rot.rotation[3]);
    (q.x.is_finite() && q.y.is_finite() && q.z.is_finite() && q.w.is_finite())
        .then(|| q.normalize())
}

/// SLERP/lerp two frames at blend factor `u` (0 → f0, 1 → f1).
fn blend_frames(f0: &IosAnimFrame, f1: &IosAnimFrame, u: f32) -> ClipSample {
    let mut bones = HashMap::with_capacity(f0.bones.len().max(f1.bones.len()));
    for (k, r0) in &f0.bones {
        let Some(q0) = frame_quat(r0) else { continue };
        let q = match f1.bones.get(k).and_then(frame_quat) {
            Some(q1) => q0.slerp(q1, u),
            None => q0,
        };
        bones.insert(k.clone(), q);
    }
    for (k, r1) in &f1.bones {
        if !bones.contains_key(k) {
            if let Some(q1) = frame_quat(r1) {
                bones.insert(k.clone(), q1);
            }
        }
    }
    let mut expressions = HashMap::new();
    if !f0.expressions.is_empty() || !f1.expressions.is_empty() {
        for k in f0.expressions.keys().chain(f1.expressions.keys()) {
            if expressions.contains_key(k) {
                continue;
            }
            let a = f0.expressions.get(k).copied().unwrap_or(0.0);
            let b = f1.expressions.get(k).copied().unwrap_or(0.0);
            expressions.insert(k.clone(), a + (b - a) * u);
        }
    }
    let root = match (f0.root_position, f1.root_position) {
        (None, None) => None,
        (a, b) => {
            let a = Vec3::from(a.unwrap_or([0.0; 3]));
            let b = Vec3::from(b.unwrap_or([0.0; 3]));
            Some(a.lerp(b, u))
        }
    };
    ClipSample { bones, expressions, root }
}

/// Blend an existing sample toward another by `w` (loop-seam crossfade).
fn blend_samples(s: &mut ClipSample, toward: &ClipSample, w: f32) {
    for (k, q) in s.bones.iter_mut() {
        if let Some(qt) = toward.bones.get(k) {
            *q = q.slerp(*qt, w);
        }
    }
    for (k, v) in s.expressions.iter_mut() {
        let t = toward.expressions.get(k).copied().unwrap_or(0.0);
        *v += (t - *v) * w;
    }
    if let (Some(r), Some(rt)) = (s.root.as_mut(), toward.root) {
        *r = r.lerp(rt, w);
    }
}

/// Sample the clip at time `t` (seconds): continuous SLERP between bracketing
/// frames, wrap-aware for looping clips, plus a `LOOP_FADE_SECS` crossfade back
/// into frame 0 across the loop seam (desktop `loop_fade` parity).
fn sample_clip_at(anim: &IosAnimFile, t: f32, frame_dur: f32, looping: bool) -> ClipSample {
    let total = anim.frames.len();
    let frame_f = (t / frame_dur).max(0.0);
    let i0 = (frame_f.floor() as usize).min(total - 1);
    let u = (frame_f - i0 as f32).clamp(0.0, 1.0);
    let i1 = if i0 + 1 < total {
        i0 + 1
    } else if looping {
        0
    } else {
        i0
    };
    let mut s = blend_frames(&anim.frames[i0], &anim.frames[i1], u);
    if looping && total > 1 {
        let total_dur = total as f32 * frame_dur;
        let remaining = total_dur - t;
        if remaining < LOOP_FADE_SECS {
            let w = 1.0 - (remaining / LOOP_FADE_SECS).clamp(0.0, 1.0);
            let first = blend_frames(&anim.frames[0], &anim.frames[0], 0.0);
            blend_samples(&mut s, &first, w);
        }
    }
    s
}

fn ios_json_anim_tick(
    time: Res<Time>,
    mut playback: ResMut<IosJsonAnimPlayback>,
    mut idle_state: ResMut<IosJsonIdleState>,
    mut playlist: ResMut<IosJsonPlaylist>,
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
    let total_dur = total as f32 * clip.frame_duration_secs;

    if !clip.finished_timeline {
        clip.elapsed += time.delta_secs();
        let (t, finished) = if clip.looping {
            (clip.elapsed.rem_euclid(total_dur), false)
        } else if clip.elapsed >= total_dur {
            (total_dur - 1e-4, true)
        } else {
            (clip.elapsed, false)
        };

        let sample = sample_clip_at(&clip.animation, t, clip.frame_duration_secs, clip.looping);
        // Entry blend: ease from the rig's pose at clip start into the clip.
        let entry = (clip.elapsed < ENTRY_BLEND_SECS)
            .then(|| (&clip.entry_pose, smoothstep01(clip.elapsed / ENTRY_BLEND_SECS)));
        apply_sample(
            &sample,
            &clip.bone_lower_to_entity,
            clip.hips_entity,
            entry,
            &mut transforms,
            &mut commands,
            clip.vrm_entity,
        );

        if finished {
            clip.finished_timeline = true;
            clip.holding_elapsed = 0.0;
        }
        return;
    }

    clip.holding_elapsed += time.delta_secs();
    if clip.holding_elapsed >= clip.hold_duration_secs {
        crate::jarvis_ios_line!("[JarvisIOS] json anim: finished {}", clip.animation.name);
        let stopped = clip.stopped_idle_vrma.clone();
        let touched = clip.touched_bones.clone();
        // Playlist continues: leave the held pose + expressions in place — the
        // next item ENTRY-BLENDS from here (no rest-pose snap between items).
        if playlist.has_next() {
            playback.stop();
            playlist.advance_pending = true;
            return;
        }
        commands.trigger(ClearExpressions {
            entity: clip.vrm_entity,
        });
        playback.stop();
        if playlist.active {
            // List exhausted (non-looping) — done.
            playlist.active = false;
            playlist.advance_pending = false;
        }
        // Restore the clip's bones to rest (rotation AND translation — root
        // motion moves the hips) so the pose doesn't persist as a stuck
        // override after the hold; idle / layers then re-compose from rest.
        for &ent in &touched {
            if let Ok((mut tf, Some(rest), _)) = transforms.get_mut(ent) {
                tf.rotation = rest.0.rotation;
                tf.translation = rest.0.translation;
            }
        }
        resume_idle_vrmas(&mut commands, &stopped);
        // JSON-clip idle configured → ask the restart system to bring it back.
        if idle_state.rel_path.is_some() {
            idle_state.pending = true;
        }
    }
}

fn apply_sample(
    sample: &ClipSample,
    bone_map: &HashMap<String, Entity>,
    hips_entity: Option<Entity>,
    entry: Option<(&HashMap<Entity, (Quat, Vec3)>, f32)>,
    transforms: &mut Query<(
        &mut Transform,
        Option<&RestTransform>,
        Option<&RestGlobalTransform>,
    )>,
    commands: &mut Commands,
    vrm_entity: Entity,
) {
    for (bone_name, &pose_q) in &sample.bones {
        let key = bone_name.to_ascii_lowercase();
        let Some(&ent) = bone_map.get(&key) else {
            continue;
        };
        let Ok((mut tf, rest, rest_world)) = transforms.get_mut(ent) else {
            continue;
        };
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
            let mut out = final_q.normalize();
            if let Some((entry_pose, w)) = entry {
                if let Some(&(eq, _)) = entry_pose.get(&ent) {
                    out = eq.slerp(out, w).normalize();
                }
            }
            tf.rotation = out;
        }
    }

    // Root motion: hips translation = rest + delta (desktop pose_driver parity).
    if let (Some(root), Some(hips)) = (sample.root, hips_entity) {
        if root.is_finite() {
            if let Ok((mut tf, Some(rest), _)) = transforms.get_mut(hips) {
                let mut target = rest.0.translation + root;
                if let Some((entry_pose, w)) = entry {
                    if let Some(&(_, et)) = entry_pose.get(&hips) {
                        target = et.lerp(target, w);
                    }
                }
                tf.translation = target;
            }
        }
    }

    if !sample.expressions.is_empty() {
        let weights: HashMap<VrmExpression, f32> = sample
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

/// Start a playlist of JSON clips (asset-relative paths). `looping` wraps the
/// list. Cancels the JSON idle while running; the idle returns when the list
/// finishes (non-looping) or the playlist is cancelled by a direct clip/VRMA.
pub(crate) fn ios_start_json_playlist(world: &mut World, items: Vec<String>, looping: bool) {
    if items.is_empty() {
        ios_cancel_json_playlist(world);
        return;
    }
    crate::jarvis_ios_line!(
        "[JarvisIOS] playlist: start {} item(s) loop={looping}",
        items.len()
    );
    {
        let mut pl = world.resource_mut::<IosJsonPlaylist>();
        pl.items = items;
        pl.looping = looping;
        pl.next_index = 0;
        pl.active = true;
        pl.advance_pending = true; // advance system starts item 0
    }
    world.resource_mut::<IosJsonIdleState>().pending = false;
}

pub(crate) fn ios_cancel_json_playlist(world: &mut World) {
    let mut pl = world.resource_mut::<IosJsonPlaylist>();
    if pl.active {
        crate::jarvis_ios_line!("[JarvisIOS] playlist: cancelled");
    }
    pl.active = false;
    pl.advance_pending = false;
    pl.items.clear();
    pl.next_index = 0;
}

/// Exclusive system: start the next playlist item when the previous finished.
/// Runs before the idle restart so the playlist owns the rig while active.
pub(crate) fn ios_json_playlist_advance(world: &mut World) {
    let next: Option<String> = {
        let pl = world.resource::<IosJsonPlaylist>();
        if !(pl.active && pl.advance_pending) {
            None
        } else if world.resource::<IosJsonAnimPlayback>().is_active() {
            None // something still playing — finish path re-flags
        } else {
            let mut pl = world.resource_mut::<IosJsonPlaylist>();
            if pl.next_index >= pl.items.len() {
                if pl.looping {
                    pl.next_index = 0;
                } else {
                    pl.active = false;
                    pl.advance_pending = false;
                    // hand back to the JSON idle if one is configured
                    drop(pl);
                    world.resource_mut::<IosJsonIdleState>().pending = true;
                    return;
                }
            }
            let item = pl.items[pl.next_index].clone();
            pl.next_index += 1;
            Some(item)
        }
    };
    let Some(item) = next else { return };
    ios_apply_json_anim_request_inner(world, item.clone(), false);
    if world.resource::<IosJsonAnimPlayback>().is_active() {
        world.resource_mut::<IosJsonPlaylist>().advance_pending = false;
        crate::jarvis_ios_line!("[JarvisIOS] playlist: → {item}");
    }
    // else: rig not ready — retry next frame (advance_pending stays set)
}

/// Exclusive system: when a JSON-clip idle is configured and `pending`, (re)build
/// it as a looping clip once the rig exists. Retries every frame until the clip
/// actually starts (entities may not be ready right after spawn / model swap).
/// A playing user clip defers the restart (the finish path re-flags `pending`).
pub(crate) fn ios_json_idle_restart(world: &mut World) {
    let (path, pending) = {
        let s = world.resource::<IosJsonIdleState>();
        (s.rel_path.clone(), s.pending)
    };
    if !pending {
        return;
    }
    if world.resource::<IosJsonPlaylist>().active {
        return; // playlist owns the rig — idle resumes when it ends
    }
    let Some(path) = path else {
        world.resource_mut::<IosJsonIdleState>().pending = false;
        return;
    };
    if !world
        .resource::<crate::ios_bevy::IosIdlePlaybackState>()
        .user_enabled
    {
        world.resource_mut::<IosJsonIdleState>().pending = false;
        return;
    }
    if world.resource::<IosJsonAnimPlayback>().is_active() {
        return; // a clip is playing — retry once it finishes
    }
    ios_apply_json_anim_request_inner(world, path, true);
    if world.resource::<IosJsonAnimPlayback>().is_active() {
        world.resource_mut::<IosJsonIdleState>().pending = false;
        crate::jarvis_ios_line!("[JarvisIOS] json idle: started looping idle clip");
    }
}

/// Same as [`crate::ios_bevy::ios_apply_json_anim_request`] but WITHOUT
/// cancelling the playlist — used by the playlist/idle machinery itself.
pub(crate) fn ios_apply_json_anim_request_inner(world: &mut World, path: String, loop_forever: bool) {
    crate::ios_bevy::ios_apply_json_anim_request_no_playlist_cancel(world, path, loop_forever);
}

pub fn plugin(app: &mut App) {
    app.init_resource::<IosJsonAnimPlayback>()
        .init_resource::<IosJsonIdleState>()
        .init_resource::<IosJsonPlaylist>()
        .add_systems(
            PostUpdate,
            // 1) After VRMA sampling (`AnimationSystems`). 2) Before VRM roll / rotation constraints
            //    (same slot as desktop `apply_pose_commands`) so constraints do not stomp our pose.
            // 3) Before transform propagation. JSON rotations are **normalized humanoid** space; see
            //    `local_from_normalized` + `apply_sample`.
            ios_json_anim_tick
                .after(AnimationSystems)
                .before(VrmSystemSets::Constraints)
                .before(TransformSystems::Propagate),
        )
        // Playlist advance runs before the JSON idle restart so a running
        // playlist owns the rig; both after this frame's user requests.
        .add_systems(Last, (ios_json_playlist_advance, ios_json_idle_restart).chain());
}
