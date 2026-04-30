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

use std::collections::VecDeque;
use std::sync::Arc;

use bevy::prelude::*;
use parking_lot::RwLock;

use jarvis_avatar::pose_library::{AnimationFile, AnimationFrame};

use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};

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
#[derive(Resource, Default)]
pub struct ActiveNativeAnimation {
    inner: Option<ActiveClip>,
    /// Last known frame applied — prevents repeated `ApplyBones` bursts when
    /// the frame clock advances less than one frame between ticks.
    last_applied_frame: Option<usize>,
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
    let Some(clip) = active.inner.as_mut() else {
        return;
    };
    if clip.animation.frames.is_empty() {
        active.stop();
        return;
    }

    clip.elapsed += time.delta_secs();
    let total_frames = clip.animation.frames.len();
    let frame_idx_raw = (clip.elapsed / clip.frame_duration_secs) as i64;

    let (frame_idx, finished) = if frame_idx_raw < total_frames as i64 {
        (frame_idx_raw as usize, false)
    } else if clip.looping {
        (
            (frame_idx_raw.rem_euclid(total_frames as i64)) as usize,
            false,
        )
    } else {
        (total_frames - 1, true)
    };

    if Some(frame_idx) != active.last_applied_frame {
        if let Some(frame) = active
            .inner
            .as_ref()
            .and_then(|c| c.animation.frames.get(frame_idx))
        {
            push_frame(sender.as_ref(), frame);
        }
        active.last_applied_frame = Some(frame_idx);
    }

    if finished {
        let clip = active.inner.as_mut().unwrap();
        clip.holding_elapsed += time.delta_secs();
        if clip.holding_elapsed >= clip.hold_duration_secs {
            active.stop();
        }
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
