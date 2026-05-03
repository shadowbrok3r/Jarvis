//! Pose / expression / bone driver for the VRM avatar.
//!
//! Owns the Bevy side of the MCP pose controller. Tokio tasks (driven by the
//! `rmcp` tool handlers) talk to this plugin only via the crossbeam
//! [`PoseCommand`] queue and a shared [`BoneSnapshot`] — no blocking on Bevy
//! ever. Every frame this plugin:
//!
//! 1. Drains queued commands and turns them into `bevy_vrm1` events
//!    (`SetExpressions`, `ModifyExpressions`) or direct `Transform` writes on
//!    bone-marker entities. MCP `animate_expressions` queues
//!    [`PoseCommand::AnimateExpressions`], then [`tick_expression_animation`]
//!    samples keyframes each frame after the queue drains (after layered
//!    `ApplyExpression` from `anim_layers`).
//! 2. Publishes the current per-bone world rotation and **VRM expression preset
//!    names** (from [`ExpressionEntityMap`]) to a `BoneSnapshot` behind an
//!    `Arc<RwLock<_>>` so `get_current_bone_state` / `adjust_bone` / MCP discovery
//!    read fresh data without stalling the ECS loop.
//!
//! The canonical camelCase bone names (`"leftUpperArm"`, `"rightHand"`, …) are
//! the identifiers used for **humanoid** bones across the MCP boundary.
//! Additional **skin-only** joints (e.g. Rigify ``DEF-toe_big.L``) are indexed by
//! their glTF node [`Name`] and driven in the **same normalized pose quaternion
//! space** as humanoid bones (see [`is_vrm_humanoid_bone`] for name classification only).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bevy::animation::{AnimationTargetId, RepeatAnimation};
use bevy::app::AnimationSystems;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_vrm1::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Convert a normalized-humanoid pose rotation to the raw rig's local
/// rotation, matching three-vrm's `VRMHumanoidRig.update()` exactly.
///
/// Poses emitted by Airi / `pose-controller` are read from three-vrm's
/// *normalized humanoid* via `getNormalizedBoneNode(name).quaternion`. That
/// quaternion is a **world-space delta from the rig's rest pose** — at
/// `pose_q = identity` every bone sits at its own raw rest (so the idle rig
/// is undisturbed), and a non-identity `pose_q` pre-multiplies the bone's
/// rest world rotation.
///
/// three-vrm's sync (three-vrm-core `VRMHumanoidRig.update`,
/// `boneNode.quaternion.copy(rigBoneNode.quaternion).multiply(parentWorldRotation).premultiply(invParentWorldRotation).multiply(boneRotation)`)
/// computes:
///
/// ```text
/// raw_local = parent_rest_world⁻¹ · pose_q · parent_rest_world · rest_local
/// ```
///
/// Using `rest_world = parent_rest_world · rest_local`, that simplifies to a
/// form that only needs the bone's *own* rest transforms — the same
/// conjugation pattern `bevy_vrm1` uses for look-at
/// (see `vrm/look_at.rs::to_eye_rotation`):
///
/// ```text
/// raw_local = rest_local · rest_world⁻¹ · pose_q · rest_world
/// ```
///
/// At `pose_q = identity` this collapses to `rest_local` (rest preserved).
/// For torso / legs / head where `rest_world ≈ identity` the middle term
/// vanishes and the formula degenerates to `rest_local · pose_q` — which is
/// exactly why the historical code worked for those chains but twisted arms
/// / shoulders / fingers / toes whose rest world rotations are non-trivial.
///
/// See [`normalized_from_local`] for the inverse used by the snapshot.
///
/// Public so the animation-layer pipeline ([`crate::plugins::anim_layers`])
/// can pre-convert saved-pose / clip frames (stored in normalized humanoid
/// space) into the raw-local space the layer accumulator blends against.
pub fn local_from_normalized(rest_local: Quat, rest_world: Quat, pose_q: Quat) -> Quat {
    rest_local * rest_world.inverse() * pose_q * rest_world
}

/// Inverse of [`local_from_normalized`]. Given a bone's current raw local
/// rotation plus its rest transforms, recover the normalized-humanoid space
/// quaternion Airi would read via `getNormalizedBoneNode(name).quaternion`.
/// Used by [`publish_bone_snapshot`] so `get_current_bone_state` →
/// `set_bones` / `adjust_bone` round-trips cleanly.
///
/// Derivation from `raw_local = rest_local · rest_world⁻¹ · pose_q · rest_world`:
///
/// ```text
/// pose_q = rest_world · rest_local⁻¹ · raw_local · rest_world⁻¹
/// ```
pub fn normalized_from_local(rest_local: Quat, rest_world: Quat, raw_local: Quat) -> Quat {
    rest_world * rest_local.inverse() * raw_local * rest_world.inverse()
}

/// Helen / Rigify per-digit skin toes `DEF-toe_{big,index,middle,ring,little}.{L,R}`:
/// intrinsic XYZ Euler from normalized `pose_q` often lands on **±180° yaw** (or equivalent
/// aliases) at bind while the pad reads neutral in the viewport. The Bones tab stores **display**
/// yaw near 0 at bind; this offset is added to **display** yaw (degrees) before
/// `Quat::from_euler(EulerRot::XYZ, …)` so MCP `pose_bones` and the sliders match the same
/// convention for **every** named toe chain (not only `DEF-toe_big`).
#[inline]
pub fn def_toe_big_yaw_slider_extra_deg(bone: &str) -> f32 {
    let b = bone.to_ascii_lowercase();
    let Some(rest) = b.strip_prefix("def-toe_") else {
        return 0.0;
    };
    let Some((digit, side)) = rest.split_once('.') else {
        return 0.0;
    };
    if !matches!(digit, "big" | "index" | "middle" | "ring" | "little") {
        return 0.0;
    }
    match side {
        "l" => 180.0,
        "r" => -180.0,
        _ => 0.0,
    }
}

use jarvis_avatar::config::Settings;

use crate::plugins::avatar::{AvatarVrmRoot, spawn_avatar_vrm};

pub struct PoseDriverPlugin;

