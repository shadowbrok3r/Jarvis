//! Per-material MToon overrides loaded from a JSON sidecar and re-applied
//! whenever [`MToonMaterial`] assets change.
//!
//! The UI writes here by calling [`MToonOverridesStore::upsert`] with a
//! material name and a [`MToonOverrideEntry`]; the store writes the sidecar
//! immediately and flags materials dirty so they get re-applied next frame.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::color::LinearRgba;
use bevy::gltf::GltfMaterialName;
use bevy::prelude::*;
use bevy_vrm1::prelude::{MToonMaterial, MToonOutline, OutlineWidthMode, RimLighting, Shade};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use jarvis_avatar::config::Settings;

// ── Per-VRM ModelOverrides directory helpers ────────────────────────────────────────────────────

/// Returns `config/ModelOverrides/{stem}/` where stem is the VRM filename without extension.
/// e.g. `models/3.vrm` → `config/ModelOverrides/3/`
pub fn vrm_model_overrides_dir(model_path: &str) -> PathBuf {
    let stem = std::path::Path::new(model_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    std::env::current_dir()
        .unwrap_or_default()
        .join("config")
        .join("ModelOverrides")
        .join(stem)
}

/// Returns `config/ModelOverrides/{stem}/mtoon_overrides.json`.
pub fn vrm_model_mtoon_override_path(model_path: &str) -> PathBuf {
    vrm_model_overrides_dir(model_path).join("mtoon_overrides.json")
}

/// Returns `config/ModelOverrides/{stem}/graphics_overrides.json`.
pub fn vrm_model_graphics_override_path(model_path: &str) -> PathBuf {
    vrm_model_overrides_dir(model_path).join("graphics_overrides.json")
}

pub struct MToonOverridesPlugin;

impl Plugin for MToonOverridesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_overrides)
            .add_systems(Update, apply_overrides_on_material_change);
    }
}

/// The on-disk override structure. Entries omit fields they don't want to
/// override; only `Some(_)` values win against the bind-pose material loaded
/// from the `.vrm`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MToonOverridesFile {
    #[serde(default)]
    pub entries: HashMap<String, MToonOverrideEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MToonOverrideEntry {
    pub base_color: Option<[f32; 4]>,
    pub emissive: Option<[f32; 4]>,
    pub shade_color: Option<[f32; 4]>,
    pub shading_shift_factor: Option<f32>,
    pub toony_factor: Option<f32>,
    pub rim_color: Option<[f32; 4]>,
    pub rim_fresnel_power: Option<f32>,
    pub rim_lift_factor: Option<f32>,
    pub rim_mix_factor: Option<f32>,
    pub outline_mode: Option<String>,
    pub outline_width_factor: Option<f32>,
    pub outline_color: Option<[f32; 4]>,
    pub outline_lighting_mix_factor: Option<f32>,
}

/// Cloneable wrapper around the in-memory override table + the sidecar path.
/// Both the UI and `apply_overrides_on_material_change` hold a clone.
#[derive(Resource, Clone)]
pub struct MToonOverridesStore {
    pub path: PathBuf,
    inner: Arc<RwLock<MToonOverridesFile>>,
    /// Bumped any time the table mutates so `apply_…` knows to re-apply.
    rev: Arc<RwLock<u64>>,
}

impl MToonOverridesStore {
    fn new(path: PathBuf, file: MToonOverridesFile) -> Self {
        // Non-empty sidecar must not start at rev 0 with a default `Local` apply
        // cursor also at 0 — that combination skips the first apply forever.
        let start_rev = u64::from(!file.entries.is_empty());
        Self {
            path,
            inner: Arc::new(RwLock::new(file)),
            rev: Arc::new(RwLock::new(start_rev)),
        }
    }

    pub fn snapshot(&self) -> MToonOverridesFile {
        self.inner.read().clone()
    }

    pub fn revision(&self) -> u64 {
        *self.rev.read()
    }

    pub fn entry(&self, name: &str) -> Option<MToonOverrideEntry> {
        self.inner.read().entries.get(name).cloned()
    }

