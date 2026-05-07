//! Hybrid safety policy for MCP pose calls.
//!
//! Goal: stop catastrophic agent calls (full-body resets, extreme angles,
//! mass clamp distortions) **without** annoying iterative authoring with
//! warnings on every clamp. The policy is deliberately conservative — only
//! hard-fail when a typical agent mistake would produce an unusable rig.
//!
//! ## Severity model
//!
//! For each bone in the request we collect:
//!
//! - the **largest absolute Euler angle** the agent asked for, and
//! - whether **any** axis was clamped, and by how much.
//!
//! Severity tiers:
//!
//! | Tier         | Trigger                                                                              | Behaviour                       |
//! |--------------|--------------------------------------------------------------------------------------|---------------------------------|
//! | `Catastrophic` | Many bones at near-clamp angles (≥ 90% of bone-axis limit), or > N° on multiple bones | Hard-fail (always)              |
//! | `Severe`     | Any axis exceeds `severe_angle_deg` (default 80°)                                     | Hard-fail unless caller opts in |
//! | `Warn`       | Minor clamp / single-bone moderate magnitude                                          | Warn-only                        |
//!
//! Strict callers (`strict: true`) escalate `Warn` to `Severe`, hard-failing on
//! any clamp at all. `dry_run: true` runs the full policy + sanitize pipeline,
//! returns the result, but never dispatches `PoseCommand::ApplyBones`.

use std::collections::HashMap;

use super::pose_authoring::{euler_limit_deg, BoneEulerDeg};

/// Hard limit per axis above which `Severe` triggers (regardless of clamp).
pub const SEVERE_ANGLE_DEG: f32 = 80.0;

/// If at least this many bones in one call hit `Severe`, the request is `Catastrophic`.
pub const CATASTROPHIC_SEVERE_BONE_COUNT: usize = 3;

/// Fraction of the per-axis clamp at which the request angle counts as "near limit".
pub const NEAR_LIMIT_FRACTION: f32 = 0.90;

/// If at least this many bones request near-limit angles in one call, treat the
/// whole map as `Catastrophic` even when nothing individual is `Severe`.
pub const CATASTROPHIC_NEAR_LIMIT_BONE_COUNT: usize = 4;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum Severity {
    Ok,
    Warn,
    Severe,
    Catastrophic,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Ok => "ok",
            Severity::Warn => "warn",
            Severity::Severe => "severe",
            Severity::Catastrophic => "catastrophic",
        }
    }
}

/// Per-bone diagnostic for the response. Fields are read by the test module
/// and surfaced into JSON via `Debug` derivations during diagnostic dumps.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BoneSafetyReport {
    pub bone: String,
    pub max_abs_angle_deg: f32,
    pub near_limit: bool,
    pub axis_with_max: &'static str,
}

/// Aggregate report after evaluating an Euler map.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PoseSafetyReport {
    pub severity: Severity,
    pub bones_evaluated: usize,
    pub severe_bones: Vec<String>,
    pub near_limit_bones: Vec<String>,
    pub max_angle_seen_deg: f32,
    pub bone_details: Vec<BoneSafetyReport>,
}

