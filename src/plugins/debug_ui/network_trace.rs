//! Per-service traffic log with JSON tree inspection.

use bevy_egui::egui;
use egui_json_tree::JsonTree;

use super::DebugUiState;
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

/// Inline Network-trace surface: a compact control row (channel picker + clear
/// + pause) with the log entries and detail stacked vertically below it. Drawn
/// inside the Diagnostics workspace "Network trace" tab.
pub fn network_trace_panel(ui: &mut egui::Ui, log: &TrafficLogSink, dbg: &mut DebugUiState) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Channel");
        let ch_before = dbg.network_trace_channel;
        egui::ComboBox::from_id_salt("net_trace_ch")
            .selected_text(dbg.network_trace_channel.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for ch in TrafficChannel::ALL {
                    ui.selectable_value(&mut dbg.network_trace_channel, *ch, ch.label());
                }
            });
        if ch_before != dbg.network_trace_channel {
            dbg.network_trace_pick = None;
        }
        if ui.button("Clear channel").clicked() {
            log.clear_one(dbg.network_trace_channel);
            dbg.network_trace_pick = None;
        }
        if ui.button("Clear all").clicked() {
            log.clear_all();
            dbg.network_trace_pick = None;
        }
        let mut paused = log.is_paused();
        if ui.checkbox(&mut paused, "Pause capture").changed() {
            log.set_paused(paused);
        }
    });
    ui.separator();

    let entries = log.snapshot_channel(dbg.network_trace_channel);
    egui::ScrollArea::vertical()
        .id_salt("net_trace_scroll")
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
                        JsonTree::new(ui.make_persistent_id(("json_tree", i)), p).show(ui);
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
}
