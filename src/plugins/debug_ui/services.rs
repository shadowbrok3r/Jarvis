//! Services overview panel for the Service Hub workspace.

use bevy::prelude::*;
use bevy_egui::egui;

use jarvis_avatar::config::Settings;
use jarvis_avatar::theme;

use crate::plugins::debug_ui::workspaces::{ServiceHubTab, ServiceHubUiState};
use crate::plugins::service_status::{ServiceId, ServiceState, ServiceStatus};

pub fn services_panel(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    status: Option<&ServiceStatus>,
    hub: &mut ServiceHubUiState,
) {
    ui.label("Live connection state for every external service the avatar talks to.");
    ui.separator();

    let Some(status) = status else {
        ui.colored_label(
            theme::error(ui),
            "ServiceStatus resource is missing — is ServiceStatusPlugin registered?",
        );
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("services_grid")
            .num_columns(4)
            .spacing([14.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Status");
                ui.strong("Service");
                ui.strong("Endpoint");
                ui.strong("Detail");
                ui.end_row();

                for id in ServiceId::ALL {
                    let entry = status.get(*id);
                    let state = entry.map(|e| e.state).unwrap_or(ServiceState::Unknown);
                    let endpoint = entry.map(|e| e.endpoint.as_str()).unwrap_or("");
                    let detail = entry.map(|e| e.detail.as_str()).unwrap_or("");

                    ui.horizontal(|ui| {
                        status_dot(ui, state);
                        ui.label(
                            egui::RichText::new(state.short())
                                .small()
                                .color(state.color()),
                        );
                    });
                    ui.label(id.label());
                    ui.monospace(if endpoint.is_empty() { "—" } else { endpoint });
                    ui.label(
                        egui::RichText::new(if detail.is_empty() { "—" } else { detail })
                            .small(),
                    );
                    ui.end_row();
                }
            });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Channel hub config…").clicked() {
                settings.ui.show_service_hub = true;
                hub.tab = ServiceHubTab::ChannelHub;
            }
            if ui.button("Gateway config…").clicked() {
                settings.ui.show_service_hub = true;
                hub.tab = ServiceHubTab::Gateway;
            }
            if ui.button("TTS config…").clicked() {
                settings.ui.show_service_hub = true;
                hub.tab = ServiceHubTab::Tts;
            }
            if ui.button("MCP config…").clicked() {
                settings.ui.show_service_hub = true;
                hub.tab = ServiceHubTab::Mcp;
            }
        });
    });
}

fn status_dot(ui: &mut egui::Ui, state: ServiceState) {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    painter.circle_filled(center, 5.0, state.color());
    painter.circle_stroke(
        center,
        5.0,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
    );
}
