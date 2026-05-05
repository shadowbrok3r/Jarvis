//! egui menu bar and tool windows for the embedded JarvisIOS Bevy view.
//! Desktop parity: [`TopBottomPanel::top`] + [`egui::MenuBar`] (see `src/plugins/debug_ui.rs`).
//! Full IronClaw / MCP / pose / rig tooling stays on desktop for now — windows here are stubs plus
//! hub manifest fields so layouts and navigation match the desktop **View** menu over time.

use bevy::prelude::*;
use bevy_egui::egui::{self, RichText};
use bevy_egui::EguiContexts;
use bevy_vrm1::prelude::{Initialized, SetExpressions, Vrm, VrmExpression};

// ── Expression categorisation ────────────────────────────────────────────────
// All lists are derived dynamically from the loaded VRM.
// The only constant used is the VRM 1.0 spec preset set (18 names defined in
// the VRMC_vrm specification — identical for every compliant VRM avatar), which
// is used to distinguish spec presets from model-specific custom expressions.

/// Derive a grouping prefix: everything before the first `_`, or the name
/// with trailing digits stripped when there is no `_`.
fn extract_expr_prefix(name: &str) -> &str {
    if let Some(idx) = name.find('_') {
        &name[..idx]
    } else {
        name.trim_end_matches(|c: char| c.is_ascii_digit())
    }
}

/// Render one `egui::Slider` for `name`, mutate `weights`, and return whether
/// it changed.
fn expr_slider(
    ui: &mut egui::Ui,
    name: &str,
    weights: &mut std::collections::HashMap<String, f32>,
) -> bool {
    let w = weights.entry(name.to_string()).or_insert(0.0);
    let resp = ui.add(egui::Slider::new(w, 0.0..=1.0).text(name).step_by(0.01));
    if resp.changed() {
        *w = w.clamp(0.0, 1.0);
        true
    } else {
        false
    }
}

/// Which panels are open; toggles come from the menu bar **View** menu.
#[derive(Resource)]
pub struct JarvisIosUiState {
    pub theme_applied: bool,
    pub show_scene_panel: bool,
    pub show_camera: bool,
    pub show_graphics: bool,
    pub show_about: bool,
    /// Desktop `sections::draw_avatar_window` — model / idle / locks / background from manifest.
    pub show_avatar: bool,
    pub show_live_test: bool,
    pub show_channel_hub: bool,
    pub show_gateway: bool,
    pub show_tts: bool,
    pub show_look_at: bool,
    pub show_mcp: bool,
    pub show_pose_controller: bool,
    pub show_services: bool,
    pub show_anim_layers: bool,
    pub show_emotion_mappings: bool,
    pub show_network_trace: bool,
    pub show_rig_editor: bool,
    pub show_graphics_advanced: bool,
    /// Full expression preset sliders (mirrors desktop Pose Controller → Expressions tab).
    pub show_expressions: bool,
}

impl Default for JarvisIosUiState {
    fn default() -> Self {
        Self {
            theme_applied: false,
            show_scene_panel: false,
            show_camera: false,
            show_graphics: false,
            show_about: false,
            show_avatar: false,
            show_live_test: false,
            show_channel_hub: false,
            show_gateway: false,
            show_tts: false,
            show_look_at: false,
            show_mcp: false,
            show_pose_controller: false,
            show_services: false,
            show_anim_layers: false,
            show_emotion_mappings: false,
            show_network_trace: false,
            show_rig_editor: false,
            show_graphics_advanced: false,
            show_expressions: false,
        }
    }
}

