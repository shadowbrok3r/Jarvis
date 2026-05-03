//! MCP-side helpers for safe pose authoring: quaternion normalization / clamping,
//! Euler-degrees → normalized pose quaternions, and canned hand shapes.

use std::collections::HashMap;

use bevy::prelude::{EulerRot, Quat};
use rmcp::schemars::JsonSchema;
use serde::Deserialize;

use crate::plugins::pose_driver::def_toe_big_yaw_slider_extra_deg;

const EPS_LEN: f32 = 1e-6;

/// Rigify-style skin joints addressed by glTF node name (`DEF-toe_big.L`, `DEF-ero*`, …).
#[inline]
pub fn is_def_skin_extra_bone(bone: &str) -> bool {
    let n = bone.to_ascii_lowercase();
    n.starts_with("def-toe") || n.starts_with("def-ero")
}

/// `DEF-toe*` only (case-insensitive prefix). Used for quaternion xyz caps: these joints
/// combine wide `euler_limit_deg` with per-digit yaw extras (±180° on named toes), so
/// legitimate unit quaternions often have |x|/|y|/|z| ≫ 0.32; a tight cap corrupts bind.
#[inline]
fn is_def_toe_bone(bone: &str) -> bool {
    bone.as_bytes()
        .get(..7)
        .is_some_and(|h| h.eq_ignore_ascii_case(b"def-toe"))
}

fn default_true() -> bool {
    true
}

/// Max absolute value allowed for any quaternion **x / y / z** component after
/// normalization, per bone class. `w` is re-derived for a unit quaternion.
#[inline]
pub fn max_xyz_component_for_bone(bone: &str) -> f32 {
    if is_thumb_bone(bone) {
        0.75
    } else if is_def_toe_bone(bone) {
        // Before `is_finger_bone`: names like `DEF-toe_index.*` contain "Index" and must
        // not inherit the finger xyz cap (same class of bug as pre-1.0 `DEF-toe_big.*`).
        1.0
    } else if is_finger_bone(bone) {
        0.72
    } else if bone == "hips" {
        // Deep sit / fold needs larger xyz before sanitize scales toward identity.
        0.72
    } else if bone.ends_with("UpperLeg") || bone.ends_with("LowerLeg") {
        0.68
    } else if bone.ends_with("UpperArm") || bone.ends_with("LowerArm") {
        0.68
    } else if bone.ends_with("Foot") {
        0.65
    } else if bone.ends_with("Hand") || bone.ends_with("Toes") {
        0.28
    } else if is_def_skin_extra_bone(bone) {
        0.32
    } else if bone.contains("Shoulder") {
        0.12
    } else {
        0.30
    }
}

#[inline]
fn is_thumb_bone(bone: &str) -> bool {
    bone.contains("Thumb")
}

#[inline]
fn is_finger_bone(bone: &str) -> bool {
    // Rigify skin toes use `DEF-toe_index.*`, `DEF-toe_middle.*`, etc. — substring
    // "Index"/"Middle"/… must not classify them as VRM finger metacarpals.
    if is_def_toe_bone(bone) {
        return false;
    }
    bone.contains("Index")
        || bone.contains("Middle")
        || bone.contains("Ring")
        || bone.contains("Little")
}

/// Return `(max_abs_pitch_deg, max_abs_yaw_deg, max_abs_roll_deg)` for `pose_bones`.
pub fn euler_limit_deg(bone: &str) -> (f32, f32, f32) {
    if is_def_skin_extra_bone(bone) {
        return (44.0, 36.0, 36.0);
    }
    if is_thumb_bone(bone) {
        // Opposition lives mostly on "yaw" (Y) in intrinsic XYZ for thumbs.
        return (40.0, 72.0, 40.0);
    }
    if is_finger_bone(bone) {
        // Curl ≈ roll (Z); keep pitch/yaw small so the AI cannot corkscrew fingers.
        return (18.0, 18.0, 86.0);
    }
    match bone {
        "hips" => (88.0, 62.0, 60.0),
        "neck" | "head" => (48.0, 58.0, 44.0),
        n if n == "spine" || n.contains("Spine") || n == "chest" || n == "upperChest" => {
            (58.0, 48.0, 44.0)
        }
        n if n.ends_with("UpperArm") => (90.0, 80.0, 62.0),
        n if n.ends_with("LowerArm") => (110.0, 58.0, 55.0),
        n if n.ends_with("Hand") => (48.0, 48.0, 48.0),
        n if n.ends_with("UpperLeg") => (78.0, 58.0, 55.0),
        n if n.ends_with("LowerLeg") => (125.0, 48.0, 48.0),
        n if n.ends_with("Foot") => (58.0, 48.0, 48.0),
        n if n.ends_with("Toes") => (42.0, 28.0, 28.0),
        _ => (48.0, 48.0, 48.0),
    }
}

