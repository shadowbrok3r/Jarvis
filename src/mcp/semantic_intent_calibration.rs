//! Per-VRM calibration for MCP semantic intent tools (`raise_leg`, `bend_knee`,
//! `arms_down_rest`). Rest pose / bone roll differs across exports; the default
//! compilers assume an airi-style rig. Signs here multiply the compiled Euler
//! degrees before sanitize — flip a sign when “forward” reads backward on a rig.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Multipliers applied to semantic-intent compiled angles (typically ±1.0).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticIntentCalibration {
    /// `raise_leg` `direction: forward` — multiplies `*UpperLeg.pitch_deg`.
    #[serde(default = "default_sign")]
    pub raise_leg_forward_pitch_sign: f32,
    /// `raise_leg` `direction: outward` — multiplies mirrored `*UpperLeg.roll_deg`.
    #[serde(default = "default_sign")]
    pub raise_leg_outward_roll_sign: f32,
    /// `bend_knee` — multiplies `*LowerLeg.pitch_deg`.
    #[serde(default = "default_sign")]
    pub bend_knee_pitch_sign: f32,
    /// `arms_down_rest` — multiplies upper-arm `roll_deg` (+/- pair).
    #[serde(default = "default_sign")]
    pub arms_down_rest_upper_arm_roll_sign: f32,
    /// `arms_down_rest` — multiplies lower-arm `pitch_deg`.
    #[serde(default = "default_sign")]
    pub arms_down_rest_elbow_pitch_sign: f32,
    /// `arms_down_rest` — multiplies shoulder `pitch_deg` / `roll_deg` / upper-arm pitch.
    #[serde(default = "default_sign")]
    pub arms_down_rest_shoulder_sign: f32,
}

fn default_sign() -> f32 {
    1.0
}

impl Default for SemanticIntentCalibration {
    fn default() -> Self {
        Self {
            raise_leg_forward_pitch_sign: 1.0,
            raise_leg_outward_roll_sign: 1.0,
            bend_knee_pitch_sign: 1.0,
            arms_down_rest_upper_arm_roll_sign: 1.0,
            arms_down_rest_elbow_pitch_sign: 1.0,
            arms_down_rest_shoulder_sign: 1.0,
        }
    }
}

/// File stored per VRM key (`spring_preset::vrm_preset_key` from logical model path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIntentCalibrationFile {
    /// Human audit trail — which `[avatar].model_path` this was tuned for.
    #[serde(default)]
    pub logical_path: String,
    #[serde(flatten)]
    pub calibration: SemanticIntentCalibration,
}

#[derive(Debug)]
pub struct SemanticIntentCalibrationStore {
    pub dir: PathBuf,
    pub entries: HashMap<String, SemanticIntentCalibration>,
}

impl SemanticIntentCalibrationStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            entries: HashMap::new(),
        }
    }

    pub fn load_dir(dir: &Path) -> Self {
        let mut store = Self::new(dir.to_path_buf());
        if !dir.exists() {
            let _ = fs::create_dir_all(dir);
            return store;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return store;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().is_some_and(|e| e == "toml") {
                if let Ok(text) = fs::read_to_string(&path) {
                    if let Ok(file) = toml::from_str::<SemanticIntentCalibrationFile>(&text) {
                        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                        if let Some(key) = stem {
                            store.entries.insert(key, file.calibration);
                        }
                    }
                }
            }
        }
        store
    }

    pub fn get(&self, key: &str) -> SemanticIntentCalibration {
        self.entries
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn insert(&mut self, key: String, cal: SemanticIntentCalibration) {
        self.entries.insert(key, cal);
    }

    pub fn save_file(&self, key: &str, logical_path: &str, cal: &SemanticIntentCalibration) -> Result<(), String> {
        let _ = fs::create_dir_all(&self.dir);
        let path = self.dir.join(format!("{key}.toml"));
        let file = SemanticIntentCalibrationFile {
            logical_path: logical_path.to_string(),
            calibration: cal.clone(),
        };
        let text =
            toml::to_string_pretty(&file).map_err(|e| format!("toml serialize: {e}"))?;
        fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
    }
}
