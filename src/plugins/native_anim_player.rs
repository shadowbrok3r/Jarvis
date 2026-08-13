//! Native (Bevy-side) player for saved animations.
//!
//! An [`AnimationFile`] is a list of keyframes; each frame carries bone
//! rotations that we re-emit as [`PoseCommand::ApplyBones`] at the clip's
//! declared FPS. Supports the same looping / hold semantics the Node
//! pose-controller honours, plus a "streaming" mode where Kimodo pushes
//! frames into a ring buffer that we drain in order.
//!
//! This plugin is deliberately decoupled from Kimodo's own playback lane —
//! the UI chooses whether a given animation is played here (per-frame on
//! Bevy's clock) or by forwarding `kimodo:play-animation` to the Python
//! peer.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use bevy::prelude::*;
use parking_lot::RwLock;

use crate::pose_library::{AnimationFile, AnimationFrame};

use crate::plugins::anim_layers::smoothstep;
use crate::plugins::pose_driver::{BoneSnapshotHandle, PoseCommand, PoseCommandSender};

/// Seconds of crossfade across a looping clip's end → start seam. The last
/// `LOOP_FADE_SECS` of the loop are blended toward frame 0 so the wrap is
/// continuous instead of hard-snapping (the "glitch reset" on loop restart).
const LOOP_FADE_SECS: f32 = 0.35;

/// Seconds to ease from the rig's CURRENT pose into a clip's first frame.
/// Applied only on the first frame after `start()` — which covers both the
/// entry from idle AND a back-to-back handoff (each new clip re-`start()`s, so
/// the rig blends from the previous clip's end pose instead of teleporting).
/// Without this, a stand→kneel clip ending in a kneel snaps the whole skeleton
/// back to standing in one frame (the 9000°/s leg spikes in the glitch log).
const ENTRY_BLEND_SECS: f32 = 0.22;

/// Seconds to ease from the rig's current pose into a Kimodo stream's first
/// frames, so a live generation never teleports the skeleton.
const STREAM_EASE_SECS: f32 = 0.25;

/// Seconds the layer stack blends from the native handoff pose back to its own
/// composition after native playback ends.
pub const RETURN_BLEND_SECS: f32 = 0.3;

/// Which engine owns body-bone emission. The layer stack composes in every
/// state (procedural state keeps advancing, expressions still emit) but only
/// writes body bones when it owns them; native playback and Kimodo streaming
/// take the body exclusively and hand it back through a timed blend.
#[derive(Resource, Default)]
pub enum BodyOwner {
    #[default]
    Stack,
    Native,
    ReturningToStack {
        t: f32,
        handoff: HashMap<String, [f32; 4]>,
    },
}

/// Tracks native/stream activity and drives the ownership state machine.
fn sync_body_owner(
    time: Res<Time>,
    active: Res<ActiveNativeAnimation>,
    streaming: Res<StreamingAnimation>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
    mut owner: ResMut<BodyOwner>,
) {
    let native_active = active.is_playing()
        || streaming.pending_frames() > 0
        || streaming.active_request_id().is_some();
    match &mut *owner {
        BodyOwner::Stack => {
            if native_active {
                *owner = BodyOwner::Native;
            }
        }
        BodyOwner::Native => {
            if !native_active {
                let handoff = snapshot
                    .as_ref()
                    .map(|h| {
                        h.0.read()
                            .bones
                            .iter()
                            .map(|(name, e)| (name.clone(), e.rotation))
                            .collect::<HashMap<String, [f32; 4]>>()
                    })
                    .unwrap_or_default();
                *owner = BodyOwner::ReturningToStack { t: 0.0, handoff };
            }
        }
        BodyOwner::ReturningToStack { t, .. } => {
            if native_active {
                *owner = BodyOwner::Native;
            } else {
                *t += time.delta_secs();
                if *t >= RETURN_BLEND_SECS {
                    *owner = BodyOwner::Stack;
                }
            }
        }
    }
}

pub struct NativeAnimPlayerPlugin;

