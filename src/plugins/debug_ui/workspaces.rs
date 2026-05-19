//! Phase-5 consolidated workspaces for the debug UI overhaul.
//!
//! These windows replace the per-service / per-feature floating windows with
//! tabbed "workspaces" that group related controls together so users only need
//! to keep one window open per concern (services, graphics, diagnostics).

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;

use super::sections::{
    channel_hub_panel, gateway_panel, look_at_panel, mcp_panel, tts_panel,
};
use super::services::services_panel;
use crate::plugins::avatar::AvatarDebugStats;
use crate::plugins::chat_pipeline_status::ChatPipelineStatus;
use crate::plugins::service_status::ServiceStatus;

// ---------- Service Hub workspace ---------------------------------------------

/// Which sub-panel is active in the consolidated Service Hub window.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHubTab {
    #[default]
    Overview,
    ChannelHub,
    Gateway,
    Tts,
    Mcp,
}

#[derive(Resource, Default)]
pub struct ServiceHubUiState {
    pub tab: ServiceHubTab,
}

pub fn draw_service_hub_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<ServiceHubUiState>,
    status: Option<Res<ServiceStatus>>,
) {
    if !settings.ui.show_service_hub {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_service_hub;
    egui::Window::new("Service Hub")
        .default_width(620.0)
        .default_height(440.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut state.tab, ServiceHubTab::Overview, "Overview");
                ui.selectable_value(&mut state.tab, ServiceHubTab::ChannelHub, "Channel Hub");
                ui.selectable_value(&mut state.tab, ServiceHubTab::Gateway, "Gateway");
                ui.selectable_value(&mut state.tab, ServiceHubTab::Tts, "TTS");
                ui.selectable_value(&mut state.tab, ServiceHubTab::Mcp, "MCP");
            });
            ui.separator();

            match state.tab {
                ServiceHubTab::Overview => {
                    services_panel(ui, &mut settings, status.as_deref(), &mut state);
                }
                ServiceHubTab::ChannelHub => {
                    channel_hub_panel(ui, &mut settings);
                }
                ServiceHubTab::Gateway => {
                    gateway_panel(ui, &mut settings);
                }
                ServiceHubTab::Tts => {
                    tts_panel(ui, &mut settings);
                }
                ServiceHubTab::Mcp => {
                    mcp_panel(ui, &mut settings);
                }
            }
        });
    settings.ui.show_service_hub = open;
}

// ---------- Graphics workspace -------------------------------------------------

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsWorkspaceTab {
    #[default]
    Lights,
    Advanced,
    LookAt,
}

#[derive(Resource, Default)]
pub struct GraphicsWorkspaceUiState {
    pub tab: GraphicsWorkspaceTab,
}

pub fn draw_graphics_workspace_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<GraphicsWorkspaceUiState>,
) {
    if !settings.ui.show_graphics_workspace {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_graphics_workspace;
    egui::Window::new("Graphics workspace")
        .default_width(420.0)
        .default_height(520.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.tab, GraphicsWorkspaceTab::Lights, "Lights / basic");
                ui.selectable_value(
                    &mut state.tab,
                    GraphicsWorkspaceTab::Advanced,
                    "Advanced",
                );
                ui.selectable_value(&mut state.tab, GraphicsWorkspaceTab::LookAt, "Look-at");
            });
            ui.separator();

            match state.tab {
                GraphicsWorkspaceTab::Lights => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        super::sections::draw_basic_graphics_inline(ui, &mut settings);
                    });
                }
                GraphicsWorkspaceTab::Advanced => {
                    ui.label(
                        "Advanced post-processing (tonemapping, bloom, MToon material editor) \
                         lives in the dedicated Graphics Advanced window because it owns mesh \
                         queries that can't be inlined here.",
                    );
                    if ui.button("Open Graphics Advanced…").clicked() {
                        settings.ui.show_graphics_advanced = true;
                    }
                }
                GraphicsWorkspaceTab::LookAt => {
                    look_at_panel(ui, &mut settings);
                }
            }
        });
    settings.ui.show_graphics_workspace = open;
}

// ---------- Diagnostics workspace ---------------------------------------------

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsTab {
    #[default]
    Pipeline,
    Avatar,
    Network,
}

#[derive(Resource, Default)]
pub struct DiagnosticsUiState {
    pub tab: DiagnosticsTab,
}

pub fn draw_diagnostics_workspace_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DiagnosticsUiState>,
    pipeline: Res<ChatPipelineStatus>,
    stats: Res<AvatarDebugStats>,
) {
    if !settings.ui.show_diagnostics_workspace {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_diagnostics_workspace;
    egui::Window::new("Diagnostics")
        .default_width(520.0)
        .default_height(420.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.tab, DiagnosticsTab::Pipeline, "Chat pipeline");
                ui.selectable_value(&mut state.tab, DiagnosticsTab::Avatar, "Avatar (Y-axis)");
                ui.selectable_value(&mut state.tab, DiagnosticsTab::Network, "Network trace");
            });
            ui.separator();

            match state.tab {
                DiagnosticsTab::Pipeline => {
                    ui.label("Live chat / TTS pipeline state:");
                    ui.monospace(pipeline.menu_line());
                    ui.add_space(6.0);
                    ui.small(
                        "The full per-stage breakdown lives in the menu-bar status line. \
                         Add deeper diagnostics here as new stages come online.",
                    );
                }
                DiagnosticsTab::Avatar => {
                    super::sections::draw_avatar_y_diag_inline(
                        ui,
                        &stats,
                        settings.avatar.world_position[1],
                    );
                }
                DiagnosticsTab::Network => {
                    ui.label(
                        "The full Network trace surface owns its own JSON tree + traffic \
                         queries; open it as a separate window.",
                    );
                    if ui.button("Open Network trace…").clicked() {
                        settings.ui.show_network_trace = true;
                    }
                }
            }
        });
    settings.ui.show_diagnostics_workspace = open;
}
