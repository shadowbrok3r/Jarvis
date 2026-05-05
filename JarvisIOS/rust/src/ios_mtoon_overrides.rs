//! Per-material MToon overrides received from the hub manifest and applied on iOS.
//!
//! The desktop hub inlines `mtoon_overrides_json` (same JSON format as
//! `config/mtoon_overrides.json` and `config/ModelOverrides/{stem}/mtoon_overrides.json`)
//! directly into the profile manifest so iOS never needs a separate file download.
//!
//! The apply system fires once per VRM load (`Added<Initialized>`) — the same trigger
//! pattern used by `ios_apply_spring_preset_on_vrm_ready` in `ios_bevy.rs`.

use std::collections::HashMap;

use bevy::color::LinearRgba;
use bevy::gltf::GltfMaterialName;
use bevy::prelude::*;
use bevy_vrm1::prelude::{Initialized, MToonMaterial, MToonOutline, OutlineWidthMode, RimLighting, Shade, Vrm};
use serde::{Deserialize, Serialize};

/// Inlined MToon overrides JSON from the hub manifest (set once on boot, updated on profile reload).
#[derive(Resource, Clone, Default)]
pub struct IosMToonOverridesJson(pub Option<String>);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IosMToonOverridesFile {
    #[serde(default)]
    pub entries: HashMap<String, IosMToonOverrideEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IosMToonOverrideEntry {
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

/// Bevy system: apply MToon overrides from the hub manifest whenever a VRM finishes loading.
///
/// Mirrors `apply_overrides_on_material_change` on desktop but is driven by the VRM `Initialized`
/// event rather than a polling cursor, since iOS has no hot-reload of the JSON sidecar.
pub fn ios_apply_mtoon_overrides_on_vrm_ready(
    overrides: Res<IosMToonOverridesJson>,
    vrm_ready: Query<(), (With<Vrm>, Added<Initialized>)>,
    mut materials: ResMut<Assets<MToonMaterial>>,
    meshes_q: Query<(
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
) {
    if vrm_ready.is_empty() {
        return;
    }
    let Some(json_str) = overrides.0.as_deref() else {
        crate::jarvis_ios_line!("[JarvisIOS] mtoon overrides: no JSON in manifest — using VRM defaults");
        return;
    };
    let file: IosMToonOverridesFile = match serde_json::from_str(json_str) {
        Ok(f) => f,
        Err(e) => {
            crate::jarvis_ios_line!("[JarvisIOS] mtoon overrides: JSON parse failed: {e}");
            return;
        }
    };
    if file.entries.is_empty() {
        crate::jarvis_ios_line!("[JarvisIOS] mtoon overrides: manifest JSON is empty — using VRM defaults");
        return;
    }

    let mut touched = 0usize;
    for (name, gltf_name, handle) in &meshes_q {
        let key = ios_mtoon_mesh_key(name, gltf_name, &handle.0);
        let Some(entry) = file.entries.get(&key) else {
            continue;
        };
        let Some(material) = materials.get_mut(&handle.0) else {
            continue;
        };
        apply_ios_mtoon_entry(material, entry);
        touched += 1;
    }
    crate::jarvis_ios_line!("[JarvisIOS] mtoon overrides: applied to {touched} material(s)");
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

fn apply_ios_mtoon_entry(material: &mut MToonMaterial, entry: &IosMToonOverrideEntry) {
    if let Some([r, g, b, a]) = entry.base_color {
        material.base_color = Color::linear_rgba(r, g, b, a);
    }
    if let Some([r, g, b, a]) = entry.emissive {
        material.emissive = LinearRgba::new(r, g, b, a);
    }
    apply_ios_shade(&mut material.shade, entry);
    apply_ios_rim(&mut material.rim_lighting, entry);
    apply_ios_outline(&mut material.outline, entry);
}

fn apply_ios_shade(shade: &mut Shade, entry: &IosMToonOverrideEntry) {
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

fn apply_ios_rim(rim: &mut RimLighting, entry: &IosMToonOverrideEntry) {
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

fn apply_ios_outline(outline: &mut MToonOutline, entry: &IosMToonOverrideEntry) {
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
