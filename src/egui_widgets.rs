//! Small egui layout helpers for debug UI.

use bevy_egui::egui::{self, Button, WidgetText};

/// Full-width selectable row with label at the top-left.
///
/// `Ui::add_sized` uses `Layout::centered_and_justified` and centers text in the cell.
pub fn full_width_selectable_row(
    ui: &mut egui::Ui,
    selected: bool,
    label: impl Into<WidgetText>,
    row_h: f32,
) -> egui::Response {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(w, row_h),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.add(
                Button::selectable(selected, label)
                    .min_size(egui::vec2(ui.available_width(), row_h)),
            )
        },
    )
    .inner
}
