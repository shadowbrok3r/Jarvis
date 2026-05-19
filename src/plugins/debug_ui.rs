//! `bevy_egui` overlay: persistent menu bar plus optional windows (Chat, Avatar,
//! Camera, Graphics, Services, Channel hub, Gateway, TTS, Look-at, MCP, Pose
//! Controller (incl. per-VRM Expressions tab), Rig editor, Graphics Advanced, Animation Layers, Emotion
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
pub mod pose_tools_toolbar;
pub mod rig_editor;
pub mod sections;
pub mod services;
mod widgets;
pub mod workspaces;

use bevy_egui::egui::Layout;
pub use chat::ChatUiState;
pub use pose_controller::{KimodoClientRes, PoseControllerUiState};

use bevy::animation::AnimationPlayer;
use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use bevy_vrm1::prelude::Vrma;

use jarvis_avatar::act::Emotion;
use jarvis_avatar::config::Settings;

use crate::plugins::chat_pipeline_status::ChatPipelineStatus;
use crate::plugins::jarvis_ios_hub::write_vrm_graphics_override;
use crate::plugins::native_anim_player::ActiveNativeAnimation;
use crate::plugins::pose_driver::PoseCommandSender;
use crate::plugins::rig_editor::RigEditorState;
use crate::plugins::traffic_log::TrafficChannel;

pub struct DebugUiPlugin;

impl Plugin for DebugUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default())
            .init_resource::<DebugUiState>()
            .init_resource::<workspaces::ServiceHubUiState>()
            .init_resource::<workspaces::GraphicsWorkspaceUiState>()
            .init_resource::<workspaces::DiagnosticsUiState>()
            .add_systems(
                Update,
                (
                    rig_editor::rig_editor_viewport_hover,
                    rig_editor::rig_editor_viewport_pick,
                    rig_editor::rig_editor_axis_drag,
                )
                    .chain(),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
                    draw_menu_bar,
                    pose_tools_toolbar::draw_pose_tools_toolbar,
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
                )
                    .chain(),
            )
            .add_systems(
                EguiPrimaryContextPass,
                network_trace::draw_network_trace_window
                    .after(home_assistant::draw_home_assistant_window),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
                    workspaces::draw_service_hub_window,
                    workspaces::draw_graphics_workspace_window,
                    workspaces::draw_diagnostics_workspace_window,
                )
                    .chain()
                    .after(network_trace::draw_network_trace_window),
            )
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
                    spacebar_global_pause_toggle,
                ),
            );
    }
}

