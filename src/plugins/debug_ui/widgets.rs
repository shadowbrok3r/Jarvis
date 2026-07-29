//! Small egui helpers shared across debug UI windows.

use bevy_egui::egui;

/// Remaining screen rect for top-level `Panel`s across chained egui systems.
///
/// egui 0.35's `Panel::show` takes `&mut Ui` (not `Context`). bevy_egui's
/// multipass discards its root `Ui`, so each panel system builds a dock root
/// from this remaining rect and commits what panels left behind.
#[derive(Default)]
pub struct EguiDockLayout {
    available: Option<egui::Rect>,
    next_salt: u64,
}

impl EguiDockLayout {
    pub fn reset(&mut self) {
        self.available = None;
        self.next_salt = 0;
    }

    pub fn begin(&mut self, ctx: &egui::Context) -> egui::Ui {
        let max_rect = self.available.unwrap_or_else(|| ctx.viewport_rect());
        let salt = self.next_salt;
        self.next_salt = salt.wrapping_add(1);
        egui::Ui::new(
            ctx.clone(),
            egui::Id::new(("jarvis_dock_root", ctx.viewport_id(), salt)),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(max_rect),
        )
    }

    pub fn end(&mut self, ui: &egui::Ui) {
        self.available = Some(ui.available_rect_before_wrap());
    }
}

pub fn vec3_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut [f32; 3],
    range: std::ops::RangeInclusive<f32>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::DragValue::new(&mut v[0])
                .speed(0.01)
                .range(*range.start()..=*range.end())
                .prefix("x "),
        );
        ui.add(
            egui::DragValue::new(&mut v[1])
                .speed(0.01)
                .range(*range.start()..=*range.end())
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut v[2])
                .speed(0.01)
                .range(*range.start()..=*range.end())
                .prefix("z "),
        );
    });
}

pub fn rgb_row(ui: &mut egui::Ui, v: &mut [f32; 3]) {
    let mut srgb = [
        linear_to_srgb(v[0]),
        linear_to_srgb(v[1]),
        linear_to_srgb(v[2]),
    ];
    if ui.color_edit_button_rgb(&mut srgb).changed() {
        v[0] = srgb_to_linear(srgb[0]);
        v[1] = srgb_to_linear(srgb[1]);
        v[2] = srgb_to_linear(srgb[2]);
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut v[0])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("r "),
        );
        ui.add(
            egui::DragValue::new(&mut v[1])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("g "),
        );
        ui.add(
            egui::DragValue::new(&mut v[2])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("b "),
        );
    });
}

pub fn rgba_row(ui: &mut egui::Ui, v: &mut [f32; 4]) {
    let mut srgba = [
        linear_to_srgb(v[0]),
        linear_to_srgb(v[1]),
        linear_to_srgb(v[2]),
        v[3].clamp(0.0, 1.0),
    ];
    if ui.color_edit_button_rgba_unmultiplied(&mut srgba).changed() {
        v[0] = srgb_to_linear(srgba[0]);
        v[1] = srgb_to_linear(srgba[1]);
        v[2] = srgb_to_linear(srgba[2]);
        v[3] = srgba[3];
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut v[0])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("r "),
        );
        ui.add(
            egui::DragValue::new(&mut v[1])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("g "),
        );
        ui.add(
            egui::DragValue::new(&mut v[2])
                .speed(0.005)
                .range(0.0..=8.0)
                .prefix("b "),
        );
        ui.add(
            egui::DragValue::new(&mut v[3])
                .speed(0.005)
                .range(0.0..=1.0)
                .prefix("a "),
        );
    });
}

pub fn linear_to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

pub fn srgb_to_linear(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.040_45 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}
