//! Per-material mesh visibility on iOS (hub manifest + in-session egui toggles).

use std::collections::HashSet;

use bevy::gltf::GltfMaterialName;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy_vrm1::prelude::{Initialized, MToonMaterial, Vrm};
use serde::{Deserialize, Serialize};

/// Inlined `material_visibility.json` from the hub manifest.
#[derive(Resource, Clone, Default)]
pub struct IosMaterialVisibilityJson(pub Option<String>);

/// Runtime hidden set; egui mutates this, apply system reads `rev`.
#[derive(Resource, Clone, Default)]
pub struct IosMaterialVisibilityStore {
    pub hidden: HashSet<String>,
    pub rev: u64,
}

impl IosMaterialVisibilityStore {
    pub fn from_json(json: Option<&str>) -> Self {
        let mut store = Self::default();
        if let Some(s) = json {
            store.load_from_json_str(s);
        }
        store
    }

    pub fn load_from_json_str(&mut self, json: &str) {
        match serde_json::from_str::<MaterialVisibilityFile>(json) {
            Ok(file) => {
                self.hidden = file.hidden.into_iter().collect();
                self.rev += 1;
            }
            Err(e) => {
                crate::jarvis_ios_line!("[JarvisIOS] material visibility: JSON parse failed: {e}");
            }
        }
    }

    pub fn is_visible(&self, key: &str) -> bool {
        !self.hidden.contains(key)
    }

    pub fn set_visible(&mut self, key: impl Into<String>, visible: bool) {
        let key = key.into();
        if visible {
            self.hidden.remove(&key);
        } else {
            self.hidden.insert(key);
        }
        self.rev += 1;
    }

    pub fn show_all(&mut self) {
        if self.hidden.is_empty() {
            return;
        }
        self.hidden.clear();
        self.rev += 1;
    }

    pub fn hide_all(&mut self, keys: impl IntoIterator<Item = impl Into<String>>) {
        self.hidden = keys.into_iter().map(Into::into).collect();
        self.rev += 1;
    }

    pub fn invert(&mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) {
        for key in keys {
            let key = key.as_ref();
            if self.hidden.contains(key) {
                self.hidden.remove(key);
            } else {
                self.hidden.insert(key.to_string());
            }
        }
        self.rev += 1;
    }

    pub fn to_json_string(&self) -> String {
        let file = MaterialVisibilityFile {
            hidden: self.hidden.iter().cloned().collect::<Vec<_>>(),
        };
        serde_json::to_string(&file).unwrap_or_else(|_| r#"{"hidden":[]}"#.to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MaterialVisibilityFile {
    #[serde(default)]
    hidden: Vec<String>,
}

fn ios_mtoon_mesh_key(
    name: Option<&Name>,
    gltf_material: Option<&GltfMaterialName>,
    material_handle: &Handle<MToonMaterial>,
) -> String {
    if let Some(g) = gltf_material {
        return g.0.clone();
    }
    if let Some(n) = name {
        return n.as_str().to_string();
    }
    format!("MaterialAsset_{:?}", material_handle.id())
}

fn ios_std_mesh_key(
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

fn entity_under_vrm(
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
pub(crate) struct IosMaterialVisibilityApplyCursor {
    rev: u64,
    mesh_count: usize,
}

/// Apply hidden materials every frame when the store revision or mesh count changes.
pub fn ios_apply_material_visibility(
    store: Res<IosMaterialVisibilityStore>,
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
    mut applied: Local<IosMaterialVisibilityApplyCursor>,
) {
    let vrm_roots: HashSet<Entity> = vrm_roots_q.iter().collect();
    if vrm_roots.is_empty() {
        return;
    }

    let mesh_count = mtoon_meshes.iter().count() + std_meshes.iter().count();
    if store.rev == applied.rev && mesh_count == applied.mesh_count {
        return;
    }
    applied.rev = store.rev;
    applied.mesh_count = mesh_count;

    for (entity, name, gltf_name, handle, mut vis) in &mut mtoon_meshes {
        if !entity_under_vrm(entity, &child_of_q, &vrm_roots) {
            continue;
        }
        let key = ios_mtoon_mesh_key(name, gltf_name, &handle.0);
        *vis = if store.hidden.contains(&key) {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }

    for (entity, name, gltf_name, handle, mut vis) in &mut std_meshes {
        if !entity_under_vrm(entity, &child_of_q, &vrm_roots) {
            continue;
        }
        let key = ios_std_mesh_key(name, gltf_name, &handle.0);
        *vis = if store.hidden.contains(&key) {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
}

/// Sorted, deduplicated material keys for meshes under the active VRM.
pub fn collect_vrm_material_keys(
    vrm_roots: &HashSet<Entity>,
    child_of: &Query<&ChildOf>,
    mtoon_meshes_q: &Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
    std_meshes_q: &Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) -> Vec<String> {
    let mut keys: Vec<String> = mtoon_meshes_q
        .iter()
        .filter(|(e, ..)| entity_under_vrm(*e, child_of, vrm_roots))
        .map(|(_, name, gltf, h)| ios_mtoon_mesh_key(name, gltf, &h.0))
        .chain(
            std_meshes_q
                .iter()
                .filter(|(e, ..)| entity_under_vrm(*e, child_of, vrm_roots))
                .map(|(_, name, gltf, h)| ios_std_mesh_key(name, gltf, &h.0)),
        )
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Seed the runtime store from the hub manifest when a VRM finishes loading.
pub fn ios_seed_material_visibility_on_vrm_ready(
    manifest: Res<IosMaterialVisibilityJson>,
    vrm_ready: Query<(), (With<Vrm>, Added<Initialized>)>,
    mut store: ResMut<IosMaterialVisibilityStore>,
) {
    if vrm_ready.is_empty() {
        return;
    }
    if let Some(json) = manifest.0.as_deref() {
        store.load_from_json_str(json);
        crate::jarvis_ios_line!(
            "[JarvisIOS] material visibility: seeded {} hidden from manifest",
            store.hidden.len()
        );
    }
}
