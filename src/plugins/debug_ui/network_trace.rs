//! Per-service traffic log with JSON tree inspection.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use egui_json_tree::JsonTree;

use jarvis_avatar::config::Settings;

use super::DebugUiState;
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

pub fn draw_network_trace_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    log: Option<Res<TrafficLogSink>>,
    mut dbg: ResMut<DebugUiState>,
) {
    if !settings.ui.show_network_trace {
        return;
    }
    let Some(log) = log.as_ref() else {
        return;
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_network_trace;
    egui::Window::new("Network trace")
        .default_width(720.0)
        .default_height(480.0)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::SidePanel::left("net_trace_sidebar")
                .resizable(true)
                .default_width(220.0)
                .width_range(160.0..=400.0)
                .show_inside(ui, |ui| {
                    ui.vertical_centered_justified(|ui| {
                        ui.strong("Channel");
                        let ch_before = dbg.network_trace_channel;
                        egui::ComboBox::from_id_salt("net_trace_ch")
                            .selected_text(dbg.network_trace_channel.label())
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                for ch in TrafficChannel::ALL {
                                    ui.selectable_value(
                                        &mut dbg.network_trace_channel,
                                        *ch,
                                        ch.label(),
                                    );
                                }
                            });
                        if ch_before != dbg.network_trace_channel {
                            dbg.network_trace_pick = None;
                        }

                        if ui
                            .add_sized(
                                [ui.available_width(), 28.0],
                                egui::Button::new("Clear channel"),
                            )
                            .clicked()
                        {
                            log.clear_one(dbg.network_trace_channel);
                            dbg.network_trace_pick = None;
                        }
                        if ui
                            .add_sized(
                                [ui.available_width(), 28.0],
                                egui::Button::new("Clear all"),
                            )
                            .clicked()
                        {
                            log.clear_all();
                            dbg.network_trace_pick = None;
                        }

                        let mut paused = log.is_paused();
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 120.0).max(0.0) * 0.5);
                            if ui.checkbox(&mut paused, "Pause capture").changed() {
                                log.set_paused(paused);
                            }
                        });
                    });
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    let entries = log.snapshot_channel(dbg.network_trace_channel);
                    egui::ScrollArea::vertical()
                        .id_salt("net_trace_central_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.strong("Log entries");
                            ui.separator();
                            for i in (0..entries.len()).rev() {
                                let e = &entries[i];
                                let dir = match e.direction {
                                    TrafficDirection::Inbound => "in",
                                    TrafficDirection::Outbound => "out",
                                };
                                let sel = dbg.network_trace_pick == Some(i);
                                let label = format!(
                                    "{} {} {}",
                                    e.unix_ms,
                                    dir,
                                    e.summary.chars().take(120).collect::<String>()
                                );
                                if ui.selectable_label(sel, label).clicked() {
                                    dbg.network_trace_pick = Some(i);
                                }
                            }

                            ui.add_space(12.0);
                            ui.strong("Detail");
                            ui.separator();
                            if let Some(i) = dbg.network_trace_pick {
                                if let Some(e) = entries.get(i) {
                                    ui.monospace(&e.summary);
                                    ui.separator();
                                    if let Some(ref p) = e.payload {
                                        JsonTree::new(
                                            ui.make_persistent_id(("json_tree", i)),
                                            p,
                                        )
                                        .show(ui);
                                    } else {
                                        ui.label("(no JSON payload)");
                                    }
                                } else {
                                    ui.label("Pick a row from the list above.");
                                }
                            } else {
                                ui.label("Select a log row above.");
                            }
                        });
                });
        });
    settings.ui.show_network_trace = open;
}
