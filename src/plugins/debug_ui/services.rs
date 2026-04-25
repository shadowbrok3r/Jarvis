//! Consolidated "Services" window.
//!
//! Renders one row per tracked service with a colored status dot, human
//! label, endpoint, and detail line. Also exposes quick jump-to-config
//! buttons that open the per-service config windows that still live in
//! `sections.rs`.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use jarvis_avatar::config::Settings;

use crate::plugins::service_status::{ServiceId, ServiceState, ServiceStatus};

pub fn draw_services_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    status: Option<Res<ServiceStatus>>,
) {
    if !settings.ui.show_services {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };

    let mut open = settings.ui.show_services;
    egui::Window::new("Services")
        .default_width(560.0)
        .default_height(360.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Live connection state for every external service the avatar talks to.");
            ui.separator();

            let Some(status) = status.as_ref() else {
                ui.colored_label(
                    egui::Color32::from_rgb(235, 85, 100),
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
                    let u = &mut settings.ui;
                    if ui.button("Channel hub config…").clicked() {
                        u.show_channel_hub = true;
                    }
                    if ui.button("Gateway config…").clicked() {
                        u.show_gateway = true;
                    }
                    if ui.button("TTS config…").clicked() {
                        u.show_tts = true;
                    }
                    if ui.button("MCP config…").clicked() {
                        u.show_mcp = true;
                    }
                });
            });
        });
    settings.ui.show_services = open;
}

fn status_dot(ui: &mut egui::Ui, state: ServiceState) {
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    painter.circle_filled(center, 5.0, state.color());
    painter.circle_stroke(
        center,
        5.0,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
    );
}
