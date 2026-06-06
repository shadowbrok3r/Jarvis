//! Per-bone mesh-influence metadata for the Bones tab (read-only sidecar).
//!
//! Generated offline by `tools/build_bone_influence.py` and stored at
//! `config/ModelOverrides/{stem}/bone_influence.json`. Drives two Bones-tab
//! declutter features:
//!   * **non-deforming** — bones whose max vertex weight is ~0 (toe `_03` tips,
//!     hair/ribbon tips). Hidden by default; posing them moves no geometry.
//!   * **bone_materials** — the glTF material name(s) each bone deforms, in the
//!     same key space as [`MaterialVisibilityStore`], so the UI can hide a bone
//!     when every mesh it influences is hidden (e.g. `waistBelt_front_03`).
//!
//! Missing / unreadable sidecar → empty resource (everything stays visible).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use bevy::prelude::*;
use serde::Deserialize;

use jarvis_avatar::avatar_defaults::vrm_model_overrides_dir;
use jarvis_avatar::config::Settings;

pub struct BoneInfluencePlugin;

impl Plugin for BoneInfluencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BoneInfluence>()
            .add_systems(Startup, load_bone_influence)
            .add_systems(Update, reload_bone_influence_on_model_change);
    }
}

/// On-disk format (extra fields like `model` / `weight_threshold` are ignored).
#[derive(Debug, Clone, Default, Deserialize)]
struct BoneInfluenceFile {
    #[serde(default)]
    non_deforming: Vec<String>,
    #[serde(default)]
    bone_materials: HashMap<String, Vec<String>>,
}

/// Loaded per-model bone influence. Empty when no sidecar exists.
#[derive(Resource, Default, Clone)]
pub struct BoneInfluence {
    non_deforming: HashSet<String>,
    bone_materials: HashMap<String, Vec<String>>,
}

impl BoneInfluence {
    fn from_file(f: BoneInfluenceFile) -> Self {
        Self {
            non_deforming: f.non_deforming.into_iter().collect(),
            bone_materials: f.bone_materials,
        }
    }

    /// True when this bone moves (essentially) no geometry.
    pub fn is_non_deforming(&self, bone: &str) -> bool {
        self.non_deforming.contains(bone)
    }

    /// glTF material name(s) this bone deforms (empty if unknown / non-skinned).
    pub fn materials(&self, bone: &str) -> &[String] {
        self.bone_materials.get(bone).map(Vec::as_slice).unwrap_or(&[])
    }

    /// True when the bone deforms at least one mesh and **every** mesh it
    /// influences is currently hidden — so the bone is invisible to pose.
    pub fn all_meshes_hidden(&self, bone: &str, is_hidden: impl Fn(&str) -> bool) -> bool {
        let mats = self.materials(bone);
        !mats.is_empty() && mats.iter().all(|m| is_hidden(m))
    }

    /// Whether any sidecar data was loaded at all.
    pub fn is_empty(&self) -> bool {
        self.non_deforming.is_empty() && self.bone_materials.is_empty()
    }
}

fn bone_influence_path(model_path: &str) -> PathBuf {
    vrm_model_overrides_dir(model_path).join("bone_influence.json")
}

fn load_bone_influence_file(path: &PathBuf) -> BoneInfluenceFile {
    match fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_else(|e| {
            warn!("bone influence: parse error for {path:?}: {e}");
            BoneInfluenceFile::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BoneInfluenceFile::default(),
        Err(e) => {
            warn!("bone influence: read {path:?}: {e}");
            BoneInfluenceFile::default()
        }
    }
}

fn load_bone_influence(mut commands: Commands, settings: Res<Settings>) {
    let path = bone_influence_path(&settings.avatar.model_path);
    let file = load_bone_influence_file(&path);
    if !file.non_deforming.is_empty() || !file.bone_materials.is_empty() {
        info!(
            "bone influence: {} non-deforming, {} bones-with-materials from {path:?}",
            file.non_deforming.len(),
            file.bone_materials.len(),
        );
    }
    commands.insert_resource(BoneInfluence::from_file(file));
}

fn reload_bone_influence_on_model_change(
    mut commands: Commands,
    settings: Res<Settings>,
    mut last: Local<Option<String>>,
) {
    let current = settings.avatar.model_path.clone();
    if last.as_deref() == Some(current.as_str()) {
        return;
    }
    let was_first = last.is_none();
    *last = Some(current.clone());
    // Startup system already loaded the initial model; skip the first tick.
    if was_first {
        return;
    }
    let path = bone_influence_path(&current);
    let file = load_bone_influence_file(&path);
    info!("bone influence: reloaded for model {current} ({path:?})");
    commands.insert_resource(BoneInfluence::from_file(file));
}
