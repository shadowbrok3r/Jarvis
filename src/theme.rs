use bevy_egui::egui::{Color32, Context, Id, Ui};

const SUCCESS_KEY: &str = "jarvis.theme.success_color";
const ACCENT2_KEY: &str = "jarvis.theme.accent_secondary";

fn success_id() -> Id { Id::new(SUCCESS_KEY) }
fn accent2_id() -> Id { Id::new(ACCENT2_KEY) }

pub fn set_success_color(ctx: &Context, c: Color32) {
    ctx.data_mut(|d| d.insert_temp(success_id(), c));
}

pub fn set_accent_secondary(ctx: &Context, c: Color32) {
    ctx.data_mut(|d| d.insert_temp(accent2_id(), c));
}

pub fn error(ui: &Ui) -> Color32 {
    ui.visuals().error_fg_color
}

pub fn warn(ui: &Ui) -> Color32 {
    ui.visuals().warn_fg_color
}

pub fn info(ui: &Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

pub fn success(ui: &Ui) -> Color32 {
    success_ctx(ui.ctx())
}

pub fn success_ctx(ctx: &Context) -> Color32 {
    ctx.data(|d| d.get_temp::<Color32>(success_id()))
        .unwrap_or(Color32::from_rgb(72, 199, 142))
}

pub fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

pub fn accent_secondary(ui: &Ui) -> Color32 {
    accent_secondary_ctx(ui.ctx())
}

pub fn accent_secondary_ctx(ctx: &Context) -> Color32 {
    ctx.data(|d| d.get_temp::<Color32>(accent2_id()))
        .unwrap_or(Color32::from_rgb(191, 33, 101))
}

pub fn strong_text(ui: &Ui) -> Color32 {
    ui.visuals().strong_text_color()
}

pub fn weak_text(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

pub fn border(ui: &Ui) -> Color32 {
    ui.visuals().window_stroke.color
}

pub fn bg_surface(ui: &Ui) -> Color32 {
    ui.visuals().panel_fill
}

pub fn bg_faint(ui: &Ui) -> Color32 {
    ui.visuals().faint_bg_color
}

pub fn bg_extreme(ui: &Ui) -> Color32 {
    ui.visuals().extreme_bg_color
}
