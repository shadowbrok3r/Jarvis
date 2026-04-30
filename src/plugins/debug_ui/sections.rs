//! Individual dedicated `egui::Window` systems (everything except Chat).
//!
//! Each `draw_*_window` is an independent Bevy system: it bails out immediately
//! when the matching `settings.ui.show_*` flag is false so closed windows cost
//! almost nothing. The menu bar in [`super::draw_menu_bar`] flips those flags.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::act::Emotion;
use jarvis_avatar::config::Settings;
use jarvis_avatar::model_catalog::{list_vrm_models, models_dir, resolve_vrm_load_argument};

use super::widgets::{rgb_row, rgba_row, vec3_row};
use super::{AvatarVrmPickerState, DebugUiState};
use crate::plugins::avatar::AvatarDebugStats;
use crate::plugins::channel_server::{
    ChatCompleteMessage, HubBroadcast, HubState, LookAtRequestMessage, TtsSpeakMessage,
};
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};

// ---------- Avatar ------------------------------------------------------------

pub fn draw_avatar_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
    stats: Res<AvatarDebugStats>,
    pose_tx: Option<Res<PoseCommandSender>>,
) {
    if !settings.ui.show_avatar {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_avatar;
    egui::Window::new("Avatar")
        .default_width(380.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let a = &mut settings.avatar;
            ui.label("Current model ([avatar].model_path):");
            ui.monospace(a.model_path.as_str());
            ui.small(
                "Hot-swap updates this path immediately (same queue as MCP load_vrm). \
                 Process cwd should be the crate root so assets/models resolves on disk.",
            );

            ui.separator();
            ui.label("Pick VRM from assets/models/");
            ui.small(format!("Scan directory: {}", models_dir().display()));
            let picker = &mut state.avatar_vrm_picker;
            ui.horizontal(|ui| {
                ui.label("Filter:");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut picker.filter)
                            .desired_width(ui.available_width().max(120.0)),
                    )
                    .changed()
                {
                    picker.op_error = None;
                }
            });

            let filter_opt = {
                let t = picker.filter.trim();
                if t.is_empty() { None } else { Some(t) }
            };

            match list_vrm_models(filter_opt) {
                Ok(entries) => {
                    picker.list_error = None;
                    ui.horizontal(|ui| {
                        let can_load = picker.selected_basename.is_some() && pose_tx.is_some();
                        if ui
                            .add_enabled(can_load, egui::Button::new("Load selected"))
                            .on_disabled_hover_text(if pose_tx.is_none() {
                                "PoseCommandSender not available"
                            } else {
                                "Select a .vrm row first"
                            })
                            .clicked()
                        {
                            if let Some(name) = picker.selected_basename.clone() {
                                queue_avatar_vrm_load(pose_tx.as_deref(), name.as_str(), picker);
                            }
                        }
                    });

                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .id_salt("avatar_vrm_model_list")
                        .show(ui, |ui| {
                            for entry in &entries {
                                let selected = picker.selected_basename.as_deref()
                                    == Some(entry.basename.as_str());
                                let r = ui.selectable_label(selected, &entry.basename);
                                if r.double_clicked() {
                                    picker.selected_basename = Some(entry.basename.clone());
                                    queue_avatar_vrm_load(
                                        pose_tx.as_deref(),
                                        entry.basename.as_str(),
                                        picker,
                                    );
                                } else if r.clicked() {
                                    picker.selected_basename = Some(entry.basename.clone());
                                }
                            }
                            if entries.is_empty() {
                                ui.weak("(no matching .vrm files)");
                            }
                        });
                }
                Err(e) => {
                    picker.list_error = Some(e);
                }
            }

            if let Some(err) = &picker.list_error {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 140), err);
            }
            if let Some(err) = &picker.op_error {
                ui.colored_label(egui::Color32::from_rgb(255, 160, 120), err);
            }

            ui.separator();
            ui.label("Default idle VRMA (spawned with VRM; edit in config to change):");
            ui.add_enabled(false, egui::TextEdit::singleline(&mut a.idle_vrma_path));

            ui.separator();
            ui.label("world_position (pulls rig toward origin/focus):");
            vec3_row(ui, "pos", &mut a.world_position, -20.0..=20.0);
            ui.add(
                egui::Slider::new(&mut a.uniform_scale, 0.1..=10.0)
                    .logarithmic(true)
                    .text("uniform_scale"),
            );

            ui.separator();
            ui.label("Root-motion locking (see Y-diagnostics below):");
            ui.checkbox(
                &mut a.lock_root_xz,
                "lock_root_xz · snap hips X/Z to bind pose after VRMA",
            );
            ui.checkbox(
                &mut a.lock_root_y,
                "lock_root_y · snap hips Y to bind pose after VRMA",
            );
            ui.checkbox(
                &mut a.lock_vrm_root_y,
                "lock_vrm_root_y · hard-clamp VRM root entity Y to world_position.y",
            );

            ui.separator();
            y_diagnostics_readout(ui, &stats, a.world_position[1]);

            ui.separator();
            ui.label("background_color (RGBA linear):");
            rgba_row(ui, &mut a.background_color);

            ui.separator();
            ui.label("Window (restart required):");
            let mut w = a.window_width as i32;
            let mut h = a.window_height as i32;
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut w).range(320..=7680).prefix("w "));
                ui.add(egui::DragValue::new(&mut h).range(240..=4320).prefix("h "));
            });
            a.window_width = w.max(0) as u32;
            a.window_height = h.max(0) as u32;
        });
    settings.ui.show_avatar = open;
}