pub fn jarvis_ios_egui_apply_theme(mut contexts: EguiContexts, mut state: ResMut<JarvisIosUiState>) -> Result {
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

fn view_menu_desktop_parity(ui: &mut egui::Ui, s: &mut JarvisIosUiState) {
    ui.label(RichText::new("Desktop parity (stubs on iOS)").weak().small());
    ui.separator();
    ui.checkbox(&mut s.show_avatar, "Avatar");
    ui.checkbox(&mut s.show_live_test, "Live · Test bench");
    ui.checkbox(&mut s.show_channel_hub, "Channel hub (IronClaw)");
    ui.checkbox(&mut s.show_gateway, "Gateway (HTTP / SSE)");
    ui.checkbox(&mut s.show_tts, "TTS (Kokoro)");
    ui.checkbox(&mut s.show_look_at, "Look-at");
    ui.checkbox(&mut s.show_mcp, "MCP / Pose controller");
    ui.checkbox(&mut s.show_pose_controller, "Pose Controller");
    ui.checkbox(&mut s.show_services, "Services");
    ui.checkbox(&mut s.show_anim_layers, "Animation Layers");
    ui.checkbox(&mut s.show_emotion_mappings, "Emotion Mappings");
    ui.checkbox(&mut s.show_network_trace, "Network trace");
    ui.checkbox(&mut s.show_rig_editor, "Rig editor");
    ui.checkbox(&mut s.show_graphics_advanced, "Graphics Advanced");
    ui.separator();
    ui.checkbox(&mut s.show_expressions, "Expressions");
}

pub fn jarvis_ios_egui_menu_bar(mut contexts: EguiContexts, mut ui_state: ResMut<JarvisIosUiState>) -> Result {
    let ctx = contexts.ctx_mut()?;
    let s = &mut *ui_state;
    egui::TopBottomPanel::top("jarvis_ios_menu_bar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut s.show_scene_panel, "Scene / HUD (Jarvis)");
                ui.checkbox(&mut s.show_camera, "Camera");
                ui.checkbox(&mut s.show_graphics, "Graphics / lights");
                ui.checkbox(&mut s.show_expressions, "Expressions");
                ui.separator();
                ui.menu_button("More windows…", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            view_menu_desktop_parity(ui, s);
                        });
                });
                ui.separator();
                ui.checkbox(&mut s.show_about, "About");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Tips…").clicked() {
                    s.show_about = true;
                    ui.close();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Jarvis iOS").weak().small());
            });
        });
    });
    Ok(())
}

fn stub_footer(ui: &mut egui::Ui) {
    ui.separator();
    ui.label(
        RichText::new(
            "Full controls live in the desktop `debug_ui` plugin. Swift covers gateway chat + hub sync.",
        )
        .weak()
        .small(),
    );
}

