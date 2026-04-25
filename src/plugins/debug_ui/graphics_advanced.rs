//! Graphics Advanced window: render-pipeline knobs (tonemap, bloom, AA,
//! environment map), the three-light rig, and the per-material MToon
//! overrides editor.
//!
//! Everything here mutates [`jarvis_avatar::config::Settings`] (so hits are
//! persisted through "Save settings") and the [`MToonOverridesStore`]
//! resource (which writes its own JSON sidecar immediately).

use std::collections::HashSet;

use bevy::gltf::GltfMaterialName;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use bevy_vrm1::prelude::{MToonMaterial, Vrm};

use jarvis_avatar::config::{
    msaa_allows_ssao, BloomSettings, GraphicsAdvancedSettings, LightRigSettings, LightSpec,
    Settings,
};

use crate::plugins::mtoon_overrides::{
    apply_override_entry, mtoon_mesh_override_key, MToonOverrideEntry, MToonOverridesStore,
};

use super::widgets::{rgb_row, rgba_row, vec3_row};

/// Transient per-window state. Persists only while the UI is open — actual
/// override entries live on [`MToonOverridesStore`] and on disk.
#[derive(Default)]
pub struct GraphicsAdvancedUiState {
    pub selected_material: Option<String>,
    pub draft: Option<MaterialDraft>,
    /// Snapshot of the selected [`MToonMaterial`] when the draft was created, so live
    /// preview can reset then re-apply `draft.to_entry()` every frame (unchecked fields
    /// revert correctly).
    pub mtoon_preview_baseline: Option<MToonMaterial>,
    pub save_status: Option<String>,
}

/// Working copy the user edits in the MToon panel. The "Save to overrides"
/// button turns this into a [`MToonOverrideEntry`] and hands it to the store.
#[derive(Clone, Debug)]
pub struct MaterialDraft {
    pub material_name: String,
    pub base_color: [f32; 4],
    pub override_base_color: bool,
    pub emissive: [f32; 4],
    pub override_emissive: bool,
    pub shade_color: [f32; 4],
    pub override_shade_color: bool,
    pub shading_shift_factor: f32,
    pub override_shading_shift_factor: bool,
    pub toony_factor: f32,
    pub override_toony_factor: bool,
    pub rim_color: [f32; 4],
    pub override_rim_color: bool,
    pub rim_fresnel_power: f32,
    pub override_rim_fresnel_power: bool,
    pub rim_lift_factor: f32,
    pub override_rim_lift_factor: bool,
    pub rim_mix_factor: f32,
    pub override_rim_mix_factor: bool,
    pub outline_mode_world: bool,
    pub override_outline_mode: bool,
    pub outline_width_factor: f32,
    pub override_outline_width_factor: bool,
    pub outline_color: [f32; 4],
    pub override_outline_color: bool,
    pub outline_lighting_mix_factor: f32,
    pub override_outline_lighting_mix_factor: bool,
}