impl Plugin for PoseDriverPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded::<PoseCommand>();
        let snapshot: Arc<RwLock<BoneSnapshot>> = Arc::new(RwLock::new(BoneSnapshot::default()));
        app.insert_resource(PoseCommandQueue(rx))
            .insert_resource(PoseCommandSender(tx))
            .insert_resource(BoneSnapshotHandle(snapshot))
            .init_resource::<BoneEntityIndex>()
            .init_resource::<IndexedBones>()
            .init_resource::<ActiveTransitions>()
            .init_resource::<ExpressionAnimationPlayback>()
            // Order: right after `AnimationSystems` (so VRMA sampling doesn't
            // overwrite us), but *before* `VrmSystemSets::Constraints` (aim /
            // roll / rotation constraints run `.after(AnimationSystems)` and
            // can overwrite arm / finger rotations we just wrote) and *before*
            // `TransformSystems::Propagate` (so our `Transform` changes actually
            // reach `GlobalTransform` this frame — otherwise the renderer sees
            // a stale pose until the next propagation tick).
            .add_systems(
                PostUpdate,
                sync_bone_entity_index
                    .after(AnimationSystems)
                    .before(VrmSystemSets::Constraints)
                    .before(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                apply_pose_commands
                    .after(AnimationSystems)
                    .after(sync_bone_entity_index)
                    .before(VrmSystemSets::Constraints)
                    .before(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                tick_active_transitions
                    .after(AnimationSystems)
                    .after(apply_pose_commands)
                    .before(VrmSystemSets::Constraints)
                    .before(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                tick_expression_animation
                    .after(AnimationSystems)
                    .after(tick_active_transitions)
                    .before(VrmSystemSets::Constraints)
                    .before(TransformSystems::Propagate),
            )
            // Runs after `tick_active_transitions` so the snapshot matches the
            // same PostUpdate pose as constraint systems that run after this
            // plugin (see `VrmSystemSets::Constraints` ordering on writers).
            .add_systems(
                PostUpdate,
                publish_bone_snapshot
                    .after(AnimationSystems)
                    .after(tick_expression_animation),
            );
    }
}

// ---------- Public API (Tokio side) --------------------------------------------

/// Command queued from a Tokio task; consumed by `apply_pose_commands` in `PostUpdate`.
#[derive(Debug, Clone)]
pub enum PoseCommand {
    /// Apply a partial set of bone rotations. Bones not listed are left alone
    /// unless `preserve_omitted_bones = false`, in which case the rig is
    /// reset to identity first.
    ///
    /// When the blend_transitions feature flag is on in
    /// `Settings::pose_controller`, `blend_weight` and `transition_seconds` are
    /// honoured via a slerp over time. Otherwise the rotations snap instantly
    /// (the historical behaviour that MCP scripts expect).
    ApplyBones {
        bones: HashMap<String, [f32; 4]>,
        preserve_omitted_bones: bool,
        /// 0..=1 scale on the delta from current rotation to the target. `None`
        /// falls back to `Settings::pose_controller.default_blend_weight`.
        blend_weight: Option<f32>,
        /// Seconds of slerp blend from current to target. `None` falls back to
        /// `Settings::pose_controller.default_transition_seconds`. `Some(0.0)`
        /// means instant.
        transition_seconds: Option<f32>,
    },
    /// Partial expression update (`vrm:apply-expression`).
    ///
    /// When `cancel_expression_animation` is true (MCP / hub / explicit UI apply),
    /// any in-flight [`ExpressionAnimationPlayback`] from `animate_expressions` is
    /// cleared so static weights stick. Layered drivers (`anim_layers`, idle tick)
    /// pass `false` so blinks and viseme layers keep working alongside MCP clips.
    ApplyExpression {
        weights: HashMap<String, f32>,
        cancel_expression_animation: bool,
    },
    /// Full-face replace via [`SetExpressions`] — used by `a2f:expression-keyframes`.
    SetExpression { weights: HashMap<String, f32> },
    /// Reset every bone to its rest pose and clear expression overrides.
    ResetPose,
    /// Reset a specific subset of bones to their rest-pose local rotation.
    /// Used by the per-bone "↺" buttons in the Bones diagnostic tab.
    ResetBones(Vec<String>),
    /// One-shot diagnostic: dump each humanoid bone's entity + name + ancestor
    /// chain + whether it's referenced by any `SkinnedMesh` in the world. Used
    /// to prove whether the entity we're writing to is actually the one the
    /// GPU uses for skinning — the single most useful piece of evidence when
    /// MMD-exported VRMs refuse to animate from direct `Transform` writes.
    DumpDiagnostics,
    /// Play a short VRM expression curve in real time (MCP `animate_expressions`).
    /// Keyframes are sorted `(time_s, weights)`; sampling is piecewise-linear in
    /// time per expression channel. Replaces any previous expression clip.
    AnimateExpressions {
        keyframes: Vec<(f32, HashMap<String, f32>)>,
        duration_seconds: f32,
        looping: bool,
    },
    /// Swap the displayed VRM at runtime (MCP `load_vrm`). `asset_path` is relative to `assets/`
    /// (e.g. `models/foo.vrm`). Respawns the avatar root; idle VRMA follows `[avatar].idle_vrma_path`.
    LoadVrm { asset_path: String },
}

/// Cloneable handle Tokio-side consumers use to enqueue work.
#[derive(Resource, Clone)]
pub struct PoseCommandSender(pub Sender<PoseCommand>);

impl PoseCommandSender {
    pub fn send(&self, cmd: PoseCommand) {
        let _ = self.0.send(cmd);
    }
}

/// Cloneable shared snapshot of every humanoid bone's current rotation
/// (camelCase VRM names → `[x, y, z, w]`).
#[derive(Resource, Clone)]
pub struct BoneSnapshotHandle(pub Arc<RwLock<BoneSnapshot>>);

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BoneSnapshot {
    pub bones: HashMap<String, BoneEntry>,
    /// VRMC_vrm expression preset names on the loaded avatar (sorted), from
    /// `bevy_vrm1::ExpressionEntityMap`. Empty until the rig finishes expression init.
    #[serde(default)]
    pub expression_presets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoneEntry {
    pub rotation: [f32; 4],
}

// ---------- Bevy resources (private) -------------------------------------------

#[derive(Resource)]
struct PoseCommandQueue(Receiver<PoseCommand>);

/// Read-only view of the set of VRM bone names currently known to the pose
/// driver, plus the bone → entity mapping so other plugins (the layered
/// animation controller in particular) can read each bone's
/// [`RestTransform`] / [`Transform`] without duplicating the marker-query
/// macro here. Refreshed whenever [`BoneEntityIndex`] grows. Safe for UI
/// systems to query without touching the internal (private)
/// `BoneEntityIndex`.
#[derive(Resource, Default, Clone)]
pub struct IndexedBones {
    pub names: std::collections::HashSet<String>,
    /// Mirrored copy of the private index's `by_name`. Populated on every
    /// refresh so read-only consumers don't need to import a new type
    /// every time we add a bone.
    pub entities: HashMap<String, Entity>,
}

impl IndexedBones {
    pub fn contains(&self, bone: &str) -> bool {
        self.names.contains(bone)
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
    pub fn entity(&self, bone: &str) -> Option<Entity> {
        self.entities.get(bone).copied()
    }
}

/// VRM humanoid bone name → bone entity (local `Transform.rotation` is driven by MCP).
/// Rebuilt every tick by `sync_bone_entity_index` until every marker from
/// [`VRM_BONE_NAMES`] exists; once we stop seeing new bones appear we consider
/// it settled. `is_ready` used to insist on all 55 bones, but many real VRMs
/// don't ship `jaw`, `toes`, or every optional finger joint — that made the
/// pose driver silently drop every `ApplyBones` command on those rigs.
#[derive(Resource, Default)]
struct BoneEntityIndex {
    by_name: HashMap<String, Entity>,
    /// Skin joints not referenced as humanoid slot entities (e.g. per-toe DEF bones).
    /// Keys are glTF node [`Name`] strings.
    extra_by_name: HashMap<String, Entity>,
    /// Frames since the last time `by_name` grew. Once this passes
    /// `SETTLE_FRAMES` the index is treated as stable.
    frames_since_growth: u32,
}

const SETTLE_FRAMES: u32 = 5;

impl BoneEntityIndex {
    /// All declared bones are present — we can stop re-querying.
    fn is_complete(&self) -> bool {
        self.by_name.len() == VRM_BONE_NAMES.len()
    }

    /// Enough bones are indexed to drive the rig: either the index settled or
    /// we've at least seen the hips (i.e. a VRM has been loaded and initialised).
    fn is_usable(&self) -> bool {
        self.by_name.contains_key("hips")
            && (self.is_complete() || self.frames_since_growth >= SETTLE_FRAMES)
    }
}

// ---------- Macro: one row per VRM humanoid bone -------------------------------

/// `(camelCase VRM name, PascalCase bevy_vrm1 marker, field ident)`
macro_rules! for_each_bone {
    ($mac:ident) => {
        $mac! {
            ("hips", Hips, hips),
            ("spine", Spine, spine),
            ("chest", Chest, chest),
            ("upperChest", UpperChest, upper_chest),
            ("neck", Neck, neck),
            ("head", Head, head),
            ("jaw", Jaw, jaw),
            ("leftEye", LeftEye, left_eye),
            ("rightEye", RightEye, right_eye),
            ("leftShoulder", LeftShoulder, left_shoulder),
            ("leftUpperArm", LeftUpperArm, left_upper_arm),
            ("leftLowerArm", LeftLowerArm, left_lower_arm),
            ("leftHand", LeftHand, left_hand),
            ("rightShoulder", RightShoulder, right_shoulder),
            ("rightUpperArm", RightUpperArm, right_upper_arm),
            ("rightLowerArm", RightLowerArm, right_lower_arm),
            ("rightHand", RightHand, right_hand),
            ("leftUpperLeg", LeftUpperLeg, left_upper_leg),
            ("leftLowerLeg", LeftLowerLeg, left_lower_leg),
            ("leftFoot", LeftFoot, left_foot),
            ("leftToes", LeftToes, left_toes),
            ("rightUpperLeg", RightUpperLeg, right_upper_leg),
            ("rightLowerLeg", RightLowerLeg, right_lower_leg),
            ("rightFoot", RightFoot, right_foot),
            ("rightToes", RightToes, right_toes),
            ("leftThumbMetacarpal", LeftThumbMetacarpal, left_thumb_metacarpal),
            ("leftThumbProximal", LeftThumbProximal, left_thumb_proximal),
            ("leftThumbDistal", LeftThumbDistal, left_thumb_distal),
            ("leftIndexProximal", LeftIndexProximal, left_index_proximal),
            ("leftIndexIntermediate", LeftIndexIntermediate, left_index_intermediate),
            ("leftIndexDistal", LeftIndexDistal, left_index_distal),
            ("leftMiddleProximal", LeftMiddleProximal, left_middle_proximal),
            ("leftMiddleIntermediate", LeftMiddleIntermediate, left_middle_intermediate),
            ("leftMiddleDistal", LeftMiddleDistal, left_middle_distal),
            ("leftRingProximal", LeftRingProximal, left_ring_proximal),
            ("leftRingIntermediate", LeftRingIntermediate, left_ring_intermediate),
            ("leftRingDistal", LeftRingDistal, left_ring_distal),
            ("leftLittleProximal", LeftLittleProximal, left_little_proximal),
            ("leftLittleIntermediate", LeftLittleIntermediate, left_little_intermediate),
            ("leftLittleDistal", LeftLittleDistal, left_little_distal),
            ("rightThumbMetacarpal", RightThumbMetacarpal, right_thumb_metacarpal),
            ("rightThumbProximal", RightThumbProximal, right_thumb_proximal),
            ("rightThumbDistal", RightThumbDistal, right_thumb_distal),
            ("rightIndexProximal", RightIndexProximal, right_index_proximal),
            ("rightIndexIntermediate", RightIndexIntermediate, right_index_intermediate),
            ("rightIndexDistal", RightIndexDistal, right_index_distal),
            ("rightMiddleProximal", RightMiddleProximal, right_middle_proximal),
            ("rightMiddleIntermediate", RightMiddleIntermediate, right_middle_intermediate),
            ("rightMiddleDistal", RightMiddleDistal, right_middle_distal),
            ("rightRingProximal", RightRingProximal, right_ring_proximal),
            ("rightRingIntermediate", RightRingIntermediate, right_ring_intermediate),
            ("rightRingDistal", RightRingDistal, right_ring_distal),
            ("rightLittleProximal", RightLittleProximal, right_little_proximal),
            ("rightLittleIntermediate", RightLittleIntermediate, right_little_intermediate),
            ("rightLittleDistal", RightLittleDistal, right_little_distal),
        }
    };
}

macro_rules! define_vrm_bone_names {
    ( $(($name:literal, $marker:ident, $field:ident)),* $(,)? ) => {
        /// Canonical list of every MCP-visible bone name.
        pub const VRM_BONE_NAMES: &[&str] = &[ $( $name, )* ];
    };
}
for_each_bone!(define_vrm_bone_names);

/// True when `name` is a VRM 1.0 humanoid bone key (camelCase), as opposed to a
/// raw glTF joint name such as ``DEF-toe_big.L``.
#[inline]
pub fn is_vrm_humanoid_bone(name: &str) -> bool {
    VRM_BONE_NAMES.iter().any(|&n| n == name)
}

/// Walk `ChildOf` up from `entity` for up to 64 steps, returning `true` if we
/// encounter any entity in `vrma_roots`. Used to skip the phantom bones that
/// `bevy_vrm1` attaches to every loaded VRMA (same PascalCase marker, same
/// `AnimationTargetId`, but not in any `SkinnedMesh.joints`).
fn descends_from_vrma(
    world: &World,
    entity: Entity,
    vrma_roots: &std::collections::HashSet<Entity>,
) -> bool {
    if vrma_roots.is_empty() {
        return false;
    }
    let mut cur = Some(entity);
    for _ in 0..64 {
        let Some(e) = cur else {
            return false;
        };
        if vrma_roots.contains(&e) {
            return true;
        }
        cur = world.get::<ChildOf>(e).map(|c| c.0);
    }
    false
}

/// `root` first, then every descendant in pre-order (`Children` only).
fn collect_descendants_preorder(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        out.push(e);
        if let Some(children) = world.get::<Children>(e) {
            for child in children.iter().rev() {
                stack.push(child);
            }
        }
    }
    out
}

macro_rules! sync_bone_index_impl {
    ( $(($name:literal, $marker:ident, $_field:ident)),* $(,)? ) => {
        fn sync_humanoid_bone_entities(world: &mut World) {
            // Fast path: once the humanoid map is complete, skip rebuilding it.
            if world
                .get_resource::<BoneEntityIndex>()
                .is_some_and(|i| i.is_complete())
            {
                return;
            }
            // Collect every VRMA root entity so we can exclude their phantom
            // bones. `apply_initialize_humanoid_bones` inserts the PascalCase
            // bone markers (e.g. `LeftHand`) on BOTH the VRM's `手首.L` and
            // the VRMA's `l_hand`, and glTF loading auto-adds
            // `AnimationTargetId` to every node an animation clip touches —
            // so `With<AnimationTargetId>` alone isn't enough to disambiguate.
            // See the diagnostics dump in `dump_diagnostics` — for this rig,
            // `leftHand` resolved to `303v0 "l_hand"` under
            // `40v0 "models/idle_loop.vrma"`, which is the VRMA's phantom
            // bone that doesn't drive any skinned mesh.
            let vrma_roots: std::collections::HashSet<Entity> = {
                let mut q = world.query_filtered::<Entity, With<Vrma>>();
                q.iter(world).collect()
            };
            let mut by_name = HashMap::new();
            $(
                {
                    let mut q = world
                        .query_filtered::<Entity, (With<$marker>, With<AnimationTargetId>)>();
                    let candidates: Vec<Entity> = q.iter(world).collect();
                    for candidate in candidates {
                        if !descends_from_vrma(world, candidate, &vrma_roots) {
                            by_name.insert($name.to_string(), candidate);
                            break;
                        }
                    }
                }
            )*
            let Some(mut index) = world.get_resource_mut::<BoneEntityIndex>() else {
                return;
            };
            let previous_len = index.by_name.len();
            let grew = by_name.len() > previous_len;
            // We must rebuild on *any* entity swap, not just growth: when a
            // VRMA loads after the VRM, the VRMA-phantom bones enter the ECS
            // and our previous indexing may have picked them up before
            // `vrma_roots` was populated. Once it is, we re-resolve to the
            // real VRM bones with the same name count — which is a content
            // swap, not a length change.
            let changed = by_name != index.by_name;
            if changed {
                index.by_name = by_name;
                if grew {
                    index.frames_since_growth = 0;
                }
                info!(
                    target: "pose_driver",
                    "bone index updated: {}/{} humanoid bones (grew={})",
                    index.by_name.len(),
                    VRM_BONE_NAMES.len(),
                    grew,
                );
            } else {
                index.frames_since_growth = index.frames_since_growth.saturating_add(1);
            }
        }
    };
}
for_each_bone!(sync_bone_index_impl);

fn refresh_indexed_bones_merged(world: &mut World) {
    let Some(index) = world.get_resource::<BoneEntityIndex>() else {
        return;
    };
    let mut names: HashSet<String> = index.by_name.keys().cloned().collect();
    let mut entities: HashMap<String, Entity> = index.by_name.clone();
    for (k, v) in &index.extra_by_name {
        names.insert(k.clone());
        entities.insert(k.clone(), *v);
    }
    if let Some(mut ib) = world.get_resource_mut::<IndexedBones>() {
        if ib.names != names || ib.entities != entities {
            ib.names = names;
            ib.entities = entities;
        }
    }
}

/// Index skin joints that are not the entity backing any humanoid slot (extra toes, Rigify
/// `DEF-*` chains, etc.).
///
/// We intentionally **do not** require [`AnimationTargetId`]: Bevy only adds that to nodes
/// referenced by loaded animation clips. Custom humanoid-adjacent joints are often skin-only
/// and never appear in a clip, so they would otherwise be invisible to the pose UI / layers
/// dropdown despite being real `SkinnedMesh` influences.
fn sync_extra_skin_bones(world: &mut World) {
    let humanoid_entities: HashSet<Entity> = world
        .get_resource::<BoneEntityIndex>()
        .map(|i| i.by_name.values().copied().collect())
        .unwrap_or_default();

    let vrma_roots: HashSet<Entity> = {
        let mut q = world.query_filtered::<Entity, With<Vrma>>();
        q.iter(world).collect()
    };

    let mut joint_entities: HashSet<Entity> = HashSet::new();
    let mut q = world.query::<&SkinnedMesh>();
    for skin in q.iter(world) {
        for &j in skin.joints.iter() {
            joint_entities.insert(j);
        }
    }

    let mut extras: HashMap<String, Entity> = HashMap::new();
    for e in joint_entities {
        if descends_from_vrma(world, e, &vrma_roots) {
            continue;
        }
        if humanoid_entities.contains(&e) {
            continue;
        }
        let Some(nm) = world.get::<Name>(e) else {
            continue;
        };
        let key = nm.as_str().to_string();
        if key.is_empty() {
            continue;
        }
        extras.insert(key, e);
    }

    let Some(mut index) = world.get_resource_mut::<BoneEntityIndex>() else {
        return;
    };
    if extras != index.extra_by_name {
        index.extra_by_name = extras;
        info!(
            target: "pose_driver",
            "extra skin bones indexed: {}",
            index.extra_by_name.len(),
        );
    }
}

pub(crate) fn sync_bone_entity_index(world: &mut World) {
    sync_humanoid_bone_entities(world);
    sync_extra_skin_bones(world);
    refresh_indexed_bones_merged(world);
}

/// A command that wants to own the rig's pose this frame — we stop every
/// `AnimationPlayer` so the write actually sticks instead of being overwritten
/// by the next `AnimationSystems` pass.
///
/// `ResetPose` is included deliberately: without stopping the idle VRMA, any
/// bone the clip animates snaps straight back to whatever the clip sampled,
/// and any bone the clip does *not* animate (e.g. toes / fingers on most
/// idle clips) stays at whatever the previous animation left it — so reset
/// appears to "work" for arms/legs but leave fingers/toes stuck. Caller can
/// still restart the VRMA by clicking "Resume idle VRMA".
fn is_manual_pose_cmd(cmd: &PoseCommand) -> bool {
    matches!(
        cmd,
        PoseCommand::ApplyBones { .. }
            | PoseCommand::ApplyExpression { .. }
            | PoseCommand::SetExpression { .. }
            | PoseCommand::ResetPose
            | PoseCommand::ResetBones(..)
            | PoseCommand::AnimateExpressions { .. },
    )
}

/// Walk `ChildOf` parents up the tree for up to `max_depth` steps, returning
/// the chain `[(entity, name)]` starting from `entity` itself. Stops when a
/// step has no parent.
fn ancestor_chain(world: &World, entity: Entity, max_depth: usize) -> Vec<(Entity, String)> {
    let mut out = Vec::new();
    let mut cur = Some(entity);
    for _ in 0..max_depth {
        let Some(e) = cur else { break };
        let name = world
            .get::<Name>(e)
            .map(|n| n.as_str().to_string())
            .unwrap_or_else(|| "<unnamed>".into());
        out.push((e, name));
        cur = world.get::<ChildOf>(e).map(|c| c.0);
    }
    out
}

/// Dump everything we know about the rig to the `pose_driver` log target at
/// INFO level. Safe to call at any time — purely read-only except for logging.
fn dump_diagnostics(world: &mut World) {
    use bevy::mesh::skinning::SkinnedMesh;

    // Clone the map up front so we never hold `&BoneEntityIndex` across
    // `World::query` — that would borrow `world` immutably and mutably at once.
    let bone_map: HashMap<String, Entity> = world
        .get_resource::<BoneEntityIndex>()
        .map(|i| i.by_name.clone())
        .unwrap_or_default();
    if bone_map.is_empty() {
        info!(target: "pose_driver", "dump_diagnostics: BoneEntityIndex empty or missing");
        return;
    }
    let bones: Vec<(String, Entity)> = bone_map.iter().map(|(n, &e)| (n.clone(), e)).collect();
    let total = bones.len();

    // Collect every SkinnedMesh and its joint entities so we can cross-check.
    let (skins, inverse_lookup): (
        Vec<(Entity, Vec<Entity>)>,
        HashMap<Entity, Vec<(usize, Entity)>>,
    ) = {
        let mut q = world.query::<(Entity, &SkinnedMesh)>();
        let mut skins_vec: Vec<(Entity, Vec<Entity>)> = Vec::new();
        let mut lookup: HashMap<Entity, Vec<(usize, Entity)>> = HashMap::new();
        for (skin_entity, skin) in q.iter(world) {
            let joints = skin.joints.clone();
            for (i, j) in joints.iter().enumerate() {
                lookup.entry(*j).or_default().push((i, skin_entity));
            }
            skins_vec.push((skin_entity, joints));
        }
        (skins_vec, lookup)
    };

    info!(
        target: "pose_driver",
        "=== BONE DIAGNOSTICS === indexed={total}/{} skins={}",
        VRM_BONE_NAMES.len(),
        skins.len(),
    );
    for (skin_entity, joints) in &skins {
        info!(
            target: "pose_driver",
            "  skin on {:?}: {} joints",
            skin_entity,
            joints.len()
        );
    }

    // Sort bones for stable output ordering matching VRM_BONE_NAMES.
    let order: HashMap<&str, usize> = VRM_BONE_NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, i))
        .collect();
    let mut bones = bones;
    bones.sort_by_key(|(n, _)| order.get(n.as_str()).copied().unwrap_or(usize::MAX));

    for (bone_name, entity) in &bones {
        let glb_name = world
            .get::<Name>(*entity)
            .map(|n| n.as_str().to_string())
            .unwrap_or_else(|| "<unnamed>".into());
        let parent = world.get::<ChildOf>(*entity).map(|c| c.0);
        let parent_name = parent
            .and_then(|p| world.get::<Name>(p).map(|n| n.as_str().to_string()))
            .unwrap_or_else(|| "<none>".into());
        let children_count = world.get::<Children>(*entity).map(|c| c.len()).unwrap_or(0);
        let local_rot = world
            .get::<Transform>(*entity)
            .map(|tf| tf.rotation)
            .unwrap_or(Quat::IDENTITY);

        let mut skin_hits: Vec<String> = Vec::new();
        if let Some(rows) = inverse_lookup.get(entity) {
            for (joint_idx, skin_entity) in rows {
                skin_hits.push(format!("skin={:?}[{}]", skin_entity, joint_idx));
            }
        }
        let skin_hit_str = if skin_hits.is_empty() {
            "NOT IN ANY SKIN".to_string()
        } else {
            skin_hits.join(", ")
        };

        info!(
            target: "pose_driver",
            "  bone {:<24} → {:?} name={:<24} parent={:?} (name={:<16}) children={} rot=({:.3},{:.3},{:.3},{:.3}) {}",
            bone_name,
            entity,
            format!("{:?}", glb_name),
            parent.unwrap_or(Entity::PLACEHOLDER),
            format!("{:?}", parent_name),
            children_count,
            local_rot.x, local_rot.y, local_rot.z, local_rot.w,
            skin_hit_str,
        );
    }

    // Ancestor chain from leftThumbProximal (a bone the user reports *works*
    // when rotated via the slider). We walk up the tree so we can compare the
    // chain's entities against what our bone index has for leftHand /
    // leftLowerArm / leftUpperArm / leftShoulder / spine / hips. A mismatch
    // means `ChildSearcher::find_from_name` picked a different entity than
    // the real parent chain the skin actually uses.
    let probe_bones: &[&str] = &[
        "leftThumbProximal",
        "leftIndexProximal",
        "leftHand",
        "leftLowerArm",
        "leftUpperArm",
        "leftShoulder",
        "leftUpperLeg",
        "leftLowerLeg",
        "leftFoot",
        "leftToes",
    ];
    for name in probe_bones {
        let Some(&entity) = bone_map.get(*name) else {
            continue;
        };
        let chain = ancestor_chain(world, entity, 24);
        let rendered = chain
            .iter()
            .map(|(e, n)| format!("{:?}[{}]", e, n))
            .collect::<Vec<_>>()
            .join(" → ");
        info!(
            target: "pose_driver",
            "  ancestor chain {:<20} {}",
            name,
            rendered,
        );
    }

    // Cross-check: for each humanoid bone, is it in ANY skin? This is the
    // money shot. Fingers/toes that "work" are in at least one skin; arms
    // that don't move might be absent or shadowed by a helper.
    let mut missing: Vec<&str> = Vec::new();
    for (bone_name, entity) in &bones {
        if !inverse_lookup.contains_key(entity) {
            missing.push(bone_name.as_str());
        }
    }
    if !missing.is_empty() {
        warn!(
            target: "pose_driver",
            "bones with NO skin reference ({}): {:?}",
            missing.len(),
            missing,
        );
    }
    info!(target: "pose_driver", "=== END BONE DIAGNOSTICS ===");
}