impl Plugin for NativeAnimPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActiveNativeAnimation>()
            .init_resource::<BodyOwner>()
            .insert_resource(StreamingAnimation::default())
            .add_systems(Update, tick_active_animation)
            .add_systems(Update, tick_streaming_animation)
            .add_systems(
                Update,
                sync_body_owner
                    .after(tick_active_animation)
                    .after(tick_streaming_animation),
            );
    }
}

// ---------- one-shot saved animation -------------------------------------------

/// Active keyframe-driven animation. `frame_index` advances on a fixed clock
/// (`1.0 / animation.fps`) and wraps when `looping` is true, or pauses on the
/// last frame for `hold_duration_secs` otherwise.
///
/// `paused` is a global hold flag — when true, [`tick_active_animation`]
/// returns early so the rig stays on whatever frame was last applied.
/// Spacebar in the menu bar toggles this.
#[derive(Resource, Default)]
pub struct ActiveNativeAnimation {
    inner: Option<ActiveClip>,
    /// Last known frame applied — prevents repeated `ApplyBones` bursts when
    /// the frame clock advances less than one frame between ticks.
    last_applied_frame: Option<usize>,
    paused: bool,
}

struct ActiveClip {
    animation: AnimationFile,
    looping: bool,
    hold_duration_secs: f32,
    elapsed: f32,
    holding_elapsed: f32,
    frame_duration_secs: f32,
    /// Rig pose (ApplyBones-space quats) captured at clip start, used to ease
    /// from the current pose into the clip over `ENTRY_BLEND_SECS` instead of
    /// teleporting. `None` until captured on the first tick; cleared once the
    /// entry window elapses.
    entry_from: Option<HashMap<String, [f32; 4]>>,
}

impl ActiveNativeAnimation {
    pub fn is_playing(&self) -> bool {
        self.inner.is_some()
    }

    pub fn current_name(&self) -> Option<&str> {
        self.inner.as_ref().map(|c| c.animation.name.as_str())
    }

    pub fn frame_count(&self) -> usize {
        self.inner
            .as_ref()
            .map(|c| c.animation.frames.len())
            .unwrap_or(0)
    }

    pub fn current_frame(&self) -> Option<usize> {
        self.last_applied_frame
    }

    pub fn start(&mut self, animation: AnimationFile, looping: bool, hold_duration_secs: f32) {
        let fps = if animation.fps > 0.0 {
            animation.fps as f32
        } else {
            30.0
        };
        self.inner = Some(ActiveClip {
            animation,
            looping,
            hold_duration_secs,
            elapsed: 0.0,
            holding_elapsed: 0.0,
            frame_duration_secs: (1.0 / fps).max(1.0 / 240.0),
            entry_from: None,
        });
        self.last_applied_frame = None;
    }