/// Global Spacebar pause/play. Pauses the [`ActiveNativeAnimation`] sampler
/// **and** every `AnimationPlayer` (idle VRMA) in one tap, then resumes them
/// on the next press. Skipped while egui owns keyboard focus (so typing in
/// any text field never triggers a global pause).
fn spacebar_global_pause_toggle(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut active: ResMut<ActiveNativeAnimation>,
    mut players_q: Query<&mut AnimationPlayer>,
    mut state: ResMut<DebugUiState>,
) {
    if !keys.just_pressed(KeyCode::Space) {
        return;
    }
    if matches!(contexts.ctx_mut(), Ok(ctx) if ctx.wants_keyboard_input()) {
        return;
    }

    // Decide the unified target state. If the native player has any clip
    // we follow its current pause state; otherwise we use whether any
    // AnimationPlayer reports a non-paused active anim.
    let any_player_playing = players_q.iter().any(|p| {
        p.playing_animations()
            .any(|(_id, anim)| !anim.is_paused())
    });
    let target_paused = if active.is_playing() {
        !active.is_paused()
    } else {
        any_player_playing
    };

    // Sync native sampler.
    if active.is_playing() && active.is_paused() != target_paused {
        active.toggle_paused();
    }

    // Sync every AnimationPlayer.
    let mut players_touched = 0usize;
    for mut player in players_q.iter_mut() {
        if target_paused {
            player.pause_all();
        } else {
            player.resume_all();
        }
        players_touched += 1;
    }

    state.pose_controller.status = Some(format!(
        "{} (space) — native:{} players:{}",
        if target_paused { "Paused" } else { "Resumed" },
        if active.is_playing() {
            if active.is_paused() { "paused" } else { "playing" }
        } else {
            "—"
        },
        players_touched,
    ));
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
    mut commands: Commands,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
    mut exit: MessageWriter<AppExit>,
    pipeline: Res<ChatPipelineStatus>,
    rig: Res<RigEditorState>,
    sender: Option<Res<PoseCommandSender>>,
    mut active_anim: ResMut<ActiveNativeAnimation>,
    vrma_q: Query<Entity, With<Vrma>>,
    mut players_q: Query<&mut AnimationPlayer>,
    snapshot: Option<Res<crate::plugins::pose_driver::BoneSnapshotHandle>>,
    undo: Res<crate::plugins::undo_history::UndoHistory>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    if state.first_run {
        state.first_run = false;
        match serde_json::from_str::<egui::Style>(jarvis_avatar::egui_theme::STYLE) {
            Ok(theme) => {
                let style = std::sync::Arc::new(theme);
                ctx.set_style(style);
            }
            Err(e) => error!("Error setting theme: {e:?}"),
        };
    }

    let vrma_entities: Vec<Entity> = vrma_q.iter().collect();
    let pose_controller_open = settings.ui.show_pose_controller;

    egui::TopBottomPanel::top("jarvis_menu_bar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            file_menu(ui, &mut settings, &mut state, &mut exit);
            view_menu(ui, &mut settings);
            test_menu(ui, &mut settings);
            help_menu(ui, &mut state);

            // Pose-controller-only inline strip — transport buttons + rig
            // hover hint live here so the user always sees `[edit] hover: …
            // selected: … axis: X` and can hit Reset / Stop native / Stop
            // idle / Resume idle no matter which workspace tab is open.
            // Only render when the Pose Controller surface is enabled, so
            // users running purely in chat / services mode keep a clean
            // menu bar.
            if pose_controller_open {
                ui.add_space(ui.available_width() / 3.);
                
                pose_controller::transport_toolbar(
                    ui,
                    &mut state.pose_controller,
                    sender.as_deref(),
                    active_anim.as_mut(),
                    &mut commands,
                    &vrma_entities,
                    &mut players_q,
                    &mut settings.pose_controller,
                    snapshot.as_deref(),
                    Some(&*undo),
                );
            }

            // Right-aligned: playback indicator, pipeline (ops), save hint.
            // Order matters: items added inside a `right_to_left` layout
            // appear right-to-left, so the **first** widget is rightmost.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(msg) = &state.save_status {
                    ui.colored_label(egui::Color32::from_rgb(150, 200, 150), msg);
                }
                ui.label(
                    egui::RichText::new(pipeline.menu_line())
                        .small()
                        .color(egui::Color32::from_rgb(200, 215, 255)),
                );
                if pose_controller_open {
                    pose_controller::playback_indicator(ui, &active_anim);
                    ui.separator();
                    pose_controller::draw_rig_hover_hint(ui, &mut state.pose_controller, &rig);
                    ui.separator();
                }
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
            .on_hover_text(
                "Writes the current values to config/user.toml (default.toml is preserved)",
            )
            .clicked()
        {
            state.save_status = Some(match settings.save_user() {
                Ok(()) => {
                    // Also write per-VRM graphics overrides so iOS picks up lighting per model.
                    if let Err(e) = write_vrm_graphics_override(settings) {
                        warn!("save per-VRM graphics override failed: {e}");
                    }
                    "saved → config/user.toml".to_string()
                }
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
                Ok(mut fresh) => {
                    fresh.migrate_workspace_visibility();
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
        ui.checkbox(&mut settings.ui.show_chat, "Chat");
        ui.separator();

        ui.label(egui::RichText::new("Workspaces").small().weak());
        ui.checkbox(&mut settings.ui.show_pose_controller, "Pose Controller")
            .on_hover_text(
                "Library / Animation / Bones (+expressions) / Rig / Intent Lab. \
                 Each tab can dock to its own side (left / right / bottom / floating) \
                 from the global Pose Tools toolbar or the per-tab ⋮ menu.",
            );
        ui.checkbox(&mut settings.ui.show_service_hub, "Service Hub")
            .on_hover_text(
                "Tabbed view of Channel hub, Gateway, TTS, MCP, and the live \
                 Services overview — replaces five floating windows.",
            );
        ui.checkbox(&mut settings.ui.show_graphics_workspace, "Graphics workspace")
            .on_hover_text(
                "Tabbed Lights / Advanced / Look-at view — Look-at no longer needs \
                 its own floating window.",
            );
        ui.checkbox(&mut settings.ui.show_diagnostics_workspace, "Diagnostics")
            .on_hover_text(
                "Chat pipeline, avatar Y-axis stats, and a quick-jump to the \
                 Network trace window.",
            );

        // ---- Pose Controller per-tab show/hide ----
        if settings.ui.show_pose_controller {
            ui.separator();
            ui.label(egui::RichText::new("Pose Controller panels").small().weak());
            pose_panel_visibility_menu(ui, settings);
        }

        ui.separator();
        ui.label(egui::RichText::new("Standalone panels").small().weak());
        anim_layers_visibility_menu(ui, settings);
        ui.checkbox(&mut settings.ui.show_avatar, "Avatar");
        ui.checkbox(&mut settings.ui.show_camera, "Camera");
        ui.checkbox(&mut settings.ui.show_emotion_mappings, "Emotion Mappings");
        ui.checkbox(&mut settings.ui.show_home_assistant, "Home Assistant");
        ui.checkbox(&mut settings.ui.show_graphics_advanced, "Graphics Advanced");
        ui.checkbox(&mut settings.ui.show_network_trace, "Network trace");
    });
}

/// Per-Pose-Controller-tab show/hide rows — each row is `[ ☑ Tab ] [ side ▼ ]`.
fn pose_panel_visibility_menu(ui: &mut egui::Ui, settings: &mut Settings) {
    use pose_controller::PoseControllerTab;
    let default_side = settings.ui.pose_controller_dock_side.clone();
    for tab in PoseControllerTab::all() {
        let key = tab.config_key().to_string();
        let current = settings
            .ui
            .pose_controller_tab_dock_sides
            .get(&key)
            .cloned()
            .unwrap_or_else(|| default_side.clone());
        let visible = current != "hidden";
        ui.horizontal(|ui| {
            let mut new_visible = visible;
            if ui.checkbox(&mut new_visible, tab.label()).changed() {
                if new_visible {
                    settings.ui.pose_controller_tab_dock_sides.remove(&key);
                } else {
                    settings
                        .ui
                        .pose_controller_tab_dock_sides
                        .insert(key.clone(), "hidden".to_string());
                }
            }
            ui.menu_button(format!("{} ▼", side_label_for(&current)), |ui| {
                let mut send = |ui: &mut egui::Ui, label: &str, target: &str| {
                    let active = current == target;
                    if ui
                        .add_enabled(!active, egui::Button::new(label))
                        .clicked()
                    {
                        settings
                            .ui
                            .pose_controller_tab_dock_sides
                            .insert(key.clone(), target.to_string());
                        ui.close();
                    }
                };
                send(ui, "◀ Left", "left");
                send(ui, "▶ Right", "right");
                send(ui, "▼ Bottom", "bottom");
                send(ui, "⬚ Floating", "floating");
                ui.separator();
                if ui.button("↺ Default side").clicked() {
                    settings.ui.pose_controller_tab_dock_sides.remove(&key);
                    ui.close();
                }
            });
        });
    }
}

/// Animation Layers show/hide + side picker — exposed under the View menu's
/// "Standalone panels" group for parity with the per-tab pose menu.
fn anim_layers_visibility_menu(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut settings.ui.show_anim_layers, "Animation Layers");
        let current = settings.ui.anim_layers_dock_side.clone();
        ui.menu_button(format!("{} ▼", side_label_for(&current)), |ui| {
            let mut button = |ui: &mut egui::Ui, label: &str, target: &str| {
                let active = current == target;
                if ui
                    .add_enabled(!active, egui::Button::new(label))
                    .clicked()
                {
                    settings.ui.anim_layers_dock_side = target.to_string();
                    settings.ui.show_anim_layers = true;
                    ui.close();
                }
            };
            button(ui, "▼ Bottom (dopesheet)", "bottom");
            button(ui, "◀ Left", "left");
            button(ui, "▶ Right", "right");
            button(ui, "⬚ Floating", "floating");
        });
    });
}