fn queue_avatar_vrm_load(
    pose_tx: Option<&PoseCommandSender>,
    load_arg: &str,
    picker: &mut AvatarVrmPickerState,
) {
    picker.op_error = None;
    let Some(tx) = pose_tx else {
        picker.op_error =
            Some("PoseCommandSender unavailable — PoseDriverPlugin must be active.".into());
        return;
    };
    match resolve_vrm_load_argument(load_arg) {
        Ok(asset_path) => {
            tx.send(PoseCommand::LoadVrm { asset_path });
        }
        Err(e) => {
            picker.op_error = Some(e);
        }
    }
}

fn y_diagnostics_readout(ui: &mut egui::Ui, stats: &AvatarDebugStats, target_y: f32) {
    ui.label("Y-axis diagnostics (this frame):");
    egui::Grid::new("avatar_y_diag_grid")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            row(ui, "target (world_position.y)", format_y(target_y));
            row(
                ui,
                "VRM root · local Y",
                if stats.has_vrm {
                    format_y(stats.vrm_root_local_y)
                } else {
                    "(no VRM loaded)".into()
                },
            );
            row(
                ui,
                "VRM root · world Y",
                if stats.has_vrm {
                    format_y(stats.vrm_root_world_y)
                } else {
                    "—".into()
                },
            );
            row(
                ui,
                "Hips · local Y",
                if stats.has_hips {
                    format_y(stats.hips_local_y)
                } else {
                    "(hips not resolved)".into()
                },
            );
            row(
                ui,
                "Hips · rest local Y",
                if stats.has_hips {
                    format_y(stats.hips_rest_local_y)
                } else {
                    "—".into()
                },
            );
            row(
                ui,
                "Hips · world Y",
                if stats.has_hips {
                    format_y(stats.hips_world_y)
                } else {
                    "—".into()
                },
            );
        });
    ui.small(
        "If 'VRM root · local Y' drifts, `lock_vrm_root_y` will pin it. \
         If 'Hips · local Y' drifts away from 'Hips · rest local Y', `lock_root_y` \
         will pin the hips. If neither drifts but the rig still looks bobbing, it's \
         spine/chest rotation (normal idle breathing).",
    );
}

fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.monospace(value);
    ui.end_row();
}

fn format_y(y: f32) -> String {
    format!("{y:+.5} m")
}

// ---------- Camera ------------------------------------------------------------