    /// Replace (or remove, when `entry` is `None`) the override for `name` and
    /// persist the sidecar. Returns the disk write result.
    pub fn upsert(
        &self,
        name: impl Into<String>,
        entry: Option<MToonOverrideEntry>,
    ) -> std::io::Result<()> {
        {
            let mut guard = self.inner.write();
            let name = name.into();
            match entry {
                Some(e) => {
                    guard.entries.insert(name, e);
                }
                None => {
                    guard.entries.remove(&name);
                }
            }
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

fn load_mtoon_overrides_file(path: &PathBuf) -> MToonOverridesFile {
    match fs::read_to_string(path) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(f) => f,
            Err(e) => {
                warn!("mtoon overrides: parse error for {path:?}: {e}");
                MToonOverridesFile::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MToonOverridesFile::default(),
        Err(e) => {
            warn!("mtoon overrides: read {path:?}: {e}");
            MToonOverridesFile::default()
        }
    }
}

fn load_overrides(mut commands: Commands, settings: Res<Settings>) {
    let per_vrm_path = vrm_model_mtoon_override_path(&settings.avatar.model_path);
    // Load from per-VRM file if it exists, else from the global sidecar.
    // Saves always target the per-VRM file so the first "Save to overrides" creates it.
    let file = if per_vrm_path.is_file() {
        let file = load_mtoon_overrides_file(&per_vrm_path);
        info!("mtoon overrides: loaded per-VRM overrides from {per_vrm_path:?}");
        file
    } else {
        let global_path = PathBuf::from(&settings.mtoon_overrides.path);
        let file = load_mtoon_overrides_file(&global_path);
        if !file.entries.is_empty() {
            info!(
                "mtoon overrides: no per-VRM file at {per_vrm_path:?}, \
                 loaded global overrides from {global_path:?}"
            );
        }
        file
    };
    // Store always points to the per-VRM path so "Save to overrides" writes there.
    commands.insert_resource(MToonOverridesStore::new(per_vrm_path, file));
}

/// Stable JSON key for one mesh primitive's MToon material (matches Graphics
/// Advanced picker). Prefer glTF material `name` when Bevy recorded it.
pub fn mtoon_mesh_override_key(
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

/// Last store revision + MToon mesh count applied to `Assets<MToonMaterial>`.
#[derive(Default)]
struct MToonApplyCursor {
    rev: u64,
    mtoon_mesh_count: usize,
}

fn apply_overrides_on_material_change(
    store: Option<Res<MToonOverridesStore>>,
    mut materials: ResMut<Assets<MToonMaterial>>,
    meshes_q: Query<(
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
    mut applied: Local<MToonApplyCursor>,
) {
    let Some(store) = store else {
        return;
    };
    let mesh_count = meshes_q.iter().count();
    let current_rev = store.revision();
    let file = store.snapshot();

    if file.entries.is_empty() {
        if current_rev != applied.rev || mesh_count != applied.mtoon_mesh_count {
            applied.rev = current_rev;
            applied.mtoon_mesh_count = mesh_count;
        }
        return;
    }

    if current_rev == applied.rev && mesh_count == applied.mtoon_mesh_count {
        return;
    }
    applied.rev = current_rev;
    applied.mtoon_mesh_count = mesh_count;

    let mut touched = 0usize;
    for (name, gltf_name, handle) in &meshes_q {
        let key = mtoon_mesh_override_key(name, gltf_name, &handle.0);
        let Some(entry) = file.entries.get(&key) else {
            continue;
        };
        let Some(material) = materials.get_mut(&handle.0) else {
            continue;
        };
        apply_override_entry(material, entry);
        touched += 1;
    }
    if touched > 0 {
        info!("mtoon overrides: applied to {touched} material(s) (rev {current_rev})");
    }
}

/// Applies JSON override fields onto a material (used by the live preview path
/// in Graphics Advanced as well as the disk-backed apply system).
pub fn apply_override_entry(material: &mut MToonMaterial, entry: &MToonOverrideEntry) {
    if let Some([r, g, b, a]) = entry.base_color {
        material.base_color = Color::linear_rgba(r, g, b, a);
    }
    if let Some([r, g, b, a]) = entry.emissive {
        material.emissive = LinearRgba::new(r, g, b, a);
    }
    apply_shade(&mut material.shade, entry);
    apply_rim(&mut material.rim_lighting, entry);
    apply_outline(&mut material.outline, entry);
}

fn apply_shade(shade: &mut Shade, entry: &MToonOverrideEntry) {
    if let Some([r, g, b, a]) = entry.shade_color {
        shade.color = LinearRgba::new(r, g, b, a);
    }
    if let Some(v) = entry.shading_shift_factor {
        shade.shading_shift_factor = v;
    }
    if let Some(v) = entry.toony_factor {
        shade.toony_factor = v;
    }
}

fn apply_rim(rim: &mut RimLighting, entry: &MToonOverrideEntry) {
    if let Some([r, g, b, a]) = entry.rim_color {
        rim.color = LinearRgba::new(r, g, b, a);
    }
    if let Some(v) = entry.rim_fresnel_power {
        rim.fresnel_power = v;
    }
    if let Some(v) = entry.rim_lift_factor {
        rim.lift_factor = v;
    }
    if let Some(v) = entry.rim_mix_factor {
        rim.mix_factor = v;
    }
}

fn apply_outline(outline: &mut MToonOutline, entry: &MToonOverrideEntry) {
    if let Some(mode) = entry.outline_mode.as_deref() {
        outline.mode = match mode {
            "worldCoordinates" | "WorldCoordinates" | "world" => OutlineWidthMode::WorldCoordinates,
            _ => OutlineWidthMode::None,
        };
    }
    if let Some(v) = entry.outline_width_factor {
        outline.width_factor = v;
    }
    if let Some([r, g, b, a]) = entry.outline_color {
        outline.color = LinearRgba::new(r, g, b, a);
    }
    if let Some(v) = entry.outline_lighting_mix_factor {
        outline.lighting_mix_factor = v;
    }
}