impl MaterialDraft {
    fn from_material(name: &str, m: &MToonMaterial, existing: Option<&MToonOverrideEntry>) -> Self {
        let base_color = color_to_arr(m.base_color);
        let emissive = [m.emissive.red, m.emissive.green, m.emissive.blue, m.emissive.alpha];
        let shade_color = [
            m.shade.color.red,
            m.shade.color.green,
            m.shade.color.blue,
            m.shade.color.alpha,
        ];
        let rim_color = [
            m.rim_lighting.color.red,
            m.rim_lighting.color.green,
            m.rim_lighting.color.blue,
            m.rim_lighting.color.alpha,
        ];
        let outline_color = [
            m.outline.color.red,
            m.outline.color.green,
            m.outline.color.blue,
            m.outline.color.alpha,
        ];
        let outline_mode_world = format!("{:?}", m.outline.mode).contains("World");

        let mut d = Self {
            material_name: name.to_string(),
            base_color,
            override_base_color: false,
            emissive,
            override_emissive: false,
            shade_color,
            override_shade_color: false,
            shading_shift_factor: m.shade.shading_shift_factor,
            override_shading_shift_factor: false,
            toony_factor: m.shade.toony_factor,
            override_toony_factor: false,
            rim_color,
            override_rim_color: false,
            rim_fresnel_power: m.rim_lighting.fresnel_power,
            override_rim_fresnel_power: false,
            rim_lift_factor: m.rim_lighting.lift_factor,
            override_rim_lift_factor: false,
            rim_mix_factor: m.rim_lighting.mix_factor,
            override_rim_mix_factor: false,
            outline_mode_world,
            override_outline_mode: false,
            outline_width_factor: m.outline.width_factor,
            override_outline_width_factor: false,
            outline_color,
            override_outline_color: false,
            outline_lighting_mix_factor: m.outline.lighting_mix_factor,
            override_outline_lighting_mix_factor: false,
        };
        if let Some(e) = existing {
            if let Some(v) = e.base_color {
                d.base_color = v;
                d.override_base_color = true;
            }
            if let Some(v) = e.emissive {
                d.emissive = v;
                d.override_emissive = true;
            }
            if let Some(v) = e.shade_color {
                d.shade_color = v;
                d.override_shade_color = true;
            }
            if let Some(v) = e.shading_shift_factor {
                d.shading_shift_factor = v;
                d.override_shading_shift_factor = true;
            }
            if let Some(v) = e.toony_factor {
                d.toony_factor = v;
                d.override_toony_factor = true;
            }
            if let Some(v) = e.rim_color {
                d.rim_color = v;
                d.override_rim_color = true;
            }
            if let Some(v) = e.rim_fresnel_power {
                d.rim_fresnel_power = v;
                d.override_rim_fresnel_power = true;
            }
            if let Some(v) = e.rim_lift_factor {
                d.rim_lift_factor = v;
                d.override_rim_lift_factor = true;
            }
            if let Some(v) = e.rim_mix_factor {
                d.rim_mix_factor = v;
                d.override_rim_mix_factor = true;
            }
            if let Some(mode) = e.outline_mode.as_deref() {
                d.outline_mode_world = matches!(mode, "worldCoordinates" | "WorldCoordinates" | "world");
                d.override_outline_mode = true;
            }
            if let Some(v) = e.outline_width_factor {
                d.outline_width_factor = v;
                d.override_outline_width_factor = true;
            }
            if let Some(v) = e.outline_color {
                d.outline_color = v;
                d.override_outline_color = true;
            }
            if let Some(v) = e.outline_lighting_mix_factor {
                d.outline_lighting_mix_factor = v;
                d.override_outline_lighting_mix_factor = true;
            }
        }
        d
    }

    fn to_entry(&self) -> MToonOverrideEntry {
        MToonOverrideEntry {
            base_color: self.override_base_color.then_some(self.base_color),
            emissive: self.override_emissive.then_some(self.emissive),
            shade_color: self.override_shade_color.then_some(self.shade_color),
            shading_shift_factor: self
                .override_shading_shift_factor
                .then_some(self.shading_shift_factor),
            toony_factor: self.override_toony_factor.then_some(self.toony_factor),
            rim_color: self.override_rim_color.then_some(self.rim_color),
            rim_fresnel_power: self
                .override_rim_fresnel_power
                .then_some(self.rim_fresnel_power),
            rim_lift_factor: self.override_rim_lift_factor.then_some(self.rim_lift_factor),
            rim_mix_factor: self.override_rim_mix_factor.then_some(self.rim_mix_factor),
            outline_mode: self.override_outline_mode.then(|| {
                if self.outline_mode_world {
                    "worldCoordinates".to_string()
                } else {
                    "none".to_string()
                }
            }),
            outline_width_factor: self
                .override_outline_width_factor
                .then_some(self.outline_width_factor),
            outline_color: self.override_outline_color.then_some(self.outline_color),
            outline_lighting_mix_factor: self
                .override_outline_lighting_mix_factor
                .then_some(self.outline_lighting_mix_factor),
        }
    }
}

fn color_to_arr(c: Color) -> [f32; 4] {
    let l = c.to_linear();
    [l.red, l.green, l.blue, l.alpha]
}

/// True when `start` is the VRM root or one of its descendants in the `ChildOf` graph.
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

// ---------- draw ---------------------------------------------------------------

