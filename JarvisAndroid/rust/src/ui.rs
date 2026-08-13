//! On-device egui overlay.
//!
//! Touch-first: no control depends on hover, and rows are sized for a finger.
//! Hover text is still attached where it is cheap, for the desktop-parity habit.

use bevy::prelude::*;
use bevy_egui::egui::{self, RichText};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use jarvis_avatar::plugins::anim_layers::LayerStackHandle;
use jarvis_avatar::plugins::channel_server::TtsSpeakMessage;
use jarvis_avatar::{egui_theme, icons, theme};

pub struct AndroidUiPlugin;

/// The overlay's draw systems, so the IME driver can order its post-UI mirror
/// after the frame's `TextEdit` state exists.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct UiDrawSet;

impl Plugin for AndroidUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AndroidUiState>().add_systems(
            EguiPrimaryContextPass,
            (apply_theme, menu_bar, windows).chain().in_set(UiDrawSet),
        );
    }
}

#[derive(Resource, Default)]
pub struct AndroidUiState {
    theme_applied: bool,
    show_layers: bool,
    show_console: bool,
    console_input: String,
    console_log: Vec<String>,
    fps: f32,
}

/// Status-bar / cutout / nav-bar insets in egui points.
///
/// Android 15 is edge-to-edge, so the surface spans the whole display and the
/// status bar would otherwise sit on top of the menu bar. `bevy_winit` does not
/// surface insets, but `AndroidApp::content_rect` does.
/// Minimum top inset. Under edge-to-edge, `content_rect` usually reports the
/// full window (top = 0) because the app *is* drawing behind the status bar, so
/// a floor keeps the menu bar clear of it. Real per-edge insets need a JNI
/// `WindowInsets` read — that arrives with the IME bridge.
const MIN_TOP_INSET_PT: f32 = 26.0;

fn safe_area(ctx: &egui::Context) -> (f32, f32) {
    let Some(app) = bevy::android::ANDROID_APP.get() else {
        return (MIN_TOP_INSET_PT, 0.0);
    };
    let Some(window) = app.native_window() else {
        return (MIN_TOP_INSET_PT, 0.0);
    };
    let rect = app.content_rect();
    let ppp = ctx.pixels_per_point().max(0.1);
    let top = (rect.top as f32 / ppp).max(MIN_TOP_INSET_PT);
    let bottom = ((window.height() - rect.bottom) as f32 / ppp).max(0.0);
    (top, bottom)
}

fn apply_theme(mut contexts: EguiContexts, mut state: ResMut<AndroidUiState>) {
    if state.theme_applied {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };
    state.theme_applied = true;

    let mut fonts = egui::FontDefinitions::default();
    icons::install_fonts(&mut fonts);
    ctx.set_fonts(fonts);

    match serde_json::from_str::<egui::Style>(egui_theme::STYLE) {
        Ok(style) => ctx.set_global_style(std::sync::Arc::new(style)),
        Err(e) => error!("egui theme parse failed: {e}"),
    }

    // Phone-sized hit targets; the desktop theme is tuned for a mouse.
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = egui::vec2(10.0, 8.0);
        s.spacing.interact_size.y = 34.0;
        s.spacing.slider_width = 160.0;
    });
}

fn menu_bar(
    mut contexts: EguiContexts,
    mut state: ResMut<AndroidUiState>,
    time: Res<Time>,
    settings: Res<jarvis_avatar::config::Settings>,
    hub: Option<Res<crate::hub_client::HubLink>>,
) {
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let (top_inset, _) = safe_area(ctx);

    let dt = time.delta_secs();
    if dt > 0.0 {
        state.fps = if state.fps <= 0.0 {
            1.0 / dt
        } else {
            state.fps * 0.9 + (1.0 / dt) * 0.1
        };
    }

    let s = &mut *state;
    let model = std::path::Path::new(&settings.avatar.model_path)
        .file_stem()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();

    // egui 0.35 dropped root-level TopBottomPanel; `Panel` only nests in a `Ui`,
    // and a CentralPanel would paint over the 3D scene. An Area floats instead.
    let width = ctx.viewport_rect().width();
    egui::Area::new(egui::Id::new("android_menu_bar"))
        .fixed_pos(egui::pos2(0.0, top_inset))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    // An Area auto-sizes to content, so `set_width` alone does not
                    // stretch the bar; allocate the span explicitly.
                    ui.allocate_ui_with_layout(
                        egui::vec2(width - 16.0, 30.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.menu_button(icons::icon(icons::LIST), |ui| {
                                ui.checkbox(&mut s.show_layers, "Layers");
                                ui.checkbox(&mut s.show_console, "Console");
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{:.0} fps", s.fps))
                                            .small()
                                            .color(theme::weak_text(ui)),
                                    );
                                    ui.separator();
                                    // Hub link: dot only, since touch has no hover.
                                    if let Some(hub) = &hub {
                                        let (glyph, color) = if hub.connected {
                                            ("• hub", theme::success(ui))
                                        } else {
                                            ("• hub", theme::weak_text(ui))
                                        };
                                        ui.label(RichText::new(glyph).small().color(color))
                                            .on_hover_text(format!(
                                                "{} ({} frames)",
                                                hub.url, hub.frames
                                            ));
                                        ui.separator();
                                    }
                                    ui.label(
                                        RichText::new(model).small().color(theme::weak_text(ui)),
                                    );
                                },
                            );
                        },
                    );
                });
        });
}

