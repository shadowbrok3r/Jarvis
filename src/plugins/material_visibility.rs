//! Per-material mesh visibility for VRM avatars (show/hide by stable material key).
//!
//! Keys match the Graphics Advanced MToon picker (`mtoon_mesh_override_key`).
//! State is persisted to `config/ModelOverrides/{stem}/material_visibility.json`
//! and inlined into the iOS hub manifest.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::gltf::GltfMaterialName;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy_vrm1::prelude::{MToonMaterial, Vrm};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::Settings;

use super::mtoon_overrides::{
    mtoon_mesh_override_key, vrm_model_material_visibility_path,
};

pub struct MaterialVisibilityPlugin;

impl Plugin for MaterialVisibilityPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_material_visibility)
            .add_systems(Update, (reload_visibility_on_model_change, apply_material_visibility));
    }
}

/// On-disk format: list of hidden material keys (absent keys are visible).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MaterialVisibilityFile {
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// In-memory hidden set + sidecar path; bumped on every mutation.
#[derive(Resource, Clone)]
pub struct MaterialVisibilityStore {
    pub path: PathBuf,
    inner: Arc<RwLock<MaterialVisibilityFile>>,
    rev: Arc<RwLock<u64>>,
}

impl MaterialVisibilityStore {
    fn new(path: PathBuf, file: MaterialVisibilityFile) -> Self {
        let start_rev = u64::from(!file.hidden.is_empty());
        Self {
            path,
            inner: Arc::new(RwLock::new(file)),
            rev: Arc::new(RwLock::new(start_rev)),
        }
    }

    pub fn revision(&self) -> u64 {
        *self.rev.read()
    }

    pub fn snapshot(&self) -> MaterialVisibilityFile {
        self.inner.read().clone()
    }

    pub fn is_visible(&self, key: &str) -> bool {
        !self.inner.read().hidden.iter().any(|h| h == key)
    }

    pub fn set_visible(&self, key: impl Into<String>, visible: bool) -> std::io::Result<()> {
        let key = key.into();
        {
            let mut guard = self.inner.write();
            if visible {
                guard.hidden.retain(|h| h != &key);
            } else if !guard.hidden.iter().any(|h| h == &key) {
                guard.hidden.push(key);
                guard.hidden.sort();
            }
        }
        *self.rev.write() += 1;
        self.flush()
    }

    pub fn show_all(&self) -> std::io::Result<()> {
        {
            let mut guard = self.inner.write();
            if guard.hidden.is_empty() {
                return Ok(());
            }
            guard.hidden.clear();
        }
        *self.rev.write() += 1;
        self.flush()
    }

    pub fn hide_all(&self, keys: impl IntoIterator<Item = impl Into<String>>) -> std::io::Result<()> {
        {
            let mut guard = self.inner.write();
            guard.hidden = keys.into_iter().map(Into::into).collect();
            guard.hidden.sort();
            guard.hidden.dedup();
        }
        *self.rev.write() += 1;
        self.flush()
    }

    pub fn invert(&self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> std::io::Result<()> {
        {
            let mut guard = self.inner.write();
            for key in keys {
                let key = key.as_ref();
                if let Some(pos) = guard.hidden.iter().position(|h| h == key) {
                    guard.hidden.remove(pos);
                } else {
                    guard.hidden.push(key.to_string());
                }
            }
            guard.hidden.sort();
            guard.hidden.dedup();
        }
        *self.rev.write() += 1;
        self.flush()
    }

    fn flush(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let body = serde_json::to_string_pretty(&*self.inner.read())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&self.path, body)
    }
}

fn load_material_visibility_file(path: &PathBuf) -> MaterialVisibilityFile {
    match fs::read_to_string(path) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(f) => f,
            Err(e) => {
                warn!("material visibility: parse error for {path:?}: {e}");
                MaterialVisibilityFile::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MaterialVisibilityFile::default(),
        Err(e) => {
            warn!("material visibility: read {path:?}: {e}");
            MaterialVisibilityFile::default()
        }
    }
}

