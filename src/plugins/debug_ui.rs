//! `bevy_egui` overlay: persistent menu bar plus optional windows (Chat, Avatar,
//! Camera, Graphics, Services, Channel hub, Gateway, TTS, Look-at, MCP, Pose
//! Controller, Rig editor, Graphics Advanced, Animation Layers, Emotion
//! Mappings, Home Assistant, Network trace, Live/Test). Open/closed flags live
//! in [`Settings::ui`] and persist in `config/user.toml`.
//!
//! The menu bar is always visible — there is no F1 toggle anymore. See the
//! **View** menu to show/hide windows, the **File** menu to save/restore
//! configuration, and the **Test** menu for one-click access to the
//! Live/Test bench.

pub mod anim_layers;
pub mod apply;
pub mod chat;
pub mod emotion_mappings;
pub mod graphics_advanced;
pub mod home_assistant;
pub mod network_trace;
pub mod pose_controller;
pub mod rig_editor;
pub mod sections;
pub mod services;
mod widgets;

pub use chat::ChatUiState;
pub use pose_controller::{PoseControllerUiState, KimodoClientRes};

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};

use jarvis_avatar::act::Emotion;
use jarvis_avatar::config::Settings;

use crate::plugins::traffic_log::TrafficChannel;

pub struct DebugUiPlugin;

impl Plugin for DebugUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default())
            .init_resource::<DebugUiState>()
            .add_systems(
                Update,
                (
                    rig_editor::rig_editor_viewport_pick,
                    rig_editor::rig_editor_alt_drag_twist,
                )
                    .chain(),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
                    draw_menu_bar,
                    draw_restore_defaults_modal,
                    draw_about_window,
                    chat::draw_chat_window,
                    sections::draw_avatar_window,
                    sections::draw_camera_window,
                    sections::draw_graphics_window,
                    sections::draw_live_test_window,
                    sections::draw_channel_hub_window,
                    sections::draw_gateway_window,
                    sections::draw_tts_window,
                    sections::draw_look_at_window,
                    sections::draw_mcp_window,
                    pose_controller::draw_pose_controller_window,
                    graphics_advanced::draw_graphics_advanced_window,
                    services::draw_services_window,
                    anim_layers::draw_anim_layers_window,
                    emotion_mappings::draw_emotion_mappings_window,
                    home_assistant::draw_home_assistant_window,
                    network_trace::draw_network_trace_window,
                )
                    .chain(),
            )
            .add_systems(EguiPrimaryContextPass, rig_editor::draw_rig_editor_window)
            .add_systems(
                EguiPrimaryContextPass,
                graphics_advanced::apply_mtoon_material_live_preview
                    .after(graphics_advanced::draw_graphics_advanced_window),
            )
            .add_systems(
                Update,
                (
                    apply::apply_camera_settings,
                    apply::apply_avatar_transform,
                    apply::sync_camera_msaa,
                    apply::apply_window_present_mode,
                    apply::apply_clear_color,
                    apply::apply_ambient_light,
                    apply::apply_exposure,
                    apply::apply_sun_light,
                    apply::apply_ground_material,
                ),
            );
    }
}

/// Transient debug-UI state that does NOT round-trip through `config/user.toml`.
/// Persistent flags (which windows are open) live on [`jarvis_avatar::config::UiSettings`].
#[derive(Resource)]
pub struct DebugUiState {
    pub save_status: Option<String>,
    /// First run for setup of style
    pub first_run: bool,
    /// Modal-confirm: user clicked "Restore defaults…" and we're waiting for yes/no.
    pub confirm_restore: bool,
    /// Set by the Camera window's "Re-center on VRM now" button; consumed by
    /// [`apply::apply_camera_settings`].
    pub resnap_requested: bool,
    /// Help / keybinds window visibility.
    pub show_about: bool,
    pub test: LiveTestUiState,
    pub chat: ChatUiState,
    pub pose_controller: PoseControllerUiState,
    pub graphics_advanced: graphics_advanced::GraphicsAdvancedUiState,
    pub anim_layers: anim_layers::AnimLayersUiState,
    pub emotion_mappings: emotion_mappings::EmotionMappingsUiState,
    /// Network trace window: which [`TrafficChannel`] tab is selected.
    pub network_trace_channel: TrafficChannel,
    /// Index into the current channel's entry `Vec` (same order as [`TrafficLogSink::snapshot_channel`]).
    pub network_trace_pick: Option<usize>,
    /// Avatar window: `assets/models` picker (filter, selection, last load/list error).
    pub avatar_vrm_picker: AvatarVrmPickerState,
}

