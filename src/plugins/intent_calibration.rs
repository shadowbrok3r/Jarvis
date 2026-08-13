//! Shared [`Arc`] resources for MCP semantic-intent calibration and the Pose
//! Controller **Intent Lab** tab. Paths stay in sync with `[avatar].model_path`.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use bevy::prelude::*;
use crate::config::Settings;

use crate::mcp::intent_calibration_wizard::IntentCalibrationWizardSession;
use crate::mcp::semantic_intent_calibration::SemanticIntentCalibrationStore;

/// Mirrors [`Settings::avatar::model_path`] every frame for MCP tool handlers.
#[derive(Resource, Clone)]
pub struct SemanticIntentModelPath(pub Arc<RwLock<String>>);

/// Per-VRM hex-key → calibration; persisted under `config/semantic_intent_calibration/`.
#[derive(Resource, Clone)]
pub struct SemanticIntentCalibrationHandle(pub Arc<RwLock<SemanticIntentCalibrationStore>>);

/// MCP + Intent Lab guided calibration wizard (one probe, then forced confirm).
#[derive(Resource, Clone)]
pub struct IntentCalibrationWizardHandle(pub Arc<RwLock<IntentCalibrationWizardSession>>);

pub struct IntentCalibrationPlugin;

fn startup_init(mut commands: Commands, settings: Res<Settings>) {
    let dir = PathBuf::from("config/semantic_intent_calibration");
    let store = SemanticIntentCalibrationStore::load_dir(&dir);
    commands.insert_resource(SemanticIntentModelPath(Arc::new(RwLock::new(
        settings.avatar.model_path.clone(),
    ))));
    commands.insert_resource(SemanticIntentCalibrationHandle(Arc::new(RwLock::new(
        store,
    ))));
    commands.insert_resource(IntentCalibrationWizardHandle(Arc::new(RwLock::new(
        IntentCalibrationWizardSession::default(),
    ))));
}

fn sync_model_path(settings: Res<Settings>, path: Res<SemanticIntentModelPath>) {
    let mut g = path.0.write().unwrap();
    if *g != settings.avatar.model_path {
        *g = settings.avatar.model_path.clone();
    }
}

impl Plugin for IntentCalibrationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, startup_init).add_systems(Update, sync_model_path);
    }
}