fn windows(
    mut contexts: EguiContexts,
    mut state: ResMut<AndroidUiState>,
    stack: Option<Res<LayerStackHandle>>,
    mut speak: MessageWriter<TtsSpeakMessage>,
) {
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let (top_inset, _) = safe_area(ctx);
    let s = &mut *state;

    if s.show_layers {
        let mut open = true;
        egui::Window::new("Layers")
            .default_pos(egui::pos2(8.0, top_inset + 52.0))
            .default_size(egui::vec2(320.0, 380.0))
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| match &stack {
                Some(stack) => draw_layers(ui, stack),
                None => {
                    ui.label("Layer stack not initialised.");
                }
            });
        s.show_layers = open;
    }

    if s.show_console {
        let mut open = true;
        egui::Window::new("Console")
            .default_pos(egui::pos2(8.0, top_inset + 120.0))
            .default_size(egui::vec2(340.0, 260.0))
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(150.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &s.console_log {
                            ui.label(RichText::new(line).small());
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let send = ui.button(icons::icon(icons::PLAY)).clicked();
                    let edit = ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::TextEdit::singleline(&mut s.console_input)
                            .hint_text("type here…")
                            .desired_width(f32::INFINITY),
                    );
                    let entered =
                        edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (send || entered) && !s.console_input.trim().is_empty() {
                        let text = std::mem::take(&mut s.console_input);
                        // Drives the same TTS path as the desktop hub: Kokoro
                        // HTTP -> bevy_audio, plus A2F visemes on the rig.
                        speak.write(TtsSpeakMessage { text: text.clone() });
                        s.console_log.push(text);
                        if s.console_log.len() > 200 {
                            s.console_log.remove(0);
                        }
                    }
                });
            });
        s.show_console = open;
    }
}

fn draw_layers(ui: &mut egui::Ui, stack: &LayerStackHandle) {
    let mut master = stack.with_read(|s| s.master_enabled);
    if ui.checkbox(&mut master, "Master enabled").changed() {
        stack.with_write(|s| s.master_enabled = master);
    }
    ui.separator();

    let rows: Vec<(u64, String, String, bool, f32)> = stack.with_read(|s| {
        s.layers
            .iter()
            .map(|l| {
                (
                    l.id,
                    l.label.clone(),
                    l.driver.kind_label().to_string(),
                    l.enabled,
                    l.weight,
                )
            })
            .collect()
    });

    if rows.is_empty() {
        ui.label(RichText::new("No layers.").color(theme::weak_text(ui)));
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (id, label, kind, enabled, weight) in rows {
                ui.horizontal(|ui| {
                    let mut on = enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        stack.with_write(|s| {
                            if let Some(l) = s.layers.iter_mut().find(|l| l.id == id) {
                                l.enabled = on;
                            }
                        });
                    }
                    ui.label(RichText::new(&label).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("[{kind}]")).small().color(theme::weak_text(ui)));
                    });
                });
                let mut w = weight;
                if ui
                    .add(egui::Slider::new(&mut w, 0.0..=1.0).show_value(false))
                    .changed()
                {
                    stack.with_write(|s| {
                        if let Some(l) = s.layers.iter_mut().find(|l| l.id == id) {
                            l.weight = w;
                        }
                    });
                }
                ui.separator();
            }
        });
}
