//! Layered animation: multiple drivers (clips + procedural) composed each frame.
//!
//! ## Why this exists
//!
//! `bevy_vrm1`'s `AnimationPlayer` can only play one VRMA at a time. The
//! native animation player (`native_anim_player.rs`) is a single-clip
//! playhead. The idle tick picks one pose *or* one clip at random. None of
//! these can *layer* — we can't have an idle breathing loop running under a
//! gesture, can't have auto-blink firing while a dance plays, can't mix in
//! finger / toe fidgets while the base body is static.
//!
//! This module defines a [`LayerStack`] of [`Layer`]s. Each layer has a
//! [`DriverKind`] (a clip playhead, a procedural breathing sine, a blink
//! state machine, etc.) that emits bone rotations and / or expression
//! weights each frame. A single ECS system composes every enabled layer
//! over the rig's rest pose and queues one [`PoseCommand::ApplyBones`] +
//! [`PoseCommand::ApplyExpression`] per tick.
//!
//! ## Tick order
//!
//! Layers are processed *in order*. The composition rule per bone is:
//!
//! ```text
//! let mut bone = Quat::IDENTITY; // placeholder meaning "no opinion yet"
//! for layer in &stack.layers where enabled && weight > 0:
//!     let target = layer.driver.sample(t, dt);
//!     match layer.blend_mode:
//!         Override    => bone = slerp(bone, target.abs,      layer.weight)
//!         RestRelative=> bone = bone * slerp(IDENTITY, target.delta, layer.weight)
//! ```
//!
//! `Override` layers overwrite earlier results by their weight (the
//! idiomatic "gesture plays over base" pattern). `RestRelative` layers
//! produce a delta rotation that's multiplied onto whatever composed so
//! far (procedural breathing / fidget on top of a base clip).
//!
//! ## Non-goals (v1)
//!
//! * Bone masking beyond include / exclude lists. Regex-style masks can be
//!   added by expanding [`BoneMask`].
//! * IK. CCDIK integration is a separate module.
//! * Gaze. `look_at.rs` already owns the eye bones; we leave it be.
//! * Conflict resolution with the per-slider `ApplyBones` in the Bones
//!   tab. The layer stack runs every frame with `preserve_omitted_bones:
//!   true`, so if a slider-driven bone is in a layer mask the layer wins
//!   on the next tick. Users who want to poke sliders should disable the
//!   stack master toggle first.
//!
//! **Pose hold** layers replay a single [`PoseFile`] (bones + VRM expression
//! weights from disk) every frame — useful as a static "start" / "end" pose
//! under procedural layers or other clips.
//!
//! ## Humanoid bone space vs `ApplyBones`
//!
//! The layer stack composes **raw local** rotations (same space as
//! [`RestTransform`] on each bone). [`PoseCommand::ApplyBones`] expects
//! **normalized humanoid** quaternions for VRM keys (see `pose_driver`). When
//! emitting `ApplyBones`, we convert each humanoid bone with
//! [`crate::plugins::pose_driver::normalized_from_local`] using the cached
//! rest-local and rest-world snapshot so MCP / UI reset and layers agree.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy_vrm1::prelude::*;
use parking_lot::RwLock;
use rand::RngExt;

use jarvis_avatar::pose_library::{AnimationFile, PoseFile};

use crate::plugins::pose_driver::{
    apply_pose_commands, normalized_from_local, sync_bone_entity_index, IndexedBones, PoseCommand,
    PoseCommandSender, VRM_BONE_NAMES,
};

pub struct AnimLayersPlugin;