/// Per-bone slerp state while a timed transition is in flight.
#[derive(Debug, Clone)]
struct BoneTransition {
    entity: Entity,
    start: Quat,
    target: Quat,
    elapsed: f32,
    duration: f32,
}

/// Active transitions keyed by bone name. `apply_pose_commands` populates /
/// overwrites entries; `tick_active_transitions` advances them.
#[derive(Resource, Default)]
pub struct ActiveTransitions {
    bones: HashMap<String, BoneTransition>,
}

/// In-world playback state for MCP `animate_expressions` (piecewise-linear
/// keyframes). Sampled in [`tick_expression_animation`] after queued
/// [`PoseCommand`]s drain so layered expression writes can run first, then
/// animated channels override for this frame.
#[derive(Resource, Default)]
pub struct ExpressionAnimationPlayback {
    pub(crate) active: Option<RunningExpressionClip>,
}

#[derive(Clone)]
pub(crate) struct RunningExpressionClip {
    pub keyframes: Vec<(f32, HashMap<String, f32>)>,
    pub duration: f32,
    pub looping: bool,
    pub elapsed: f32,
    pub vrm_entity: Entity,
    /// True when this clip was started in a frame where `apply_pose_commands` stopped every
    /// `AnimationPlayer` for manual pose ownership — we should replay idle VRMA when the clip
    /// ends or is cleared (same as Pose Controller "Resume idle VRMA").
    pub resume_idle_vrma_after: bool,
}