pub fn draw_camera_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
) {
    if !settings.ui.show_camera {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_camera;
    egui::Window::new("Camera")
        .default_width(360.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let cam = &mut settings.camera;
            ui.label("LMB orbit · MMB pan · scroll zoom");
            ui.separator();

            ui.label("Orbit focus (fallback before VRM snap):");
            vec3_row(ui, "focus", &mut cam.focus, -100.0..=100.0);

            ui.add(egui::Slider::new(&mut cam.initial_radius, 0.1..=50.0).text("initial_radius"));
            ui.add(
                egui::Slider::new(&mut cam.min_radius, 0.001..=5.0)
                    .logarithmic(true)
                    .text("min_radius (zoom in)"),
            );
            ui.add(
                egui::Slider::new(&mut cam.max_radius, 1.0..=500.0).text("max_radius (zoom out)"),
            );

            ui.separator();
            ui.add(
                egui::Slider::new(&mut cam.orbit_sensitivity, 0.05..=5.0).text("orbit_sensitivity"),
            );
            ui.add(egui::Slider::new(&mut cam.pan_sensitivity, 0.05..=5.0).text("pan_sensitivity"));
            ui.add(
                egui::Slider::new(&mut cam.zoom_sensitivity, 0.05..=5.0).text("zoom_sensitivity"),
            );

            ui.separator();
            ui.add(
                egui::Slider::new(&mut cam.orbit_smoothness, 0.0..=0.99).text("orbit_smoothness"),
            );
            ui.add(egui::Slider::new(&mut cam.zoom_smoothness, 0.0..=0.99).text("zoom_smoothness"));
            ui.add(egui::Slider::new(&mut cam.pan_smoothness, 0.0..=0.99).text("pan_smoothness"));

            ui.separator();
            ui.checkbox(&mut cam.focus_follow_vrm, "focus_follow_vrm");
            ui.add(egui::Slider::new(&mut cam.focus_y_lift, -2.0..=3.0).text("focus_y_lift"));
            ui.add(egui::Slider::new(&mut cam.snap_wait_frames, 0..=240).text("snap_wait_frames"));

            if ui
                .button("Re-center on VRM now")
                .on_hover_text("Snap orbit focus to the current VRM root this frame")
                .clicked()
            {
                state.resnap_requested = true;
            }
        });
    settings.ui.show_camera = open;
}

// ---------- Graphics ----------------------------------------------------------

pub fn draw_graphics_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_graphics {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_graphics;
    egui::Window::new("Graphics / lights")
        .default_width(360.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let g = &mut settings.graphics;

            let mut samples = g.msaa_samples as i32;
            ui.add(egui::Slider::new(&mut samples, 0..=8).text("msaa_samples"))
                .on_hover_text(
                    "0/1 = off; 2/4/8 = multisample. SSAO auto-disables while MSAA >= 2.",
                );
            g.msaa_samples = samples.clamp(0, 8) as u32;
            if g.msaa_samples >= 2 {
                g.advanced.ssao_enabled = false;
            }
            ui.checkbox(&mut g.hdr, "hdr")
                .on_hover_text("Restart required to attach/detach HDR on the camera.");

            ui.separator();
            ui.label("present_mode")
                .on_hover_text("Swapchain policy. Fifo is classic VSync.");
            egui::ComboBox::from_id_salt("graphics_present_mode")
                .selected_text(g.present_mode.clone())
                .show_ui(ui, |ui| {
                    for mode in [
                        "Fifo",
                        "AutoVsync",
                        "AutoNoVsync",
                        "FifoRelaxed",
                        "Mailbox",
                        "Immediate",
                    ] {
                        ui.selectable_value(&mut g.present_mode, mode.to_string(), mode);
                    }
                });

            ui.separator();
            ui.add(egui::Slider::new(&mut g.exposure_ev100, -6.0..=17.0).text("exposure_ev100"));

            ui.separator();
            ui.label("Ambient");
            ui.add(
                egui::Slider::new(&mut g.ambient_brightness, 0.0..=5.0).text("ambient_brightness"),
            );
            rgba_row(ui, &mut g.ambient_color);

            ui.separator();
            ui.label("Directional");
            ui.add(
                egui::Slider::new(&mut g.directional_illuminance, 0.0..=200_000.0)
                    .logarithmic(true)
                    .text("illuminance"),
            );
            ui.checkbox(&mut g.directional_shadows, "shadows_enabled");
            ui.label("position");
            vec3_row(ui, "sun_pos", &mut g.directional_position, -50.0..=50.0);
            ui.label("look_at");
            vec3_row(ui, "sun_look", &mut g.directional_look_at, -50.0..=50.0);

            ui.separator();
            ui.checkbox(&mut g.show_ground_plane, "show_ground_plane");
            ui.add(egui::Slider::new(&mut g.ground_size, 1.0..=400.0).text("ground_size"));
            ui.label("ground_color");
            rgb_row(ui, &mut g.ground_base_color);
        });
    settings.ui.show_graphics = open;
}