impl Plugin for AnimLayersPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LayerStackHandle::default())
            .insert_resource(RestPoseSnapshot::default())
            // PostUpdate chain (see `pose_driver`): VRMA / `AnimationSystems` first, then
            // `sync_bone_entity_index` fills `IndexedBones`, we refresh rest locals, layers
            // enqueue `PoseCommand`s, `apply_pose_commands` writes transforms. All of this stays
            // `.before(VrmSystemSets::Constraints)` like the pose driver so aim/roll constraints
            // do not clobber our bones; `bevy_vrm1` runs `SpringBone` only after
            // `PropagateAfterExpressions`, so we never run after `SpringBone` and secondary
            // motion stays valid on top of the humanoid pose we authored.
            .add_systems(
                PostUpdate,
                (
                    refresh_rest_pose_snapshot
                        .after(AnimationSystems)
                        .after(sync_bone_entity_index),
                    advance_and_apply_layers
                        .after(refresh_rest_pose_snapshot)
                        .before(apply_pose_commands)
                        .before(VrmSystemSets::Constraints),
                ),
            );
    }
}

// ============================================================================
// Public, cloneable handle
// ============================================================================

/// Thread-safe wrapper around a shared [`LayerStack`]. The debug UI holds a
/// `Res<LayerStackHandle>` and locks briefly to mutate layer state; the ECS
/// system holds the same handle and locks to read / advance.
#[derive(Resource, Clone, Default)]
pub struct LayerStackHandle {
    pub inner: Arc<RwLock<LayerStack>>,
}

impl LayerStackHandle {
    pub fn with_write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut LayerStack) -> R,
    {
        let mut guard = self.inner.write();
        f(&mut *guard)
    }

    pub fn with_read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&LayerStack) -> R,
    {
        let guard = self.inner.read();
        f(&*guard)
    }
}

// ============================================================================
// Core types
// ============================================================================

/// Master container for all animation layers.
#[derive(Debug, Default)]
pub struct LayerStack {
    /// When `false`, the system short-circuits and emits nothing — so the
    /// rig is entirely driven by manual / MCP / idle_tick writes and feels
    /// identical to the pre-layer-stack behaviour. Defaults to `false` so
    /// enabling the plugin is a no-op until the user opts in via the UI.
    pub master_enabled: bool,
    /// Layers in processing order. Later layers override earlier ones (for
    /// `BlendMode::Override`) or compound on them (for `RestRelative`).
    pub layers: Vec<Layer>,
    /// Monotonic seconds counter — used as the `t` input to drivers so
    /// pausing the stack doesn't rewind phase.
    pub clock: f32,
    /// Next id issued by `add_layer`; monotonically increasing so deleting
    /// + re-adding a layer gives it a fresh id (egui needs stable widget
    /// ids).
    next_id: u64,
}

impl LayerStack {
    pub fn add_layer(&mut self, mut layer: Layer) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        layer.id = self.next_id;
        let id = layer.id;
        self.layers.push(layer);
        id
    }

    pub fn remove_layer(&mut self, id: u64) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| l.id != id);
        self.layers.len() != before
    }

    pub fn move_layer(&mut self, from: usize, to: usize) {
        if from >= self.layers.len() || to >= self.layers.len() {
            return;
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// Convenience for the UI: build the default built-in stack —
    /// breathing, auto-blink, finger fidget, toe fidget, weight shift. All
    /// low-weight so the first paint matches the "alive but not noisy"
    /// target the GF2 feel calls for.
    pub fn install_default_procedural_layers(&mut self) {
        let presets = [
            Layer::new("breathing", "Breathing", DriverKind::breathing_default())
                .blend(BlendMode::RestRelative)
                .weight(1.0),
            Layer::new("auto-blink", "Auto-Blink", DriverKind::blink_default())
                .blend(BlendMode::Override)
                .weight(1.0),
            Layer::new("weight-shift", "Weight Shift", DriverKind::weight_shift_default())
                .blend(BlendMode::RestRelative)
                .weight(0.8),
            Layer::new("finger-fidget", "Finger Fidget", DriverKind::finger_fidget_default())
                .blend(BlendMode::RestRelative)
                .weight(0.6),
            Layer::new("toe-fidget", "Toe Fidget", DriverKind::toe_fidget_default())
                .blend(BlendMode::RestRelative)
                .weight(0.4),
        ];
        for layer in presets {
            self.add_layer(layer);
        }
    }
}

/// A single layer in the stack.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Assigned by [`LayerStack::add_layer`]; 0 until then.
    pub id: u64,
    /// Stable short slug, used in status messages.
    pub slug: String,
    /// Human-readable label for the UI.
    pub label: String,
    /// Per-layer driver config + state.
    pub driver: DriverKind,
    /// Multiplier applied to the driver's output.
    pub weight: f32,
    /// Master switch — a disabled layer is skipped entirely.
    pub enabled: bool,
    /// How the layer composes onto previous layers.
    pub blend_mode: BlendMode,
    /// Which bones this layer is allowed to touch. `None` = all.
    pub mask: BoneMask,
    /// Playback clock (seconds into the clip / since driver started).
    pub time: f32,
    /// Time scale (1.0 = real time).
    pub speed: f32,
    /// Whether the playhead is advancing. UI "⏸" sets this false.
    pub playing: bool,
    /// Clip-style layers only. `None` means procedural / endless.
    pub duration: Option<f32>,
    /// Loop vs hold-last-frame. Procedural layers ignore this.
    pub looping: bool,
}