/// Clears in-flight expression sampling. When `resume_idle_if_stopped_for_clip` is true and the
/// cleared clip had [`RunningExpressionClip::resume_idle_vrma_after`], replays idle VRMA.
fn clear_expression_animation_playback(world: &mut World, resume_idle_if_stopped_for_clip: bool) {
    let should_resume = resume_idle_if_stopped_for_clip
        && world
            .resource::<ExpressionAnimationPlayback>()
            .active
            .as_ref()
            .is_some_and(|c| c.resume_idle_vrma_after);
    if let Some(mut p) = world.get_resource_mut::<ExpressionAnimationPlayback>() {
        p.active = None;
    }
    if should_resume {
        resume_idle_vrma_if_configured(world);
    }
}

fn sample_expression_keyframes(
    keyframes: &[(f32, HashMap<String, f32>)],
    t: f32,
) -> HashMap<String, f32> {
    if keyframes.is_empty() {
        return HashMap::new();
    }
    if t <= keyframes[0].0 {
        return keyframes[0].1.clone();
    }
    let last = keyframes.last().expect("non-empty");
    if t >= last.0 {
        return last.1.clone();
    }
    let mut lo = 0usize;
    let mut hi = keyframes.len() - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if keyframes[mid].0 <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, w0) = &keyframes[lo];
    let (t1, w1) = &keyframes[lo + 1];
    let span = (*t1 - *t0).max(1e-6);
    let u = ((t - *t0) / span).clamp(0.0, 1.0);
    let mut out = HashMap::new();
    let keys: HashSet<String> = w0.keys().chain(w1.keys()).cloned().collect();
    for k in keys {
        let a = w0.get(&k).copied().unwrap_or(0.0);
        let b = w1.get(&k).copied().unwrap_or(0.0);
        out.insert(k, (a + (b - a) * u).clamp(0.0, 1.0));
    }
    out
}

