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
use bevy_egui::{EguiContexts, egui};
use bevy_vrm1::prelude::{MToonMaterial, Vrm};

use jarvis_avatar::config::{
    apply_character_showcase_lighting_preset, BloomSettings, LightRigSettings, LightSpec,
    Settings, msaa_allows_ssao,
};
use jarvis_avatar::icons;

use crate::plugins::graphics_advanced::EnvironmentMapStatus;
use crate::plugins::material_visibility::{
    MaterialVisibilityStore, std_mesh_material_key,
};
use crate::plugins::mtoon_overrides::{
    MToonOverrideEntry, MToonOverridesStore, apply_override_entry, mtoon_mesh_override_key,
};

use super::widgets::{rgb_row, rgba_row, vec3_row};

/// Tabs for the consolidated Graphics Workspace window.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsWsTab {
    #[default]
    Lights,
    Post,
    Materials,
    LookAt,
}

/// Transient per-window state. Persists only while the UI is open — actual
/// override entries live on [`MToonOverridesStore`] and on disk.
#[derive(Default)]
pub struct GraphicsAdvancedUiState {
    pub tab: GraphicsWsTab,
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
    pub gi_equalization_factor: f32,
    pub override_gi_equalization_factor: bool,
}

impl MaterialDraft {
    fn from_material(name: &str, m: &MToonMaterial, existing: Option<&MToonOverrideEntry>) -> Self {
        let base_color = color_to_arr(m.base_color);
        let emissive = [
            m.emissive.red,
            m.emissive.green,
            m.emissive.blue,
            m.emissive.alpha,
        ];
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
            gi_equalization_factor: m.gi_equalization_factor,
            override_gi_equalization_factor: false,
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
                d.outline_mode_world =
                    matches!(mode, "worldCoordinates" | "WorldCoordinates" | "world");
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
            if let Some(v) = e.gi_equalization_factor {
                d.gi_equalization_factor = v;
                d.override_gi_equalization_factor = true;
            }
        }
        d
    }

    /// After widgets render, compare to a pre-edit snapshot. Any field whose
    /// **value** changed (regardless of its toggle state) auto-ticks its
    /// `override_*` flag so Save persists what the user actually moved. The
    /// per-field toggle still lets the user manually opt out by un-checking it
    /// after editing, or by clicking **Clear override** at the bottom.
    fn diff_and_tick(&mut self, prev: &MaterialDraft) {
        fn changed_vec(a: [f32; 4], b: [f32; 4]) -> bool {
            (a[0] - b[0]).abs() > f32::EPSILON
                || (a[1] - b[1]).abs() > f32::EPSILON
                || (a[2] - b[2]).abs() > f32::EPSILON
                || (a[3] - b[3]).abs() > f32::EPSILON
        }
        fn changed_f32(a: f32, b: f32) -> bool {
            (a - b).abs() > f32::EPSILON
        }
        if changed_vec(self.base_color, prev.base_color) {
            self.override_base_color = true;
        }
        if changed_vec(self.emissive, prev.emissive) {
            self.override_emissive = true;
        }
        if changed_vec(self.shade_color, prev.shade_color) {
            self.override_shade_color = true;
        }
        if changed_f32(self.shading_shift_factor, prev.shading_shift_factor) {
            self.override_shading_shift_factor = true;
        }
        if changed_f32(self.toony_factor, prev.toony_factor) {
            self.override_toony_factor = true;
        }
        if changed_vec(self.rim_color, prev.rim_color) {
            self.override_rim_color = true;
        }
        if changed_f32(self.rim_fresnel_power, prev.rim_fresnel_power) {
            self.override_rim_fresnel_power = true;
        }
        if changed_f32(self.rim_lift_factor, prev.rim_lift_factor) {
            self.override_rim_lift_factor = true;
        }
        if changed_f32(self.rim_mix_factor, prev.rim_mix_factor) {
            self.override_rim_mix_factor = true;
        }
        if self.outline_mode_world != prev.outline_mode_world {
            self.override_outline_mode = true;
        }
        if changed_f32(self.outline_width_factor, prev.outline_width_factor) {
            self.override_outline_width_factor = true;
        }
        if changed_vec(self.outline_color, prev.outline_color) {
            self.override_outline_color = true;
        }
        if changed_f32(
            self.outline_lighting_mix_factor,
            prev.outline_lighting_mix_factor,
        ) {
            self.override_outline_lighting_mix_factor = true;
        }
        if changed_f32(self.gi_equalization_factor, prev.gi_equalization_factor) {
            self.override_gi_equalization_factor = true;
        }
    }

    /// Number of `override_*` flags currently true — surfaced above the Save
    /// button so the user can see how many fields will actually be written.
    fn override_count(&self) -> usize {
        [
            self.override_base_color,
            self.override_emissive,
            self.override_shade_color,
            self.override_shading_shift_factor,
            self.override_toony_factor,
            self.override_rim_color,
            self.override_rim_fresnel_power,
            self.override_rim_lift_factor,
            self.override_rim_mix_factor,
            self.override_outline_mode,
            self.override_outline_width_factor,
            self.override_outline_color,
            self.override_outline_lighting_mix_factor,
            self.override_gi_equalization_factor,
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
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
            rim_lift_factor: self
                .override_rim_lift_factor
                .then_some(self.rim_lift_factor),
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
            gi_equalization_factor: self
                .override_gi_equalization_factor
                .then_some(self.gi_equalization_factor),
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
    env_status: Option<Res<EnvironmentMapStatus>>,
    materials: Res<Assets<MToonMaterial>>,
    mtoon_meshes_q: Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
    vrm_roots_q: Query<Entity, With<Vrm>>,
    child_of_q: Query<&ChildOf>,
    std_meshes_q: Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    store: Option<Res<MToonOverridesStore>>,
    vis_store: Option<Res<MaterialVisibilityStore>>,
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
        .filter(|(e, ..)| entity_under_vrm(*e, &child_of_q, &vrm_roots))
        .count();

    let mut open = settings.ui.show_graphics_advanced;
    egui::Window::new("Graphics Workspace")
        .default_size([540.0, 620.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let tab = &mut state.graphics_advanced.tab;
            ui.horizontal(|ui| {
                ui.selectable_value(tab, GraphicsWsTab::Lights, "Lights");
                ui.selectable_value(tab, GraphicsWsTab::Post, "Post");
                ui.selectable_value(tab, GraphicsWsTab::Materials, "Materials");
                ui.selectable_value(tab, GraphicsWsTab::LookAt, "Look-at");
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match state.graphics_advanced.tab {
                    GraphicsWsTab::Lights => {
                        super::sections::draw_basic_graphics_inline(ui, &mut settings);
                        ui.separator();
                        draw_light_rig(ui, &mut settings.light_rig);
                    }
                    GraphicsWsTab::Post => {
                        draw_post_process(ui, &mut settings, env_status.as_deref());
                    }
                    GraphicsWsTab::Materials => {
                        draw_material_visibility(
                            ui,
                            &vrm_roots,
                            &child_of_q,
                            &mtoon_meshes_q,
                            &std_meshes_q,
                            vis_store.as_deref(),
                        );
                        ui.separator();
                        draw_mtoon_editor(
                            ui,
                            &mut state.graphics_advanced,
                            &materials,
                            &mtoon_meshes_q,
                            store.as_deref(),
                            (!vrm_roots.is_empty()).then_some((std_under_vrm, mtoon_under_vrm)),
                        );
                    }
                    GraphicsWsTab::LookAt => {
                        super::sections::look_at_panel(ui, &mut settings);
                    }
                });
        });
    settings.ui.show_graphics_advanced = open;
}

fn draw_post_process(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    env_status: Option<&EnvironmentMapStatus>,
) {
    let mut apply_preset = false;
    ui.horizontal(|ui| {
        if ui.button("Showcase lighting preset").clicked() {
            apply_preset = true;
        }
    });
    if apply_preset {
        apply_character_showcase_lighting_preset(settings);
    }
    let msaa_samples = settings.graphics.msaa_samples;
    let ambient_brightness = settings.graphics.ambient_brightness;
    let graphics = &mut settings.graphics;
    let adv = &mut graphics.advanced;
    ui.separator();
    ui.heading("Ambient");
    ui.add(egui::Slider::new(&mut graphics.ambient_brightness, 0.0..=1.5).text("brightness"));
    rgba_row(ui, &mut graphics.ambient_color);
    ui.add(egui::Slider::new(&mut graphics.exposure_ev100, -6.0..=17.0).text("exposure_ev100"));
    ui.separator();
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
    ui.label("Environment map (IBL)");
    ui.checkbox(&mut adv.environment_map_enabled, "Attach to camera")
        .on_hover_text("Uncheck to A/B compare with flat ambient only.");
    ui.checkbox(&mut adv.environment_map_follow_camera, "Follow camera (skybox)")
        .on_hover_text(
            "Cubemap rotates with the view. Turn off for world-fixed studio orientation.",
        );
    ui.text_edit_singleline(&mut adv.environment_map)
        .on_hover_text(format!("assets/<stem>_diffuse.ktx2 + <stem>_specular.ktx2 (e.g. maps/ {} maps/_diffuse.ktx2)", icons::ARROW_RIGHT));
    ui.add(
        egui::Slider::new(&mut adv.environment_intensity, 0.0..=500.0).text("intensity (nits)"),
    );
    ui.add(
        egui::Slider::new(&mut adv.environment_map_mtoon_boost, 0.5..=12.0).text("mtoon_boost"),
    );
    ui.add(
        egui::Slider::new(&mut adv.environment_map_mtoon_body_gain, 0.5..=16.0)
            .text("mtoon_body_gain"),
    )
    .on_hover_text("Extra cubemap strength on MToon only (debug PBR sphere ignores this).");
    ui.add(
        egui::Slider::new(&mut adv.environment_map_rotation_yaw_deg, 0.0..=360.0)
            .text("yaw offset °"),
    )
    .on_hover_text("Added on top of camera rotation when Follow camera is on.");
    ui.add(
        egui::Slider::new(&mut adv.environment_ambient_scale_when_active, 0.0..=1.0)
            .text("ambient_scale_when_ibl"),
    )
    .on_hover_text(
        "While attached: flat ambient = ambient_brightness × this. \
         Drag to 0 with intensity at 30+ and orbit behind the model.",
    );
    ui.checkbox(&mut adv.environment_map_debug_sphere, "IBL debug sphere (PBR)")
        .on_hover_text(
            "White StandardMaterial ball to the right — if only the ball reacts to IBL, maps are fine and MToon path is the issue.",
        );
    ui.checkbox(&mut adv.environment_map_debug_visualize, "MToon IBL debug tint")
        .on_hover_text(
            "Shows raw cubemap color on the avatar (toggle off after test). Orbit to see it change.",
        );
    ui.horizontal(|ui| {
        if ui.button("IBL off (intensity 0)").clicked() {
            adv.environment_intensity = 0.0;
        }
        if ui.button("IBL studio (~30 nits)").clicked() {
            adv.environment_intensity = 30.0;
        }
    });
    if let Some(st) = env_status {
        let color = if st.attached && st.diffuse_is_cubemap {
            egui::Color32::from_rgb(120, 200, 140)
        } else {
            egui::Color32::from_rgb(210, 160, 90)
        };
        ui.colored_label(color, &st.message);
        ui.small(format!(
            "Diffuse: {} · {} · {} bytes · sample lum {:.4}",
            st.diffuse_load_state,
            st.diffuse_format,
            st.diffuse_data_bytes,
            st.diffuse_sample_luminance,
        ));
        if st.attached {
            ui.small(format!(
                "Live: {:.1} nits on camera · yaw {:.0}° · ambient {:.2}",
                st.camera_intensity_nits,
                st.rotation_yaw_deg,
                st.effective_ambient_brightness,
            ));
        }
        if st.diffuse_loaded && !st.maps_look_valid {
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                "Maps load but look empty on CPU — re-export with Khronos glTF IBL Sampler.",
            );
        }
        if ambient_brightness > 1.5 && !adv.environment_map.trim().is_empty() {
            ui.small("Tip: lower ambient_brightness (Graphics section) or ambient_scale_when_ibl.");
        }
    }

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
    ui.add_enabled(
        ssao_allowed,
        egui::Checkbox::new(&mut adv.ssao_enabled, "enabled"),
    );
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
    ui.add(egui::Slider::new(&mut b.low_frequency_boost, 0.0..=1.5).text("low_frequency_boost"));
    ui.add(egui::Slider::new(&mut b.high_pass_frequency, 0.0..=1.0).text("high_pass_frequency"));
    ui.add(egui::Slider::new(&mut b.threshold, 0.0..=5.0).text("threshold"));
    ui.add(egui::Slider::new(&mut b.threshold_softness, 0.0..=1.0).text("threshold_softness"));
    egui::ComboBox::from_label("composite_mode")
        .selected_text(b.composite_mode.clone())
        .show_ui(ui, |ui| {
            for name in ["energy_conserving", "additive"] {
                ui.selectable_value(&mut b.composite_mode, name.to_string(), name);
            }
        });
}

fn draw_light_rig(ui: &mut egui::Ui, rig: &mut LightRigSettings) {
    ui.heading("Light rig");
    ui.checkbox(&mut rig.enabled, "enable rig");
    ui.checkbox(&mut rig.show_light_gizmos, "show light gizmos");
    ui.add(egui::Slider::new(&mut rig.gizmo_distance, 0.5..=8.0).text("gizmo distance"));
    ui.checkbox(&mut rig.use_avatar_focus_for_gizmos, "anchor gizmos on VRM");
    ui.collapsing("Key light", |ui| draw_light_spec(ui, "key", &mut rig.key));
    ui.collapsing("Fill light", |ui| {
        draw_light_spec(ui, "fill", &mut rig.fill)
    });
    ui.collapsing("Rim light (silhouette)", |ui| {
        draw_light_spec(ui, "rim", &mut rig.rim)
    });
    ui.collapsing("Back light (hair / cape)", |ui| {
        draw_light_spec(ui, "back", &mut rig.back)
    });
}

fn draw_light_spec(ui: &mut egui::Ui, tag: &str, l: &mut LightSpec) {
    ui.checkbox(&mut l.enabled, format!("{tag}.enabled"));
    ui.add(
        egui::Slider::new(&mut l.illuminance, 0.0..=50_000.0)
            .logarithmic(true)
            .text(format!("{tag}.illuminance")),
    );
    ui.label(format!("{tag}.direction"));
    vec3_row(ui, &format!("{tag}_dir"), &mut l.direction, -5.0..=5.0);
    ui.label(format!("{tag}.color (linear RGB)"));
    rgb_row(ui, &mut l.color);
    ui.checkbox(&mut l.shadows, format!("{tag}.shadows"));
}

fn collect_vrm_material_keys(
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
        .map(|(_, name, gltf, h)| mtoon_mesh_override_key(name, gltf, &h.0))
        .chain(
            std_meshes_q
                .iter()
                .filter(|(e, ..)| entity_under_vrm(*e, child_of, vrm_roots))
                .map(|(_, name, gltf, h)| std_mesh_material_key(name, gltf, &h.0)),
        )
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

fn draw_material_visibility(
    ui: &mut egui::Ui,
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
    store: Option<&MaterialVisibilityStore>,
) {
    ui.heading("Material visibility")
        .on_hover_text("Show or hide each VRM mesh part by material name.");
    let Some(store) = store else {
        ui.label("MaterialVisibilityStore not initialised yet.");
        return;
    };

    let keys = collect_vrm_material_keys(vrm_roots, child_of, mtoon_meshes_q, std_meshes_q);
    if keys.is_empty() {
        ui.label("No materials found under the active VRM.");
        return;
    }

    ui.horizontal(|ui| {
        if ui.button("Show all").clicked() {
            let _ = store.show_all();
        }
        if ui.button("Hide all").clicked() {
            let _ = store.hide_all(keys.iter().cloned());
        }
        if ui.button("Invert").clicked() {
            let _ = store.invert(&keys);
        }
    });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(200.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for key in &keys {
                let mut visible = store.is_visible(key);
                if ui.checkbox(&mut visible, key).changed() {
                    let _ = store.set_visible(key.clone(), visible);
                }
            }
        });
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
            (mtoon_mesh_override_key(name, gltf_name, &h.0), h.0.clone())
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

    // Snapshot the draft *before* the widgets render this frame. After widgets,
    // `diff_and_tick` auto-enables `override_*` for any field the user moved,
    // so dragging a slider is enough — no need to tick the checkbox manually.
    let prev_draft = draft.clone();

    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_base_color, "base_color");
        rgba_row(ui, &mut draft.base_color);
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_emissive, "emissive");
        rgba_row(ui, &mut draft.emissive);
    });

    ui.separator();
    ui.label("Shade");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_shade_color, "shade_color");
        rgba_row(ui, &mut draft.shade_color);
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_shading_shift_factor,
            "shading_shift_factor",
        );
        ui.add(egui::Slider::new(
            &mut draft.shading_shift_factor,
            -1.0..=1.0,
        ));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_toony_factor, "toony_factor");
        ui.add(egui::Slider::new(&mut draft.toony_factor, 0.0..=1.0));
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_gi_equalization_factor,
            "gi_equalization_factor",
        )
        .on_hover_text(
            "Per-material IBL strength. Lower toward 0 on saturated/dark cloth so the \
             environment map can't lift it toward gray; 1.0 leaves it untouched.",
        );
        ui.add(egui::Slider::new(
            &mut draft.gi_equalization_factor,
            0.0..=1.0,
        ));
    });

    ui.separator();
    ui.label("Rim");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_color, "rim_color");
        rgba_row(ui, &mut draft.rim_color);
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_fresnel_power, "rim_fresnel_power");
        ui.add(egui::Slider::new(&mut draft.rim_fresnel_power, 0.0..=16.0));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_lift_factor, "rim_lift_factor");
        ui.add(egui::Slider::new(&mut draft.rim_lift_factor, 0.0..=1.0));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_rim_mix_factor, "rim_mix_factor");
        ui.add(egui::Slider::new(&mut draft.rim_mix_factor, 0.0..=1.0));
    });

    ui.separator();
    ui.label("Outline")
        .on_hover_text("Try worldCoordinates with width around 0.002–0.01 depending on scale.");
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_outline_mode, "outline_mode");
        ui.add(egui::Checkbox::new(
            &mut draft.outline_mode_world,
            "worldCoordinates (off = None)",
        ));
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_outline_width_factor,
            "outline_width_factor",
        );
        ui.add(egui::Slider::new(&mut draft.outline_width_factor, 0.0..=0.1));
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.override_outline_color, "outline_color");
        rgba_row(ui, &mut draft.outline_color);
    });
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut draft.override_outline_lighting_mix_factor,
            "outline_lighting_mix_factor",
        );
        ui.add(egui::Slider::new(
            &mut draft.outline_lighting_mix_factor,
            0.0..=1.0,
        ));
    });

    draft.diff_and_tick(&prev_draft);

    ui.small("Live preview; save to persist. Editing a value auto-ticks its override.");
    ui.label(format!(
        "{} field(s) will be written on save.",
        draft.override_count(),
    ));
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