impl Layer {
    pub fn new(slug: impl Into<String>, label: impl Into<String>, driver: DriverKind) -> Self {
        let duration = driver.duration_hint();
        Self {
            id: 0,
            slug: slug.into(),
            label: label.into(),
            driver,
            weight: 1.0,
            enabled: true,
            blend_mode: BlendMode::Override,
            mask: BoneMask::default(),
            time: 0.0,
            speed: 1.0,
            playing: true,
            duration,
            looping: true,
        }
    }

    pub fn weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }

    pub fn blend(mut self, mode: BlendMode) -> Self {
        self.blend_mode = mode;
        self
    }

    /// Returns `(time, duration)` for the timeline widget. Procedural
    /// drivers without a duration report `(time mod 10.0, 10.0)` so the
    /// playhead still sweeps visibly.
    pub fn timeline_progress(&self) -> (f32, f32) {
        let duration = self.duration.unwrap_or(10.0).max(0.01);
        let t = if self.duration.is_some() {
            self.time.rem_euclid(duration)
        } else {
            self.time.rem_euclid(duration)
        };
        (t, duration)
    }
}

/// How a layer folds onto the accumulated result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// Driver's output is an *absolute* local rotation for the bone. The
    /// accumulated rotation is slerp'd toward it by `weight`. Weight = 1
    /// replaces earlier layers entirely (on touched bones).
    Override,
    /// Driver's output is a *delta* relative to the bone's rest pose. The
    /// delta is scaled by `weight` (via `slerp(IDENTITY, delta, weight)`)
    /// and multiplied on top of the accumulated rotation. Use for
    /// procedural breathing / fidgets that should ride on top of whatever
    /// base / gesture layers did.
    RestRelative,
}

impl BlendMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::RestRelative => "additive",
        }
    }
}