    pub fn stop(&mut self) {
        self.inner = None;
        self.last_applied_frame = None;
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Flip the pause flag. Returns the new state. When there's no active
    /// clip the flag is reset to `false` so a future `start()` begins
    /// playing immediately.
    pub fn toggle_paused(&mut self) -> bool {
        if self.inner.is_none() {
            self.paused = false;
            return false;
        }
        self.paused = !self.paused;
        self.paused
    }
}

fn tick_active_animation(
    time: Res<Time>,
    mut active: ResMut<ActiveNativeAnimation>,
    sender: Option<Res<PoseCommandSender>>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
) {
    let Some(sender) = sender else {
        return;
    };
    if active.paused {
        return;
    }
    let Some(clip) = active.inner.as_mut() else {
        return;
    };
    if clip.animation.frames.is_empty() {
        active.stop();
        return;
    }

    clip.elapsed += time.delta_secs();
    // Entry ease-in: for the first ENTRY_BLEND_SECS of a clip (fresh play OR a
    // back-to-back handoff — `start()` resets `elapsed` to 0), capture the rig's
    // current pose and slerp from it into the clip so the skeleton doesn't
    // teleport (the 9000°/s clip-boundary spikes in the glitch log). The blend
    // is baked into the emitted pose, so playback stays instant-applied and
    // independent of the global `blend_transitions_enabled` flag.
    let in_entry_window = clip.elapsed <= ENTRY_BLEND_SECS;
    if in_entry_window && clip.entry_from.is_none() {
        let captured = snapshot
            .as_ref()
            .map(|h| {
                h.0.read()
                    .bones
                    .iter()
                    .map(|(name, e)| (name.clone(), e.rotation))
                    .collect::<HashMap<String, [f32; 4]>>()
            })
            .unwrap_or_default();
        clip.entry_from = Some(captured);
    } else if !in_entry_window {
        clip.entry_from = None;
    }
    let total_frames = clip.animation.frames.len();
    let fps = 1.0 / clip.frame_duration_secs;
    // Loop period: frame i sits at t = i / fps, so the wrap back to frame 0
    // happens one frame-duration after the last frame.
    let loop_period = total_frames as f32 * clip.frame_duration_secs;

    // Continuous playhead time + which frame index we're nearest (for UI).
    let (t, finished) = if clip.looping {
        (clip.elapsed.rem_euclid(loop_period), false)
    } else {
        let last_t = (total_frames.saturating_sub(1)) as f32 * clip.frame_duration_secs;
        (clip.elapsed.min(last_t), clip.elapsed >= last_t)
    };

    let loop_fade = if clip.looping { LOOP_FADE_SECS } else { 0.0 };
    let (mut bones, expressions) =
        sampled_clip_pose(&clip.animation, t, fps, loop_period, loop_fade, clip.looping);
    if in_entry_window {
        if let Some(from) = clip.entry_from.as_ref() {
            let w = smoothstep((clip.elapsed / ENTRY_BLEND_SECS).clamp(0.0, 1.0));
            for (name, q) in bones.iter_mut() {
                if let Some(f) = from.get(name) {
                    let a = Quat::from_xyzw(f[0], f[1], f[2], f[3]);
                    let b = Quat::from_xyzw(q[0], q[1], q[2], q[3]);
                    let blended = a.slerp(b, w);
                    *q = [blended.x, blended.y, blended.z, blended.w];
                }
            }
        }
    }
    push_pose(sender.as_ref(), bones, expressions);

    // Root motion: clips carrying a `root_position` track (e.g. the squat's hip
    // drop) emit a hips translation delta. The anti-slide lock runs *before*
    // `apply_pose_commands`, so this deliberate delta survives; rotation-only
    // clips emit nothing and stay locked to bind.
    if let Some(root) =
        crate::plugins::anim_layers::sample_clip_root_position(&clip.animation, t, clip.looping)
    {
        push_root_translation(sender.as_ref(), root);
    }

    let nearest = ((t * fps).round() as usize).min(total_frames - 1);
    active.last_applied_frame = Some(nearest);

    if finished {
        let clip = active.inner.as_mut().unwrap();
        clip.holding_elapsed += time.delta_secs();
        if clip.holding_elapsed >= clip.hold_duration_secs {
            active.stop();
        }
    }
}

/// Interpolated clip pose (bones + expressions) at continuous time `t`
/// (seconds). When `loop_fade > 0`, the last `loop_fade` seconds are blended
/// toward the clip's first frame so a looping clip's end → start wrap is
/// seamless even when the author's last frame ≠ first frame.
fn sampled_clip_pose(
    animation: &AnimationFile,
    t: f32,
    fps: f32,
    loop_period: f32,
    loop_fade: f32,
    wrap: bool,
) -> (HashMap<String, [f32; 4]>, HashMap<String, f32>) {
    let total = animation.frames.len();
    let frame_f = if wrap && total > 1 {
        (t * fps).rem_euclid(total as f32)
    } else {
        (t * fps).clamp(0.0, (total.saturating_sub(1)) as f32)
    };
    let (mut bones, mut expr) = lerp_frames(animation, frame_f, wrap);

    if loop_fade > 1e-4 && loop_period > loop_fade {
        let fade_start = loop_period - loop_fade;
        if t >= fade_start {
            let w = smoothstep(((t - fade_start) / loop_fade).clamp(0.0, 1.0));
            let (start_bones, start_expr) = lerp_frames(animation, 0.0, false);
            for (name, target) in start_bones {
                let blended = bones
                    .get(&name)
                    .map(|cur| cur.slerp(target, w))
                    .unwrap_or(target);
                bones.insert(name, blended);
            }
            for (name, target) in start_expr {
                let cur = expr.get(&name).copied().unwrap_or(0.0);
                expr.insert(name, cur + (target - cur) * w);
            }
        }
    }

    let bones_out = bones
        .into_iter()
        .map(|(name, q)| (name, [q.x, q.y, q.z, q.w]))
        .collect();
    (bones_out, expr)
}

/// Linearly (slerp for bones) interpolate between the two frames bracketing a
/// fractional frame index. `wrap` interpolates the final interval last→first;
/// otherwise the index clamps to the end frame.
fn lerp_frames(
    animation: &AnimationFile,
    frame_f: f32,
    wrap: bool,
) -> (HashMap<String, Quat>, HashMap<String, f32>) {
    let total = animation.frames.len();
    let (idx0, idx1, frac) = if wrap && total > 1 {
        let frame_f = frame_f.rem_euclid(total as f32);
        let idx0 = (frame_f.floor() as usize).min(total - 1);
        (idx0, (idx0 + 1) % total, frame_f.fract())
    } else {
        let frame_f = frame_f.clamp(0.0, (total - 1) as f32);
        let idx0 = frame_f.floor() as usize;
        (idx0, (idx0 + 1).min(total - 1), frame_f.fract())
    };
    let a = &animation.frames[idx0];
    let b = &animation.frames[idx1];

    let mut names: std::collections::BTreeSet<&String> = a.bones.keys().collect();
    names.extend(b.bones.keys());
    let mut bones = HashMap::with_capacity(names.len());
    for name in names {
        let qa = a
            .bones
            .get(name)
            .map(|r| crate::plugins::anim_layers::stored_quat(r.rotation));
        let qb = b
            .bones
            .get(name)
            .map(|r| crate::plugins::anim_layers::stored_quat(r.rotation));
        let q = match (qa, qb) {
            (Some(qa), Some(qb)) if frac > 0.0 => qa.slerp(qb, frac),
            (Some(qa), _) => qa,
            (None, Some(qb)) => qb,
            (None, None) => continue,
        };
        bones.insert(name.clone(), q);
    }

    let mut expr_names: std::collections::BTreeSet<&String> = a.expressions.keys().collect();
    expr_names.extend(b.expressions.keys());
    let mut expressions = HashMap::with_capacity(expr_names.len());
    for name in expr_names {
        let wa = a.expressions.get(name).copied().unwrap_or(0.0);
        let wb = b.expressions.get(name).copied().unwrap_or(wa);
        expressions.insert(name.clone(), wa + (wb - wa) * frac);
    }
    (bones, expressions)
}

fn push_pose(
    sender: &PoseCommandSender,
    bones: HashMap<String, [f32; 4]>,
    expressions: HashMap<String, f32>,
) {
    if !bones.is_empty() {
        sender.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: Some(0.0),
        });
    }
    if !expressions.is_empty() {
        sender.send(PoseCommand::ApplyExpression {
            weights: expressions,
            cancel_expression_animation: false,
        });
    }
}