fn load_material_visibility(mut commands: Commands, settings: Res<Settings>) {
    let path = vrm_model_material_visibility_path(&settings.avatar.model_path);
    let file = load_material_visibility_file(&path);
    if !file.hidden.is_empty() {
        info!(
            "material visibility: loaded {} hidden material(s) from {path:?}",
            file.hidden.len()
        );
    }
    commands.insert_resource(MaterialVisibilityStore::new(path, file));
}

fn reload_visibility_on_model_change(
    mut commands: Commands,
    settings: Res<Settings>,
    store: Option<Res<MaterialVisibilityStore>>,
    mut last: Local<Option<String>>,
) {
    let current = settings.avatar.model_path.clone();
    if last.as_deref() == Some(current.as_str()) {
        return;
    }
    let was_first = last.is_none();
    *last = Some(current.clone());
    if was_first {
        if let Some(store) = store {
            if store.path == vrm_model_material_visibility_path(&current) {
                return;
            }
        }
    }
    let path = vrm_model_material_visibility_path(&current);
    let file = load_material_visibility_file(&path);
    info!("material visibility: reloaded for model {current} ({path:?})");
    commands.insert_resource(MaterialVisibilityStore::new(path, file));
}

/// Stable key for a mesh using [`StandardMaterial`] (same precedence as MToon keys).
pub fn std_mesh_material_key(
    name: Option<&Name>,
    gltf_material: Option<&GltfMaterialName>,
    material_handle: &Handle<StandardMaterial>,
) -> String {
    if let Some(g) = gltf_material {
        return g.0.clone();
    }
    if let Some(n) = name {
        return n.as_str().to_string();
    }
    format!("MaterialAsset_{:?}", material_handle.id())
}

/// True when `start` is the VRM root or a descendant in the `ChildOf` graph.
pub fn entity_under_vrm(
    mut entity: Entity,
    child_of: &Query<&ChildOf>,
    vrm_roots: &HashSet<Entity>,
) -> bool {
    for _ in 0..128 {
        if vrm_roots.contains(&entity) {
            return true;
        }
        let Ok(co) = child_of.get(entity) else {
            return false;
        };
        let parent = co.parent();
        if parent == entity {
            return false;
        }
        entity = parent;
    }
    false
}

#[derive(Default)]
struct MaterialVisibilityApplyCursor {
    rev: u64,
    mesh_count: usize,
}

fn apply_material_visibility(
    store: Option<Res<MaterialVisibilityStore>>,
    vrm_roots_q: Query<Entity, With<Vrm>>,
    child_of_q: Query<&ChildOf>,
    mut mtoon_meshes: Query<
        (
            Entity,
            Option<&Name>,
            Option<&GltfMaterialName>,
            &MeshMaterial3d<MToonMaterial>,
            &mut Visibility,
        ),
        Without<MeshMaterial3d<StandardMaterial>>,
    >,
    mut std_meshes: Query<
        (
            Entity,
            Option<&Name>,
            Option<&GltfMaterialName>,
            &MeshMaterial3d<StandardMaterial>,
            &mut Visibility,
        ),
        Without<MeshMaterial3d<MToonMaterial>>,
    >,
    mut applied: Local<MaterialVisibilityApplyCursor>,
) {
    let Some(store) = store else {
        return;
    };
    let vrm_roots: HashSet<Entity> = vrm_roots_q.iter().collect();
    if vrm_roots.is_empty() {
        return;
    }

    let mesh_count = mtoon_meshes.iter().count() + std_meshes.iter().count();
    let current_rev = store.revision();
    if current_rev == applied.rev && mesh_count == applied.mesh_count {
        return;
    }
    applied.rev = current_rev;
    applied.mesh_count = mesh_count;

    let hidden: HashSet<String> = store.snapshot().hidden.into_iter().collect();

    for (entity, name, gltf_name, handle, mut vis) in &mut mtoon_meshes {
        if !entity_under_vrm(entity, &child_of_q, &vrm_roots) {
            continue;
        }
        let key = mtoon_mesh_override_key(name, gltf_name, &handle.0);
        *vis = if hidden.contains(&key) {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }

    for (entity, name, gltf_name, handle, mut vis) in &mut std_meshes {
        if !entity_under_vrm(entity, &child_of_q, &vrm_roots) {
            continue;
        }
        let key = std_mesh_material_key(name, gltf_name, &handle.0);
        *vis = if hidden.contains(&key) {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
}
