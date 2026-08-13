//! Bilateral mirroring helper for the rig editor / Pose Controller.
//!
//! The rig editor and Bones tab both write **normalized humanoid pose
//! quaternions** — i.e. the same space three-vrm exposes via
//! `getNormalizedBoneNode(name).quaternion`. In that space the rest pose is
//! identity, and the model's sagittal plane is the world `YZ` plane (X is
//! "left/right" of the rig). Mirroring a rotation across that plane is the
//! standard trick of negating the components that swap sign under the
//! reflection — for a quaternion that's `(qx, qy, qz, qw) → (qx, -qy, -qz, qw)`
//! when the bone itself stays on the same side, but here we **pair**
//! left↔right bones, so the bone identity also flips. The combined "mirror to
//! the other side" map for a paired bone is:
//!
//! ```text
//! (qx, qy, qz, qw)  →  (qx, -qy, -qz, qw)
//! ```
//!
//! That comes from reflecting the rotation axis across the YZ plane while
//! leaving the angle the same — see Sebastian Lague-style derivation: a
//! quaternion `(v, w)` represents a rotation by `2·atan2(|v|, w)` around `v`;
//! reflecting `v` by negating its Y and Z components leaves the angle
//! unchanged but moves the rotation onto the mirrored bone. Center-line bones
//! (hips, spine, chest, head, …) use the same formula with no name swap —
//! they're their own pair under reflection.
//!
//! This module is **side-effect free**: it produces a `HashMap` of mirrored
//! bones and the caller (rig editor, Bones tab, MCP `pose_bones` mirror flag,
//! etc.) routes it through the existing `PoseCommand::ApplyBones` path so the
//! mirror lands on the same write pipeline real edits use.

use std::collections::HashMap;

use bevy::math::Quat;
use bevy::prelude::Resource;

use crate::plugins::pose_driver::{VRM_BONE_NAMES, is_vrm_humanoid_bone};

/// Realtime mirror toggle + per-VRM pair cache. Living as a plain Bevy
/// `Resource` keeps it cheap to read from any Pose Controller subsystem.
#[derive(Resource, Default)]
pub struct MirrorState {
    /// Master toggle — when on, every single-bone write the user makes also
    /// queues the mirrored counterpart (when one exists). The `apply` helper
    /// at [`MirrorState::expand`] is the single entry point — call it on
    /// every bone-list / rig-drag write path.
    pub realtime: bool,
    /// Last paired-bone resolution attempt — exposed for debug UI / hover
    /// hints so the user can see which side a write will mirror to.
    pub last_pair_status: Option<String>,
}

/// Result of a mirror lookup: which bone(s) get the rotation. `Same` covers
/// center-line bones (hips, spine, chest, …) where mirroring is in-place; in
/// realtime mode we still apply the mirrored rotation so the visible behavior
/// is symmetric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorPair {
    /// Bone has no left/right counterpart on this rig (center-line); the
    /// mirrored quaternion is applied to *the same bone*.
    Same(String),
    /// Bone has a sibling on the other side; the mirrored quaternion is
    /// applied to `partner`.
    Partner { partner: String },
    /// Bone name doesn't fit any known mirror pattern (extras, rare DEF-* not
    /// matching the standard `.L`/`.R` suffix). Realtime mirror skips it.
    None,
}

/// Resolve the mirror partner of a bone by name.
///
/// Order of patterns checked (first match wins):
///
/// 1. **VRM humanoid camelCase**: `leftFoo` ↔ `rightFoo` (both directions).
/// 2. **Blender / Rigify suffix**: `Foo.L` ↔ `Foo.R`.
/// 3. **Underscore suffix**: `Foo_L` ↔ `Foo_R`.
/// 4. **Period-Camel**: `leftFoo.001` ↔ `rightFoo.001` (preserves the suffix).
/// 5. Otherwise classified as center-line (no partner) — `Same` for known
///    humanoid bones, `None` otherwise.
pub fn resolve_pair(bone: &str) -> MirrorPair {
    if let Some(rest) = bone.strip_prefix("left") {
        let candidate = format!("right{rest}");
        if is_vrm_humanoid_bone(&candidate) || ends_with_paired_suffix(&candidate) {
            return MirrorPair::Partner { partner: candidate };
        }
    }
    if let Some(rest) = bone.strip_prefix("right") {
        let candidate = format!("left{rest}");
        if is_vrm_humanoid_bone(&candidate) || ends_with_paired_suffix(&candidate) {
            return MirrorPair::Partner { partner: candidate };
        }
    }
    if let Some(stem) = bone.strip_suffix(".L") {
        return MirrorPair::Partner {
            partner: format!("{stem}.R"),
        };
    }
    if let Some(stem) = bone.strip_suffix(".R") {
        return MirrorPair::Partner {
            partner: format!("{stem}.L"),
        };
    }
    if let Some(stem) = bone.strip_suffix("_L") {
        return MirrorPair::Partner {
            partner: format!("{stem}_R"),
        };
    }
    if let Some(stem) = bone.strip_suffix("_R") {
        return MirrorPair::Partner {
            partner: format!("{stem}_L"),
        };
    }
    if is_vrm_humanoid_bone(bone) {
        return MirrorPair::Same(bone.to_string());
    }
    MirrorPair::None
}

