//! Semantic / "intent" pose helpers — high-level abstractions that compile to
//! a bounded Euler map agents can apply via `pose_bones` semantics.
//!
//! The goal is to give models a small palette of **named intents** with
//! amount sliders (`0..=1`) so they don't have to pick raw axes / signs and
//! mirror them by hand.
//!
//! Default compilers target the airi/MMD humanoid family (`POSE_GUIDE.md`).
//! Per-VRM sign overrides live in [`super::semantic_intent_calibration::SemanticIntentCalibration`]
//! (Intent Lab UI + `config/semantic_intent_calibration/<key>.toml`).

use std::collections::HashMap;

use rmcp::schemars::JsonSchema;
use serde::Deserialize;

use super::pose_authoring::BoneEulerDeg;
use super::semantic_intent_calibration::SemanticIntentCalibration;

/// Hard cap so semantic intents never push agents into `Severe` territory.
/// Each intent's max amount maps to ≤ this many degrees on the dominant axis.
const INTENT_MAX_PRIMARY_DEG: f32 = 70.0;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn lower(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    /// Sign multiplier where +1 looks "outward" on the right side (hip
    /// abduction / arm-spread), and -1 mirrors it to the left.
    fn outward_sign(self) -> f32 {
        match self {
            Side::Right => 1.0,
            Side::Left => -1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegRaiseDirection {
    /// Hip flexion — knee comes forward and up.
    Forward,
    /// Hip abduction — leg fans out to the side (uses `roll_deg`).
    Outward,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RaiseLegArgs {
    pub side: Side,
    /// 0..=1, scaled to a safe interior range so we never hit clamp limits.
    pub amount: f32,
    /// Default `forward` (hip flex). `outward` uses `roll_deg` for clean
    /// abduction (avoids the thigh-yaw trap documented in POSE_GUIDE).
    #[serde(default)]
    pub direction: Option<LegRaiseDirection>,
    /// When true (default false), also queue a `dry_run` so the caller can
    /// inspect the compiled Euler map without applying.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BendKneeArgs {
    pub side: Side,
    /// 0..=1 — knee flexion. 0 = straight, 1 = ≈ 70° bent.
    pub amount: f32,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ArmsDownRestArgs {
    /// 0..=1 — how strongly to drop the arms toward the body (mirror-roll).
    /// Default 0.85 (typical natural rest).
    #[serde(default)]
    pub amount: Option<f32>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// Compile `raise_leg` to a bounded Euler map.
///
/// `forward`: positive `pitch_deg` on `*UpperLeg` (hip flexion) × calibration.
/// `outward`: mirrored `roll_deg` × calibration.
pub fn compile_raise_leg(
    args: &RaiseLegArgs,
    cal: &SemanticIntentCalibration,
) -> HashMap<String, BoneEulerDeg> {
    let amt = args.amount.clamp(0.0, 1.0);
    let dir = args.direction.unwrap_or(LegRaiseDirection::Forward);
    let bone = format!("{}UpperLeg", args.side.lower());
    let primary = INTENT_MAX_PRIMARY_DEG * amt;
    let mut map = HashMap::new();
    let euler = match dir {
        LegRaiseDirection::Forward => BoneEulerDeg {
            pitch_deg: Some(primary * cal.raise_leg_forward_pitch_sign),
            yaw_deg: None,
            roll_deg: None,
        },
        LegRaiseDirection::Outward => BoneEulerDeg {
            pitch_deg: None,
            yaw_deg: None,
            roll_deg: Some(
                args.side.outward_sign()
                    * primary
                    * 0.8
                    * cal.raise_leg_outward_roll_sign,
            ),
        },
    };
    map.insert(bone, euler);
    map
}

/// Compile `bend_knee` to a bounded Euler map.
///
/// On airi-family rigs the safe knee-flex axis tends to be **positive
/// `pitch_deg`** on `*LowerLeg` (the guide flips the historical negative
/// convention because it hyperextends backward on this default rig).
pub fn compile_bend_knee(
    args: &BendKneeArgs,
    cal: &SemanticIntentCalibration,
) -> HashMap<String, BoneEulerDeg> {
    let amt = args.amount.clamp(0.0, 1.0);
    let bone = format!("{}LowerLeg", args.side.lower());
    let primary = INTENT_MAX_PRIMARY_DEG * amt * cal.bend_knee_pitch_sign;
    let mut map = HashMap::new();
    map.insert(
        bone,
        BoneEulerDeg {
            pitch_deg: Some(primary),
            yaw_deg: None,
            roll_deg: None,
        },
    );
    map
}

/// Compile `arms_down_rest` to a mirror-symmetric upper-body map.
///
/// Drops both arms beside the torso using the mirror-asymmetric `roll_deg`
/// convention from POSE_GUIDE: left = -k, right = +k. Adds a soft elbow
/// pitch on the lower arms.
pub fn compile_arms_down_rest(
    args: &ArmsDownRestArgs,
    cal: &SemanticIntentCalibration,
) -> HashMap<String, BoneEulerDeg> {
    let amt = args.amount.unwrap_or(0.85).clamp(0.0, 1.0);
    let ru = cal.arms_down_rest_upper_arm_roll_sign;
    let el = cal.arms_down_rest_elbow_pitch_sign;
    let sh = cal.arms_down_rest_shoulder_sign;
    let primary = 60.0 * amt * ru; // upper-arm roll
    let elbow = -10.0 * amt * el; // soft elbow pitch
    let shoulder = 4.0 * amt * sh;
    let mut map = HashMap::new();
    map.insert(
        "leftUpperArm".into(),
        BoneEulerDeg {
            pitch_deg: Some(2.0 * amt * sh),
            yaw_deg: None,
            roll_deg: Some(-primary),
        },
    );
    map.insert(
        "rightUpperArm".into(),
        BoneEulerDeg {
            pitch_deg: Some(2.0 * amt * sh),
            yaw_deg: None,
            roll_deg: Some(primary),
        },
    );
    map.insert(
        "leftLowerArm".into(),
        BoneEulerDeg {
            pitch_deg: Some(elbow),
            yaw_deg: None,
            roll_deg: None,
        },
    );
    map.insert(
        "rightLowerArm".into(),
        BoneEulerDeg {
            pitch_deg: Some(elbow),
            yaw_deg: None,
            roll_deg: None,
        },
    );
    map.insert(
        "leftShoulder".into(),
        BoneEulerDeg {
            pitch_deg: Some(shoulder),
            yaw_deg: None,
            roll_deg: Some(-shoulder),
        },
    );
    map.insert(
        "rightShoulder".into(),
        BoneEulerDeg {
            pitch_deg: Some(shoulder),
            yaw_deg: None,
            roll_deg: Some(shoulder),
        },
    );
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::semantic_intent_calibration::SemanticIntentCalibration;

    fn around(actual: f32, expected: f32, tol: f32) -> bool {
        (actual - expected).abs() <= tol
    }

    #[test]
    fn raise_leg_forward_uses_pitch() {
        let cal = SemanticIntentCalibration::default();
        let map = compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Left,
                amount: 0.5,
                direction: Some(LegRaiseDirection::Forward),
                dry_run: None,
            },
            &cal,
        );
        let e = map.get("leftUpperLeg").expect("left upper leg");
        let p = e.pitch_deg.unwrap_or(0.0);
        assert!(around(p, INTENT_MAX_PRIMARY_DEG * 0.5, 0.01));
        assert!(e.yaw_deg.is_none());
        assert!(e.roll_deg.is_none());
    }

    #[test]
    fn raise_leg_outward_uses_signed_roll_per_side() {
        let cal = SemanticIntentCalibration::default();
        let left = compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Left,
                amount: 1.0,
                direction: Some(LegRaiseDirection::Outward),
                dry_run: None,
            },
            &cal,
        );
        let right = compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Right,
                amount: 1.0,
                direction: Some(LegRaiseDirection::Outward),
                dry_run: None,
            },
            &cal,
        );
        let l = left.get("leftUpperLeg").unwrap().roll_deg.unwrap();
        let r = right.get("rightUpperLeg").unwrap().roll_deg.unwrap();
        assert!(l < 0.0 && r > 0.0, "outward roll mirror: left {l}, right {r}");
        assert!((l + r).abs() < 0.01, "must be exactly opposite signs");
    }

    #[test]
    fn raise_leg_amount_is_clamped() {
        let cal = SemanticIntentCalibration::default();
        let map = compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Right,
                amount: 5.0,
                direction: Some(LegRaiseDirection::Forward),
                dry_run: None,
            },
            &cal,
        );
        let p = map.get("rightUpperLeg").unwrap().pitch_deg.unwrap();
        assert!(p <= INTENT_MAX_PRIMARY_DEG + 1e-3);
    }

    #[test]
    fn bend_knee_uses_lower_leg_pitch() {
        let cal = SemanticIntentCalibration::default();
        let map = compile_bend_knee(
            &BendKneeArgs {
                side: Side::Right,
                amount: 0.5,
                dry_run: None,
            },
            &cal,
        );
        let e = map.get("rightLowerLeg").expect("right lower leg");
        assert!(e.pitch_deg.is_some());
        assert!(e.yaw_deg.is_none());
        assert!(e.roll_deg.is_none());
    }

    #[test]
    fn arms_down_rest_mirrors_signs() {
        let cal = SemanticIntentCalibration::default();
        let map = compile_arms_down_rest(
            &ArmsDownRestArgs {
                amount: Some(1.0),
                dry_run: None,
            },
            &cal,
        );
        let l = map.get("leftUpperArm").unwrap().roll_deg.unwrap();
        let r = map.get("rightUpperArm").unwrap().roll_deg.unwrap();
        assert!(l < 0.0 && r > 0.0, "left should drop with -roll, right with +roll");
        assert!((l + r).abs() < 0.01);
    }
}