/// Bone inclusion / exclusion list. Both empty → all bones allowed.
#[derive(Debug, Clone, Default)]
pub struct BoneMask {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl BoneMask {
    pub fn allows(&self, bone: &str) -> bool {
        if !self.include.is_empty() && !self.include.iter().any(|n| n == bone) {
            return false;
        }
        if self.exclude.iter().any(|n| n == bone) {
            return false;
        }
        true
    }
}

// ============================================================================
// Driver variants
// ============================================================================

/// A sum of concrete driver states. Each variant carries its own config
/// and stateful fields (phase, RNG seed, frame cursor). The variant
/// matches the user-facing "what kind of layer is this" dropdown in the
/// UI — adding a new driver = adding a new variant + arm in `sample`.
#[derive(Debug, Clone)]
pub enum DriverKind {
    /// Replays a saved [`AnimationFile`] keyframe by keyframe. Emits
    /// absolute local rotations — use [`BlendMode::Override`].
    Clip {
        animation: Box<AnimationFile>,
    },
    /// Holds one [`PoseFile`] from the pose library (static bones + optional
    /// expression weights). Emits absolute rotations — use [`BlendMode::Override`].
    PoseHold {
        pose: Box<PoseFile>,
    },
    /// Sinusoidal chest / upper-chest pitch + roll. Emits rest-relative
    /// deltas — use [`BlendMode::RestRelative`].
    Breathing {
        rate_hz: f32,
        pitch_deg: f32,
        roll_deg: f32,
    },
    /// Poisson-fired eye blinks. Emits expression weights only (no
    /// bones). Use [`BlendMode::Override`].
    Blink {
        next_in: f32,
        phase: BlinkPhase,
        phase_t: f32,
        mean_interval: f32,
        double_blink_chance: f32,
    },
    /// Slow hip / spine counter-rotation. Emits rest-relative deltas.
    WeightShift {
        rate_hz: f32,
        hip_roll_deg: f32,
        spine_counter_deg: f32,
    },
    /// Per-finger tiny additive rotations gated by a pseudo-random
    /// wander. Emits rest-relative deltas.
    FingerFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
    },
    /// Same, for toes. Split so users can disable one without the other.
    ToeFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
    },
}