pub fn draw_graphics_advanced_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    materials: Res<Assets<MToonMaterial>>,
    mtoon_meshes_q: Query<
        (
            Entity,
            Option<&Name>,
            Option<&GltfMaterialName>,
            &MeshMaterial3d<MToonMaterial>,
        ),
    >,
    vrm_roots_q: Query<Entity, With<Vrm>>,
    child_of_q: Query<&ChildOf>,
    std_meshes_q: Query<Entity, With<MeshMaterial3d<StandardMaterial>>>,
    store: Option<Res<MToonOverridesStore>>,
    mut state: ResMut<super::DebugUiState>,
) {
    if !settings.ui.show_graphics_advanced {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let vrm_roots: HashSet<Entity> = vrm_roots_q.iter().collect();
    let mtoon_under_vrm = mtoon_meshes_q
        .iter()
        .filter(|(e, ..)| entity_under_vrm(*e, &child_of_q, &vrm_roots))
        .count();
    let std_under_vrm = std_meshes_q
        .iter()
        .filter(|e| entity_under_vrm(*e, &child_of_q, &vrm_roots))
        .count();

    let mut open = settings.ui.show_graphics_advanced;
    egui::Window::new("Graphics Advanced")
        .default_size([520.0, 620.0])
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                draw_post_process(
                    ui,
                    settings.graphics.msaa_samples,
                    &mut settings.graphics.advanced,
                );
                ui.separator();
                draw_light_rig(ui, &mut settings.light_rig);
                ui.separator();
                draw_mtoon_editor(
                    ui,
                    &mut state.graphics_advanced,
                    &materials,
                    &mtoon_meshes_q,
                    store.as_deref(),
                    (!vrm_roots.is_empty()).then_some((std_under_vrm, mtoon_under_vrm)),
                );
            });
        });
    settings.ui.show_graphics_advanced = open;
}

fn draw_post_process(ui: &mut egui::Ui, msaa_samples: u32, adv: &mut GraphicsAdvancedSettings) {
    ui.heading("Post-processing");
    egui::ComboBox::from_label("Tonemapping")
        .selected_text(adv.tonemapping.clone())
        .show_ui(ui, |ui| {
            for name in [
                "None",
                "Reinhard",
                "ReinhardLuminance",
                "AcesFitted",
                "AgX",
                "SomewhatBoringDisplayTransform",
                "TonyMcMapface",
                "BlenderFilmic",
            ] {
                ui.selectable_value(&mut adv.tonemapping, name.to_string(), name);
            }
        });

    ui.separator();
    draw_bloom(ui, &mut adv.bloom);

    ui.separator();
    egui::ComboBox::from_label("SMAA preset")
        .selected_text(adv.smaa_preset.clone())
        .show_ui(ui, |ui| {
            for name in ["Low", "Medium", "High", "Ultra"] {
                ui.selectable_value(&mut adv.smaa_preset, name.to_string(), name);
            }
        });
    ui.checkbox(&mut adv.fxaa_enabled, "FXAA")
        .on_hover_text("Cheap AA fallback; can blur details.");
    ui.checkbox(&mut adv.auto_exposure, "AutoExposure")
        .on_hover_text("Requires HDR.");

    ui.separator();
    ui.label("Environment map")
        .on_hover_text("Asset stem only: <stem>_diffuse.ktx2 + <stem>_specular.ktx2");
    ui.text_edit_singleline(&mut adv.environment_map);
    ui.add(
        egui::Slider::new(&mut adv.environment_intensity, 0.0..=80.0)
            .text("environment_intensity"),
    );

    ui.separator();
    ui.label("SSAO")
        .on_hover_text("Screen-space ambient occlusion; requires MSAA off.");
    let ssao_allowed = msaa_allows_ssao(msaa_samples);
    if !ssao_allowed {
        ui.colored_label(
            egui::Color32::from_rgb(210, 160, 90),
            "SSAO requires MSAA off (set msaa_samples to 0 under Graphics / lights). Toggle is disabled while MSAA ≥ 2.",
        );
    }
    ui.add_enabled(ssao_allowed, egui::Checkbox::new(&mut adv.ssao_enabled, "enabled"));
    ui.add_enabled_ui(ssao_allowed, |ui| {
        egui::ComboBox::from_label("ssao_quality")
            .selected_text(adv.ssao_quality.clone())
            .show_ui(ui, |ui| {
                for name in ["Low", "Medium", "High", "Ultra"] {
                    ui.selectable_value(&mut adv.ssao_quality, name.to_string(), name);
                }
            });
        ui.add(
            egui::Slider::new(&mut adv.ssao_constant_object_thickness, 0.02..=2.0)
                .text("object_thickness (AO radius / self-occlusion tradeoff)"),
        );
    });
}

fn draw_bloom(ui: &mut egui::Ui, b: &mut BloomSettings) {
    ui.checkbox(&mut b.enabled, "enabled");
    ui.add(egui::Slider::new(&mut b.intensity, 0.0..=1.0).text("intensity"));
    ui.add(
        egui::Slider::new(&mut b.low_frequency_boost, 0.0..=1.5)
            .text("low_frequency_boost"),
    );
    ui.add(
        egui::Slider::new(&mut b.high_pass_frequency, 0.0..=1.0).text("high_pass_frequency"),
    );
    ui.add(egui::Slider::new(&mut b.threshold, 0.0..=5.0).text("threshold"));
    ui.add(
        egui::Slider::new(&mut b.threshold_softness, 0.0..=1.0).text("threshold_softness"),
    );
    egui::ComboBox::from_label("composite_mode")
        .selected_text(b.composite_mode.clone())
        .show_ui(ui, |ui| {
            for name in ["energy_conserving", "additive"] {
                ui.selectable_value(&mut b.composite_mode, name.to_string(), name);
            }
        });
}

