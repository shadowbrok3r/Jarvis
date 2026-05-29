//! Phase-5 consolidated workspaces for the debug UI overhaul.
//!
//! These windows replace the per-service / per-feature floating windows with
//! tabbed "workspaces" that group related controls together so users only need
//! to keep one window open per concern (services, graphics, diagnostics).

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;

use super::network_trace::network_trace_panel;
use super::sections::{channel_hub_panel, gateway_panel, mcp_panel, tts_panel};
use super::services::services_panel;
use super::DebugUiState;
use crate::plugins::avatar::AvatarDebugStats;
use crate::plugins::chat_pipeline_status::ChatPipelineStatus;
use crate::plugins::service_status::ServiceStatus;
use crate::plugins::traffic_log::TrafficLogSink;

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
    log: Option<Res<TrafficLogSink>>,
    mut dbg: ResMut<DebugUiState>,
) {
    if !settings.ui.show_diagnostics_workspace {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_diagnostics_workspace;
    egui::Window::new("Diagnostics")
        .default_width(560.0)
        .default_height(520.0)
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
                DiagnosticsTab::Network => match log.as_deref() {
                    Some(log) => network_trace_panel(ui, log, &mut dbg),
                    None => {
                        ui.label("Traffic log not initialised yet.");
                    }
                },
            }
        });
    settings.ui.show_diagnostics_workspace = open;
}