fn side_label_for(side: &str) -> &'static str {
    match side {
        "left" => "◀",
        "right" => "▶",
        "bottom" => "▼",
        "floating" => "⬚",
        "hidden" => "⊘",
        _ => "—",
    }
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
            ui.label(
                "This deletes `config/user.toml` and reloads values from `config/default.toml`.",
            );
            ui.label("Any unsaved changes in this session will be lost.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.confirm_restore = false;
                }
                if ui
                    .add(
                        egui::Button::new("Yes, restore")
                            .fill(egui::Color32::from_rgb(120, 60, 60)),
                    )
                    .clicked()
                {
                    state.save_status = Some(match Settings::restore_defaults() {
                        Ok(mut fresh) => {
                            fresh.migrate_workspace_visibility();
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
        .default_width(440.0)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                ui.label("Native Rust Bevy VRM client for IronClaw.");
                ui.separator();

                ui.label("Keybinds:");
                ui.monospace("  LMB drag     orbit camera");
                ui.monospace("  MMB drag     pan camera");
                ui.monospace("  scroll       zoom");
                ui.monospace("  Ctrl+Enter   (chat) send message");
                ui.monospace("  RMB on bone  pick a bone (rig edit mode)");
                ui.monospace("  Shift+drag   precision (rig handle / bone slider)");
                ui.separator();

                ui.label("Pose Controller tips:");
                ui.small(
                    "• Right-click a bone row to reset that bone to rest.\n\
                     • Pop tabs out into floating windows from the tab bar (📌).\n\
                     • Switch the dock side or undock the whole panel from the\n  workspace header (Left / Right / Float).\n\
                     • Mirror modes: realtime toggle in the Rig tab + per-chain\n  one-shot mirror actions (arm / leg / side / all paired).\n\
                     • Bone-list slider Shift = 0.1× sensitivity for fine tuning.",
                );
                ui.separator();

                ui.label("Workspaces:");
                ui.small(
                    "• Service Hub — Channel hub, Gateway, TTS, MCP and the live\n  Services overview in one tabbed window.\n\
                     • Graphics workspace — Lights, Advanced shortcut, Look-at.\n\
                     • Diagnostics — pipeline status, avatar Y-axis stats, Network trace.",
                );
                ui.separator();

                ui.label("Config files:");
                ui.monospace("  config/default.toml   factory defaults");
                ui.monospace("  config/user.toml      your overrides");
            });
        });
    state.show_about = open;
}