fn draw_light_rig(ui: &mut egui::Ui, rig: &mut LightRigSettings) {
    ui.heading("Light rig")
        .on_hover_text("Three directional lights: key, fill, and rim.");
    ui.checkbox(&mut rig.enabled, "enable rig (disables default sun)");
    ui.collapsing("Key light", |ui| draw_light_spec(ui, "key", &mut rig.key));
    ui.collapsing("Fill light", |ui| draw_light_spec(ui, "fill", &mut rig.fill));
    ui.collapsing("Rim light", |ui| draw_light_spec(ui, "rim", &mut rig.rim));
}

fn draw_light_spec(ui: &mut egui::Ui, tag: &str, l: &mut LightSpec) {
    ui.checkbox(&mut l.enabled, format!("{tag}.enabled"));
    ui.add(
        egui::Slider::new(&mut l.illuminance, 0.0..=50_000.0)
            .logarithmic(true)
            .text(format!("{tag}.illuminance")),
    );
    ui.label(format!("{tag}.direction (pointing towards)"));
    vec3_row(ui, &format!("{tag}_dir"), &mut l.direction, -5.0..=5.0);
    ui.label(format!("{tag}.color (linear RGB)"));
    rgb_row(ui, &mut l.color);
    ui.checkbox(&mut l.shadows, format!("{tag}.shadows"));
}