/// Transient state for the Avatar window's runtime VRM list (not persisted to `user.toml`).
#[derive(Debug, Clone)]
pub struct AvatarVrmPickerState {
    pub filter: String,
    pub selected_basename: Option<String>,
    /// `list_vrm_models` / missing `assets/models` (refreshed each frame while the window is open).
    pub list_error: Option<String>,
    /// Last failed hot-swap (resolve path or missing `PoseCommandSender`).
    pub op_error: Option<String>,
}

impl Default for AvatarVrmPickerState {
    fn default() -> Self {
        Self {
            filter: String::new(),
            selected_basename: None,
            list_error: None,
            op_error: None,
        }
    }
}

impl Default for DebugUiState {
    fn default() -> Self {
        Self {
            save_status: None,
            first_run: true,
            confirm_restore: false,
            resnap_requested: false,
            show_about: false,
            test: LiveTestUiState::default(),
            chat: ChatUiState::default(),
            pose_controller: PoseControllerUiState::default(),
            graphics_advanced: graphics_advanced::GraphicsAdvancedUiState::default(),
            anim_layers: anim_layers::AnimLayersUiState::default(),
            emotion_mappings: emotion_mappings::EmotionMappingsUiState::default(),
            network_trace_channel: TrafficChannel::ChannelHubWsInbound,
            network_trace_pick: None,
            avatar_vrm_picker: AvatarVrmPickerState::default(),
        }
    }
}

pub struct LiveTestUiState {
    pub input_text: String,
    pub tts_text: String,
    pub look_at: [f32; 3],
    pub emotion: Emotion,
}

impl Default for LiveTestUiState {
    fn default() -> Self {
        Self {
            input_text: "jarvis, say something nice".into(),
            tts_text: "Online and ready.".into(),
            look_at: [0.4, 1.5, 0.8],
            emotion: Emotion::Happy,
        }
    }
}

// ---------- Menu bar ----------------------------------------------------------

fn draw_menu_bar(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    if state.first_run {
        state.first_run = false;
        match serde_json::from_str::<egui::Style>(crate::STYLE) {
            Ok(theme) => {
                let style = std::sync::Arc::new(theme);
                ctx.set_style(style);
            }
            Err(e) => error!("Error setting theme: {e:?}")
        };
    }

    egui::TopBottomPanel::top("jarvis_menu_bar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            file_menu(ui, &mut settings, &mut state, &mut exit);
            view_menu(ui, &mut settings);
            test_menu(ui, &mut settings);
            help_menu(ui, &mut state);

            // Right-aligned status/hint.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(msg) = &state.save_status {
                    ui.colored_label(egui::Color32::from_rgb(150, 200, 150), msg);
                }
                ui.label(
                    egui::RichText::new("jarvis-avatar")
                        .color(egui::Color32::GRAY)
                        .small(),
                );
            });
        });
    });
}

fn file_menu(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    state: &mut DebugUiState,
    exit: &mut MessageWriter<AppExit>,
) {
    ui.menu_button("File", |ui| {
        if ui
            .button("Save settings")
            .on_hover_text("Writes the current values to config/user.toml (default.toml is preserved)")
            .clicked()
        {
            state.save_status = Some(match settings.save_user() {
                Ok(()) => "saved → config/user.toml".to_string(),
                Err(e) => format!("save failed: {e}"),
            });
            ui.close();
        }
        if ui
            .button("Reload from disk")
            .on_hover_text("Re-load config/default.toml + config/user.toml")
            .clicked()
        {
            state.save_status = Some(match Settings::load() {
                Ok(fresh) => {
                    *settings = fresh;
                    "reloaded from disk".to_string()
                }
                Err(e) => format!("reload failed: {e}"),
            });
            ui.close();
        }
        if ui
            .button("Restore defaults…")
            .on_hover_text("Delete config/user.toml and revert to default.toml values")
            .clicked()
        {
            state.confirm_restore = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            exit.write(AppExit::Success);
            ui.close();
        }
    });
}