/// True when `name` exists as a known VRM humanoid bone, OR ends in any of
/// the side-suffix patterns we treat as paired skin bones (DEF-*).
fn ends_with_paired_suffix(name: &str) -> bool {
    name.ends_with(".L") || name.ends_with(".R") || name.ends_with("_L") || name.ends_with("_R")
}

/// Mirror a normalized-humanoid pose quaternion across the rig's sagittal
/// plane. See module docs for derivation.
#[inline]
pub fn mirror_quat(q: Quat) -> Quat {
    Quat::from_xyzw(q.x, -q.y, -q.z, q.w).normalize()
}

/// Mirror an Euler XYZ degree triple. Faster than going through quaternions
/// when the caller already has Euler degrees (Bones tab, rig editor sliders),
/// and matches the quaternion mirror exactly because intrinsic XYZ Euler
/// inverts on Y and Z under the same reflection.
#[inline]
pub fn mirror_euler_deg(deg: [f32; 3]) -> [f32; 3] {
    [deg[0], -deg[1], -deg[2]]
}

impl MirrorState {
    /// Build a `(bone_name, [x,y,z,w])` map for the user's primary write +
    /// (when [`Self::realtime`] is on) the mirrored counterpart. The caller
    /// routes the result through `PoseCommand::ApplyBones` so mirrored writes
    /// share the exact same retarget / DEF-toe / animation-layer pipeline as
    /// real edits.
    ///
    /// Does **not** mutate `self` — the caller updates `last_pair_status`
    /// from the returned [`PairOutcome`] so debug UI can show the resolved
    /// partner without a `&mut MirrorState` borrow.
    pub fn expand(&self, primary_bone: &str, primary_q: [f32; 4]) -> ExpandResult {
        let mut bones: HashMap<String, [f32; 4]> = HashMap::new();
        bones.insert(primary_bone.to_string(), primary_q);
        let q_primary = Quat::from_xyzw(primary_q[0], primary_q[1], primary_q[2], primary_q[3]);
        let pair = resolve_pair(primary_bone);
        let outcome = match (self.realtime, pair) {
            (false, p) => PairOutcome::SkippedDisabled { pair: p },
            (true, MirrorPair::Same(_)) => PairOutcome::CenterLine,
            (true, MirrorPair::Partner { partner }) => {
                let mirrored = mirror_quat(q_primary);
                bones.insert(
                    partner.clone(),
                    [mirrored.x, mirrored.y, mirrored.z, mirrored.w],
                );
                PairOutcome::Mirrored { partner }
            }
            (true, MirrorPair::None) => PairOutcome::NoPattern,
        };
        ExpandResult { bones, outcome }
    }

    /// Build a "mirror only" map — primary not included. Used by per-bone
    /// "Mirror selected to other side" actions where the user wants to
    /// snapshot the current rotation onto the partner without re-applying
    /// the same value to themselves.
    pub fn one_shot_partner(
        primary_bone: &str,
        primary_q: [f32; 4],
    ) -> Option<(String, [f32; 4])> {
        let q = Quat::from_xyzw(primary_q[0], primary_q[1], primary_q[2], primary_q[3]);
        match resolve_pair(primary_bone) {
            MirrorPair::Partner { partner } => {
                let m = mirror_quat(q);
                Some((partner, [m.x, m.y, m.z, m.w]))
            }
            MirrorPair::Same(_) | MirrorPair::None => None,
        }
    }
}

/// Outcome of a single `expand` call — surfaced so the Pose Controller can
/// show "mirrored to leftFoo" hints in the status bar and (when no pattern
/// matched) explain why nothing else moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairOutcome {
    SkippedDisabled { pair: MirrorPair },
    CenterLine,
    Mirrored { partner: String },
    NoPattern,
}

/// Combined result of [`MirrorState::expand`]: the bones map ready to feed
/// into `PoseCommand::ApplyBones`, plus a [`PairOutcome`] for status text.
pub struct ExpandResult {
    pub bones: HashMap<String, [f32; 4]>,
    pub outcome: PairOutcome,
}