// ---------- Live / Test -------------------------------------------------------

pub fn draw_live_test_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
    hub_state: Res<HubState>,
    hub_out: Option<Res<HubBroadcast>>,
    mut chat_writer: MessageWriter<ChatCompleteMessage>,
    mut look_writer: MessageWriter<LookAtRequestMessage>,
    mut tts_writer: MessageWriter<TtsSpeakMessage>,
) {
    if !settings.ui.show_live_test {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_live_test;
    egui::Window::new("Live · Test bench")
        .default_width(380.0)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let (status, color) = if hub_state.peer_count > 0 {
                    (
                        format!("{} peer(s) connected", hub_state.peer_count),
                        egui::Color32::from_rgb(80, 200, 120),
                    )
                } else if hub_state.bound_to.is_some() {
                    (
                        "listening · no peers yet".into(),
                        egui::Color32::from_rgb(230, 200, 80),
                    )
                } else {
                    ("not bound".into(), egui::Color32::from_rgb(220, 90, 90))
                };
                ui.horizontal(|ui| {
                    ui.label("Channel hub:");
                    ui.colored_label(color, status);
                    if let Some(bind) = &hub_state.bound_to {
                        ui.label(format!("@ ws://{bind}/ws"));
                    }
                });

                ui.separator();
                ui.label("Broadcast input:text to all peers (simulate a Wyoming utterance):");
                ui.horizontal(|ui| {
                    let avail = (ui.available_width() - 90.0).max(140.0);
                    ui.add_sized(
                        [avail, 22.0],
                        egui::TextEdit::singleline(&mut state.test.input_text),
                    );
                    let disabled = hub_out.is_none();
                    if ui
                        .add_enabled(!disabled, egui::Button::new("send"))
                        .on_disabled_hover_text("hub broadcaster unavailable")
                        .clicked()
                    {
                        if let Some(out) = hub_out.as_deref() {
                            out.send_input_text(
                                &state.test.input_text,
                                &settings.ironclaw.module_name,
                            );
                        }
                    }
                });

                ui.separator();
                ui.label("Expression test (fires ACT-style ChatCompleteMessage):");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_label("emotion")
                        .selected_text(format!("{:?}", state.test.emotion))
                        .show_ui(ui, |ui| {
                            for e in [
                                Emotion::Happy,
                                Emotion::Sad,
                                Emotion::Angry,
                                Emotion::Think,
                                Emotion::Surprised,
                                Emotion::Awkward,
                                Emotion::Question,
                                Emotion::Curious,
                                Emotion::Neutral,
                            ] {
                                ui.selectable_value(
                                    &mut state.test.emotion,
                                    e.clone(),
                                    format!("{e:?}"),
                                );
                            }
                        });
                    if ui.button("trigger").clicked() {
                        let emotion_label = format!("{:?}", state.test.emotion).to_lowercase();
                        chat_writer.write(ChatCompleteMessage {
                            content: format!("<|ACT:{{\"emotion\":\"{emotion_label}\"}}|>test"),
                        });
                    }
                });

                ui.separator();
                ui.label("Look-at target (rig-local meters):");
                vec3_row(ui, "look", &mut state.test.look_at, -3.0..=3.0);
                ui.horizontal(|ui| {
                    if ui.button("look at point").clicked() {
                        look_writer.write(LookAtRequestMessage {
                            local_target: Some(Vec3::from_array(state.test.look_at)),
                        });
                    }
                    if ui.button("back to cursor").clicked() {
                        look_writer.write(LookAtRequestMessage { local_target: None });
                    }
                });

                ui.separator();
                ui.label("TTS test (Kokoro):");
                ui.horizontal(|ui| {
                    let avail = (ui.available_width() - 90.0).max(140.0);
                    ui.add_sized(
                        [avail, 22.0],
                        egui::TextEdit::singleline(&mut state.test.tts_text),
                    );
                    let disabled = !settings.tts.enabled;
                    if ui
                        .add_enabled(!disabled, egui::Button::new("speak"))
                        .on_disabled_hover_text("tts.enabled is false")
                        .clicked()
                    {
                        tts_writer.write(TtsSpeakMessage {
                            text: state.test.tts_text.clone(),
                        });
                    }
                });
            });
        });
    settings.ui.show_live_test = open;
}

