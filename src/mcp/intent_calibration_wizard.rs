//! Guided semantic-intent calibration — one probe at a time with a hard gate
//! until the human confirms the pose looked correct (or flips the sign).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::pose_authoring::BoneEulerDeg;
use super::pose_intents::{
    compile_arms_down_rest, compile_bend_knee, compile_raise_leg, ArmsDownRestArgs, BendKneeArgs,
    LegRaiseDirection, RaiseLegArgs, Side,
};
use super::semantic_intent_calibration::SemanticIntentCalibration;

/// Which calibration multiplier a wizard step tunes when the user says "flip".
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalField {
    RaiseLegForwardPitch,
    RaiseLegOutwardRoll,
    BendKneePitch,
    ArmsDownRestAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardStepDef {
    pub id: &'static str,
    pub label: &'static str,
    pub user_question: &'static str,
    pub cal_field: CalField,
}

pub const WIZARD_STEPS: &[WizardStepDef] = &[
    WizardStepDef {
        id: "raise_leg_forward",
        label: "raise_leg forward (right leg)",
        user_question: "Did the RIGHT leg lift FORWARD (knee toward the camera), not backward or sideways?",
        cal_field: CalField::RaiseLegForwardPitch,
    },
    WizardStepDef {
        id: "raise_leg_outward",
        label: "raise_leg outward (right leg)",
        user_question: "Did the RIGHT leg fan OUTWARD to the side (abduction), not twist inward or forward?",
        cal_field: CalField::RaiseLegOutwardRoll,
    },
    WizardStepDef {
        id: "bend_knee",
        label: "bend_knee (right leg)",
        user_question: "Did the RIGHT knee bend forward (lower leg folds back), not hyperextend or twist wrong?",
        cal_field: CalField::BendKneePitch,
    },
    WizardStepDef {
        id: "arms_down_rest",
        label: "arms_down_rest",
        user_question: "Did BOTH arms drop naturally to the sides (not T-pose, not crossed behind)?",
        cal_field: CalField::ArmsDownRestAll,
    },
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmVerdict {
    Correct,
    Flip,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: String,
    pub verdict: ConfirmVerdict,
    pub sign_after: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwaitingConfirm {
    pub step_id: String,
    pub step_index: usize,
    pub label: String,
    pub user_question: String,
    pub applied_bone_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WizardPhase {
    Idle,
    Active {
        vrm_key: String,
        logical_path: String,
        draft: SemanticIntentCalibration,
        step_index: usize,
        awaiting: Option<AwaitingConfirm>,
        log: Vec<StepRecord>,
    },
    Complete {
        vrm_key: String,
        logical_path: String,
        draft: SemanticIntentCalibration,
        log: Vec<StepRecord>,
    },
}

impl Default for WizardPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, Default)]
pub struct IntentCalibrationWizardSession {
    pub phase: WizardPhase,
}

impl IntentCalibrationWizardSession {
    pub fn begin(
        &mut self,
        vrm_key: String,
        logical_path: String,
        baseline: SemanticIntentCalibration,
    ) {
        self.phase = WizardPhase::Active {
            vrm_key,
            logical_path,
            draft: baseline,
            step_index: 0,
            awaiting: None,
            log: Vec::new(),
        };
    }

    pub fn abort(&mut self) {
        self.phase = WizardPhase::Idle;
    }

    pub fn status_snapshot(&self) -> serde_json::Value {
        match &self.phase {
            WizardPhase::Idle => serde_json::json!({
                "phase": "idle",
                "stepsTotal": WIZARD_STEPS.len(),
            }),
            WizardPhase::Active {
                vrm_key,
                logical_path,
                draft,
                step_index,
                awaiting,
                log,
            } => {
                let current = WIZARD_STEPS.get(*step_index).map(|s| serde_json::json!({
                    "id": s.id,
                    "label": s.label,
                    "userQuestion": s.user_question,
                }));
                serde_json::json!({
                    "phase": "active",
                    "vrmKey": vrm_key,
                    "logicalPath": logical_path,
                    "stepIndex": step_index,
                    "stepsTotal": WIZARD_STEPS.len(),
                    "currentStep": current,
                    "awaitingUserConfirm": awaiting.is_some(),
                    "awaiting": awaiting,
                    "draftCalibration": draft,
                    "completedSteps": log,
                })
            }
            WizardPhase::Complete {
                vrm_key,
                logical_path,
                draft,
                log,
            } => serde_json::json!({
                "phase": "complete",
                "vrmKey": vrm_key,
                "logicalPath": logical_path,
                "draftCalibration": draft,
                "completedSteps": log,
            }),
        }
    }

    pub fn probe_bones(
        &mut self,
    ) -> Result<(HashMap<String, BoneEulerDeg>, AwaitingConfirm), String> {
        let WizardPhase::Active {
            draft,
            step_index,
            awaiting,
            ..
        } = &mut self.phase
        else {
            return Err(
                "no active wizard — call begin_intent_calibration_wizard first".into(),
            );
        };

        if awaiting.is_some() {
            return Err(
                "previous step still awaiting user confirm — call intent_calibration_confirm \
(before another probe) or ask the user to click Correct / Flip / Skip in Intent Lab"
                    .into(),
            );
        }

        let step = WIZARD_STEPS
            .get(*step_index)
            .ok_or_else(|| "wizard already finished all steps — call confirm on last step or save".to_string())?;

        let bones = compile_probe_bones(step.id, draft);
        if bones.is_empty() {
            return Err(format!("step {} compiled empty bone map", step.id));
        }

        let pending = AwaitingConfirm {
            step_id: step.id.to_string(),
            step_index: *step_index,
            label: step.label.to_string(),
            user_question: step.user_question.to_string(),
            applied_bone_keys: bones.keys().cloned().collect(),
        };
        *awaiting = Some(pending.clone());
        Ok((bones, pending))
    }

    pub fn confirm(&mut self, step_id: &str, verdict: ConfirmVerdict) -> Result<serde_json::Value, String> {
        let WizardPhase::Active {
            draft,
            step_index,
            awaiting,
            log,
            ..
        } = &mut self.phase
        else {
            return Err("no active wizard session".into());
        };

        let Some(pending) = awaiting.take() else {
            return Err("nothing awaiting confirm — call intent_calibration_probe first".into());
        };
        if pending.step_id != step_id {
            let expected = pending.step_id.clone();
            *awaiting = Some(pending);
            return Err(format!(
                "step_id mismatch: awaiting {expected:?}, got {step_id:?}"
            ));
        }

        let step_def = WIZARD_STEPS
            .get(pending.step_index)
            .ok_or_else(|| format!("invalid step index {}", pending.step_index))?;

        let sign_after = match verdict {
            ConfirmVerdict::Flip => {
                flip_cal_field(draft, step_def.cal_field);
                read_cal_field(draft, step_def.cal_field)
            }
            ConfirmVerdict::Correct | ConfirmVerdict::Skip => {
                read_cal_field(draft, step_def.cal_field)
            }
        };

        log.push(StepRecord {
            step_id: step_id.to_string(),
            verdict,
            sign_after,
        });

        *step_index += 1;

        if *step_index >= WIZARD_STEPS.len() {
            let WizardPhase::Active {
                vrm_key,
                logical_path,
                draft,
                log,
                ..
            } = std::mem::replace(&mut self.phase, WizardPhase::Idle)
            else {
                unreachable!();
            };
            self.phase = WizardPhase::Complete {
                vrm_key,
                logical_path,
                draft,
                log,
            };
            return Ok(serde_json::json!({
                "advanced": true,
                "wizardComplete": true,
                "message": "All calibration probes confirmed. Call save_intent_calibration to persist, or begin_intent_calibration_wizard to restart.",
            }));
        }

        let next = &WIZARD_STEPS[*step_index];
        Ok(serde_json::json!({
            "advanced": true,
            "wizardComplete": false,
            "nextStep": {
                "id": next.id,
                "label": next.label,
                "userQuestion": next.user_question,
            },
        }))
    }

    pub fn draft_if_active(&self) -> Option<&SemanticIntentCalibration> {
        match &self.phase {
            WizardPhase::Active { draft, .. } | WizardPhase::Complete { draft, .. } => Some(draft),
            WizardPhase::Idle => None,
        }
    }

    pub fn take_complete_draft(&mut self) -> Option<(String, String, SemanticIntentCalibration)> {
        if let WizardPhase::Complete {
            vrm_key,
            logical_path,
            draft,
            ..
        } = std::mem::replace(&mut self.phase, WizardPhase::Idle)
        {
            Some((vrm_key, logical_path, draft))
        } else {
            None
        }
    }
}

pub fn compile_probe_bones(
    step_id: &str,
    cal: &SemanticIntentCalibration,
) -> HashMap<String, BoneEulerDeg> {
    match step_id {
        "raise_leg_forward" => compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Right,
                amount: 0.65,
                direction: Some(LegRaiseDirection::Forward),
                dry_run: None,
            },
            cal,
        ),
        "raise_leg_outward" => compile_raise_leg(
            &RaiseLegArgs {
                side: Side::Right,
                amount: 0.65,
                direction: Some(LegRaiseDirection::Outward),
                dry_run: None,
            },
            cal,
        ),
        "bend_knee" => compile_bend_knee(
            &BendKneeArgs {
                side: Side::Right,
                amount: 0.75,
                dry_run: None,
            },
            cal,
        ),
        "arms_down_rest" => compile_arms_down_rest(
            &ArmsDownRestArgs {
                amount: Some(0.85),
                dry_run: None,
            },
            cal,
        ),
        _ => HashMap::new(),
    }
}

fn read_cal_field(cal: &SemanticIntentCalibration, field: CalField) -> f32 {
    match field {
        CalField::RaiseLegForwardPitch => cal.raise_leg_forward_pitch_sign,
        CalField::RaiseLegOutwardRoll => cal.raise_leg_outward_roll_sign,
        CalField::BendKneePitch => cal.bend_knee_pitch_sign,
        CalField::ArmsDownRestAll => cal.arms_down_rest_upper_arm_roll_sign,
    }
}

fn flip_cal_field(cal: &mut SemanticIntentCalibration, field: CalField) {
    match field {
        CalField::RaiseLegForwardPitch => cal.raise_leg_forward_pitch_sign *= -1.0,
        CalField::RaiseLegOutwardRoll => cal.raise_leg_outward_roll_sign *= -1.0,
        CalField::BendKneePitch => cal.bend_knee_pitch_sign *= -1.0,
        CalField::ArmsDownRestAll => {
            cal.arms_down_rest_upper_arm_roll_sign *= -1.0;
            cal.arms_down_rest_elbow_pitch_sign *= -1.0;
            cal.arms_down_rest_shoulder_sign *= -1.0;
        }
    }
}