fn view_menu(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.menu_button("View", |ui| {
        let u = &mut settings.ui;
        ui.checkbox(&mut u.show_chat, "Chat");
        ui.separator();
        ui.checkbox(&mut u.show_avatar, "Avatar");
        ui.checkbox(&mut u.show_camera, "Camera");
        ui.checkbox(&mut u.show_graphics, "Graphics / lights");
        ui.separator();
        ui.checkbox(&mut u.show_services, "Services (all)");
        ui.checkbox(&mut u.show_channel_hub, "Channel hub");
        ui.checkbox(&mut u.show_gateway, "Gateway (HTTP/SSE)");
        ui.checkbox(&mut u.show_tts, "TTS (Kokoro)");
        ui.checkbox(&mut u.show_look_at, "Look-at");
        ui.checkbox(&mut u.show_mcp, "MCP");
        ui.separator();
        ui.checkbox(&mut u.show_pose_controller, "Pose Controller");
        ui.checkbox(&mut u.show_rig_editor, "Rig editor (pick + springs)");
        ui.checkbox(&mut u.show_graphics_advanced, "Graphics Advanced");
        ui.checkbox(&mut u.show_anim_layers, "Animation Layers");
        ui.checkbox(&mut u.show_emotion_mappings, "Emotion Mappings");
        ui.separator();
        ui.checkbox(&mut u.show_home_assistant, "Home Assistant");
        ui.checkbox(&mut u.show_network_trace, "Network trace");
    });
}

fn test_menu(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.menu_button("Test", |ui| {
        if ui
            .button("Open Live / Test bench")
            .on_hover_text("Broadcast hub messages, trigger expressions, send look-at + TTS")
            .clicked()
        {
            settings.ui.show_live_test = true;
            ui.close();
        }
    });
}

fn help_menu(ui: &mut egui::Ui, state: &mut DebugUiState) {
    ui.menu_button("Help", |ui| {
        if ui.button("About jarvis-avatar").clicked() {
            state.show_about = true;
            ui.close();
        }
    });
}

// ---------- Confirm modal -----------------------------------------------------

fn draw_restore_defaults_modal(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
) {
    if !state.confirm_restore {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    egui::Window::new("Restore defaults?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.label("This deletes `config/user.toml` and reloads values from `config/default.toml`.");
            ui.label("Any unsaved changes in this session will be lost.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.confirm_restore = false;
                }
                if ui
                    .add(egui::Button::new("Yes, restore").fill(egui::Color32::from_rgb(120, 60, 60)))
                    .clicked()
                {
                    state.save_status = Some(match Settings::restore_defaults() {
                        Ok(fresh) => {
                            *settings = fresh;
                            "restored defaults → reloaded".to_string()
                        }
                        Err(e) => format!("restore failed: {e}"),
                    });
                    state.confirm_restore = false;
                }
            });
        });
}

// ---------- About window ------------------------------------------------------

fn draw_about_window(mut contexts: EguiContexts, mut state: ResMut<DebugUiState>) {
    if !state.show_about {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = state.show_about;
    egui::Window::new("About jarvis-avatar")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Native Rust Bevy VRM client for IronClaw.");
            ui.separator();
            ui.label("Keybinds:");
            ui.monospace("  LMB drag     orbit camera");
            ui.monospace("  MMB drag     pan camera");
            ui.monospace("  scroll       zoom");
            ui.monospace("  Ctrl+Enter   (chat) send message");
            ui.separator();
            ui.label("Config files:");
            ui.monospace("  config/default.toml   factory defaults");
            ui.monospace("  config/user.toml      your overrides");
        });
    state.show_about = open;
}