fn push_root_translation(sender: &PoseCommandSender, root: Vec3) {
    let mut translations = HashMap::with_capacity(1);
    translations.insert("hips".to_string(), [root.x, root.y, root.z]);
    sender.send(PoseCommand::ApplyBoneTranslations(translations));
}

fn push_frame(sender: &PoseCommandSender, frame: &AnimationFrame) {
    let bones = frame
        .bones
        .iter()
        .map(|(k, v)| (k.clone(), v.rotation))
        .collect();
    sender.send(PoseCommand::ApplyBones {
        bones,
        preserve_omitted_bones: true,
        blend_weight: None,
        // Sending each frame with a transition equal to the frame duration
        // would require reading Time here; instant-snap is fine for 30+ FPS
        // native playback and keeps latency deterministic.
        transition_seconds: Some(0.0),
    });
    if !frame.expressions.is_empty() {
        sender.send(PoseCommand::ApplyExpression {
            weights: frame.expressions.clone(),
            cancel_expression_animation: false,
        });
    }
}

// ---------- streaming (Kimodo live) --------------------------------------------

/// Shared ring buffer Kimodo writes into (via [`crate::kimodo::KimodoClient`])
/// and `tick_streaming_animation` drains at a fixed FPS.
#[derive(Resource, Clone, Default)]
pub struct StreamingAnimation {
    inner: Arc<RwLock<StreamingState>>,
}