pub(crate) fn tick_expression_animation(world: &mut World) {
    let Some(mut clip) = world
        .get_resource::<ExpressionAnimationPlayback>()
        .and_then(|p| p.active.clone())
    else {
        return;
    };
    if world.get_entity(clip.vrm_entity).is_err() {
        let resume = clip.resume_idle_vrma_after;
        if let Some(mut p) = world.get_resource_mut::<ExpressionAnimationPlayback>() {
            p.active = None;
        }
        if resume {
            resume_idle_vrma_if_configured(world);
        }
        return;
    }
    let dt = world.resource::<Time>().delta_secs();
    let t_sample = if clip.looping {
        clip.elapsed.rem_euclid(clip.duration.max(1e-4))
    } else {
        clip.elapsed.min(clip.duration)
    };
    let weights = sample_expression_keyframes(&clip.keyframes, t_sample);
    let vrm_e = clip.vrm_entity;
    let pairs: Vec<(VrmExpression, f32)> = weights
        .into_iter()
        .map(|(k, v)| (VrmExpression::from(k.as_str()), v.clamp(0.0, 1.0)))
        .collect();
    if !pairs.is_empty() {
        world
            .commands()
            .trigger(ModifyExpressions::from_iter(vrm_e, pairs));
        // Same ordering contract as `apply_pose_commands`: expression triggers must flush before
        // `bind_expressions` in this PostUpdate pass.
        world.flush();
    }
    let resume_after = clip.resume_idle_vrma_after;
    clip.elapsed += dt;
    let done = !clip.looping && clip.elapsed >= clip.duration;
    if let Some(mut p) = world.get_resource_mut::<ExpressionAnimationPlayback>() {
        if done {
            p.active = None;
        } else {
            p.active = Some(clip);
        }
    }
    if done && resume_after {
        resume_idle_vrma_if_configured(world);
    }
}

