//! Consolidated "Settings" workspace.
//!
//! Folds the former Avatar, Camera, Emotion Mappings, and Graphics Advanced
//! windows into one tabbed window so users keep a single settings surface open.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;

use super::emotion_mappings::{emotion_mappings_panel, EmotionPanelParams};
use super::graphics_advanced::{graphics_panel, GraphicsPanelParams};
use super::sections::{avatar_panel, camera_panel, AvatarPanelParams};
use super::DebugUiState;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Avatar,
    Camera,
    Emotion,
    Graphics,
}

#[derive(Resource, Default)]
pub struct SettingsUiState {
    pub tab: SettingsTab,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_settings_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut dbg: ResMut<DebugUiState>,
    mut sw: ResMut<SettingsUiState>,
    mut avatar_p: AvatarPanelParams,
    mut emotion_p: EmotionPanelParams,
    mut graphics_p: GraphicsPanelParams,
) {
    if !settings.ui.show_settings {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_settings;
    egui::Window::new("Settings")
        .default_width(560.0)
        .default_height(620.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut sw.tab, SettingsTab::Avatar, "Avatar");
                ui.selectable_value(&mut sw.tab, SettingsTab::Camera, "Camera");
                ui.selectable_value(&mut sw.tab, SettingsTab::Emotion, "Emotion mappings");
                ui.selectable_value(&mut sw.tab, SettingsTab::Graphics, "Graphics");
            });
            ui.separator();

            match sw.tab {
                SettingsTab::Avatar => {
                    avatar_panel(ui, &mut settings, &mut dbg, &mut avatar_p);
                }
                SettingsTab::Camera => {
                    camera_panel(ui, &mut settings, &mut dbg);
                }
                SettingsTab::Emotion => {
                    emotion_mappings_panel(ui, &mut dbg, &mut emotion_p);
                }
                SettingsTab::Graphics => {
                    graphics_panel(ui, &mut settings, &mut dbg, &mut graphics_p);
                }
            }
        });
    settings.ui.show_settings = open;
}