// ---------- Channel hub (IronClaw protocol) -----------------------------------

pub fn draw_channel_hub_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_channel_hub {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_channel_hub;
    egui::Window::new("Channel hub (IronClaw protocol)")
        .default_width(360.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let i = &mut settings.ironclaw;
            ui.label("We HOST the IronClaw-style WS hub. Peers connect to ws://<this-host>/ws.");
            ui.label("bind_address (restart to rebind):");
            ui.text_edit_singleline(&mut i.bind_address);
            ui.label("auth_token (empty = accept any peer):");
            ui.text_edit_singleline(&mut i.auth_token);
            ui.label("module_name (identity on envelopes we publish):");
            ui.text_edit_singleline(&mut i.module_name);
        });
    settings.ui.show_channel_hub = open;
}

// ---------- Gateway -----------------------------------------------------------

pub fn draw_gateway_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_gateway {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_gateway;
    egui::Window::new("Gateway (IronClaw HTTP/SSE)")
        .default_width(360.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let g = &mut settings.gateway;
            ui.label("IronClaw gateway (used by the chat client; SSE + thread CRUD).");
            ui.label("base_url (no trailing slash; restart to apply):");
            ui.text_edit_singleline(&mut g.base_url);
            ui.label("auth_token (override IRONCLAW_GATEWAY_TOKEN env; restart to apply):");
            ui.text_edit_singleline(&mut g.auth_token);
            ui.label("default_thread_id (empty = use whatever the gateway returns active):");
            ui.text_edit_singleline(&mut g.default_thread_id);

            let mut t = g.request_timeout_ms as i64;
            if ui
                .add(
                    egui::DragValue::new(&mut t)
                        .speed(50)
                        .range(1_000..=120_000)
                        .prefix("timeout_ms "),
                )
                .changed()
            {
                g.request_timeout_ms = t.max(1_000) as u64;
            }
            let mut h = g.history_limit as i64;
            if ui
                .add(
                    egui::DragValue::new(&mut h)
                        .speed(1)
                        .range(1..=500)
                        .prefix("history_limit "),
                )
                .changed()
            {
                g.history_limit = h.max(1) as u32;
            }
        });
    settings.ui.show_gateway = open;
}

// ---------- TTS ---------------------------------------------------------------

pub fn draw_tts_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_tts {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_tts;
    egui::Window::new("TTS (Kokoro)")
        .default_width(320.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let t = &mut settings.tts;
            ui.checkbox(&mut t.enabled, "enabled");
            ui.label("kokoro_url:");
            ui.text_edit_singleline(&mut t.kokoro_url);
            ui.label("voice:");
            ui.text_edit_singleline(&mut t.voice);
            ui.label("response_format (wav | pcm | mp3 | …):");
            ui.text_edit_singleline(&mut t.response_format);
            ui.checkbox(&mut t.stream, "stream (leave off for one-shot WAV/PCM)");
            let mut sr = t.pcm_sample_rate as i64;
            if ui
                .add(
                    egui::DragValue::new(&mut sr)
                        .speed(100)
                        .range(8000..=48_000)
                        .prefix("pcm_sample_rate "),
                )
                .changed()
            {
                t.pcm_sample_rate = sr.clamp(8000, 48_000) as u32;
            }
        });
    settings.ui.show_tts = open;
}

// ---------- Look-at -----------------------------------------------------------

pub fn draw_look_at_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_look_at {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_look_at;
    egui::Window::new("Look-at")
        .default_width(320.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.add(
                egui::Slider::new(&mut settings.look_at.idle_return_speed, 0.0..=20.0)
                    .text("idle_return_speed"),
            );
        });
    settings.ui.show_look_at = open;
}

// ---------- MCP / Pose / A2F / Kimodo -----------------------------------------