/// Normalize, then clamp xyz magnitude by bone-safe cap; fix `w` for unit length.
pub fn sanitize_quat(bone: &str, mut q: [f32; 4]) -> ([f32; 4], Vec<String>) {
    let mut warnings = Vec::new();

    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if !len.is_finite() || len < EPS_LEN {
        warnings.push(format!(
            "{bone}: quaternion length invalid ({len:.4}), replaced with identity"
        ));
        return ([0.0, 0.0, 0.0, 1.0], warnings);
    }

    if (len - 1.0).abs() > 0.001 {
        warnings.push(format!(
            "{bone}: non-unit quaternion (length {len:.4}), normalized"
        ));
        q[0] /= len;
        q[1] /= len;
        q[2] /= len;
        q[3] /= len;
    }

    // Shortest-path hemisphere: keep w >= 0 when possible (stable round-trip).
    if q[3] < 0.0 {
        q[0] = -q[0];
        q[1] = -q[1];
        q[2] = -q[2];
        q[3] = -q[3];
    }

    let cap = max_xyz_component_for_bone(bone);
    let max_xyz = q[0].abs().max(q[1].abs()).max(q[2].abs());
    if max_xyz > cap {
        let scale = cap / max_xyz;
        warnings.push(format!(
            "{bone}: |x|/|y|/|z| max {max_xyz:.3} exceeds cap {cap:.2}, scaled toward identity"
        ));
        q[0] *= scale;
        q[1] *= scale;
        q[2] *= scale;
        let s = q[0] * q[0] + q[1] * q[1] + q[2] * q[2];
        let ww = (1.0_f32 - s).max(0.0).sqrt();
        q[3] = ww;
    }

    // Final renormalize (correct drift from scaling xyz).
    let len2 = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if (len2 - 1.0).abs() > 0.0001 && len2 >= EPS_LEN {
        q[0] /= len2;
        q[1] /= len2;
        q[2] /= len2;
        q[3] /= len2;
    }
    (q, warnings)
}

