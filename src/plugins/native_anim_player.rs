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

use jarvis_avatar::pose_library::{AnimationFile, AnimationFrame};

use crate::plugins::anim_layers::smoothstep;
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};

/// Seconds of crossfade across a looping clip's end → start seam. The last
/// `LOOP_FADE_SECS` of the loop are blended toward frame 0 so the wrap is
/// continuous instead of hard-snapping (the "glitch reset" on loop restart).
const LOOP_FADE_SECS: f32 = 0.35;

pub struct NativeAnimPlayerPlugin;

impl Plugin for NativeAnimPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActiveNativeAnimation>()
            .insert_resource(StreamingAnimation::default())
            .add_systems(Update, tick_active_animation)
            .add_systems(Update, tick_streaming_animation);
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
    let (bones, expressions) = sampled_clip_pose(&clip.animation, t, fps, loop_period, loop_fade);
    push_pose(sender.as_ref(), bones, expressions);

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
) -> (HashMap<String, [f32; 4]>, HashMap<String, f32>) {
    let total = animation.frames.len();
    let frame_f = (t * fps).clamp(0.0, (total.saturating_sub(1)) as f32);
    let (mut bones, mut expr) = lerp_frames(animation, frame_f);

    if loop_fade > 1e-4 && loop_period > loop_fade {
        let fade_start = loop_period - loop_fade;
        if t >= fade_start {
            let w = smoothstep(((t - fade_start) / loop_fade).clamp(0.0, 1.0));
            let (start_bones, start_expr) = lerp_frames(animation, 0.0);
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
/// fractional frame index. Index is clamped — wrap blending is the caller's job
/// (via the loop crossfade).
fn lerp_frames(
    animation: &AnimationFile,
    frame_f: f32,
) -> (HashMap<String, Quat>, HashMap<String, f32>) {
    let total = animation.frames.len();
    let frame_f = frame_f.clamp(0.0, (total - 1) as f32);
    let idx0 = frame_f.floor() as usize;
    let idx1 = (idx0 + 1).min(total - 1);
    let frac = frame_f.fract();
    let a = &animation.frames[idx0];
    let b = &animation.frames[idx1];

    let mut names: std::collections::BTreeSet<&String> = a.bones.keys().collect();
    names.extend(b.bones.keys());
    let mut bones = HashMap::with_capacity(names.len());
    for name in names {
        let qa = a
            .bones
            .get(name)
            .map(|r| Quat::from_xyzw(r.rotation[0], r.rotation[1], r.rotation[2], r.rotation[3]));
        let qb = b
            .bones
            .get(name)
            .map(|r| Quat::from_xyzw(r.rotation[0], r.rotation[1], r.rotation[2], r.rotation[3]));
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
}

impl StreamingAnimation {
    pub fn begin(&self, request_id: impl Into<String>, fps: f32) {
        let mut s = self.inner.write();
        s.queue.clear();
        s.fps = fps.max(1.0);
        s.last_emit = 0.0;
        s.active_request_id = Some(request_id.into());
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
) {
    let Some(sender) = sender else {
        return;
    };
    let mut s = streaming.inner.write();
    if s.queue.is_empty() {
        s.last_emit = 0.0;
        return;
    }
    let frame_dt = 1.0 / s.fps.max(1.0);
    s.last_emit += time.delta_secs();
    while s.last_emit >= frame_dt {
        let Some(frame) = s.queue.pop_front() else {
            break;
        };
        push_frame(sender.as_ref(), &frame);
        s.last_emit -= frame_dt;
    }
}
