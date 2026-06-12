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
//! * Bone masking: flat include/exclude plus subtree roots (`include_subtrees` /
//!   `exclude_subtrees`) that match a bone and all indexed descendants.
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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy_vrm1::prelude::*;
use parking_lot::RwLock;
use rand::RngExt;

use jarvis_avatar::config::Settings;
use jarvis_avatar::pose_library::{AnimationFile, PoseFile};

use crate::plugins::pose_driver::{
    BoneHierarchy, IndexedBones, PoseCommand, PoseCommandSender, VRM_BONE_NAMES, apply_pose_commands,
    is_vrm_humanoid_bone, local_from_normalized, normalized_from_local, sync_bone_entity_index,
};

pub struct AnimLayersPlugin;

impl Plugin for AnimLayersPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LayerStackHandle::default())
            .insert_resource(RestPoseSnapshot::default())
            .init_resource::<LayerGlitchMonitor>()
            .add_systems(Startup, maybe_auto_install_default_layers)
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

fn maybe_auto_install_default_layers(
    settings: Res<Settings>,
    stack: Res<LayerStackHandle>,
    layer_sets: Option<Res<crate::plugins::anim_layer_sets::LayerSetsStore>>,
    library: Option<Res<crate::plugins::pose_library_assets::PoseLibraryAssets>>,
) {
    if !settings.anim_layers.auto_install_procedural {
        return;
    }
    let master = settings.anim_layers.master_enabled_default;
    let boot_set = settings.anim_layers.boot_layer_set.trim().to_string();
    stack.with_write(|s| {
        if !s.layers.is_empty() {
            return;
        }
        if !boot_set.is_empty() {
            if let (Some(store), Some(lib)) = (layer_sets.as_deref(), library.as_deref()) {
                match store.load_into(&boot_set, s, &lib.library) {
                    Ok(n) => {
                        s.master_enabled = master;
                        info!("boot layer set '{boot_set}': {n} layer(s) loaded");
                    }
                    Err(e) => warn!("boot layer set '{boot_set}': {e}"),
                }
                return;
            }
        }
        s.install_default_procedural_layers();
        s.master_enabled = master;
    });
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
#[derive(Debug, Default, Clone)]
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
    /// When true, only layers listed in [`Self::solo_only_ids`] are sampled
    /// and advanced — used by the Animation Layers UI "Solo visible" control.
    pub solo_mode: bool,
    /// Layer ids included in solo playback (captured from the current filter
    /// + visibility when solo is toggled on).
    pub solo_only_ids: HashSet<u64>,
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

    fn unique_slug(&self, base: &str) -> String {
        if !self.layers.iter().any(|l| l.slug == base) {
            return base.to_string();
        }
        for n in 2..10_000 {
            let candidate = format!("{base}-{n}");
            if !self.layers.iter().any(|l| l.slug == candidate) {
                return candidate;
            }
        }
        format!("{base}-{}", self.next_id)
    }

    /// Clone a layer and insert it directly below `id`. When `flip_reverse` is
    /// true the copy plays in the opposite direction (for toe ripple chains).
    pub fn duplicate_layer(&mut self, id: u64, flip_reverse: bool) -> Option<u64> {
        let idx = self.layers.iter().position(|l| l.id == id)?;
        let source = self.layers[idx].clone();
        let source_slug = source.slug.clone();
        let source_label = source.label.clone();
        let source_reverse = source.reverse;
        let mut copy = source;
        copy.id = 0;
        copy.time = 0.0;
        copy.slug = self.unique_slug(&source_slug);
        if flip_reverse {
            copy.reverse = !source_reverse;
            copy.label = format!("{source_label} ↺");
        } else {
            copy.label = format!("{source_label} ↳");
        }
        self.next_id = self.next_id.saturating_add(1);
        copy.id = self.next_id;
        let new_id = copy.id;
        self.layers.insert(idx + 1, copy);
        Some(new_id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// Insert (or replace) a looping clip layer at the bottom of the stack for base idle motion.
    pub fn install_idle_clip_at_base(
        &mut self,
        animation: AnimationFile,
        looping: bool,
    ) {
        self.layers.retain(|l| l.slug != "idle-base");
        let mut layer = Layer::new(
            "idle-base",
            "Idle (clip)",
            DriverKind::Clip {
                animation: Box::new(animation),
            },
        )
        .blend(BlendMode::Override)
        .weight(1.0);
        layer.looping = looping;
        layer.enabled = true;
        layer.playing = true;
        if let Some(d) = layer.driver.duration_hint() {
            layer.duration = Some(d);
        }
        self.next_id = self.next_id.saturating_add(1);
        layer.id = self.next_id;
        self.layers.insert(0, layer);
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
            // Coordinated lower body — supersedes the old roll-only weight-shift
            // + standalone sway (those variants still exist for saved sets and
            // manual use; leg-shift just covers the same ground more fully:
            // lateral lean, forward/back sway, knees and ankles).
            Layer::new("leg-shift", "Leg Shift", DriverKind::leg_shift_default())
                .blend(BlendMode::RestRelative)
                .weight(0.85),
            Layer::new(
                "finger-fidget",
                "Finger Fidget",
                DriverKind::finger_fidget_default(),
            )
            .blend(BlendMode::RestRelative)
            .weight(0.9),
            Layer::new("toe-fidget", "Toe Fidget", DriverKind::toe_fidget_default())
                .blend(BlendMode::RestRelative)
                .weight(0.7),
            Layer::new("look-around", "Look Around", DriverKind::look_around_default())
                .blend(BlendMode::RestRelative)
                .weight(1.0),
            Layer::new("arm-sway", "Arm Sway", DriverKind::arm_sway_default())
                .blend(BlendMode::RestRelative)
                .weight(0.6),
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
    /// When true, sample the clip / envelope backwards
    pub reverse: bool,
    /// Seconds of crossfade across the clip loop seam. The last `loop_fade`
    /// seconds of a looping clip are blended toward its first frame so the
    /// wrap from end → start is continuous (kills the "twitch" on restart).
    /// `0` = hard cut (old behaviour). Ignored by procedural drivers.
    pub loop_fade: f32,
    /// Bounce at the clip ends instead of wrapping. Position stays continuous
    /// across the turnaround (no seam), unlike `looping` + `reverse` which
    /// hard-flips. When set, `reverse` is ignored.
    pub ping_pong: bool,
    /// Runtime-only smoothed enable/disable envelope in `[0, 1]`. Ramps toward
    /// `enabled ? 1 : 0` over [`WEIGHT_FADE_SECS`] so toggling a layer (or
    /// swapping presets) fades instead of popping. Not serialized.
    pub gain: f32,
}

/// Seconds for a layer's enable/disable weight envelope ([`Layer::gain`]) to
/// ramp between 0 and 1. Short enough to feel responsive, long enough to hide
/// the pop when a layer turns on/off or a preset is swapped.
pub const WEIGHT_FADE_SECS: f32 = 0.3;

/// Hermite smoothstep on `[0, 1]` — eases the ends of a ramp so fades/blends
/// start and stop gently instead of linearly.
#[inline]
pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
            reverse: false,
            loop_fade: 0.25,
            ping_pong: false,
            gain: 1.0,
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

    /// Returns `(time, duration)` for the timeline widget. Uses the effective
    /// sample time (honours [`Self::reverse`]) so the playhead sweeps backwards.
    pub fn timeline_progress(&self) -> (f32, f32) {
        match self.duration.or_else(|| self.driver.duration_hint()) {
            // Clip-style layer with a real length: show the true playhead.
            Some(d) => (layer_sample_time(self), d.max(0.01)),
            // Endless procedural driver: `layer_sample_time` now returns an
            // unbounded clock, which would pin the bar at 100%. Show a cosmetic
            // sweep over a fixed window so the marker keeps moving.
            None => {
                let window = 10.0;
                (self.time.rem_euclid(window), window)
            }
        }
    }
}

/// Effective sample time for clip / envelope evaluation (forwards or reversed).
pub fn layer_sample_time(layer: &Layer) -> f32 {
    // Endless procedural drivers (breathing, weight-shift, fidgets, look-around)
    // have no finite duration. They must NOT wrap: the old `unwrap_or(10.0)`
    // fallback chopped `layer.time` at 10 s via `rem_euclid`, and since 10 s is
    // not a whole number of sine cycles the phase jumped mid-cycle on every wrap
    // — that was the breathing / weight-shift "twitch". These drivers are
    // internally periodic, so a freely accumulating time is exactly right.
    let Some(duration) = layer.duration.or_else(|| layer.driver.duration_hint()) else {
        return layer.time;
    };
    let duration = duration.max(0.01);
    if layer.ping_pong {
        // Triangle fold over a 2·duration period: 0→dur→0. Position is
        // continuous at both turnarounds, so the bounce never seams.
        let cycle = layer.time.rem_euclid(2.0 * duration);
        return if cycle <= duration {
            cycle
        } else {
            2.0 * duration - cycle
        };
    }
    let phase = layer.time.rem_euclid(duration);
    if layer.reverse {
        (duration - phase).rem_euclid(duration)
    } else {
        phase
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
///
/// Flat `include` / `exclude` match exact bone names. `include_subtrees` /
/// `exclude_subtrees` match the named root **and every indexed descendant**
/// (e.g. `leftFoot` → toe DEF chains).
#[derive(Debug, Clone, Default)]
pub struct BoneMask {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub include_subtrees: Vec<String>,
    pub exclude_subtrees: Vec<String>,
}

impl BoneMask {
    pub fn allows(&self, bone: &str, hierarchy: Option<&BoneHierarchy>) -> bool {
        if let Some(h) = hierarchy {
            if self
                .exclude_subtrees
                .iter()
                .any(|root| h.is_under(bone, root))
            {
                return false;
            }
        }
        if self.exclude.iter().any(|n| n == bone) {
            return false;
        }
        let restricted =
            !self.include.is_empty() || !self.include_subtrees.is_empty();
        if !restricted {
            return true;
        }
        if self.include.iter().any(|n| n == bone) {
            return true;
        }
        if let Some(h) = hierarchy {
            if self
                .include_subtrees
                .iter()
                .any(|root| h.is_under(bone, root))
            {
                return true;
            }
        }
        false
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
    Clip { animation: Box<AnimationFile> },
    /// Holds one [`PoseFile`] from the pose library (static bones + optional
    /// expression weights). Emits absolute rotations — use [`BlendMode::Override`].
    PoseHold { pose: Box<PoseFile> },
    /// Pin VRM expression / morph preset weights (no bones).
    ExpressionHold {
        expressions: HashMap<String, f32>,
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
        /// Static inward curl (degrees) the fingers oscillate *around*, so the
        /// resting hand reads relaxed instead of flat/stiff. `0` = straight.
        curl_bias_deg: f32,
        /// Static thumb *opposition* (degrees) — how far the thumb tucks toward
        /// the palm at rest. Drives the thumb on its own yaw axis (not the
        /// finger curl axis) so it reads relaxed instead of sticking up like a
        /// thumbs-up. `0` = thumb in line with the hand.
        curl_bias_thumb_deg: f32,
    },
    /// Same, for toes. Split so users can disable one without the other.
    ToeFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
        /// Static curl the fidget oscillates around (see [`Self::FingerFidget`]).
        curl_bias_deg: f32,
    },
    /// Ambient head/neck "look around" — slow random glances so the avatar
    /// reads as awake and present while idle. Emits rest-relative deltas on the
    /// **neck + head only** (never the eyes — `look_at.rs` owns those, and its
    /// `bevy_vrm1` driver runs after the layer stack so any eye writes here are
    /// clobbered anyway). When the avatar is actively tracking a face (Home
    /// Assistant gaze), the motion collapses toward forward so the head stays
    /// oriented at the user while the eyes do the tracking. Use
    /// [`BlendMode::RestRelative`].
    LookAround {
        /// Mean seconds between picking a new glance target.
        mean_interval: f32,
        /// Max horizontal glance (left/right), degrees.
        yaw_deg: f32,
        /// Max vertical glance (up/down), degrees.
        pitch_deg: f32,
        // --- transient state (not authored; runtime only) ---
        next_in: f32,
        cur_yaw: f32,
        cur_pitch: f32,
        target_yaw: f32,
        target_pitch: f32,
        /// Smoothed gaze-damping gain (1 = free wander, →0 = locked forward).
        damp: f32,
    },
    /// Slow whole-body balance sway — the perpetual forward/back + circular
    /// micro-lean a standing person makes to stay balanced. Drives the spine
    /// chain only (hips/spine/chest), so it composes additively with
    /// [`Self::WeightShift`] (which is roll/yaw only) and adds the missing
    /// forward/back pitch dimension. Emits rest-relative deltas — use
    /// [`BlendMode::RestRelative`].
    Sway {
        rate_hz: f32,
        amount_deg: f32,
    },
    /// Relaxed pendular arm sway — arms are never perfectly still, they drift
    /// with the body's weight shifts. Drives upper/lower arms with a slight
    /// per-side phase offset so the two arms don't swing in lockstep. Emits
    /// rest-relative deltas — use [`BlendMode::RestRelative`].
    ArmSway {
        rate_hz: f32,
        amount_deg: f32,
    },
    /// Coordinated lower-body weight transfer — a *contrapposto* standing idle.
    /// One slow wandering "weight" signal drives the whole lower body coherently:
    /// the hips lean toward the loaded leg (pelvic obliquity), the pelvis rotates
    /// slightly into the stance, the free-side **knee softens and bends**, the
    /// thighs rotate into/out of the stance, and a faster **ankle postural
    /// micro-sway** rides on both feet (the constant balance correction a real
    /// person makes). Self-contained for the lower body: drives hips/spine/chest
    /// + both `UpperLeg`/`LowerLeg`/`Foot`. Leg bones are authored in normalized
    /// humanoid space and converted through the rest snapshot (so knee = +pitch
    /// etc. hold regardless of bind orientation), like the finger fidget. Emits
    /// rest-relative deltas — use [`BlendMode::RestRelative`]. Supersedes the
    /// roll-only [`Self::WeightShift`]; don't stack both on the hips at once.
    LegShift {
        rate_hz: f32,
        /// Lateral hip lean toward the weighted leg (degrees).
        shift_deg: f32,
        /// Knee flexion on the unweighted (free) leg (degrees).
        knee_bend_deg: f32,
        /// Thigh rotation / abduction into the stance (degrees).
        hip_sway_deg: f32,
        /// Ankle postural micro-sway amplitude (degrees).
        ankle_deg: f32,
        /// Phase seed so multiple instances / restarts differ.
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
            curl_bias_deg: 9.0,
            curl_bias_thumb_deg: 8.0,
        }
    }
    pub fn toe_fidget_default() -> Self {
        Self::ToeFidget {
            amplitude_deg: 1.2,
            frequency_hz: 0.25,
            seed: 0xBF58_476D_1CE4_E5B9,
            curl_bias_deg: 4.0,
        }
    }
    pub fn look_around_default() -> Self {
        Self::LookAround {
            mean_interval: 3.5,
            yaw_deg: 12.0,
            pitch_deg: 6.0,
            next_in: 1.5,
            cur_yaw: 0.0,
            cur_pitch: 0.0,
            target_yaw: 0.0,
            target_pitch: 0.0,
            damp: 1.0,
        }
    }
    pub fn sway_default() -> Self {
        Self::Sway {
            rate_hz: 0.05,
            amount_deg: 1.2,
        }
    }
    pub fn arm_sway_default() -> Self {
        Self::ArmSway {
            rate_hz: 0.08,
            amount_deg: 1.5,
        }
    }
    pub fn leg_shift_default() -> Self {
        Self::LegShift {
            rate_hz: 0.05,
            shift_deg: 3.5,
            knee_bend_deg: 8.0,
            hip_sway_deg: 2.5,
            ankle_deg: 1.8,
            seed: 0xD1B5_4A32_D192_ED03,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Clip { .. } => "clip",
            Self::PoseHold { .. } => "pose-hold",
            Self::ExpressionHold { .. } => "expression-hold",
            Self::Breathing { .. } => "breathing",
            Self::Blink { .. } => "auto-blink",
            Self::WeightShift { .. } => "weight-shift",
            Self::FingerFidget { .. } => "finger-fidget",
            Self::ToeFidget { .. } => "toe-fidget",
            Self::LookAround { .. } => "look-around",
            Self::Sway { .. } => "sway",
            Self::ArmSway { .. } => "arm-sway",
            Self::LegShift { .. } => "leg-shift",
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
            Self::ExpressionHold { .. } => Some(2.0),
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
    let snapshot_covers_all_rt =
        indexed
            .entities
            .iter()
            .all(|(name, entity)| match rest_q.get(*entity) {
                Ok(_) => snap.rest.contains_key(name),
                Err(_) => true,
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
    hierarchy: Option<Res<BoneHierarchy>>,
    look_at: Option<Res<crate::plugins::look_at::LookAtRuntime>>,
    mut glitch: ResMut<LayerGlitchMonitor>,
) {
    let Some(sender) = sender else { return };
    let Some(indexed) = indexed else { return };
    if snap.captured == 0 || indexed.is_empty() {
        return;
    }
    let dt = time.delta_secs().min(0.05);
    let hierarchy = hierarchy.as_deref();
    // Drives the `LookAround` driver's damping: when a face is being tracked,
    // the ambient head wander collapses so the eyes can do the tracking.
    let gaze_active = look_at.as_deref().is_some_and(|r| r.gaze_active());

    let mut bones_out: HashMap<String, [f32; 4]> = HashMap::new();
    let mut expressions_out: HashMap<String, f32> = HashMap::new();
    let mut root_translation: Option<Vec3> = None;

    handle.with_write(|stack| {
        stack.clock += dt;
        if !stack.master_enabled {
            return;
        }

        let solo = stack.solo_mode.then_some(stack.solo_only_ids.clone());
        let (accumulator, expressions) = compose_layers(
            &mut stack.layers,
            &snap,
            hierarchy,
            dt,
            solo.as_ref(),
            gaze_active,
            Some(&mut glitch),
        );
        expressions_out = expressions;

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
            bones_out.insert(
                (*name).to_string(),
                [pose_q.x, pose_q.y, pose_q.z, pose_q.w],
            );
        }

        // Root motion: blend hips translation from any clip layer carrying a
        // `root_position` track (rotation-only clips contribute nothing, so
        // existing sets are unaffected). Weighted by each layer's gain*weight.
        let mut root_acc = Vec3::ZERO;
        let mut root_w = 0.0f32;
        for layer in stack.layers.iter() {
            if !layer.enabled {
                continue;
            }
            if let DriverKind::Clip { animation } = &layer.driver {
                let dur = animation.frames.len() as f32 / (animation.fps.max(1.0) as f32);
                let t_eff = if layer.looping && dur > 0.0 {
                    layer.time.rem_euclid(dur)
                } else {
                    layer.time
                };
                if let Some(p) = sample_clip_root_position(animation, t_eff) {
                    let w = (layer.gain * layer.weight).max(0.0);
                    root_acc += p * w;
                    root_w += w;
                }
            }
        }
        if root_w > 1e-4 {
            root_translation = Some(root_acc / root_w);
        }
    });

    if bones_out.is_empty() && expressions_out.is_empty() && root_translation.is_none() {
        return;
    }

    if let Some(t) = root_translation {
        let mut translations: HashMap<String, [f32; 3]> = HashMap::new();
        translations.insert("hips".to_string(), [t.x, t.y, t.z]);
        sender.send(PoseCommand::ApplyBoneTranslations(translations));
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

/// Advance playheads for every layer (used by live tick and offline bake).
pub fn advance_layer_playheads(
    layers: &mut [Layer],
    dt: f32,
    solo_only_ids: Option<&HashSet<u64>>,
) {
    for layer in layers {
        if let Some(ids) = solo_only_ids {
            if !ids.contains(&layer.id) {
                continue;
            }
        }
        // Weight envelope ramps every tick — independent of `playing` — so a
        // just-disabled layer keeps composing until it has fully faded out.
        let target = if layer.enabled { 1.0 } else { 0.0 };
        let step = if WEIGHT_FADE_SECS > 0.0 {
            dt / WEIGHT_FADE_SECS
        } else {
            1.0
        };
        if layer.gain < target {
            layer.gain = (layer.gain + step).min(target);
        } else if layer.gain > target {
            layer.gain = (layer.gain - step).max(target);
        }

        if !layer.playing {
            continue;
        }
        // Ping-pong always advances forward; the triangle fold in
        // `layer_sample_time` handles the bounce.
        let dir = if layer.reverse && !layer.ping_pong {
            -1.0f32
        } else {
            1.0
        };
        layer.time += dt * layer.speed * dir;
        if let Some(duration) = layer.duration {
            if layer.ping_pong {
                layer.time = layer.time.rem_euclid(2.0 * duration);
            } else if layer.looping {
                layer.time = layer.time.rem_euclid(duration);
            } else if layer.reverse {
                if layer.time <= 0.0 {
                    layer.time = 0.0;
                    layer.playing = false;
                }
            } else if layer.time >= duration {
                layer.time = duration;
                layer.playing = false;
            }
        }
    }
}

/// Sample every enabled layer once and fold into raw-local bone rotations +
/// expression weights. Also advances layer playheads when `dt > 0`.
/// One detected per-layer motion spike, surfaced to the debug UI so the
/// offending layer can be flashed at the instant it pops.
#[derive(Debug, Clone)]
pub struct GlitchEvent {
    /// [`LayerGlitchMonitor::now`] timestamp when the spike fired.
    pub at: f32,
    /// Id of the layer that glitched (for the flash lookup + log identity).
    pub layer_id: u64,
    /// Layer label at the time of the spike (for the copyable log).
    pub layer_label: String,
    /// The layer's own playhead position (seconds into clip / since start) when
    /// the spike fired — i.e. where on *this layer's* timeline it happened.
    pub layer_time: f32,
    /// Peak angular velocity across the layer's bones, in degrees/second.
    pub peak_dps: f32,
    /// Bone that carried the peak (e.g. `rightLowerLeg`).
    pub bone: String,
    /// How many times the spike exceeded the layer's recent baseline.
    pub ratio: f32,
}

/// Per-layer rolling state the monitor keeps between frames.
#[derive(Debug, Default)]
struct GlitchState {
    /// Previous frame's raw sample (`bone -> delta/absolute quat`).
    prev: HashMap<String, Quat>,
    /// Exponentially-weighted mean of the per-frame peak angular velocity.
    /// Used as the "normal motion" baseline a spike must clear.
    ema_dps: f32,
    /// Frames this layer has been observed. Until it reaches
    /// [`LayerGlitchMonitor::WARMUP_FRAMES`] the baseline is still 0-ish, so any
    /// motion reads as a huge ratio — we let it settle before recording spikes.
    frames: u32,
}

/// Runtime-only diagnostic: watches each layer's sampled output frame-to-frame
/// and records a [`GlitchEvent`] when a bone's angular velocity spikes far
/// above that layer's own recent baseline — i.e. a discontinuity/pop that the
/// steady procedural motion never produces. The debug UI reads `events` to
/// flash the layer the moment it glitches, so the user can identify *which*
/// layer jitters and *when*.
#[derive(Resource, Debug)]
pub struct LayerGlitchMonitor {
    /// Master on/off for detection (cheap, but lets the user silence flashes).
    pub enabled: bool,
    /// A frame's peak must exceed `sensitivity × baseline` to count as a spike.
    pub sensitivity: f32,
    /// …and also clear this absolute floor (deg/s), so slow steady motion and
    /// sub-degree numerical noise never trip a flash.
    pub floor_dps: f32,
    /// Seconds a flash stays lit in the UI after a spike.
    pub flash_secs: f32,
    /// Monotonic clock advanced by `dt` each live compose (own time base so the
    /// UI fade is independent of the stack's pausable `clock`).
    pub now: f32,
    /// Smoothed frame delta (seconds). A frame whose `dt` is much larger than
    /// this is a frame-pacing hitch, not a per-layer glitch — on those frames
    /// every driver's apparent angular velocity (`angle/dt`) spikes together, so
    /// we skip spike detection rather than blame the layers. See [`Self::note_frame`].
    dt_ema: f32,
    states: HashMap<u64, GlitchState>,
    /// Last spike per layer id (most recent wins). Read by the UI to flash.
    pub events: HashMap<u64, GlitchEvent>,
    /// Append-only history of spikes (newest last), capped at [`Self::LOG_CAP`].
    /// The UI shows this as a copyable list so the user can read glitches that
    /// flashed too fast to catch live.
    pub log: Vec<GlitchEvent>,
}

impl LayerGlitchMonitor {
    /// Max entries kept in [`Self::log`]; oldest are dropped past this.
    pub const LOG_CAP: usize = 400;
    /// A frame counts as a pacing hitch (and is skipped for detection) when its
    /// `dt` exceeds `dt_ema × HITCH_RATIO`. Tuned to let normal 60↔120 Hz jitter
    /// through while catching dropped/stalled frames that distort `angle/dt`.
    const HITCH_RATIO: f32 = 1.6;
    /// Frames a layer must be observed before its spikes are trusted (lets the
    /// baseline EMA settle so the first real motion isn't logged as a glitch).
    const WARMUP_FRAMES: u32 = 8;
}

impl Default for LayerGlitchMonitor {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitivity: 5.0,
            floor_dps: 45.0,
            flash_secs: 0.7,
            now: 0.0,
            dt_ema: 1.0 / 60.0,
            states: HashMap::new(),
            events: HashMap::new(),
            log: Vec::new(),
        }
    }
}

impl LayerGlitchMonitor {
    /// Update the smoothed frame cadence and report whether this frame's `dt` is
    /// "normal" (not a pacing hitch). Called once per frame before observing any
    /// layer. On a hitch frame, callers should still advance `prev` but must not
    /// record spikes — a long `dt` inflates every layer's `angle/dt` at once.
    fn note_frame(&mut self, dt: f32) -> bool {
        if dt <= 0.0 {
            return false;
        }
        let base = if self.dt_ema > 0.0 { self.dt_ema } else { dt };
        let ok = dt <= base * Self::HITCH_RATIO;
        // Cap a hitch's pull on the average so one stall doesn't raise the bar.
        let capped = dt.min(base * 2.0);
        self.dt_ema = base * 0.9 + capped * 0.1;
        ok
    }

    /// Compare this frame's sample to the last one for `layer_id`, update the
    /// baseline, and record a [`GlitchEvent`] if the peak angular velocity is a
    /// spike. `dt` must be > 0. `dt_ok` is the per-frame hitch verdict from
    /// [`Self::note_frame`]; on a hitch frame we refresh `prev` but never log.
    fn observe(
        &mut self,
        layer_id: u64,
        layer_label: &str,
        layer_time: f32,
        sample: &HashMap<String, Quat>,
        dt: f32,
        dt_ok: bool,
    ) {
        if dt <= 0.0 {
            return;
        }
        let floor = self.floor_dps;
        let sensitivity = self.sensitivity;
        let now = self.now;
        let st = self.states.entry(layer_id).or_default();

        let mut peak_dps = 0.0f32;
        let mut peak_bone: Option<&str> = None;
        for (bone, q) in sample {
            if let Some(prev) = st.prev.get(bone) {
                // `angle_between` is unsigned in [0, π]; per-frame that's the
                // magnitude of the rotation this bone took this tick.
                let dps = prev.angle_between(*q).to_degrees() / dt;
                if dps > peak_dps {
                    peak_dps = dps;
                    peak_bone = Some(bone.as_str());
                }
            }
        }

        let base = st.ema_dps;
        let warmed = st.frames >= Self::WARMUP_FRAMES;
        // Only trust a spike on a normal-cadence frame, after warmup, when it
        // clears both the absolute floor and the layer's own baseline.
        let is_spike =
            dt_ok && warmed && peak_dps >= floor && peak_dps >= sensitivity * (base + 1.0);
        if is_spike {
            if let Some(bone) = peak_bone {
                let ev = GlitchEvent {
                    at: now,
                    layer_id,
                    layer_label: layer_label.to_string(),
                    layer_time,
                    peak_dps,
                    bone: bone.to_string(),
                    ratio: peak_dps / (base + 1.0),
                };
                self.events.insert(layer_id, ev.clone());
                self.log.push(ev);
                if self.log.len() > Self::LOG_CAP {
                    let overflow = self.log.len() - Self::LOG_CAP;
                    self.log.drain(0..overflow);
                }
            }
        }

        let st = self.states.entry(layer_id).or_default();
        st.frames = st.frames.saturating_add(1);
        // Only fold normal-cadence frames into the baseline; a hitch frame's
        // velocity is dt-distorted and would raise the bar for real glitches.
        // Cap the contribution so a single pop doesn't poison the baseline.
        if dt_ok {
            let capped = peak_dps.min(base * 3.0 + floor);
            st.ema_dps = base * 0.9 + capped * 0.1;
        }
        st.prev.clear();
        st.prev
            .extend(sample.iter().map(|(k, v)| (k.clone(), *v)));
    }

    /// Drop state/events for layers that no longer exist.
    fn gc(&mut self, live_ids: &HashSet<u64>) {
        self.states.retain(|id, _| live_ids.contains(id));
        self.events.retain(|id, _| live_ids.contains(id));
    }
}

pub fn compose_layers(
    layers: &mut [Layer],
    snap: &RestPoseSnapshot,
    hierarchy: Option<&BoneHierarchy>,
    dt: f32,
    solo_only_ids: Option<&HashSet<u64>>,
    gaze_active: bool,
    mut glitch: Option<&mut LayerGlitchMonitor>,
) -> (HashMap<String, Quat>, HashMap<String, f32>) {
    let mut accumulator: HashMap<String, Quat> = snap.rest.clone();
    let mut expressions_out: HashMap<String, f32> = HashMap::new();

    // Per-frame hitch verdict, computed once: a long `dt` inflates every
    // layer's `angle/dt` together, so on those frames we skip spike logging.
    let mut frame_dt_ok = true;
    if let Some(mon) = glitch.as_deref_mut() {
        if mon.enabled {
            mon.now += dt;
            frame_dt_ok = mon.note_frame(dt);
        }
    }

    for layer in &mut *layers {
        if let Some(ids) = solo_only_ids {
            if !ids.contains(&layer.id) {
                continue;
            }
        }
        // Effective weight folds in the smoothed enable/disable envelope so a
        // toggled or preset-swapped layer fades instead of popping. A disabled
        // layer keeps composing until `gain` finishes ramping to 0.
        let weight = layer.weight.clamp(0.0, 1.0) * smoothstep(layer.gain);
        if weight <= 0.0 {
            continue;
        }

        let sample = sample_layer(layer, snap, dt, gaze_active);

        // Spike detection runs on the raw driver output (before weight/mask
        // fold) so it isolates the driver's own math, not blend dynamics.
        if let Some(mon) = glitch.as_deref_mut() {
            if mon.enabled && !sample.bones.is_empty() {
                mon.observe(
                    layer.id,
                    &layer.label,
                    layer.time,
                    &sample.bones,
                    dt,
                    frame_dt_ok,
                );
            }
        }

        for (bone, quat) in sample.bones {
            if !layer.mask.allows(&bone, hierarchy) {
                continue;
            }
            let rest = snap.rest.get(&bone).copied().unwrap_or(Quat::IDENTITY);
            let current = accumulator.get(&bone).copied().unwrap_or(rest);
            let folded = match layer.blend_mode {
                BlendMode::Override => current.slerp(quat, weight),
                BlendMode::RestRelative => {
                    let scaled = Quat::IDENTITY.slerp(quat, weight);
                    current * scaled
                }
            };
            accumulator.insert(bone, folded);
        }

        for (name, weight_in) in sample.expressions {
            let current = expressions_out.get(&name).copied().unwrap_or(0.0);
            let folded = match layer.blend_mode {
                BlendMode::Override => current + (weight_in - current) * weight,
                BlendMode::RestRelative => (current + weight_in * weight).clamp(0.0, 1.0),
            };
            expressions_out.insert(name, folded.clamp(0.0, 1.0));
        }
    }

    if let Some(mon) = glitch.as_deref_mut() {
        if mon.enabled {
            let live: HashSet<u64> = layers.iter().map(|l| l.id).collect();
            mon.gc(&live);
        }
    }

    if dt > 0.0 {
        advance_layer_playheads(layers, dt, solo_only_ids);
    }

    (accumulator, expressions_out)
}

fn reset_driver_transient_state(driver: &mut DriverKind) {
    match driver {
        DriverKind::Blink {
            next_in,
            phase,
            phase_t,
            mean_interval,
            ..
        } => {
            *next_in = *mean_interval;
            *phase = BlinkPhase::Idle;
            *phase_t = 0.0;
        }
        DriverKind::LookAround {
            next_in,
            cur_yaw,
            cur_pitch,
            target_yaw,
            target_pitch,
            damp,
            ..
        } => {
            *next_in = 1.5;
            *cur_yaw = 0.0;
            *cur_pitch = 0.0;
            *target_yaw = 0.0;
            *target_pitch = 0.0;
            *damp = 1.0;
        }
        _ => {}
    }
}

/// Layer-set name used when editing a library animation via the stack.
pub fn animation_edit_layer_set_name(filename: &str) -> String {
    format!("anim-edit:{filename}")
}

/// Default JSON library filename for the configured idle VRMA path stem.
pub fn idle_clip_library_filename(idle_vrma_path: &str) -> String {
    let stem = Path::new(idle_vrma_path.trim())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("idle_loop");
    format!("{stem}.json")
}

/// Collect every bone name that appears in any keyframe.
pub fn bones_in_animation(anim: &AnimationFile) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for frame in &anim.frames {
        for name in frame.bones.keys() {
            set.insert(name.clone());
        }
    }
    let mut bones: Vec<String> = set.into_iter().collect();
    bones.sort_by(|a, b| {
        let ia = VRM_BONE_NAMES.iter().position(|&n| n == a.as_str());
        let ib = VRM_BONE_NAMES.iter().position(|&n| n == b.as_str());
        match (ia, ib) {
            (Some(i), Some(j)) => i.cmp(&j),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
        }
    });
    bones
}

fn animation_has_expressions(anim: &AnimationFile) -> bool {
    anim.frames.iter().any(|f| !f.expressions.is_empty())
}

fn expressions_in_animation(anim: &AnimationFile) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for frame in &anim.frames {
        for name in frame.expressions.keys() {
            set.insert(name.clone());
        }
    }
    let mut names: Vec<String> = set.into_iter().collect();
    names.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    names
}

/// One morph preset's keyframes only (bones empty).
fn slice_animation_for_expression(source: &AnimationFile, preset: &str) -> AnimationFile {
    use jarvis_avatar::pose_library::AnimationFrame;
    let frames: Vec<AnimationFrame> = source
        .frames
        .iter()
        .map(|f| {
            let mut expressions = HashMap::new();
            if let Some(w) = f.expressions.get(preset) {
                expressions.insert(preset.to_string(), *w);
            }
            AnimationFrame {
                bones: HashMap::new(),
                duration_ms: f.duration_ms,
                expressions,
                root_position: None,
            }
        })
        .collect();
    AnimationFile {
        name: format!("{} · {preset}", source.name),
        prompt: source.prompt.clone(),
        fps: source.fps,
        frame_count: frames.len(),
        frames,
        category: source.category.clone(),
        looping: source.looping,
        hold_duration: source.hold_duration,
    }
}

/// One bone's keyframes only (no morphs — those go in a separate expressions layer).
fn slice_animation_for_bone(source: &AnimationFile, bone: &str) -> AnimationFile {
    use jarvis_avatar::pose_library::AnimationFrame;
    let frames: Vec<AnimationFrame> = source
        .frames
        .iter()
        .map(|f| {
            let mut bones = HashMap::new();
            if let Some(r) = f.bones.get(bone) {
                bones.insert(bone.to_string(), r.clone());
            }
            AnimationFrame {
                bones,
                duration_ms: f.duration_ms,
                expressions: HashMap::new(),
                root_position: f.root_position,
            }
        })
        .collect();
    AnimationFile {
        name: format!("{} · {bone}", source.name),
        prompt: source.prompt.clone(),
        fps: source.fps,
        frame_count: frames.len(),
        frames,
        category: source.category.clone(),
        looping: source.looping,
        hold_duration: source.hold_duration,
    }
}

/// Expression-only keyframes (bones empty) — legacy helper; prefer [`slice_animation_for_expression`].
#[allow(dead_code)]
fn slice_animation_expressions(source: &AnimationFile) -> AnimationFile {
    use jarvis_avatar::pose_library::AnimationFrame;
    let frames: Vec<AnimationFrame> = source
        .frames
        .iter()
        .map(|f| AnimationFrame {
            bones: HashMap::new(),
            duration_ms: f.duration_ms,
            expressions: f.expressions.clone(),
            root_position: None,
        })
        .collect();
    AnimationFile {
        name: format!("{} · expressions", source.name),
        prompt: source.prompt.clone(),
        fps: source.fps,
        frame_count: frames.len(),
        frames,
        category: source.category.clone(),
        looping: source.looping,
        hold_duration: source.hold_duration,
    }
}

fn layer_from_clip(
    slug: &str,
    label: &str,
    animation: AnimationFile,
    mask: BoneMask,
    looping: bool,
) -> Layer {
    let mut layer = Layer::new(
        slug,
        label,
        DriverKind::Clip {
            animation: Box::new(animation),
        },
    )
    .blend(BlendMode::Override)
    .weight(1.0);
    layer.mask = mask;
    layer.looping = looping;
    layer.playing = true;
    if let Some(d) = layer.driver.duration_hint() {
        layer.duration = Some(d);
    }
    layer
}

/// Explode a library clip into one layer per animated bone (+ optional expressions layer).
pub fn layers_from_animation_per_bone(anim: AnimationFile, looping: bool) -> Vec<Layer> {
    let parent = anim.name.clone();
    let mut layers = Vec::new();
    for bone in bones_in_animation(&anim) {
        let slug = format!("bone-{}", bone.replace('.', "_"));
        let sliced = slice_animation_for_bone(&anim, &bone);
        let mut mask = BoneMask::default();
        mask.include.push(bone.clone());
        layers.push(layer_from_clip(
            &slug,
            &format!("{parent} · {bone}"),
            sliced,
            mask,
            looping,
        ));
    }
    if animation_has_expressions(&anim) {
        for preset in expressions_in_animation(&anim) {
            let slug = format!("expr-{}", preset.replace('.', "_"));
            let sliced = slice_animation_for_expression(&anim, &preset);
            layers.push(layer_from_clip(
                &slug,
                &format!("{parent} · {preset}"),
                sliced,
                BoneMask {
                    exclude: bones_in_animation(&anim),
                    ..Default::default()
                },
                looping,
            ));
        }
    }
    layers
}

/// Replace the stack with per-bone clip layers (bottom → top bone order).
pub fn install_animation_per_bone_layers(
    stack: &mut LayerStack,
    animation: AnimationFile,
    looping: bool,
) {
    stack.layers.clear();
    for layer in layers_from_animation_per_bone(animation, looping) {
        stack.add_layer(layer);
    }
}

/// Load (or bootstrap) a layer stack for editing a pose-library animation.
pub fn begin_library_animation_edit(
    filename: &str,
    library: &jarvis_avatar::pose_library::PoseLibrary,
    layer_sets: &crate::plugins::anim_layer_sets::LayerSetsStore,
    stack: &LayerStackHandle,
) -> Result<String, String> {
    let set_name = animation_edit_layer_set_name(filename);
    stack.with_write(|s| {
        let loaded = layer_sets.load_into(&set_name, s, library).unwrap_or(0);
        if loaded == 0 {
            s.layers.clear();
            let anim = library
                .load_animation(filename)
                .map_err(|e| e.to_string())?;
            let looping = anim.looping.unwrap_or(false);
            install_animation_per_bone_layers(s, anim, looping);
        }
        s.master_enabled = true;
        for layer in &mut s.layers {
            layer.playing = true;
        }
        Ok(format!(
            "layer edit: {set_name} ({} bone/morph layers)",
            s.layers.len()
        ))
    })
}

fn bake_duration(stack: &LayerStack, fallback: f32) -> f32 {
    stack
        .layers
        .iter()
        .filter(|l| l.enabled)
        .filter_map(|l| l.duration)
        .fold(fallback, f32::max)
        .max(1.0 / 30.0)
}

/// Offline sample of the full layer stack into a pose-library animation file.
pub fn bake_layer_stack_to_animation(
    stack: &LayerStack,
    snap: &RestPoseSnapshot,
    hierarchy: Option<&BoneHierarchy>,
    fps: f64,
    duration: Option<f32>,
) -> AnimationFile {
    use jarvis_avatar::pose_library::{AnimationFrame, BoneRotation};

    let mut work = stack.clone();
    for layer in &mut work.layers {
        layer.time = 0.0;
        layer.playing = true;
        layer.gain = if layer.enabled { 1.0 } else { 0.0 };
        reset_driver_transient_state(&mut layer.driver);
    }
    work.clock = 0.0;

    let fps = fps.max(1.0);
    let dt = (1.0 / fps) as f32;
    let dur = duration.unwrap_or_else(|| bake_duration(&work, 3.0));
    let frame_count = ((dur * fps as f32).ceil() as usize).max(1);
    let mut frames = Vec::with_capacity(frame_count);

    for _ in 0..frame_count {
        let (accumulator, expressions) =
            compose_layers(&mut work.layers, snap, hierarchy, dt, None, false, None);

        let mut bones = HashMap::with_capacity(accumulator.len());
        for (name, q_raw) in &accumulator {
            let rest_local = snap.rest.get(name).copied().unwrap_or(Quat::IDENTITY);
            if quat_close(*q_raw, rest_local, 1e-4) {
                continue;
            }
            let rest_world = snap.rest_world.get(name).copied().unwrap_or(Quat::IDENTITY);
            let pose_q = normalized_from_local(rest_local, rest_world, *q_raw);
            bones.insert(
                name.clone(),
                BoneRotation {
                    rotation: [pose_q.x, pose_q.y, pose_q.z, pose_q.w],
                },
            );
        }
        frames.push(AnimationFrame {
            bones,
            duration_ms: Some((1000.0 / fps).max(1.0)),
            expressions,
            root_position: None,
        });
        work.clock += dt;
    }

    let name = work
        .layers
        .iter()
        .find_map(|l| match &l.driver {
            DriverKind::Clip { animation } => Some(animation.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "baked".into());

    AnimationFile {
        name,
        prompt: String::new(),
        fps,
        frame_count: frames.len(),
        frames,
        category: None,
        looping: None,
        hold_duration: None,
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

/// 0→1 ramp (one-shot) or triangle pulse (looping) for expression-hold layers.
fn expression_hold_envelope(t: f32, duration: Option<f32>, looping: bool) -> f32 {
    let d = duration.unwrap_or(2.0).max(0.01);
    if looping {
        let phase = t.rem_euclid(d) / d;
        if phase <= 0.5 {
            phase * 2.0
        } else {
            (1.0 - phase) * 2.0
        }
    } else {
        (t / d).clamp(0.0, 1.0)
    }
}

fn sample_layer(layer: &mut Layer, snap: &RestPoseSnapshot, dt: f32, gaze_active: bool) -> DriverSample {
    let sample_t = layer_sample_time(layer);
    let dur = layer
        .duration
        .or_else(|| layer.driver.duration_hint())
        .unwrap_or(10.0)
        .max(0.01);
    // Loop crossfade only applies to forward, wrapping clips — ping-pong and
    // reverse don't have an end→start seam to hide.
    let loop_fade = if layer.looping && !layer.ping_pong && !layer.reverse {
        layer.loop_fade.max(0.0)
    } else {
        0.0
    };
    let mut sample = sample_driver(&mut layer.driver, sample_t, dt, snap, dur, loop_fade, gaze_active);
    if matches!(layer.driver, DriverKind::ExpressionHold { .. }) {
        let env = expression_hold_envelope(sample_t, layer.duration, layer.looping);
        for w in sample.expressions.values_mut() {
            *w *= env;
        }
    }
    sample
}

/// Interpolated ROOT MOTION (hips translation delta from bind, meters) of a
/// clip at `t` seconds. `None` when the clip carries no `root_position` track,
/// so rotation-only clips behave exactly as before (no translation emitted).
pub(crate) fn sample_clip_root_position(animation: &AnimationFile, t: f32) -> Option<Vec3> {
    let total = animation.frames.len();
    if total == 0 {
        return None;
    }
    let fps = animation.fps.max(1.0) as f32;
    let frame_f = (t * fps).clamp(0.0, (total - 1) as f32);
    let idx0 = frame_f.floor() as usize;
    let idx1 = (idx0 + 1).min(total - 1);
    let frac = frame_f.fract();
    let a = animation.frames[idx0].root_position.map(Vec3::from_array);
    let b = animation.frames[idx1].root_position.map(Vec3::from_array);
    match (a, b) {
        (Some(a), Some(b)) => Some(a.lerp(b, frac)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn sample_driver(
    driver: &mut DriverKind,
    t: f32,
    dt: f32,
    snap: &RestPoseSnapshot,
    loop_dur: f32,
    loop_fade: f32,
    gaze_active: bool,
) -> DriverSample {
    match driver {
        DriverKind::Clip { animation } => sample_clip(animation, t, loop_dur, loop_fade, snap),
        DriverKind::PoseHold { pose } => sample_pose_hold(pose, snap),
        DriverKind::ExpressionHold { expressions } => DriverSample {
            bones: HashMap::new(),
            expressions: expressions.clone(),
        },
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
        } => sample_blink(
            dt,
            next_in,
            phase,
            phase_t,
            *mean_interval,
            *double_blink_chance,
        ),
        DriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => sample_weight_shift(t, *rate_hz, *hip_roll_deg, *spine_counter_deg),
        DriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
            curl_bias_thumb_deg,
        } => sample_finger_fidget(
            t,
            *amplitude_deg,
            *frequency_hz,
            *seed,
            *curl_bias_deg,
            *curl_bias_thumb_deg,
            snap,
        ),
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
        } => sample_toe_fidget(t, *amplitude_deg, *frequency_hz, *seed, *curl_bias_deg),
        DriverKind::LookAround {
            mean_interval,
            yaw_deg,
            pitch_deg,
            next_in,
            cur_yaw,
            cur_pitch,
            target_yaw,
            target_pitch,
            damp,
        } => sample_look_around(
            dt,
            next_in,
            cur_yaw,
            cur_pitch,
            target_yaw,
            target_pitch,
            damp,
            *mean_interval,
            *yaw_deg,
            *pitch_deg,
            gaze_active,
        ),
        DriverKind::Sway {
            rate_hz,
            amount_deg,
        } => sample_sway(t, *rate_hz, *amount_deg),
        DriverKind::ArmSway {
            rate_hz,
            amount_deg,
        } => sample_arm_sway(t, *rate_hz, *amount_deg),
        DriverKind::LegShift {
            rate_hz,
            shift_deg,
            knee_bend_deg,
            hip_sway_deg,
            ankle_deg,
            seed,
        } => sample_leg_shift(
            t,
            *rate_hz,
            *shift_deg,
            *knee_bend_deg,
            *hip_sway_deg,
            *ankle_deg,
            *seed,
            snap,
        ),
    }
}

/// Saved poses and animation clips are recorded by `pose_driver` in the same
/// normalized humanoid space as `getNormalizedBoneNode().quaternion` in
/// three-vrm. The layer accumulator however blends in **raw bone-local
/// space** (so it can mix with rest-relative procedural deltas like breathing
/// and finger-fidget). Convert each stored quaternion to its raw-local form
/// using the cached rest snapshot before handing it back to the accumulator.
/// Without this conversion the compose step would mix two different bases and
/// every pose-hold / clip layer would silently land on the rig's bind pose.
fn convert_normalized_to_local(
    name: &str,
    pose_q: Quat,
    snap: &RestPoseSnapshot,
) -> Quat {
    let rest_local = snap.rest.get(name).copied().unwrap_or(Quat::IDENTITY);
    let rest_world = snap.rest_world.get(name).copied().unwrap_or(Quat::IDENTITY);
    local_from_normalized(rest_local, rest_world, pose_q)
}

fn sample_pose_hold(pose: &PoseFile, snap: &RestPoseSnapshot) -> DriverSample {
    let mut bones = HashMap::with_capacity(pose.bones.len());
    for (name, r) in &pose.bones {
        let [x, y, z, w] = r.rotation;
        let normalized = Quat::from_xyzw(x, y, z, w);
        bones.insert(name.clone(), convert_normalized_to_local(name, normalized, snap));
    }
    let expressions = pose.expressions.clone();
    DriverSample { bones, expressions }
}

/// Interpolated **normalized** pose (bones + expressions) at a fractional
/// frame index. Bones are still in three-vrm normalized humanoid space — the
/// caller converts to raw-local after any crossfade blending.
fn clip_pose_normalized(
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
        // Skip non-humanoid tracks (hair / skirt / other spring bones). Those
        // are secondary motion owned by the spring-bone sim; a keyframed clip
        // driving them fights physics and pops at the loop wrap.
        if !is_vrm_humanoid_bone(name) {
            continue;
        }
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

fn sample_clip(
    animation: &AnimationFile,
    t: f32,
    loop_dur: f32,
    loop_fade: f32,
    snap: &RestPoseSnapshot,
) -> DriverSample {
    if animation.frames.is_empty() {
        return DriverSample::default();
    }
    let fps = animation.fps.max(1.0) as f32;
    let total = animation.frames.len();
    if total == 1 {
        let frame = &animation.frames[0];
        let mut bones = HashMap::with_capacity(frame.bones.len());
        for (name, r) in &frame.bones {
            // Humanoid bones only — spring bones (hair/skirt) are sim-owned.
            if !is_vrm_humanoid_bone(name) {
                continue;
            }
            let [x, y, z, w] = r.rotation;
            let normalized = Quat::from_xyzw(x, y, z, w);
            bones.insert(name.clone(), convert_normalized_to_local(name, normalized, snap));
        }
        return DriverSample {
            bones,
            expressions: frame.expressions.clone(),
        };
    }

    let (mut bones_n, mut expr) = clip_pose_normalized(animation, t * fps);

    // Loop crossfade: over the last `loop_fade` seconds, blend the tail toward
    // the clip's first frame so that the pose at the wrap point (t → loop_dur)
    // equals the pose at t = 0. Eliminates the hard "twitch" on loop restart
    // even when the author's last frame ≠ first frame.
    if loop_fade > 1e-4 && loop_dur > loop_fade {
        let fade_start = loop_dur - loop_fade;
        if t >= fade_start {
            let w = smoothstep(((t - fade_start) / loop_fade).clamp(0.0, 1.0));
            let (start_bones, start_expr) = clip_pose_normalized(animation, 0.0);
            for (name, target) in start_bones {
                let blended = bones_n
                    .get(&name)
                    .map(|cur| cur.slerp(target, w))
                    .unwrap_or(target);
                bones_n.insert(name, blended);
            }
            for (name, target) in start_expr {
                let cur = expr.get(&name).copied().unwrap_or(0.0);
                expr.insert(name, cur + (target - cur) * w);
            }
        }
    }

    let mut bones = HashMap::with_capacity(bones_n.len());
    for (name, q) in bones_n {
        let local = convert_normalized_to_local(&name, q, snap);
        bones.insert(name, local);
    }
    DriverSample {
        bones,
        expressions: expr,
    }
}

fn sample_breathing(t: f32, rate_hz: f32, pitch_deg: f32, roll_deg: f32) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    // Real breath isn't a symmetric sine: the inhale is quicker than the long
    // exhale. Warp the phase (still periodic + continuous at the wrap) to skew
    // the curve, then add a small second harmonic for chest "shape" so it
    // doesn't read as a single mechanical oscillation.
    let phase = (omega * t).rem_euclid(std::f32::consts::TAU);
    let warped = phase - 0.35 * phase.sin();
    let env = warped.sin() + 0.18 * (2.0 * warped).sin();
    let pitch = env * pitch_deg.to_radians();
    // Phase-shift roll by ~90° so the chest rocks in a figure-eight rather
    // than a straight line (AIRI plan §3 "Procedural idle layering").
    let roll = (warped + std::f32::consts::FRAC_PI_2).sin() * roll_deg.to_radians();

    let chest = Quat::from_euler(EulerRot::XYZ, pitch, 0.0, roll);
    // Tiny counter-roll on the upper chest for shape, plus a touch of breath on
    // the neck so the head floats rather than sitting rigid on the torso.
    let upper_chest = Quat::from_euler(EulerRot::XYZ, pitch * 0.4, 0.0, -roll * 0.3);
    let neck = Quat::from_euler(EulerRot::XYZ, pitch * -0.25, 0.0, roll * 0.15);

    let mut bones = HashMap::new();
    bones.insert("chest".into(), chest);
    bones.insert("upperChest".into(), upper_chest);
    bones.insert("neck".into(), neck);
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

#[allow(clippy::too_many_arguments)]
fn sample_look_around(
    dt: f32,
    next_in: &mut f32,
    cur_yaw: &mut f32,
    cur_pitch: &mut f32,
    target_yaw: &mut f32,
    target_pitch: &mut f32,
    damp: &mut f32,
    mean_interval: f32,
    yaw_deg: f32,
    pitch_deg: f32,
    gaze_active: bool,
) -> DriverSample {
    // Pick a new glance target when the dwell timer expires. ~35% of glances
    // return to center so the head doesn't drift permanently off-axis.
    *next_in -= dt;
    if *next_in <= 0.0 {
        let mut rng = rand::rng();
        if rng.random_bool(0.35) {
            *target_yaw = 0.0;
            *target_pitch = 0.0;
        } else {
            *target_yaw = rng.random_range(-1.0_f32..1.0) * yaw_deg.to_radians();
            *target_pitch = rng.random_range(-1.0_f32..1.0) * pitch_deg.to_radians();
        }
        let base = mean_interval.max(0.5);
        *next_in = base * rng.random_range(0.6_f32..1.6);
    }

    // Exponential ease toward the active target — fast enough to feel like a
    // deliberate glance, slow enough to avoid snapping.
    let follow = (dt * 2.5).clamp(0.0, 1.0);
    *cur_yaw += (*target_yaw - *cur_yaw) * follow;
    *cur_pitch += (*target_pitch - *cur_pitch) * follow;

    // While a face is being tracked, collapse the wander toward forward so the
    // head holds its orientation and the eyes do the tracking. Ramp the gain
    // smoothly (~0.5 s) so engaging / releasing gaze never pops the head.
    let damp_target = if gaze_active { 0.12 } else { 1.0 };
    let damp_step = (dt / 0.5).clamp(0.0, 1.0);
    *damp += (damp_target - *damp) * damp_step;

    let yaw = *cur_yaw * *damp;
    let pitch = *cur_pitch * *damp;

    // Split the look across neck + head so it reads as a natural head turn,
    // not a stiff pivot at one joint. Yaw = local Y, pitch = local X.
    let neck = Quat::from_euler(EulerRot::XYZ, pitch * 0.4, yaw * 0.4, 0.0);
    let head = Quat::from_euler(EulerRot::XYZ, pitch * 0.6, yaw * 0.6, 0.0);

    let mut bones = HashMap::new();
    bones.insert("neck".into(), neck);
    bones.insert("head".into(), head);
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

fn sample_weight_shift(
    t: f32,
    rate_hz: f32,
    hip_roll_deg: f32,
    spine_counter_deg: f32,
) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    // Two incommensurate sines so the sway wanders and never repeats exactly,
    // instead of pacing back and forth on a fixed metronome.
    let primary = (omega * t).sin();
    let drift = 0.35 * (omega * 0.37 * t + 1.3).sin();
    let phase = (primary + drift).clamp(-1.3, 1.3);
    // Slow yaw lets the hips rotate slightly into the shifted side for weight.
    let yaw = 0.4 * (omega * 0.53 * t + 0.7).sin();

    let hip = Quat::from_euler(
        EulerRot::XYZ,
        0.0,
        yaw * hip_roll_deg.to_radians() * 0.5,
        phase * hip_roll_deg.to_radians(),
    );
    let spine = Quat::from_euler(
        EulerRot::XYZ,
        0.0,
        0.0,
        -phase * spine_counter_deg.to_radians(),
    );
    // Carry a little of the counter-rotation up the chain so the upper body
    // stays vertical as the hips shift (real standing-idle weight transfer).
    let chest = Quat::from_euler(
        EulerRot::XYZ,
        0.0,
        0.0,
        phase * spine_counter_deg.to_radians() * 0.4,
    );

    let mut bones = HashMap::new();
    bones.insert("hips".into(), hip);
    bones.insert("spine".into(), spine);
    bones.insert("chest".into(), chest);
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

/// Slow whole-body balance sway. Forward/back lean (local X) on the hips with
/// an incommensurate side-to-side drift (local Z), then counter-rotations up
/// the spine so the head stays roughly over the feet — the silhouette leans
/// and recovers the way a real person standing still constantly does.
fn sample_sway(t: f32, rate_hz: f32, amount_deg: f32) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let fwd = (omega * t).sin();
    // 0.63× + phase offset → never repeats exactly with the fwd term.
    let lat = (omega * 0.63 * t + 0.9).sin();
    let amt = amount_deg.to_radians();

    let hips = Quat::from_euler(EulerRot::XYZ, fwd * amt, 0.0, lat * amt * 0.6);
    let spine = Quat::from_euler(EulerRot::XYZ, -fwd * amt * 0.5, 0.0, -lat * amt * 0.3);
    let chest = Quat::from_euler(EulerRot::XYZ, -fwd * amt * 0.25, 0.0, 0.0);

    let mut bones = HashMap::new();
    bones.insert("hips".into(), hips);
    bones.insert("spine".into(), spine);
    bones.insert("chest".into(), chest);
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

/// Relaxed pendular arm sway. The two arms swing on the same slow clock but a
/// little out of phase (0.6 rad) so they don't move in lockstep. Forward swing
/// (local X) shares a sign across sides (X isn't mirrored); the in/out
/// abduction (local Z) is negated on the right to mirror the rig.
fn sample_arm_sway(t: f32, rate_hz: f32, amount_deg: f32) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let lphase = (omega * t).sin();
    let rphase = (omega * t + 0.6).sin();
    let amt = amount_deg.to_radians();

    let mut bones = HashMap::new();
    bones.insert(
        "leftUpperArm".into(),
        Quat::from_euler(EulerRot::XYZ, lphase * amt * 0.5, 0.0, lphase * amt),
    );
    bones.insert(
        "rightUpperArm".into(),
        Quat::from_euler(EulerRot::XYZ, rphase * amt * 0.5, 0.0, -rphase * amt),
    );
    bones.insert(
        "leftLowerArm".into(),
        Quat::from_euler(EulerRot::XYZ, lphase * amt * 0.3, 0.0, 0.0),
    );
    bones.insert(
        "rightLowerArm".into(),
        Quat::from_euler(EulerRot::XYZ, rphase * amt * 0.3, 0.0, 0.0),
    );
    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

/// Coordinated lower-body weight transfer (contrapposto standing idle).
///
/// A single slow wandering signal `w ∈ [-1, 1]` (`+1` = fully weighted on the
/// **right** leg) drives everything so the lower body reads as one coherent
/// motion instead of a pile of independent jitters:
/// * **hips** lean toward the loaded leg (`shift_deg`, local Z) + rotate a touch
///   into the stance (Y) + a hair of forward/back from the postural sway.
/// * **spine / chest** counter-rotate so the torso stays roughly vertical (the
///   classic S-curve of weight-on-one-leg).
/// * each **thigh** (`UpperLeg`) flexes slightly forward when free and adducts
///   under the COM when loaded.
/// * each **knee** (`LowerLeg`) keeps a soft baseline bend, with most of the
///   `knee_bend_deg` flex landing on the **free** leg (`+pitch`, the rig's
///   natural knee-flex axis).
/// * each **ankle** (`Foot`) carries a faster, smaller postural micro-sway —
///   the constant balance correction — with the free foot freer than the
///   planted one.
///
/// Leg bones are authored in normalized humanoid space and converted to
/// raw-local rest-relative deltas via [`rest_relative_delta`], so the axis
/// meanings (knee = +pitch, abduction = signed roll) hold regardless of how the
/// leg bones are bound.
#[allow(clippy::too_many_arguments)]
fn sample_leg_shift(
    t: f32,
    rate_hz: f32,
    shift_deg: f32,
    knee_bend_deg: f32,
    hip_sway_deg: f32,
    ankle_deg: f32,
    seed: u64,
    snap: &RestPoseSnapshot,
) -> DriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let p1 = hash_phase(seed, 0x11);
    let p2 = hash_phase(seed, 0x22);
    let p3 = hash_phase(seed, 0x33);

    // Wandering weight signal in [-1, 1]; +1 = fully on the RIGHT leg. Two
    // incommensurate sines mean it lingers on a leg, drifts through center, and
    // occasionally settles harder to one side instead of pacing metronomically.
    let s = 0.74 * (omega * t).sin() + 0.34 * (omega * 0.41 * t + p1).sin();
    let w = s.clamp(-1.0, 1.0);

    // Faster, smaller postural sway (the perpetual ankle balance correction).
    let sway_ml = (omega * 2.3 * t + p2).sin(); // medial-lateral
    let sway_ap = (omega * 1.7 * t + p3).sin(); // anterior-posterior

    let shift = shift_deg.to_radians();
    let knee = knee_bend_deg.to_radians();
    let hsway = hip_sway_deg.to_radians();
    let ank = ankle_deg.to_radians();

    let mut bones: HashMap<String, Quat> = HashMap::with_capacity(11);
    let put = |bones: &mut HashMap<String, Quat>, name: &str, q: Quat| {
        bones.insert(name.to_string(), rest_relative_delta(name, q, snap));
    };

    // --- pelvis + spine: lean toward the weighted leg, counter up the chain ---
    let hips_q = Quat::from_euler(
        EulerRot::XYZ,
        sway_ap * ank * 0.3, // tiny forward/back at the pelvis
        w * hsway * 0.5,     // rotate pelvis slightly toward stance
        w * shift,           // lateral lean / pelvic obliquity
    );
    let spine_q = Quat::from_euler(EulerRot::XYZ, 0.0, 0.0, -w * shift * 0.5);
    let chest_q = Quat::from_euler(EulerRot::XYZ, 0.0, 0.0, -w * shift * 0.2);
    put(&mut bones, "hips", hips_q);
    put(&mut bones, "spine", spine_q);
    put(&mut bones, "chest", chest_q);

    // --- per-leg: weighted leg straightens / adducts, free leg softens / abducts ---
    for side in ["left", "right"] {
        let right = side == "right";
        // Weight on THIS leg: 1 when fully loaded, 0 when free.
        let weight = (if right { 0.5 + 0.5 * w } else { 0.5 - 0.5 * w }).clamp(0.0, 1.0);
        let free = 1.0 - weight;
        // Abduction roll is mirrored: outward is +Z on the right, -Z on the left.
        let z_sign = if right { 1.0 } else { -1.0 };

        // Thigh: free leg flexes a touch forward and rotates outward; weighted
        // leg adducts slightly under the centre of mass.
        let thigh_pitch = free * knee * 0.22;
        let thigh_roll = (free * 0.6 - 0.2) * hsway * z_sign;
        put(
            &mut bones,
            &format!("{side}UpperLeg"),
            Quat::from_euler(EulerRot::XYZ, thigh_pitch, 0.0, thigh_roll),
        );

        // Knee: soft baseline on both, most of the bend on the free leg
        // (+pitch is the rig's natural knee-flex axis — see compile_bend_knee).
        let knee_amt = (0.12 + 0.88 * free) * knee;
        put(
            &mut bones,
            &format!("{side}LowerLeg"),
            Quat::from_euler(EulerRot::XYZ, knee_amt, 0.0, 0.0),
        );

        // Ankle: postural micro-sway, the free foot freer than the planted one.
        let foot_gain = 0.45 + 0.55 * free;
        put(
            &mut bones,
            &format!("{side}Foot"),
            Quat::from_euler(
                EulerRot::XYZ,
                sway_ap * ank * foot_gain,
                0.0,
                sway_ml * ank * foot_gain * z_sign,
            ),
        );
    }

    DriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

/// The four non-thumb fingers, index→little, with a per-finger curl multiplier.
/// A relaxed hand curls progressively more toward the little finger (a gentle
/// arc), so the multiplier grows down the row.
const FINGERS: &[(&str, f32)] = &[
    ("Index", 0.75),
    ("Middle", 0.92),
    ("Ring", 1.0),
    ("Little", 1.05),
];

/// Per-joint curl scale along a finger (proximal → intermediate → distal). The
/// middle joint flexes most in a relaxed curl; the tip trails.
const FINGER_JOINTS3: &[(&str, f32)] = &[
    ("Proximal", 0.9),
    ("Intermediate", 1.0),
    ("Distal", 0.6),
];

/// VRM thumb chain — **Metacarpal / Proximal / Distal** (there is no
/// `ThumbIntermediate` in the VRM humanoid spec). Per-joint opposition scale.
const THUMB_JOINTS: &[(&str, f32)] = &[
    ("Metacarpal", 0.7),
    ("Proximal", 1.0),
    ("Distal", 0.45),
];

/// Convert a desired **normalized-humanoid-space** rotation `pose_q` (relative
/// to the bone's rest) into the raw-local **post-multiply delta** the
/// `RestRelative` accumulator expects. From `raw_local = rest_local ·
/// rest_world⁻¹ · pose_q · rest_world` and the accumulator's `current * delta`
/// with `current = rest_local`, the delta is `rest_world⁻¹ · pose_q ·
/// rest_world` (`rest_local` cancels). Conjugating by `rest_world` is what
/// lets us specify curl in the same clean axes the fist/relaxed pose templates
/// use, regardless of how the finger bones are bound.
fn rest_relative_delta(name: &str, pose_q: Quat, snap: &RestPoseSnapshot) -> Quat {
    let rw = snap.rest_world.get(name).copied().unwrap_or(Quat::IDENTITY);
    rw.inverse() * pose_q * rw
}

/// Natural relaxed-hand fidget.
///
/// Curl is authored in **normalized humanoid space** (the same space the
/// `make_fist` / relaxed-hand templates use) and converted per-bone to a
/// raw-local delta. That gives the correct axes for free:
/// * **Fingers** curl on normalized **+Z (roll)** — right hand `+Z`, left hand
///   `−Z` (the relaxed/fist templates mirror by negating x & z).
/// * **The thumb** opposes on normalized **−Y (yaw)** on *both* hands (the
///   mirror leaves y alone), with smaller per-side flex (X) and roll (Z). This
///   tucks the thumb toward the palm instead of pitching it up into a
///   "thumbs-up".
///
/// On top of the resting bias, each digit gets its own phase, a slightly
/// detuned rate, a fast micro-tremor + slow wander (two incommensurate sines),
/// and a very slow swell envelope, so the fingers never move in lockstep and
/// the hand reads alive rather than mechanical.
#[allow(clippy::too_many_arguments)]
fn sample_finger_fidget(
    t: f32,
    amplitude_deg: f32,
    frequency_hz: f32,
    seed: u64,
    curl_bias_deg: f32,
    curl_bias_thumb_deg: f32,
    snap: &RestPoseSnapshot,
) -> DriverSample {
    let mut bones = HashMap::with_capacity(28);
    let amp = amplitude_deg.to_radians();
    let bias = curl_bias_deg.to_radians();
    let thumb_bias = curl_bias_thumb_deg.to_radians();
    let tau = std::f32::consts::TAU;

    let mut digit = 0u64;
    for side in ["left", "right"] {
        // Finger curl sign in normalized space: right hand +Z, left hand −Z.
        let z_sign = if side == "right" { 1.0 } else { -1.0 };

        for (finger, fmul) in FINGERS {
            let ph = hash_phase(seed, digit.wrapping_mul(2654435761));
            let rate = frequency_hz * (0.75 + (digit as f32 * 0.11) % 0.6);
            let omega = tau * rate;
            // micro-tremor + slow wander + a very slow swell so the finger's
            // activity rises and falls instead of buzzing at a fixed amplitude.
            let micro = (omega * t + ph).sin();
            let wander = 0.5 * (omega * 0.41 * t + ph * 1.7).sin();
            let swell = 0.6 + 0.4 * (omega * 0.13 * t + ph).sin();
            let osc = (micro + wander) * amp * swell;
            digit += 1;

            for (ji, (joint, jmul)) in FINGER_JOINTS3.iter().enumerate() {
                let name = format!("{side}{finger}{joint}");
                let joint_bias = bias * fmul * jmul;
                let curl = (joint_bias + osc * jmul) * z_sign;
                // A whisper of knuckle splay (normalized Y) on the proximal
                // joint only, so the fingers fan a hair instead of staying
                // glued parallel. Kept tiny — relaxed hands barely splay.
                let splay = if ji == 0 {
                    0.12 * amp * (omega * 0.23 * t + ph).sin() * z_sign
                } else {
                    0.0
                };
                let pose_q = Quat::from_euler(EulerRot::XYZ, 0.0, splay, curl);
                bones.insert(name.clone(), rest_relative_delta(&name, pose_q, snap));
            }
        }

        // Thumb: opposition (−Y) dominant on both hands; flex (X) and roll (Z)
        // flip per side to mirror the rig (fist template signs).
        let x_sign = if side == "right" { -1.0 } else { 1.0 };
        let tz_sign = if side == "right" { 1.0 } else { -1.0 };
        let tph = hash_phase(seed, 0x7000 + if side == "right" { 1 } else { 0 });
        let tomega = tau * frequency_hz * 0.6;
        let tosc = (tomega * t + tph).sin() * amp * 0.6;

        for (joint, jmul) in THUMB_JOINTS {
            let name = format!("{side}Thumb{joint}");
            let opp = (thumb_bias + tosc) * jmul;
            let pitch = x_sign * opp * 0.4;
            let yaw = -opp; // opposition: tuck toward palm
            let roll = tz_sign * opp * 0.5;
            let pose_q = Quat::from_euler(EulerRot::XYZ, pitch, yaw, roll);
            bones.insert(name.clone(), rest_relative_delta(&name, pose_q, snap));
        }
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
    curl_bias_deg: f32,
) -> DriverSample {
    let mut bones = HashMap::new();
    let amp = amplitude_deg.to_radians();
    let bias = curl_bias_deg.to_radians();
    for (i, name) in TOE_BONES.iter().enumerate() {
        let phase_offset = hash_phase(seed, (i as u64) ^ 0xA5);
        let omega = std::f32::consts::TAU * frequency_hz;
        let curl = bias + (omega * t + phase_offset).sin() * amp;
        // Mirror the right side (see `sample_finger_fidget`).
        let dir = if name.starts_with("right") { -1.0 } else { 1.0 };
        bones.insert((*name).into(), Quat::from_rotation_x(curl * dir));
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