impl DriverKind {
    pub fn breathing_default() -> Self {
        Self::Breathing {
            rate_hz: 0.25,
            pitch_deg: 0.6,
            roll_deg: 0.3,
        }
    }
    pub fn blink_default() -> Self {
        Self::Blink {
            next_in: 2.5,
            phase: BlinkPhase::Idle,
            phase_t: 0.0,
            mean_interval: 4.0,
            double_blink_chance: 0.18,
        }
    }
    pub fn weight_shift_default() -> Self {
        Self::WeightShift {
            rate_hz: 0.07,
            hip_roll_deg: 1.5,
            spine_counter_deg: 0.8,
        }
    }
    pub fn finger_fidget_default() -> Self {
        Self::FingerFidget {
            amplitude_deg: 1.5,
            frequency_hz: 0.35,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
    pub fn toe_fidget_default() -> Self {
        Self::ToeFidget {
            amplitude_deg: 1.2,
            frequency_hz: 0.25,
            seed: 0xBF58_476D_1CE4_E5B9,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Clip { .. } => "clip",
            Self::PoseHold { .. } => "pose-hold",
            Self::Breathing { .. } => "breathing",
            Self::Blink { .. } => "auto-blink",
            Self::WeightShift { .. } => "weight-shift",
            Self::FingerFidget { .. } => "finger-fidget",
            Self::ToeFidget { .. } => "toe-fidget",
        }
    }

    /// Declared total length for timeline display. `None` = procedural /
    /// infinite (timeline widget draws a sweeping marker).
    pub fn duration_hint(&self) -> Option<f32> {
        match self {
            Self::Clip { animation } => {
                let fps = animation.fps.max(1.0) as f32;
                Some(animation.frames.len() as f32 / fps)
            }
            Self::PoseHold { .. } => None,
            _ => None,
        }
    }
}

/// Discrete blink phases. Matches ChatVRM's `autoBlink.ts` state machine
/// (see §3 of the AIRI plan, "BlinkDriver").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlinkPhase {
    Idle,
    Close,
    Hold,
    Open,
}

// ============================================================================
// Sample type
// ============================================================================

/// One driver's per-frame contribution.
#[derive(Debug, Default)]
pub struct DriverSample {
    /// Bone name → rotation. Semantics depend on the parent [`Layer`]'s
    /// [`BlendMode`] — either absolute local rotations (Override) or
    /// rest-relative deltas (RestRelative).
    pub bones: HashMap<String, Quat>,
    /// VRM expression name → 0..=1 weight. Layers summing past 1.0 are
    /// clamped on the apply side.
    pub expressions: HashMap<String, f32>,
}

// ============================================================================
// Rest-pose snapshot
// ============================================================================

/// Per-bone bind-pose local rotation, sampled once the pose driver's
/// [`BoneEntityIndex`] stabilises. Procedural drivers that need to return
/// "rest" explicitly (rather than `IDENTITY`) read from here; see the
/// `ResetPose` in `pose_driver.rs` (full `Vrm` subtree + [`RestTransform`]) for
/// why this matters.
#[derive(Resource, Default)]
pub struct RestPoseSnapshot {
    pub rest: HashMap<String, Quat>,
    /// Per-bone rest orientation in parent/world rest frame (`RestGlobalTransform`
    /// rotation). Used with `rest` to map composed raw locals → normalized pose
    /// space for `ApplyBones` (same basis as `pose_driver::publish_bone_snapshot`).
    pub rest_world: HashMap<String, Quat>,
    /// Monotonic counter of how many bones we've captured — lets the UI
    /// report "0/55 indexed".
    pub captured: usize,
}

// ============================================================================
// Systems
// ============================================================================

fn refresh_rest_pose_snapshot(
    indexed: Option<Res<IndexedBones>>,
    rest_q: Query<&RestTransform>,
    rest_global_q: Query<&RestGlobalTransform>,
    mut snap: ResMut<RestPoseSnapshot>,
) {
    let Some(indexed) = indexed else { return };
    if indexed.is_empty() {
        return;
    }
    // Extra skin joints (Rigify `DEF-*`, etc.) are often in `IndexedBones` but
    // **never** get `bevy_vrm1::RestTransform`. Requiring `rest.len() ==
    // indexed.len()` before committing blocked forever and left `snap.captured
    // == 0`, which makes [`advance_and_apply_layers`] bail on every frame.
    //
    // We still **re**-snapshot when `RestTransform` appears later on joints that
    // already had entities in the index (VRM init ordering), by checking that
    // every joint that *has* `RestTransform` is represented in `snap.rest`.
    let indexed_len = indexed.len();
    let snapshot_covers_all_rt = indexed.entities.iter().all(|(name, entity)| {
        match rest_q.get(*entity) {
            Ok(_) => snap.rest.contains_key(name),
            Err(_) => true,
        }
    });
    if snap.captured == indexed_len
        && snapshot_covers_all_rt
        && snap.rest_world.len() == snap.rest.len()
    {
        return;
    }
    let mut missing_rt = 0usize;
    let mut rest = HashMap::with_capacity(indexed_len);
    let mut rest_world = HashMap::with_capacity(indexed_len);
    for (name, entity) in &indexed.entities {
        if let Ok(rt) = rest_q.get(*entity) {
            rest.insert(name.clone(), rt.0.rotation);
            let rw = rest_global_q
                .get(*entity)
                .map(|rgt| rgt.0.rotation())
                .unwrap_or(Quat::IDENTITY);
            rest_world.insert(name.clone(), rw);
        } else {
            missing_rt += 1;
        }
    }
    snap.rest = rest;
    snap.rest_world = rest_world;
    snap.captured = indexed_len;
    if missing_rt > 0 {
        info!(
            target: "anim_layers",
            "rest pose snapshot: {} named joints indexed, {} with RestTransform (extras/skin-only joints use live defaults in layers)",
            indexed_len,
            snap.rest.len(),
        );
    } else {
        info!(
            target: "anim_layers",
            "rest pose snapshot refreshed ({} bones)",
            snap.captured
        );
    }
}

/// Main per-tick system: advance every enabled layer, sample its driver,
/// compose over rest pose, and queue `ApplyBones` + `ApplyExpression`.
fn advance_and_apply_layers(
    time: Res<Time>,
    handle: Res<LayerStackHandle>,
    sender: Option<Res<PoseCommandSender>>,
    snap: Res<RestPoseSnapshot>,
    indexed: Option<Res<IndexedBones>>,
) {
    let Some(sender) = sender else { return };
    let Some(indexed) = indexed else { return };
    if snap.captured == 0 || indexed.is_empty() {
        return;
    }
    let dt = time.delta_secs().min(0.05);

    let mut bones_out: HashMap<String, [f32; 4]> = HashMap::new();
    let mut expressions_out: HashMap<String, f32> = HashMap::new();

    handle.with_write(|stack| {
        stack.clock += dt;
        if !stack.master_enabled {
            return;
        }

        // Seed the accumulator with rest-pose rotations so procedural
        // `RestRelative` layers have something meaningful to multiply
        // against, and so any bone no layer touches lands back at rest.
        let mut accumulator: HashMap<String, Quat> = snap.rest.clone();

        for layer in &mut stack.layers {
            if !layer.enabled || layer.weight <= 0.0 {
                continue;
            }

            // Advance playhead.
            if layer.playing {
                let advance = dt * layer.speed;
                layer.time += advance;
                if let Some(duration) = layer.duration {
                    if layer.time >= duration {
                        if layer.looping {
                            layer.time = layer.time.rem_euclid(duration);
                        } else {
                            layer.time = duration;
                            layer.playing = false;
                        }
                    }
                }
            }

            let sample = sample_driver(&mut layer.driver, layer.time, dt, &snap.rest);

            let weight = layer.weight.clamp(0.0, 1.0);

            for (bone, quat) in sample.bones {
                if !layer.mask.allows(&bone) {
                    continue;
                }
                let rest = snap.rest.get(&bone).copied().unwrap_or(Quat::IDENTITY);
                let current = accumulator.get(&bone).copied().unwrap_or(rest);
                let folded = match layer.blend_mode {
                    BlendMode::Override => current.slerp(quat, weight),
                    BlendMode::RestRelative => {
                        // quat is a delta — scale by weight, multiply onto current.
                        let scaled = Quat::IDENTITY.slerp(quat, weight);
                        current * scaled
                    }
                };
                accumulator.insert(bone, folded);
            }

            for (name, weight_in) in sample.expressions {
                let entry = expressions_out.entry(name).or_insert(0.0);
                *entry = (*entry + weight_in * weight).clamp(0.0, 1.0);
            }
        }

        // Only emit bones whose composed rotation differs meaningfully
        // from rest — otherwise we'd overwrite every bone in the rig with
        // rest every frame and clobber `ApplyBones` requests from the UI
        // sliders / MCP between frames.
        for name in VRM_BONE_NAMES {
            let Some(q_raw) = accumulator.get(*name) else {
                continue;
            };
            let rest_local = snap.rest.get(*name).copied().unwrap_or(Quat::IDENTITY);
            if quat_close(*q_raw, rest_local, 1e-4) {
                continue;
            }
            let rest_world = snap
                .rest_world
                .get(*name)
                .copied()
                .unwrap_or(Quat::IDENTITY);
            let pose_q = normalized_from_local(rest_local, rest_world, *q_raw);
            bones_out.insert((*name).to_string(), [pose_q.x, pose_q.y, pose_q.z, pose_q.w]);
        }
    });

    if bones_out.is_empty() && expressions_out.is_empty() {
        return;
    }

    if !bones_out.is_empty() {
        sender.send(PoseCommand::ApplyBones {
            bones: bones_out,
            preserve_omitted_bones: true,
            blend_weight: Some(1.0),
            transition_seconds: Some(0.0),
        });
    }
    if !expressions_out.is_empty() {
        sender.send(PoseCommand::ApplyExpression {
            weights: expressions_out,
            cancel_expression_animation: false,
        });
    }
}

fn quat_close(a: Quat, b: Quat, eps: f32) -> bool {
    // Quaternions represent the same rotation iff they're equal OR antipodal.
    let dot = a.dot(b).abs();
    (1.0 - dot).abs() <= eps
}

// ============================================================================
// Per-driver sampling
// ============================================================================

fn sample_driver(
    driver: &mut DriverKind,
    t: f32,
    dt: f32,
    rest: &HashMap<String, Quat>,
) -> DriverSample {
    match driver {
        DriverKind::Clip { animation } => sample_clip(animation, t),
        DriverKind::PoseHold { pose } => sample_pose_hold(pose),
        DriverKind::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => sample_breathing(t, *rate_hz, *pitch_deg, *roll_deg),
        DriverKind::Blink {
            next_in,
            phase,
            phase_t,
            mean_interval,
            double_blink_chance,
        } => sample_blink(dt, next_in, phase, phase_t, *mean_interval, *double_blink_chance),
        DriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => sample_weight_shift(t, *rate_hz, *hip_roll_deg, *spine_counter_deg),
        DriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => sample_finger_fidget(t, *amplitude_deg, *frequency_hz, *seed, rest),
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => sample_toe_fidget(t, *amplitude_deg, *frequency_hz, *seed),
    }
}

fn sample_pose_hold(pose: &PoseFile) -> DriverSample {
    let mut bones = HashMap::with_capacity(pose.bones.len());
    for (name, r) in &pose.bones {
        let [x, y, z, w] = r.rotation;
        bones.insert(name.clone(), Quat::from_xyzw(x, y, z, w));
    }
    let expressions = pose.expressions.clone();
    DriverSample {
        bones,
        expressions,
    }
}

fn sample_clip(animation: &AnimationFile, t: f32) -> DriverSample {
    if animation.frames.is_empty() {
        return DriverSample::default();
    }
    let fps = animation.fps.max(1.0) as f32;
    let total = animation.frames.len();
    let idx = ((t * fps) as usize).min(total.saturating_sub(1));
    let frame = &animation.frames[idx];
    let mut bones = HashMap::with_capacity(frame.bones.len());
    for (name, r) in &frame.bones {
        let [x, y, z, w] = r.rotation;
        bones.insert(name.clone(), Quat::from_xyzw(x, y, z, w));
    }
    let expressions = frame.expressions.clone();
    DriverSample {
        bones,
        expressions,
    }
}

fn sample_breathing(t: f32, rate_hz: f32, pitch_deg: f32, roll_deg: f32) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let pitch = (omega * t).sin() * pitch_deg.to_radians();
    // Phase-shift roll by ~90° so the chest rocks in a figure-eight rather
    // than a straight line (AIRI plan §3 "Procedural idle layering").
    let roll = (omega * t + std::f32::consts::FRAC_PI_2).sin() * roll_deg.to_radians();