pub fn draw_mcp_window(mut contexts: EguiContexts, mut settings: ResMut<Settings>) {
    if !settings.ui.show_mcp {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_mcp;
    egui::Window::new("MCP / Pose controller")
        .default_width(380.0)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(
                    "RMCP streamable-HTTP server exposing the pose / A2F / Kimodo tools\n\
                     to IronClaw (and any other MCP client). Changes marked (restart) take\n\
                     effect on the next launch.",
                );

                ui.separator();
                ui.label("MCP server:");
                ui.checkbox(&mut settings.mcp.enabled, "enabled (restart)");
                ui.horizontal(|ui| {
                    ui.label("bind_address (restart):");
                    ui.text_edit_singleline(&mut settings.mcp.bind_address);
                });
                ui.horizontal(|ui| {
                    ui.label("path (restart):");
                    ui.text_edit_singleline(&mut settings.mcp.path);
                });
                ui.horizontal(|ui| {
                    ui.label("bearer auth_token (restart, empty = none):");
                    ui.text_edit_singleline(&mut settings.mcp.auth_token);
                });
                ui.colored_label(
                    egui::Color32::from_rgb(160, 200, 240),
                    format!(
                        "URL: http://{}{}{}",
                        settings.mcp.bind_address,
                        if settings.mcp.path.starts_with('/') { "" } else { "/" },
                        settings.mcp.path,
                    ),
                );

                ui.separator();
                ui.label("Audio2Face-3D:");
                ui.checkbox(&mut settings.a2f.enabled, "enabled (restart)");
                ui.checkbox(
                    &mut settings.a2f.apply_from_tts,
                    "apply_from_tts — Kokoro → A2F → face clip after each chat utterance (restart)",
                );
                ui.horizontal(|ui| {
                    ui.label("gRPC endpoint:");
                    ui.text_edit_singleline(&mut settings.a2f.endpoint);
                });
                ui.horizontal(|ui| {
                    ui.label("health URL:");
                    ui.text_edit_singleline(&mut settings.a2f.health_url);
                });
                ui.horizontal(|ui| {
                    ui.label("function_id (match A2F --function-id, e.g. Claire):");
                    ui.text_edit_singleline(&mut settings.a2f.function_id);
                });
                ui.label(
                    "Tip: run the `a2f_status` MCP tool to probe `/v1/health/ready` and\n\
                     confirm the gRPC stream can be opened. Test Kokoro→A2F with MCP `a2f_from_text`.",
                );

                ui.separator();
                ui.label("Kimodo defaults:");
                let mut dur = settings.kimodo.default_duration_sec;
                if ui
                    .add(
                        egui::Slider::new(&mut dur, 0.5..=20.0).text("default_duration_sec"),
                    )
                    .changed()
                {
                    settings.kimodo.default_duration_sec = dur;
                }
                let mut steps = settings.kimodo.default_steps as i32;
                if ui
                    .add(egui::Slider::new(&mut steps, 10..=500).text("default_steps"))
                    .changed()
                {
                    settings.kimodo.default_steps = steps.max(1) as u32;
                }
                let mut to = settings.kimodo.generate_timeout_sec as i64;
                if ui
                    .add(
                        egui::Slider::new(&mut to, 10..=600).text("generate_timeout_sec"),
                    )
                    .changed()
                {
                    settings.kimodo.generate_timeout_sec = to.max(1) as u64;
                }
                ui.label(
                    "Kimodo connects to our channel hub as a WS peer and consumes\n\
                     `kimodo:generate` envelopes; see kimodo-motion-service.py.",
                );

                ui.separator();
                ui.label("Pose / animation library (shared with the Node pose-controller):");
                ui.horizontal(|ui| {
                    ui.label("poses_dir:");
                    ui.text_edit_singleline(&mut settings.pose_library.poses_dir);
                });
                ui.horizontal(|ui| {
                    ui.label("animations_dir:");
                    ui.text_edit_singleline(&mut settings.pose_library.animations_dir);
                });
                ui.label(
                    "Poses are re-read from disk on every MCP tool call, so edits made\n\
                     here apply immediately without a reload button.",
                );
            });
        });
    settings.ui.show_mcp = open;
}