fn draw_mtoon_editor(
    ui: &mut egui::Ui,
    state: &mut GraphicsAdvancedUiState,
    materials: &Assets<MToonMaterial>,
    meshes_q: &Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
    store: Option<&MToonOverridesStore>,
    vrm_mesh_counts: Option<(usize, usize)>,
) {
    ui.heading("MToon per-material overrides");
    let Some(store) = store else {
        ui.label("MToonOverridesStore not initialised yet.");
        return;
    };

    let mut choices: Vec<(String, Handle<MToonMaterial>)> = meshes_q
        .iter()
        .map(|(_, name, gltf_name, h)| {
            (
                mtoon_mesh_override_key(name, gltf_name, &h.0),
                h.0.clone(),
            )
        })
        .collect();
    choices.sort_by(|a, b| a.0.cmp(&b.0));
    choices.dedup_by(|a, b| a.0 == b.0);

    if choices.is_empty() {
        ui.label("No MToon materials found in the scene.");
        ui.small("No MToon materials found.");
        if let Some((std_n, mtoon_n)) = vrm_mesh_counts {
            if mtoon_n == 0 && std_n > 0 {
                ui.small("Avatar currently uses StandardMaterial slots.");
            }
        }
        return;
    }

    let current = state
        .selected_material
        .clone()
        .unwrap_or_else(|| choices[0].0.clone());
    egui::ComboBox::from_label("Material")
        .selected_text(current.clone())
        .show_ui(ui, |ui| {
            for (n, _) in &choices {
                if ui
                    .selectable_label(state.selected_material.as_deref() == Some(n), n)
                    .clicked()
                {
                    state.selected_material = Some(n.clone());
                    state.draft = None;
                }
            }
        });

    let Some(selected_name) = state.selected_material.clone().or_else(|| Some(current)) else {
        return;
    };
    let Some((_, handle)) = choices.iter().find(|(n, _)| n == &selected_name) else {
        return;
    };
    let Some(material) = materials.get(handle) else {
        ui.label("Material asset not yet loaded.");
        return;
    };

    if state
        .draft
        .as_ref()
        .map(|d| d.material_name != selected_name)
        .unwrap_or(true)
    {
        state.mtoon_preview_baseline = Some(material.clone());
        state.draft = Some(MaterialDraft::from_material(
            &selected_name,
            material,
            store.entry(&selected_name).as_ref(),
        ));
    }

    let Some(draft) = state.draft.as_mut() else {
        return;
    };

    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_base_color, "base_color");
        ui.add_enabled_ui(draft.override_base_color, |ui| rgba_row(ui, &mut draft.base_color));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_emissive, "emissive");
        ui.add_enabled_ui(draft.override_emissive, |ui| rgba_row(ui, &mut draft.emissive));
    });

    ui.separator();
    ui.label("Shade");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_shade_color, "shade_color");
        ui.add_enabled_ui(draft.override_shade_color, |ui| rgba_row(ui, &mut draft.shade_color));
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_shading_shift_factor,
            "shading_shift_factor",
        );
        ui.add_enabled(
            draft.override_shading_shift_factor,
            egui::Slider::new(&mut draft.shading_shift_factor, -1.0..=1.0),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_toony_factor, "toony_factor");
        ui.add_enabled(
            draft.override_toony_factor,
            egui::Slider::new(&mut draft.toony_factor, 0.0..=1.0),
        );
    });

    ui.separator();
    ui.label("Rim");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_color, "rim_color");
        ui.add_enabled_ui(draft.override_rim_color, |ui| rgba_row(ui, &mut draft.rim_color));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_fresnel_power, "rim_fresnel_power");
        ui.add_enabled(
            draft.override_rim_fresnel_power,
            egui::Slider::new(&mut draft.rim_fresnel_power, 0.0..=16.0),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_lift_factor, "rim_lift_factor");
        ui.add_enabled(
            draft.override_rim_lift_factor,
            egui::Slider::new(&mut draft.rim_lift_factor, 0.0..=1.0),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_mix_factor, "rim_mix_factor");
        ui.add_enabled(
            draft.override_rim_mix_factor,
            egui::Slider::new(&mut draft.rim_mix_factor, 0.0..=1.0),
        );
    });

    ui.separator();
    ui.label("Outline")
        .on_hover_text("Try worldCoordinates with width around 0.002–0.01 depending on scale.");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_outline_mode, "outline_mode");
        ui.add_enabled(
            draft.override_outline_mode,
            egui::Checkbox::new(&mut draft.outline_mode_world, "worldCoordinates (off = None)"),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_outline_width_factor,
            "outline_width_factor",
        );
        let slider_enabled = draft.override_outline_width_factor
            && (!draft.override_outline_mode || draft.outline_mode_world);
        ui.add_enabled(
            slider_enabled,
            egui::Slider::new(&mut draft.outline_width_factor, 0.0..=0.1),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_outline_color, "outline_color");
        ui.add_enabled_ui(draft.override_outline_color, |ui| rgba_row(ui, &mut draft.outline_color));
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_outline_lighting_mix_factor,
            "outline_lighting_mix_factor",
        );
        ui.add_enabled(
            draft.override_outline_lighting_mix_factor,
            egui::Slider::new(&mut draft.outline_lighting_mix_factor, 0.0..=1.0),
        );
    });

    ui.small("Live preview; save to persist.");
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Save to overrides").clicked() {
            let entry = draft.to_entry();
            let name = draft.material_name.clone();
            state.save_status = Some(match store.upsert(&name, Some(entry)) {
                Ok(()) => format!("saved override for {name}"),
                Err(e) => format!("save failed: {e}"),
            });
        }
        if ui.button("Clear override").clicked() {
            let name = draft.material_name.clone();
            state.save_status = Some(match store.upsert(&name, None) {
                Ok(()) => format!("cleared override for {name}"),
                Err(e) => format!("clear failed: {e}"),
            });
        }
    });
    if let Some(status) = &state.save_status {
        ui.label(status);
    }
}

/// Runs after the Graphics Advanced egui pass so [`MaterialDraft`] reflects the
/// latest sliders, then pushes the working copy onto the selected asset handle.
pub fn apply_mtoon_material_live_preview(
    settings: Res<Settings>,
    mut debug: ResMut<super::DebugUiState>,
    mut materials: ResMut<Assets<MToonMaterial>>,
    meshes_q: Query<(
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
) {
    let ga = &mut debug.graphics_advanced;
    if !settings.ui.show_graphics_advanced {
        ga.mtoon_preview_baseline = None;
        return;
    }
    let Some(baseline) = ga.mtoon_preview_baseline.as_ref() else {
        return;
    };
    let Some(draft) = ga.draft.as_ref() else {
        return;
    };
    let key = draft.material_name.as_str();
    let mut found: Option<Handle<MToonMaterial>> = None;
    for (name, gltf, h) in &meshes_q {
        if mtoon_mesh_override_key(name, gltf, &h.0) == key {
            found = Some(h.0.clone());
            break;
        }
    }
    let Some(handle) = found else {
        return;
    };
    let Some(m) = materials.get_mut(&handle) else {
        return;
    };
    *m = baseline.clone();
    apply_override_entry(m, &draft.to_entry());
}