pub fn sanitize_bone_map(
    bones: HashMap<String, [f32; 4]>,
) -> (HashMap<String, [f32; 4]>, Vec<String>) {
    let mut out = HashMap::with_capacity(bones.len());
    let mut all = Vec::new();
    for (name, q) in bones {
        let (fixed, mut w) = sanitize_quat(&name, q);
        out.insert(name, fixed);
        all.append(&mut w);
    }
    (out, all)
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BoneEulerDeg {
    /// Intrinsic-local X rotation in degrees (positive flexes "forward" for most limb bones in this rig).
    #[serde(default)]
    pub pitch_deg: Option<f32>,
    #[serde(default)]
    pub yaw_deg: Option<f32>,
    #[serde(default)]
    pub roll_deg: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PoseBonesArgs {
    /// Bone key → Euler degrees. Omitted axis = 0. Angles are **clamped** per bone; response lists clamped values.
    pub bones: HashMap<String, BoneEulerDeg>,
    #[serde(default = "default_true")]
    pub preserve_omitted_bones: bool,
    /// Optional VRM expression weights (0..=1) applied after bones via `ModifyExpressions` (same as `set_expression`). Keys must exist on the loaded VRM (`list_expressions` / `get_bone_reference` → `expressionPresets`) when that list is non-empty.
    #[serde(default)]
    pub expressions: Option<HashMap<String, f32>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MakeFistArgs {
    /// 0 = open relaxed template, 1 = full fist (per assets/POSE_GUIDE.md reference).
    pub amount: f32,
    #[serde(default)]
    pub left: Option<bool>,
    #[serde(default)]
    pub right: Option<bool>,
}

/// Degrees → radians
fn to_rad(d: f32) -> f32 {
    d.to_radians()
}

fn quat_to_xyzw(q: Quat) -> [f32; 4] {
    [q.x, q.y, q.z, q.w]
}

fn slerp_quat(a: Quat, b: Quat, t: f32) -> Quat {
    a.slerp(b, t.clamp(0.0, 1.0))
}

fn euler_quat_clamped(bone: &str, euler: &BoneEulerDeg) -> (Quat, Vec<String>) {
    let mut warn = Vec::new();
    let mut p = euler.pitch_deg.unwrap_or(0.0);
    let mut y = euler.yaw_deg.unwrap_or(0.0);
    let mut r = euler.roll_deg.unwrap_or(0.0);
    let (max_p, max_y, max_r) = euler_limit_deg(bone);
    if p.abs() > max_p {
        warn.push(format!("{bone}: pitch_deg {p:.1} clamped to ±{max_p:.1}"));
        p = p.clamp(-max_p, max_p);
    }
    if y.abs() > max_y {
        warn.push(format!("{bone}: yaw_deg {y:.1} clamped to ±{max_y:.1}"));
        y = y.clamp(-max_y, max_y);
    }
    if r.abs() > max_r {
        warn.push(format!("{bone}: roll_deg {r:.1} clamped to ±{max_r:.1}"));
        r = r.clamp(-max_r, max_r);
    }
    // Match Bones-tab sliders: display yaw + Helen per-digit offset for
    // `DEF-toe_{big,index,middle,ring,little}.{L,R}` (±180° L/R), same family as the
    // historical big-toe-only helper. Keeps intrinsic-Y (length-axis twist) in MCP space
    // aligned with pad-facing for all child toes from `helen_add_individual_toe_bones.py`.
    let yaw_for_quat = y + def_toe_big_yaw_slider_extra_deg(bone);
    let q = Quat::from_euler(EulerRot::XYZ, to_rad(p), to_rad(yaw_for_quat), to_rad(r));
    (q, warn)
}

/// Build pose quaternions from Euler map + optional sanitize pass (double safety).
pub fn bone_map_from_euler_deg(
    bones: &HashMap<String, BoneEulerDeg>,
) -> (HashMap<String, [f32; 4]>, Vec<String>) {
    let mut quats: HashMap<String, [f32; 4]> = HashMap::new();
    let mut warnings = Vec::new();
    for (bone, euler) in bones {
        let (q, mut w) = euler_quat_clamped(bone, euler);
        warnings.append(&mut w);
        let arr = quat_to_xyzw(q);
        let (arr2, mut w2) = sanitize_quat(bone, arr);
        warnings.append(&mut w2);
        quats.insert(bone.clone(), arr2);
    }
    (quats, warnings)
}

// --- Relaxed / fist template quaternions (same reference numbers as POSE_GUIDE) ---

fn relaxed_right_quats() -> HashMap<&'static str, Quat> {
    [
        (
            "rightThumbMetacarpal",
            Quat::from_xyzw(0.0, -0.04, 0.02, 0.999),
        ),
        (
            "rightThumbProximal",
            Quat::from_xyzw(0.0, -0.06, 0.0, 0.998),
        ),
        ("rightIndexProximal", Quat::from_xyzw(0.0, 0.0, 0.1, 0.995)),
        (
            "rightIndexIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.08, 0.997),
        ),
        (
            "rightMiddleProximal",
            Quat::from_xyzw(0.0, 0.0, 0.12, 0.993),
        ),
        (
            "rightMiddleIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.1, 0.995),
        ),
        ("rightRingProximal", Quat::from_xyzw(0.0, 0.0, 0.12, 0.993)),
        (
            "rightRingIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.1, 0.995),
        ),
        ("rightLittleProximal", Quat::from_xyzw(0.0, 0.0, 0.1, 0.995)),
        (
            "rightLittleIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.08, 0.997),
        ),
    ]
    .into_iter()
    .collect()
}

fn fist_right_quats() -> HashMap<&'static str, Quat> {
    [
        (
            "rightThumbProximal",
            Quat::from_xyzw(-0.21, -0.57, 0.40, 0.68),
        ),
        ("rightIndexProximal", Quat::from_xyzw(0.0, 0.0, 0.42, 0.908)),
        (
            "rightIndexIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.68, 0.733),
        ),
        ("rightIndexDistal", Quat::from_xyzw(0.0, 0.0, 0.35, 0.937)),
        (
            "rightMiddleProximal",
            Quat::from_xyzw(0.0, 0.0, 0.44, 0.898),
        ),
        (
            "rightMiddleIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.70, 0.714),
        ),
        ("rightMiddleDistal", Quat::from_xyzw(0.0, 0.0, 0.35, 0.937)),
        ("rightRingProximal", Quat::from_xyzw(0.0, 0.0, 0.43, 0.903)),
        (
            "rightRingIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.68, 0.733),
        ),
        ("rightRingDistal", Quat::from_xyzw(0.0, 0.0, 0.36, 0.933)),
        (
            "rightLittleProximal",
            Quat::from_xyzw(0.0, 0.0, 0.45, 0.893),
        ),
        (
            "rightLittleIntermediate",
            Quat::from_xyzw(0.0, 0.0, 0.70, 0.714),
        ),
        ("rightLittleDistal", Quat::from_xyzw(0.0, 0.0, 0.42, 0.908)),
    ]
    .into_iter()
    .collect()
}

/// Heuristic mirror used only for canned hand templates (not a general rig mirror).
fn mirror_template_to_left(right_bone: &str, q: Quat) -> Option<(String, Quat)> {
    let left = right_bone.replacen("right", "left", 1);
    if left == right_bone {
        return None;
    }
    let m = Quat::from_xyzw(-q.x, q.y, -q.z, q.w);
    Some((left, m))
}

/// `amount` in 0..1: slerp relaxed → fist per bone for enabled sides.
pub fn make_fist_bones(amount: f32, do_left: bool, do_right: bool) -> HashMap<String, [f32; 4]> {
    let t = amount.clamp(0.0, 1.0);
    let relax_r = relaxed_right_quats();
    let fist_r = fist_right_quats();
    let mut keys = std::collections::HashSet::new();
    for k in relax_r.keys() {
        keys.insert(*k);
    }
    for k in fist_r.keys() {
        keys.insert(*k);
    }
    let mut out: HashMap<String, [f32; 4]> = HashMap::new();

    if do_right {
        for k in &keys {
            let a = relax_r.get(*k).copied().unwrap_or(Quat::IDENTITY);
            let b = fist_r.get(*k).copied().unwrap_or(a);
            let q = slerp_quat(a, b, t).normalize();
            out.insert((*k).to_string(), quat_to_xyzw(q));
        }
    }
    if do_left {
        for k in &keys {
            let a = relax_r.get(*k).copied().unwrap_or(Quat::IDENTITY);
            let b = fist_r.get(*k).copied().unwrap_or(a);
            let q_right = slerp_quat(a, b, t).normalize();
            if let Some((lk, ql)) = mirror_template_to_left(k, q_right) {
                out.insert(lk, quat_to_xyzw(ql.normalize()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::pose_driver::def_toe_big_yaw_slider_extra_deg;

    #[test]
    fn helen_named_toes_get_lr_yaw_offset() {
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_big.L"), 180.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("def-toe_big.r"), -180.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_index.L"), 180.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_middle.R"), -180.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_ring.L"), 180.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_little.R"), -180.0);
    }

    #[test]
    fn non_digit_def_toe_prefixs_skip_yaw_offset() {
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe.L"), 0.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe.R"), 0.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("DEF-toe_ero.L"), 0.0);
        assert_eq!(def_toe_big_yaw_slider_extra_deg("hips"), 0.0);
    }

    #[test]
    fn pose_bones_child_toe_matches_big_toe_yaw_path() {
        let mut m = HashMap::new();
        m.insert(
            "DEF-toe_index.L".into(),
            BoneEulerDeg {
                pitch_deg: Some(5.0),
                yaw_deg: Some(0.0),
                roll_deg: Some(-3.0),
            },
        );
        let (quats, _) = bone_map_from_euler_deg(&m);
        let mut m2 = HashMap::new();
        m2.insert(
            "DEF-toe_big.L".into(),
            BoneEulerDeg {
                pitch_deg: Some(5.0),
                yaw_deg: Some(0.0),
                roll_deg: Some(-3.0),
            },
        );
        let (qu2, _) = bone_map_from_euler_deg(&m2);
        assert_eq!(
            quats.get("DEF-toe_index.L").copied(),
            qu2.get("DEF-toe_big.L").copied(),
            "same display euler should yield same pose_q when L yaw extra matches"
        );
    }
}
