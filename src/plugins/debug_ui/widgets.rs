//! Small egui helpers shared across debug UI windows.

use bevy_egui::egui;

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
