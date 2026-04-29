//! egui menu bar and tool windows for the embedded JarvisIOS Bevy view.
//! Desktop parity: [`TopBottomPanel::top`] + [`egui::MenuBar`] (see `src/plugins/debug_ui.rs`).
//! Full IronClaw / MCP / pose / rig tooling stays on desktop for now — windows here are stubs plus
//! hub manifest fields so layouts and navigation match the desktop **View** menu over time.

use bevy::prelude::*;
use bevy_egui::egui::{self, RichText};
use bevy_egui::EguiContexts;

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
}

impl Default for JarvisIosUiState {
    fn default() -> Self {
        Self {
            theme_applied: false,
            show_scene_panel: true,
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