#[derive(Default)]
struct StreamingState {
    queue: VecDeque<AnimationFrame>,
    fps: f32,
    last_emit: f32,
    active_request_id: Option<String>,
    /// Rig pose captured when the first frame drains; eased toward the stream
    /// over [`STREAM_EASE_SECS`].
    ease_from: Option<HashMap<String, [f32; 4]>>,
    /// Seconds of stream emitted so far (drives the entry ease window).
    emitted_time: f32,
}

impl StreamingAnimation {
    pub fn begin(&self, request_id: impl Into<String>, fps: f32) {
        let mut s = self.inner.write();
        s.queue.clear();
        s.fps = fps.max(1.0);
        s.last_emit = 0.0;
        s.active_request_id = Some(request_id.into());
        s.ease_from = None;
        s.emitted_time = 0.0;
    }

    pub fn push_frame(&self, frame: AnimationFrame) {
        self.inner.write().queue.push_back(frame);
    }

    pub fn end(&self) {
        let mut s = self.inner.write();
        s.active_request_id = None;
    }

    pub fn active_request_id(&self) -> Option<String> {
        self.inner.read().active_request_id.clone()
    }

    pub fn pending_frames(&self) -> usize {
        self.inner.read().queue.len()
    }
}

fn tick_streaming_animation(
    time: Res<Time>,
    streaming: Res<StreamingAnimation>,
    sender: Option<Res<PoseCommandSender>>,
    snapshot: Option<Res<BoneSnapshotHandle>>,
) {
    let Some(sender) = sender else {
        return;
    };
    let mut s = streaming.inner.write();
    if s.queue.is_empty() {
        s.last_emit = 0.0;
        if s.active_request_id.is_none() {
            s.ease_from = None;
            s.emitted_time = 0.0;
        }
        return;
    }
    // Capture the live rig pose before the first frame lands so the stream
    // eases in from wherever the skeleton currently is.
    if s.ease_from.is_none() && s.emitted_time == 0.0 {
        let captured = snapshot
            .as_ref()
            .map(|h| {
                h.0.read()
                    .bones
                    .iter()
                    .map(|(name, e)| (name.clone(), e.rotation))
                    .collect::<HashMap<String, [f32; 4]>>()
            })
            .unwrap_or_default();
        s.ease_from = Some(captured);
    }
    let frame_dt = 1.0 / s.fps.max(1.0);
    s.last_emit += time.delta_secs();
    while s.last_emit >= frame_dt {
        let Some(mut frame) = s.queue.pop_front() else {
            break;
        };
        s.emitted_time += frame_dt;
        if s.emitted_time < STREAM_EASE_SECS {
            if let Some(from) = s.ease_from.as_ref() {
                let w = smoothstep(s.emitted_time / STREAM_EASE_SECS);
                for (name, r) in frame.bones.iter_mut() {
                    if let Some(f) = from.get(name) {
                        let a = Quat::from_xyzw(f[0], f[1], f[2], f[3]);
                        let b = crate::plugins::anim_layers::stored_quat(r.rotation);
                        let q = a.slerp(b, w);
                        r.rotation = [q.x, q.y, q.z, q.w];
                    }
                }
            }
        } else {
            s.ease_from = None;
        }
        push_frame(sender.as_ref(), &frame);
        s.last_emit -= frame_dt;
    }
}