    let chest = Quat::from_euler(EulerRot::XYZ, pitch, 0.0, roll);
    // Tiny counter-roll on the upper chest for shape.
    let upper_chest = Quat::from_euler(EulerRot::XYZ, pitch * 0.4, 0.0, -roll * 0.3);

    let mut bones = HashMap::new();
    bones.insert("chest".into(), chest);
    bones.insert("upperChest".into(), upper_chest);
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

fn sample_blink(
    dt: f32,
    next_in: &mut f32,
    phase: &mut BlinkPhase,
    phase_t: &mut f32,
    mean_interval: f32,
    double_blink_chance: f32,
) -> DriverSample {
    const CLOSE: f32 = 0.06;
    const OPEN: f32 = 0.12;

    let mut weight = 0.0;
    match *phase {
        BlinkPhase::Idle => {
            *next_in -= dt;
            if *next_in <= 0.0 {
                *phase = BlinkPhase::Close;
                *phase_t = 0.0;
            }
        }
        BlinkPhase::Close => {
            *phase_t += dt;
            weight = (*phase_t / CLOSE).clamp(0.0, 1.0);
            if *phase_t >= CLOSE {
                *phase = BlinkPhase::Hold;
                *phase_t = 0.0;
            }
        }
        BlinkPhase::Hold => {
            *phase_t += dt;
            weight = 1.0;
            let hold = 0.03 + (mean_interval * 0.01);
            if *phase_t >= hold {
                *phase = BlinkPhase::Open;
                *phase_t = 0.0;
            }
        }
        BlinkPhase::Open => {
            *phase_t += dt;
            weight = 1.0 - (*phase_t / OPEN).clamp(0.0, 1.0);
            if *phase_t >= OPEN {
                *phase = BlinkPhase::Idle;
                *phase_t = 0.0;
                let mut rng = rand::rng();
                let base = mean_interval.max(0.5);
                let jitter: f32 = rng.random_range(0.5_f32..1.5);
                let mut next = base * jitter;
                if rng.random_bool(double_blink_chance as f64) {
                    next = 0.25 + rng.random_range(0.0_f32..0.3);
                }
                *next_in = next;
            }
        }
    }

    // Ease in/out with a sin curve so the close/open feels organic.
    let eased = (weight * std::f32::consts::FRAC_PI_2).sin();

    let mut expressions = HashMap::new();
    expressions.insert("blink".into(), eased);
    DriverSample {
        bones: HashMap::new(),
        expressions,
    }
}

fn sample_weight_shift(
    t: f32,
    rate_hz: f32,
    hip_roll_deg: f32,
    spine_counter_deg: f32,
) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let phase = (omega * t).sin();
    let hip = Quat::from_euler(EulerRot::XYZ, 0.0, 0.0, phase * hip_roll_deg.to_radians());
    let spine = Quat::from_euler(
        EulerRot::XYZ,
        0.0,
        0.0,
        -phase * spine_counter_deg.to_radians(),
    );
    let mut bones = HashMap::new();
    bones.insert("hips".into(), hip);
    bones.insert("spine".into(), spine);
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

const FINGER_BONES: &[&str] = &[
    "leftThumbProximal",
    "leftIndexIntermediate",
    "leftMiddleIntermediate",
    "leftRingIntermediate",
    "leftLittleIntermediate",
    "rightThumbProximal",
    "rightIndexIntermediate",
    "rightMiddleIntermediate",
    "rightRingIntermediate",
    "rightLittleIntermediate",
];

fn sample_finger_fidget(
    t: f32,
    amplitude_deg: f32,
    frequency_hz: f32,
    seed: u64,
    _rest: &HashMap<String, Quat>,
) -> DriverSample {
    let mut bones = HashMap::new();
    let amp = amplitude_deg.to_radians();
    for (i, name) in FINGER_BONES.iter().enumerate() {
        let phase_offset = hash_phase(seed, i as u64);
        let omega = std::f32::consts::TAU * (frequency_hz * (0.8 + (i as f32 * 0.07) % 0.5));
        let curl = (omega * t + phase_offset).sin() * amp;
        // Finger curl is mostly around local X axis (humanoid convention).
        bones.insert((*name).into(), Quat::from_rotation_x(curl));
    }
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

const TOE_BONES: &[&str] = &["leftToes", "rightToes"];

fn sample_toe_fidget(
    t: f32,
    amplitude_deg: f32,
    frequency_hz: f32,
    seed: u64,
) -> DriverSample {
    let mut bones = HashMap::new();
    let amp = amplitude_deg.to_radians();
    for (i, name) in TOE_BONES.iter().enumerate() {
        let phase_offset = hash_phase(seed, (i as u64) ^ 0xA5);
        let omega = std::f32::consts::TAU * frequency_hz;
        let curl = (omega * t + phase_offset).sin() * amp;
        bones.insert((*name).into(), Quat::from_rotation_x(curl));
    }
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

/// Deterministic phase offset in [0, 2π) from a 64-bit seed + index. Used
/// so restarting the app with the same seed produces the same fidget
/// cadence (nice for "it looked alive a moment ago — can I reproduce
/// that?" debugging).
fn hash_phase(seed: u64, idx: u64) -> f32 {
    let mut x = seed.wrapping_add(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    ((x as f32) / (u64::MAX as f32)) * std::f32::consts::TAU
}