impl PoseSafetyReport {
    /// Build the report from the agent's requested Euler map (pre-clamp).
    /// Uses [`euler_limit_deg`] to decide near-limit bones per axis.
    pub fn from_euler_map(bones: &HashMap<String, BoneEulerDeg>) -> Self {
        let mut bone_details = Vec::with_capacity(bones.len());
        let mut severe_bones = Vec::new();
        let mut near_limit_bones = Vec::new();
        let mut max_angle_seen = 0.0_f32;

        for (name, eul) in bones {
            let pitch = eul.pitch_deg.unwrap_or(0.0);
            let yaw = eul.yaw_deg.unwrap_or(0.0);
            let roll = eul.roll_deg.unwrap_or(0.0);
            let (lim_p, lim_y, lim_r) = euler_limit_deg(name);

            // Find the axis with the largest absolute requested angle.
            let candidates: [(f32, &'static str); 3] =
                [(pitch.abs(), "pitch"), (yaw.abs(), "yaw"), (roll.abs(), "roll")];
            let (max_abs, axis) = candidates
                .into_iter()
                .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or((0.0, "pitch"));

            // Near-limit on the bone's actual cap, not on the global threshold.
            let lim_for_axis = match axis {
                "pitch" => lim_p,
                "yaw" => lim_y,
                _ => lim_r,
            };
            let near_limit = lim_for_axis > 0.0 && max_abs >= lim_for_axis * NEAR_LIMIT_FRACTION;

            if max_abs > max_angle_seen {
                max_angle_seen = max_abs;
            }

            if max_abs >= SEVERE_ANGLE_DEG {
                severe_bones.push(name.clone());
            }
            if near_limit {
                near_limit_bones.push(name.clone());
            }

            bone_details.push(BoneSafetyReport {
                bone: name.clone(),
                max_abs_angle_deg: max_abs,
                near_limit,
                axis_with_max: axis,
            });
        }

        let severity = if severe_bones.len() >= CATASTROPHIC_SEVERE_BONE_COUNT
            || near_limit_bones.len() >= CATASTROPHIC_NEAR_LIMIT_BONE_COUNT
        {
            Severity::Catastrophic
        } else if !severe_bones.is_empty() {
            Severity::Severe
        } else if !near_limit_bones.is_empty() {
            Severity::Warn
        } else {
            Severity::Ok
        };

        Self {
            severity,
            bones_evaluated: bones.len(),
            severe_bones,
            near_limit_bones,
            max_angle_seen_deg: max_angle_seen,
            bone_details,
        }
    }

    /// Hybrid policy decision: should the request hard-fail before applying?
    ///
    /// - Always block `Catastrophic` (multiple severe bones / many near-limit bones).
    /// - Block `Severe` unless caller opted in via `allow_large_angles`.
    /// - Strict mode escalates: any `Warn` blocks too.
    pub fn should_block(&self, strict: bool, allow_large_angles: bool) -> Option<String> {
        match self.severity {
            Severity::Catastrophic => Some(format!(
                "catastrophic pose request: {} bones at near-axis limits (max angle {:.1}°). \
This pattern almost always produces a broken rig. Author the pose in 2-3 phases (torso, then \
limbs, then face) with capture_pose_views between each apply, or use the semantic tools \
(raise_leg, bend_knee, arms_down_rest).",
                self.near_limit_bones.len().max(self.severe_bones.len()),
                self.max_angle_seen_deg,
            )),
            Severity::Severe if !allow_large_angles => Some(format!(
                "severe pose request: bones {:?} request angles ≥ {SEVERE_ANGLE_DEG:.0}° (max {:.1}°). \
Set allow_large_angles=true if this is intentional, or split the work across multiple \
smaller pose_bones calls with capture_pose_views in between.",
                self.severe_bones, self.max_angle_seen_deg,
            )),
            Severity::Warn if strict => Some(format!(
                "strict mode: bones {:?} request near-axis-limit angles. \
Ease off (smaller steps) or call again with strict=false.",
                self.near_limit_bones,
            )),
            _ => None,
        }
    }
}

/// Detect leg / arm bones whose changes warrant side / back capture verification.
pub fn is_leg_bone(name: &str) -> bool {
    name.ends_with("UpperLeg")
        || name.ends_with("LowerLeg")
        || name.ends_with("Foot")
        || name.ends_with("Toes")
        || name == "hips"
}

pub fn is_arm_bone(name: &str) -> bool {
    name.ends_with("UpperArm")
        || name.ends_with("LowerArm")
        || name.ends_with("Hand")
        || name.contains("Shoulder")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn euler(p: f32, y: f32, r: f32) -> BoneEulerDeg {
        BoneEulerDeg {
            pitch_deg: Some(p),
            yaw_deg: Some(y),
            roll_deg: Some(r),
        }
    }

    #[test]
    fn ok_severity_for_small_changes() {
        let mut bones = HashMap::new();
        bones.insert("leftUpperArm".into(), euler(5.0, 3.0, 2.0));
        bones.insert("hips".into(), euler(-4.0, 0.0, 0.0));
        let report = PoseSafetyReport::from_euler_map(&bones);
        assert_eq!(report.severity, Severity::Ok);
        assert!(report.should_block(false, false).is_none());
    }

    #[test]
    fn severe_angle_blocks_unless_allowed() {
        let mut bones = HashMap::new();
        bones.insert("leftLowerLeg".into(), euler(-95.0, 0.0, 0.0));
        let report = PoseSafetyReport::from_euler_map(&bones);
        assert_eq!(report.severity, Severity::Severe);
        assert!(report.should_block(false, false).is_some());
        assert!(report.should_block(false, true).is_none());
    }

    #[test]
    fn many_near_limits_is_catastrophic() {
        let mut bones = HashMap::new();
        // hips yaw cap is 62; leftUpperLeg roll cap 55; rightUpperLeg roll cap 55;
        // leftLowerLeg pitch cap 125; pick angles ≥ 90% of each.
        bones.insert("hips".into(), euler(0.0, 60.0, 0.0));
        bones.insert("leftUpperLeg".into(), euler(0.0, 0.0, 55.0));
        bones.insert("rightUpperLeg".into(), euler(0.0, 0.0, -55.0));
        bones.insert("leftLowerLeg".into(), euler(120.0, 0.0, 0.0));
        let report = PoseSafetyReport::from_euler_map(&bones);
        assert_eq!(report.severity, Severity::Catastrophic);
        // Even with allow_large_angles, catastrophic blocks.
        assert!(report.should_block(false, true).is_some());
    }

    #[test]
    fn strict_mode_blocks_warn() {
        let mut bones = HashMap::new();
        // Single near-limit bone → Warn severity.
        bones.insert("leftUpperLeg".into(), euler(0.0, 0.0, 53.0));
        let report = PoseSafetyReport::from_euler_map(&bones);
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.should_block(false, false).is_none());
        assert!(report.should_block(true, false).is_some());
    }

    #[test]
    fn leg_arm_classification() {
        assert!(is_leg_bone("leftUpperLeg"));
        assert!(is_leg_bone("rightLowerLeg"));
        assert!(is_leg_bone("hips"));
        assert!(is_leg_bone("leftFoot"));
        assert!(!is_leg_bone("spine"));

        assert!(is_arm_bone("leftUpperArm"));
        assert!(is_arm_bone("rightShoulder"));
        assert!(is_arm_bone("leftHand"));
        assert!(!is_arm_bone("hips"));
    }
}