/// Replay configured idle VRMA(s) (startup / Pose Controller "Resume idle VRMA" path).
/// Callers gate on [`RunningExpressionClip::resume_idle_vrma_after`] so we do not restart when
/// auto-stop never ran for this clip.
fn resume_idle_vrma_if_configured(world: &mut World) {
    let Some(settings) = world.get_resource::<Settings>() else {
        return;
    };
    if settings.avatar.idle_vrma_path.trim().is_empty() {
        return;
    }
    let vrma_entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Vrma>>()
        .iter(world)
        .collect();
    if vrma_entities.is_empty() {
        return;
    }
    for vrma in vrma_entities {
        world.commands().trigger(PlayVrma {
            vrma,
            repeat: RepeatAnimation::Forever,
            transition_duration: Duration::from_millis(300),
            reset_spring_bones: true,
        });
    }
}

// ---------- Systems ------------------------------------------------------------

/// Map a pose-control quaternion to the bone's raw local rotation.
///
/// All driven bones (humanoid + extra skin joints) use the same **normalized
/// pose space** as three-vrm / Airi: [`local_from_normalized`]. At
/// `pose_q = identity` every bone returns its bind `rest_local`, so MCP
/// `pose_bones` / UI sliders round-trip with [`normalized_from_local`] in
/// [`publish_bone_snapshot`] without a separate “delta quaternion” path for
/// Rigify `DEF-*` joints (which previously broke Euler slider extraction).
fn bone_target_local_rotation(
    world: &World,
    _bone_name: &str,
    entity: Entity,
    pose_q: Quat,
) -> Quat {
    let rest_local = world
        .get::<RestTransform>(entity)
        .map(|rt| rt.0.rotation)
        .unwrap_or(Quat::IDENTITY);
    let rest_world = world
        .get::<RestGlobalTransform>(entity)
        .map(|rgtf| rgtf.0.rotation())
        .unwrap_or(Quat::IDENTITY);
    local_from_normalized(rest_local, rest_world, pose_q)
}

fn execute_avatar_vrm_hot_swap(world: &mut World, asset_path: &str) {
    crate::plugins::look_at::detach_look_at_target_for_vrm_hot_swap(world);

    let roots: Vec<Entity> = world
        .query_filtered::<Entity, With<AvatarVrmRoot>>()
        .iter(world)
        .collect();
    for e in roots {
        let _ = world.despawn(e);
    }

    if let Some(mut settings) = world.get_resource_mut::<Settings>() {
        settings.avatar.model_path = asset_path.to_string();
    }

    let asset_server = world.resource::<AssetServer>().clone();
    let settings = world.resource::<Settings>().clone();
    spawn_avatar_vrm(&mut world.commands(), &asset_server, &settings);
    world.flush();

    world.insert_resource(BoneEntityIndex::default());
    world.insert_resource(IndexedBones::default());
    if let Some(mut at) = world.get_resource_mut::<ActiveTransitions>() {
        at.bones.clear();
    }
    clear_expression_animation_playback(world, false);
    *world.resource::<BoneSnapshotHandle>().0.write() = BoneSnapshot::default();

    info!(target: "pose_driver", "hot-swapped VRM to {asset_path}");
}