/// Predefined chain selections — matches the user's mental model in the plan:
/// per-bone, per-arm, per-leg, per-side, full body. Returned as ordered
/// slices of canonical VRM bone names; non-humanoid extras are matched on
/// demand by the caller.
pub fn chain_bones(chain: MirrorChain) -> Vec<&'static str> {
    match chain {
        MirrorChain::LeftArm => left_arm_chain(),
        MirrorChain::RightArm => right_arm_chain(),
        MirrorChain::LeftLeg => vec![
            "leftUpperLeg",
            "leftLowerLeg",
            "leftFoot",
            "leftToes",
        ],
        MirrorChain::RightLeg => vec![
            "rightUpperLeg",
            "rightLowerLeg",
            "rightFoot",
            "rightToes",
        ],
        MirrorChain::LeftHand => left_hand_fingers(),
        MirrorChain::RightHand => right_hand_fingers(),
        MirrorChain::LeftSide => {
            let mut v = vec![];
            v.extend(chain_bones(MirrorChain::LeftArm));
            v.extend(chain_bones(MirrorChain::LeftLeg));
            v.extend(chain_bones(MirrorChain::LeftHand));
            v
        }
        MirrorChain::RightSide => {
            let mut v = vec![];
            v.extend(chain_bones(MirrorChain::RightArm));
            v.extend(chain_bones(MirrorChain::RightLeg));
            v.extend(chain_bones(MirrorChain::RightHand));
            v
        }
        MirrorChain::AllPaired => VRM_BONE_NAMES
            .iter()
            .copied()
            .filter(|n| matches!(resolve_pair(n), MirrorPair::Partner { .. }))
            .filter(|n| n.starts_with("left"))
            .collect(),
    }
}

fn left_arm_chain() -> Vec<&'static str> {
    let mut v = vec![
        "leftShoulder",
        "leftUpperArm",
        "leftLowerArm",
        "leftHand",
    ];
    v.extend(left_hand_fingers());
    v
}

fn right_arm_chain() -> Vec<&'static str> {
    let mut v = vec![
        "rightShoulder",
        "rightUpperArm",
        "rightLowerArm",
        "rightHand",
    ];
    v.extend(right_hand_fingers());
    v
}

fn left_hand_fingers() -> Vec<&'static str> {
    VRM_BONE_NAMES
        .iter()
        .copied()
        .filter(|n| {
            n.starts_with("left")
                && (n.contains("Thumb")
                    || n.contains("Index")
                    || n.contains("Middle")
                    || n.contains("Ring")
                    || n.contains("Little"))
        })
        .collect()
}

fn right_hand_fingers() -> Vec<&'static str> {
    VRM_BONE_NAMES
        .iter()
        .copied()
        .filter(|n| {
            n.starts_with("right")
                && (n.contains("Thumb")
                    || n.contains("Index")
                    || n.contains("Middle")
                    || n.contains("Ring")
                    || n.contains("Little"))
        })
        .collect()
}

/// Predefined chains exposed in the Pose Controller's mirror dropdown.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum MirrorChain {
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
    LeftHand,
    RightHand,
    /// All left-side humanoid bones (arm + leg + fingers).
    LeftSide,
    /// All right-side humanoid bones.
    RightSide,
    /// Every paired humanoid bone (left → right). Use the "copy and mirror"
    /// per-bone helper to snapshot a whole pose to the other side.
    AllPaired,
}

impl MirrorChain {
    pub fn label(self) -> String {
        self.menu_label()
    }

    pub fn menu_label(self) -> String {
        let arrow = crate::icons::ARROW_RIGHT;
        match self {
            MirrorChain::LeftArm => format!("Left arm {arrow} Right"),
            MirrorChain::RightArm => format!("Right arm {arrow} Left"),
            MirrorChain::LeftLeg => format!("Left leg {arrow} Right"),
            MirrorChain::RightLeg => format!("Right leg {arrow} Left"),
            MirrorChain::LeftHand => format!("Left hand {arrow} Right"),
            MirrorChain::RightHand => format!("Right hand {arrow} Left"),
            MirrorChain::LeftSide => format!("Left side {arrow} Right"),
            MirrorChain::RightSide => format!("Right side {arrow} Left"),
            MirrorChain::AllPaired => format!("All paired (L {arrow} R)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanoid_pairing_round_trip() {
        let pair = resolve_pair("leftUpperArm");
        assert!(matches!(pair, MirrorPair::Partner { partner } if partner == "rightUpperArm"));
        let pair = resolve_pair("rightLowerLeg");
        assert!(matches!(pair, MirrorPair::Partner { partner } if partner == "leftLowerLeg"));
    }

    #[test]
    fn center_line_self_pair() {
        assert!(matches!(resolve_pair("hips"), MirrorPair::Same(_)));
        assert!(matches!(resolve_pair("spine"), MirrorPair::Same(_)));
        assert!(matches!(resolve_pair("head"), MirrorPair::Same(_)));
    }

    #[test]
    fn def_suffix_pairing() {
        let pair = resolve_pair("DEF-toe_big.L");
        assert!(matches!(pair, MirrorPair::Partner { partner } if partner == "DEF-toe_big.R"));
        let pair = resolve_pair("DEF-upper_arm_R");
        assert!(matches!(pair, MirrorPair::Partner { partner } if partner == "DEF-upper_arm_L"));
    }

    #[test]
    fn mirror_identity_is_identity() {
        let q = Quat::IDENTITY;
        let m = mirror_quat(q);
        assert!((m.x - 0.0).abs() < 1e-6);
        assert!((m.y - 0.0).abs() < 1e-6);
        assert!((m.z - 0.0).abs() < 1e-6);
        assert!((m.w - 1.0).abs() < 1e-6);
    }
}
