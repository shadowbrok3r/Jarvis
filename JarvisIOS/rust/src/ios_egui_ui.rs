//! egui menu bar and tool windows for the embedded JarvisIOS Bevy view.
//! Only panels with real controls live here; gateway chat, expressions, motion,
//! and layers are in Swift (`AvatarToolsOverlay`).

use std::collections::HashSet;

use bevy::gltf::GltfMaterialName;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy_egui::egui::{self, RichText};
use bevy_egui::EguiContexts;
use bevy_vrm1::prelude::{MToonMaterial, Vrm};

/// Which egui panels are open (View menu toggles).
#[derive(Resource)]
pub struct JarvisIosUiState {
    pub theme_applied: bool,
    pub show_graphics_advanced: bool,
    pub show_logging: bool,
}

impl Default for JarvisIosUiState {
    fn default() -> Self {
        Self {
            theme_applied: false,
            show_graphics_advanced: false,
            show_logging: false,
        }
    }
}

pub fn jarvis_ios_egui_apply_theme(
    mut contexts: EguiContexts,
    mut state: ResMut<JarvisIosUiState>,
) -> Result {
    if state.theme_applied {
        return Ok(());
    }
    state.theme_applied = true;
    let ctx = contexts.ctx_mut()?;
    match serde_json::from_str::<egui::Style>(crate::jarvis_egui_theme::STYLE_JSON) {
        Ok(theme) => {
            ctx.set_style(std::sync::Arc::new(theme));
        }
        Err(e) => {
            bevy::log::error!("Error setting JarvisIOS egui theme: {e:?}");
        }
    }
    Ok(())
}

pub fn jarvis_ios_egui_menu_bar(
    mut contexts: EguiContexts,
    mut ui_state: ResMut<JarvisIosUiState>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let s = &mut *ui_state;
    egui::TopBottomPanel::top("jarvis_ios_menu_bar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut s.show_graphics_advanced, "Materials");
                ui.checkbox(&mut s.show_logging, "Logging");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Jarvis iOS").weak().small());
            });
        });
    });
    Ok(())
}

pub fn jarvis_ios_egui_windows(
    mut contexts: EguiContexts,
    mut ui_state: ResMut<JarvisIosUiState>,
    avatar: Res<crate::ios_profile_manifest::IosAvatarSettings>,
    mut vis_store: ResMut<crate::ios_material_visibility::IosMaterialVisibilityStore>,
    vrm_roots_q: Query<Entity, With<Vrm>>,
    child_of_q: Query<&ChildOf>,
    mtoon_meshes_q: Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<MToonMaterial>,
    )>,
    std_meshes_q: Query<(
        Entity,
        Option<&Name>,
        Option<&GltfMaterialName>,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) -> Result {
    let ctx = contexts.ctx_mut()?;

    if ui_state.show_graphics_advanced {
        let vrm_roots: HashSet<Entity> = vrm_roots_q.iter().collect();
        let material_keys = crate::ios_material_visibility::collect_vrm_material_keys(
            &vrm_roots,
            &child_of_q,
            &mtoon_meshes_q,
            &std_meshes_q,
        );
        let model_path = avatar.model_path.clone();
        let mut save_status: Option<String> = None;
        let mut show = true;
        egui::Window::new("Materials")
            .default_pos(egui::pos2(8.0, 36.0))
            .default_size(egui::vec2(300.0, 480.0))
            .collapsible(true)
            .resizable(true)
            .open(&mut show)
            .show(ctx, |ui| {
                ui.label(RichText::new("Material visibility").strong());
                ui.small(
                    "Toggles apply immediately. Save stores per-model on this device; hub sync can also push desktop presets.",
                );
                if material_keys.is_empty() {
                    ui.label("No materials found under the active VRM.");
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Show all").clicked() {
                            vis_store.show_all();
                        }
                        if ui.button("Hide all").clicked() {
                            vis_store.hide_all(material_keys.iter().cloned());
                        }
                        if ui.button("Invert").clicked() {
                            vis_store.invert(&material_keys);
                        }
                    });
                    if ui.button("Save on device").clicked() {
                        let ok =
                            crate::ios_user_prefs::save_material_visibility(&model_path, &vis_store);
                        save_status = Some(if ok {
                            "Saved material visibility for this model.".into()
                        } else {
                            "Save failed (prefs directory unavailable).".into()
                        });
                    }
                    if let Some(msg) = &save_status {
                        ui.label(RichText::new(msg).small().weak());
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(600.0)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            for key in &material_keys {
                                let mut visible = vis_store.is_visible(key);
                                if ui.checkbox(&mut visible, key).changed() {
                                    vis_store.set_visible(key.clone(), visible);
                                }
                            }
                        });
                }
            });
        if !show {
            ui_state.show_graphics_advanced = false;
        }
    }

    if ui_state.show_logging {
        let mut show = true;
        let mut current = crate::debug_log::log_verbosity();
        egui::Window::new("Logging")
            .default_pos(egui::pos2(320.0, 36.0))
            .default_size(egui::vec2(260.0, 0.0))
            .collapsible(true)
            .resizable(true)
            .open(&mut show)
            .show(ctx, |ui| {
                ui.label(RichText::new("Verbosity").strong());
                ui.label(
                    RichText::new(
                        "Lower verbosity = fewer file writes. Use QUIET / OFF to test whether logging contributes to frame stalls.",
                    )
                    .weak()
                    .small(),
                );
                ui.separator();
                let prev = current;
                ui.radio_value(
                    &mut current,
                    crate::debug_log::LOG_VERBOSITY_OFF,
                    "Off — drop everything (ERROR still logged)",
                );
                ui.radio_value(
                    &mut current,
                    crate::debug_log::LOG_VERBOSITY_QUIET,
                    "Quiet — only crit lifecycle + WARN/ERROR",
                );
                ui.radio_value(
                    &mut current,
                    crate::debug_log::LOG_VERBOSITY_NORMAL,
                    "Normal — default (every-30-frame stats)",
                );
                ui.radio_value(
                    &mut current,
                    crate::debug_log::LOG_VERBOSITY_DEBUG,
                    "Debug — every-frame stats (very chatty)",
                );
                if current != prev {
                    crate::debug_log::set_log_verbosity(current);
                }
                ui.label(
                    RichText::new("Saved automatically for the next launch.")
                        .small()
                        .weak(),
                );
            });
        if !show {
            ui_state.show_logging = false;
        }
    }

    Ok(())
}