pub(crate) fn apply_pose_commands(world: &mut World) {
    let cmds: Vec<PoseCommand> = {
        let queue = world.resource::<PoseCommandQueue>();
        let mut v = Vec::new();
        while let Ok(c) = queue.0.try_recv() {
            v.push(c);
        }
        v
    };
    if cmds.is_empty() {
        return;
    }

    let mut last_swap: Option<String> = None;
    let mut rest: Vec<PoseCommand> = Vec::with_capacity(cmds.len());
    for c in cmds {
        match c {
            PoseCommand::LoadVrm { asset_path } => last_swap = Some(asset_path),
            other => rest.push(other),
        }
    }

    if let Some(ref path) = last_swap {
        execute_avatar_vrm_hot_swap(world, path);
    }

    if rest.is_empty() {
        return;
    }

    let index_ready = world
        .get_resource::<BoneEntityIndex>()
        .is_some_and(|i| i.is_usable());
    let bone_map: HashMap<String, Entity> = if index_ready {
        let i = world.resource::<BoneEntityIndex>();
        let mut m = i.by_name.clone();
        m.extend(i.extra_by_name.iter().map(|(k, v)| (k.clone(), *v)));
        m
    } else {
        // Drop any manual commands that arrive before the rig is indexed so
        // they don't sit in the queue forever and replay a stale pose on the
        // very first frame the rig becomes usable.
        let dropped = rest.len();
        if dropped > 0 {
            debug!(
                target: "pose_driver",
                "bone index not usable yet, dropping {dropped} command(s)"
            );
        }
        HashMap::new()
    };

    let vrm_entity = {
        let mut q = world.query_filtered::<Entity, With<Vrm>>();
        q.iter(world).next()
    };

    // Snapshot config so we can close over a `Settings` read without holding
    // the borrow across `world.get_mut::<Transform>` below.
    let (blends_enabled, default_blend, default_transition, auto_stop_vrma, idle_vrma_configured) =
        world
            .get_resource::<Settings>()
            .map(|s| {
                (
                    s.pose_controller.blend_transitions_enabled,
                    s.pose_controller.default_blend_weight,
                    s.pose_controller.default_transition_seconds,
                    s.pose_controller.auto_stop_idle_vrma,
                    !s.avatar.idle_vrma_path.trim().is_empty(),
                )
            })
            .unwrap_or((false, 1.0, 0.0, true, false));

    // A manual pose / expression / animation frame landed. The idle VRMA is
    // still running its `AnimationPlayer` and will overwrite whatever we write
    // to the bone transforms on the next `AnimationSystems` pass. Stop every
    // `AnimationPlayer` in the world so our writes actually stick. We do this
    // directly rather than via `bevy_vrm1::StopVrma` because the library's
    // `apply_stop_vrma` observer only walks children of the VRMA entity — but
    // the actual `AnimationPlayer` lives on the rig root bone (sibling, child
    // of the VRM parent), so `StopVrma` never reaches it. See
    // `bevy_vrm1-0.7.0/src/vrma/animation/play.rs` `stop_animations`.
    //
    // In this app, only VRM/VRMA playback uses `AnimationPlayer`, so stopping
    // every player is safe. If that ever changes, switch to walking from each
    // `Vrm` entity downward and only stopping its own rig's players.
    if auto_stop_vrma && rest.iter().any(is_manual_pose_cmd) {
        let mut q = world.query::<&mut AnimationPlayer>();
        for mut player in q.iter_mut(world) {
            player.stop_all();
        }
    }

    for cmd in rest {
        match cmd {
            PoseCommand::ApplyBones {
                bones,
                preserve_omitted_bones,
                blend_weight,
                transition_seconds,
            } => {
                if !index_ready {
                    continue;
                }
                let weight = blend_weight.unwrap_or(default_blend).clamp(0.0, 1.0);
                let transition = transition_seconds.unwrap_or(default_transition).max(0.0);
                let blend_path = blends_enabled && (transition > 0.0 || weight < 1.0);

                if !preserve_omitted_bones {
                    for (name, &e) in &bone_map {
                        let target =
                            bone_target_local_rotation(world, name.as_str(), e, Quat::IDENTITY);
                        if let Some(mut tf) = world.get_mut::<Transform>(e) {
                            tf.rotation = target;
                        }
                    }
                    // Resetting omitted bones also clears any in-flight transitions for them.
                    if let Some(mut act) = world.get_resource_mut::<ActiveTransitions>() {
                        act.bones.retain(|n, _| bones.contains_key(n));
                    }
                }

                let requested = bones.len();
                let matched = bones
                    .keys()
                    .filter(|n| bone_map.contains_key(n.as_str()))
                    .count();
                if requested != matched {
                    let missing: Vec<&str> = bones
                        .keys()
                        .filter(|n| !bone_map.contains_key(n.as_str()))
                        .map(String::as_str)
                        .collect();
                    warn!(
                        target: "pose_driver",
                        "ApplyBones: requested={requested} matched={matched} missing={:?}",
                        missing,
                    );
                } else {
                    debug!(
                        target: "pose_driver",
                        "ApplyBones: requested={requested} matched={matched} blend_path={blend_path}",
                    );
                }
                if blend_path {
                    let mut new_transitions: Vec<(String, BoneTransition)> = Vec::new();
                    for (name, q_arr) in bones {
                        let Some(&e) = bone_map.get(&name) else {
                            continue;
                        };
                        let Some(tf) = world.get::<Transform>(e) else {
                            continue;
                        };
                        let current = tf.rotation;
                        let pose_q =
                            Quat::from_xyzw(q_arr[0], q_arr[1], q_arr[2], q_arr[3]).normalize();
                        let target_raw =
                            bone_target_local_rotation(world, name.as_str(), e, pose_q);
                        // Partial weight = slerp toward the target by `weight` up front.
                        let target = if weight >= 1.0 {
                            target_raw
                        } else {
                            current.slerp(target_raw, weight)
                        };
                        if transition == 0.0 {
                            if let Some(mut tf) = world.get_mut::<Transform>(e) {
                                tf.rotation = target;
                            }
                        } else {
                            new_transitions.push((
                                name,
                                BoneTransition {
                                    entity: e,
                                    start: current,
                                    target,
                                    elapsed: 0.0,
                                    duration: transition,
                                },
                            ));
                        }
                    }
                    if let Some(mut active) = world.get_resource_mut::<ActiveTransitions>() {
                        for (name, tr) in new_transitions {
                            active.bones.insert(name, tr);
                        }
                    }
                } else {
                    // Instant path (historical behaviour).
                    let mut written = 0usize;
                    for (name, q_arr) in bones {
                        let Some(&e) = bone_map.get(&name) else {
                            continue;
                        };
                        let pose_q =
                            Quat::from_xyzw(q_arr[0], q_arr[1], q_arr[2], q_arr[3]).normalize();
                        let final_q = bone_target_local_rotation(world, name.as_str(), e, pose_q);
                        if let Some(mut tf) = world.get_mut::<Transform>(e) {
                            tf.rotation = final_q;
                            written += 1;
                        }
                        if let Some(mut active) = world.get_resource_mut::<ActiveTransitions>() {
                            active.bones.remove(&name);
                        }
                    }
                    debug!(
                        target: "pose_driver",
                        "ApplyBones (instant): wrote {written} bone rotations"
                    );
                }
            }
            PoseCommand::ApplyExpression {
                weights,
                cancel_expression_animation,
            } => {
                if cancel_expression_animation {
                    clear_expression_animation_playback(world, true);
                }
                let Some(e) = vrm_entity else {
                    continue;
                };
                let pairs: Vec<(VrmExpression, f32)> = weights
                    .into_iter()
                    .map(|(k, v)| (VrmExpression::from(k.as_str()), v))
                    .collect();
                world
                    .commands()
                    .trigger(ModifyExpressions::from_iter(e, pairs));
            }
            PoseCommand::SetExpression { weights } => {
                clear_expression_animation_playback(world, true);
                let Some(e) = vrm_entity else {
                    continue;
                };
                let pairs: Vec<(VrmExpression, f32)> = weights
                    .into_iter()
                    .map(|(k, v)| (VrmExpression::from(k.as_str()), v))
                    .collect();
                world
                    .commands()
                    .trigger(SetExpressions::from_iter(e, pairs));
            }
            PoseCommand::DumpDiagnostics => {
                dump_diagnostics(world);
            }
            PoseCommand::ResetBones(names) => {
                if !index_ready {
                    continue;
                }
                for name in &names {
                    let Some(&e) = bone_map.get(name) else {
                        continue;
                    };
                    // Match `ResetPose` semantics for this joint: restore the **full** bind
                    // `Transform` from `RestTransform` (rotation + translation + scale). Toes
                    // and other Rigify `DEF-*` extras often carry non-zero bind translation; only
                    // overwriting rotation while translation stayed edited left the toe mesh
                    // translated / flipped when sliders were zeroed.
                    if let Some(bind) = world.get::<RestTransform>(e).map(|rt| rt.0) {
                        if let Some(mut tf) = world.get_mut::<Transform>(e) {
                            *tf = bind;
                        }
                    } else {
                        let target =
                            bone_target_local_rotation(world, name.as_str(), e, Quat::IDENTITY);
                        if let Some(mut tf) = world.get_mut::<Transform>(e) {
                            tf.rotation = target;
                        }
                    }
                    if let Some(mut active) = world.get_resource_mut::<ActiveTransitions>() {
                        active.bones.remove(name);
                    }
                }
            }
            PoseCommand::AnimateExpressions {
                keyframes,
                duration_seconds,
                looping,
            } => {
                let Some(e) = vrm_entity else {
                    continue;
                };
                if keyframes.is_empty() {
                    clear_expression_animation_playback(world, true);
                    continue;
                }
                let duration = duration_seconds.max(1e-3);
                let resume_idle_vrma_after = auto_stop_vrma && idle_vrma_configured;
                if let Some(mut p) = world.get_resource_mut::<ExpressionAnimationPlayback>() {
                    p.active = Some(RunningExpressionClip {
                        keyframes,
                        duration,
                        looping,
                        elapsed: 0.0,
                        vrm_entity: e,
                        resume_idle_vrma_after,
                    });
                }
            }
            PoseCommand::ResetPose => {
                clear_expression_animation_playback(world, true);
                if index_ready {
                    let vrma_roots: HashSet<Entity> = world
                        .query_filtered::<Entity, With<Vrma>>()
                        .iter(world)
                        .collect();
                    // Restore **full** bind locals from `RestTransform` (translation,
                    // rotation, scale) — not rotation-only on a subset of bones.
                    // `bevy_vrm1` roll / rotation node constraints spread twist
                    // across extra joints; if those intermediates still carry VRMA
                    // deltas while humanoid nodes snap to bind, skin can collapse to
                    // a pinched ribbon at shoulder / elbow / wrist. Walking the whole
                    // `Vrm` subtree keeps twist chains coherent. Skip entities under
                    // loaded VRMA files (`Vrma`) and joints with no bind snapshot.
                    if let Some(vrm_e) = vrm_entity {
                        for e in collect_descendants_preorder(world, vrm_e) {
                            if descends_from_vrma(world, e, &vrma_roots) {
                                continue;
                            }
                            let Some(bind) = world.get::<RestTransform>(e).map(|rt| rt.0) else {
                                continue;
                            };
                            if let Some(mut tf) = world.get_mut::<Transform>(e) {
                                *tf = bind;
                            }
                        }
                    }
                    // Rare: indexed skin extras not under the `Vrm` entity — still
                    // snap full bind when `RestTransform` exists.
                    for &e in bone_map.values() {
                        if descends_from_vrma(world, e, &vrma_roots) {
                            continue;
                        }
                        let Some(bind) = world.get::<RestTransform>(e).map(|rt| rt.0) else {
                            continue;
                        };
                        if let Some(mut tf) = world.get_mut::<Transform>(e) {
                            *tf = bind;
                        }
                    }
                }
                if let Some(mut active) = world.get_resource_mut::<ActiveTransitions>() {
                    active.bones.clear();
                }
                if let Some(e) = vrm_entity {
                    let empty: Vec<(VrmExpression, f32)> = Vec::new();
                    world
                        .commands()
                        .trigger(SetExpressions::from_iter(e, empty));
                }
            }
            PoseCommand::LoadVrm { .. } => {
                // Handled earlier in this frame when draining the queue (before `rest`).
            }
        }
    }
    // `Commands::trigger(SetExpressions|ModifyExpressions|…)` only queues work until the world
    // command queue flushes. `bevy_vrm1::bind_expressions` runs later in this same PostUpdate
    // (`VrmSystemSets::Expressions`); without an explicit flush here, morph weights are sampled
    // **before** `ExpressionOverride` inserts land — manual expression UI / MCP appear inert.
    world.flush();
}