pub fn jarvis_ios_egui_windows(
    mut contexts: EguiContexts,
    mut ui_state: ResMut<JarvisIosUiState>,
    avatar: Res<crate::ios_profile_manifest::IosAvatarSettings>,
    graphics: Res<crate::ios_graphics::IosGraphicsSettings>,
    mut expr_state: ResMut<crate::ios_bevy::IosExpressionsState>,
    vrm_q: Query<Entity, (With<Vrm>, With<Initialized>)>,
    mut commands: Commands,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let mut close_about = false;

    if ui_state.show_scene_panel {
        egui::Window::new("Jarvis")
            .default_pos(egui::pos2(8.0, 36.0))
            .default_size(egui::vec2(280.0, 0.0))
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Scene / HUD").strong());
                ui.separator();
                ui.label(RichText::new("Quick readout from the hub manifest + env overrides.").weak());
                ui.monospace(format!("model_path:\n{}", avatar.model_path));
                ui.monospace(format!(
                    "idle_vrma_path:\n{}",
                    if avatar.idle_vrma_path.is_empty() {
                        "(none)"
                    } else {
                        avatar.idle_vrma_path.as_str()
                    }
                ));
            });
    }

    if ui_state.show_camera {
        egui::Window::new("Camera")
            .default_pos(egui::pos2(300.0, 36.0))
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Orbit + exposure").strong());
                ui.separator();
                ui.label(
                    RichText::new(
                        "While a touch is over any egui surface, PanOrbit is disconnected so windows and sliders stay usable.",
                    )
                    .weak(),
                );
                stub_footer(ui);
            });
    }

    if ui_state.show_graphics {
        egui::Window::new("Graphics / lights")
            .default_pos(egui::pos2(8.0, 220.0))
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Graphics (manifest snapshot)").strong());
                ui.separator();
                ui.monospace(format!("msaa_samples: {}", graphics.msaa_samples));
                ui.monospace(format!("hdr: {}", graphics.hdr));
                ui.monospace(format!("show_ground_plane: {}", graphics.show_ground_plane));
                stub_footer(ui);
            });
    }

    if ui_state.show_avatar {
        egui::Window::new("Avatar")
            .default_pos(egui::pos2(8.0, 400.0))
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Avatar (hub + env)").strong());
                ui.separator();
                ui.monospace(format!("model_path:\n{}", avatar.model_path));
                ui.monospace(format!("idle_vrma_path:\n{}", avatar.idle_vrma_path));
                ui.monospace(format!("world_position: {:?}", avatar.world_position));
                ui.monospace(format!("uniform_scale: {}", avatar.uniform_scale));
                ui.monospace(format!(
                    "locks: root_xz={} root_y={} vrm_root_y={}",
                    avatar.lock_root_xz, avatar.lock_root_y, avatar.lock_vrm_root_y
                ));
                ui.monospace(format!("auto_load_spring_preset: {}", avatar.auto_load_spring_preset));
                ui.monospace(format!("background_color: {:?}", avatar.background_color));
                stub_footer(ui);
            });
    }

    if ui_state.show_live_test {
        egui::Window::new("Live · Test bench")
            .default_pos(egui::pos2(320.0, 200.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Live / Test").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_channel_hub {
        egui::Window::new("Channel hub (IronClaw protocol)")
            .default_pos(egui::pos2(340.0, 220.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Channel hub").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_gateway {
        egui::Window::new("Gateway (IronClaw HTTP/SSE)")
            .default_pos(egui::pos2(360.0, 240.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Gateway").strong());
                ui.separator();
                ui.label(RichText::new("Use the Swift **Chat** tab or avatar overlay for `/api/chat/*`.").weak());
                stub_footer(ui);
            });
    }

    if ui_state.show_tts {
        egui::Window::new("TTS (Kokoro)")
            .default_pos(egui::pos2(380.0, 260.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("TTS").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_look_at {
        egui::Window::new("Look-at")
            .default_pos(egui::pos2(400.0, 280.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Look-at").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_mcp {
        egui::Window::new("MCP / Pose controller")
            .default_pos(egui::pos2(420.0, 300.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("MCP / Pose").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_pose_controller {
        egui::Window::new("Pose Controller")
            .default_pos(egui::pos2(440.0, 320.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Pose Controller").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_services {
        egui::Window::new("Services")
            .default_pos(egui::pos2(460.0, 340.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Services").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_anim_layers {
        egui::Window::new("Animation Layers")
            .default_pos(egui::pos2(480.0, 360.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Animation Layers").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_emotion_mappings {
        egui::Window::new("Emotion Mappings")
            .default_pos(egui::pos2(500.0, 380.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Emotion Mappings").strong());
                ui.separator();
                ui.label(
                    RichText::new("Swift **About → Emotion & animation map** edits `config/emotions.json` on device.")
                        .weak(),
                );
                stub_footer(ui);
            });
    }

    if ui_state.show_network_trace {
        egui::Window::new("Network trace")
            .default_pos(egui::pos2(520.0, 400.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Network trace").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_rig_editor {
        egui::Window::new("Rig editor")
            .default_pos(egui::pos2(540.0, 420.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Rig editor").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    if ui_state.show_graphics_advanced {
        egui::Window::new("Graphics Advanced")
            .default_pos(egui::pos2(560.0, 440.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Graphics Advanced").strong());
                ui.separator();
                stub_footer(ui);
            });
    }

    // ── Expressions window ──────────────────────────────────────────────
    if ui_state.show_expressions {
        let vrm_entity = vrm_q.single().ok();
        let mut expr_dirty = false;
        let mut show_expressions = true;

        // ── Pre-compute categorised lists ────────────────────────────────────
        // All categories are derived from the runtime expression list; no
        // hardcoded model-specific name lists.

        // Spec presets (18 names from VRMC_vrm spec, same for all VRMs).
        let mut spec_exprs: Vec<String> = expr_state
            .presets
            .iter()
            .filter(|n| crate::ios_bevy::is_vrm1_spec_preset(n))
            .cloned()
            .collect();
        spec_exprs.sort();

        // Custom expressions: everything not in the spec preset list.
        // Prefix-group those that share a common prefix (≥2 names); rest are standalone.
        let mut custom_all: Vec<String> = expr_state
            .presets
            .iter()
            .filter(|n| !crate::ios_bevy::is_vrm1_spec_preset(n))
            .cloned()
            .collect();
        custom_all.sort();

        let mut by_prefix: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for name in &custom_all {
            by_prefix
                .entry(extract_expr_prefix(name).to_string())
                .or_default()
                .push(name.clone());
        }
        let mut standalone_custom: Vec<String> = Vec::new();
        let mut prefix_groups: Vec<(String, Vec<String>)> = Vec::new();
        for (prefix, mut names) in by_prefix {
            names.sort();
            if names.len() >= 2 {
                prefix_groups.push((prefix, names));
            } else {
                standalone_custom.extend(names);
            }
        }
        prefix_groups.sort_by(|a, b| a.0.cmp(&b.0));
        standalone_custom.sort();

        let total = expr_state.presets.len();

        egui::Window::new("Expressions")
            .default_pos(egui::pos2(8.0, 60.0))
            .default_size(egui::vec2(280.0, 480.0))
            .collapsible(true)
            .resizable(true)
            .open(&mut show_expressions)
            .show(ctx, |ui| {
                // ── Action bar ───────────────────────────────────────────
                ui.horizontal(|ui| {
                    if ui.button("Zero all").clicked() {
                        for w in expr_state.weights.values_mut() {
                            *w = 0.0;
                        }
                        expr_dirty = true;
                    }
                    if ui
                        .button("Neutral @ 1")
                        .on_hover_text("Zero all then set `neutral` to 1.0")
                        .clicked()
                    {
                        for w in expr_state.weights.values_mut() {
                            *w = 0.0;
                        }
                        if let Some(w) = expr_state.weights.get_mut("neutral") {
                            *w = 1.0;
                        }
                        expr_dirty = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{total} total")).weak().small(),
                        );
                    });
                });
                ui.separator();

                if total == 0 {
                    ui.label(
                        RichText::new(
                            "No expressions yet — waiting for VRM to finish loading.",
                        )
                        .weak(),
                    );
                    return;
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // ── Standard VRM presets (spec-defined, same for all VRMs) ──
                        if !spec_exprs.is_empty() {
                            egui::CollapsingHeader::new(
                                RichText::new(format!("Standard  ({})", spec_exprs.len()))
                                    .strong(),
                            )
                            .default_open(true)
                            .show(ui, |ui| {
                                for name in &spec_exprs {
                                    if expr_slider(ui, name, &mut expr_state.weights) {
                                        expr_dirty = true;
                                    }
                                }
                            });
                            ui.add_space(2.0);
                        }

                        // ── Custom expressions (all model-specific, prefix-grouped) ──
                        let custom_count = custom_all.len();
                        if custom_count > 0 {
                            egui::CollapsingHeader::new(
                                RichText::new(format!("Custom  ({custom_count})")).strong(),
                            )
                            .default_open(false)
                            .show(ui, |ui| {
                                for name in &standalone_custom {
                                    if expr_slider(ui, name, &mut expr_state.weights) {
                                        expr_dirty = true;
                                    }
                                }
                                if !standalone_custom.is_empty() && !prefix_groups.is_empty() {
                                    ui.add_space(2.0);
                                }
                                for (prefix, names) in &prefix_groups {
                                    egui::CollapsingHeader::new(
                                        format!("{}…  ({})", prefix, names.len()),
                                    )
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        for name in names {
                                            if expr_slider(ui, name, &mut expr_state.weights) {
                                                expr_dirty = true;
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    });
            });

        if !show_expressions {
            ui_state.show_expressions = false;
        }

        if expr_dirty {
            if let Some(vrm_e) = vrm_entity {
                let weights: std::collections::HashMap<VrmExpression, f32> = expr_state
                    .weights
                    .iter()
                    .filter_map(|(k, &v)| {
                        let n = k.trim();
                        if n.is_empty() {
                            None
                        } else {
                            Some((VrmExpression::from(n), v))
                        }
                    })
                    .collect();
                commands.trigger(SetExpressions::from_iter(vrm_e, weights));
            }
        }
    }

    if ui_state.show_about {
        egui::Window::new("About")
            .collapsible(false)
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(env!("CARGO_PKG_NAME")).strong());
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
                ui.separator();
                ui.label(
                    RichText::new("Gateway chat + hub sync are in Swift; egui here mirrors desktop window names and hub fields.")
                        .weak(),
                );
                if ui.button("Close").clicked() {
                    close_about = true;
                }
            });
    }

    if close_about {
        ui_state.show_about = false;
    }

    Ok(())
}