fn tick_active_transitions(
    time: Res<Time>,
    mut active: ResMut<ActiveTransitions>,
    mut bones_q: Query<&mut Transform>,
) {
    if active.bones.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    let mut done: Vec<String> = Vec::new();
    for (name, tr) in active.bones.iter_mut() {
        tr.elapsed += dt;
        let t = if tr.duration <= f32::EPSILON {
            1.0
        } else {
            (tr.elapsed / tr.duration).clamp(0.0, 1.0)
        };
        // smoothstep gives a nicer ease-in/out without needing a full curve crate.
        let eased = t * t * (3.0 - 2.0 * t);
        if let Ok(mut tf) = bones_q.get_mut(tr.entity) {
            tf.rotation = tr.start.slerp(tr.target, eased);
        }
        if t >= 1.0 {
            done.push(name.clone());
        }
    }
    for name in done {
        active.bones.remove(&name);
    }
}

/// VRMC_vrm expression preset names (preset + custom) once `ExpressionEntityMap` exists.
fn collect_expression_preset_names(world: &mut World) -> Vec<String> {
    let mut q = world.query_filtered::<&ExpressionEntityMap, With<Vrm>>();
    let mut names: Vec<String> = q
        .iter(world)
        .flat_map(|map| map.keys().map(|k| k.0.clone()))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn publish_bone_snapshot(world: &mut World) {
    let handle = world.resource::<BoneSnapshotHandle>().clone();
    let preset_names = collect_expression_preset_names(world);
    {
        let mut w = handle.0.write();
        w.expression_presets.clone_from(&preset_names);
    }

    let Some(index) = world.get_resource::<BoneEntityIndex>() else {
        return;
    };
    if !index.is_usable() {
        return;
    }
    let mut snap = BoneSnapshot::default();
    snap.expression_presets = preset_names;
    // Humanoid keys: normalized-humanoid space (Airi / pose-controller).
    for (name, &e) in &index.by_name {
        let Some(tf) = world.get::<Transform>(e) else {
            continue;
        };
        let rest_local = world
            .get::<RestTransform>(e)
            .map(|rt| rt.0.rotation)
            .unwrap_or(Quat::IDENTITY);
        let rest_world = world
            .get::<RestGlobalTransform>(e)
            .map(|rgtf| rgtf.0.rotation())
            .unwrap_or(Quat::IDENTITY);
        let normalized = normalized_from_local(rest_local, rest_world, tf.rotation);
        snap.bones.insert(
            name.clone(),
            BoneEntry {
                rotation: [normalized.x, normalized.y, normalized.z, normalized.w],
            },
        );
    }
    // Extra skin joints: same normalized pose space as humanoid keys so MCP
    // `pose_bones` / `get_current_bone_state` / Pose Controller sliders agree.
    for (name, &e) in &index.extra_by_name {
        let Some(tf) = world.get::<Transform>(e) else {
            continue;
        };
        let rest_local = world
            .get::<RestTransform>(e)
            .map(|rt| rt.0.rotation)
            .unwrap_or(Quat::IDENTITY);
        let rest_world = world
            .get::<RestGlobalTransform>(e)
            .map(|rgtf| rgtf.0.rotation())
            .unwrap_or(Quat::IDENTITY);
        let normalized = normalized_from_local(rest_local, rest_world, tf.rotation);
        snap.bones.insert(
            name.clone(),
            BoneEntry {
                rotation: [normalized.x, normalized.y, normalized.z, normalized.w],
            },
        );
    }

    *handle.0.write() = snap;
}
