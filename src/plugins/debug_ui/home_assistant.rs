//! Home Assistant connection, device registry, presence routing (Airi-style).

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use jarvis_avatar::config::Settings;
use jarvis_avatar::home_assistant::{self, HaCameraEntity, HaDetectionSensorEntity, HaMediaEntity};

use crate::plugins::VrmEyeLookatDebug;
use crate::plugins::ha_vision_gaze::HaVisionGazeRuntime;
use crate::plugins::home_assistant::{HaDiscoverBridge, HaDiscoveryUiCache};
use crate::plugins::home_assistant_routing::{self as routing, PresenceRouting};
use crate::plugins::ironclaw_chat::ChatState;
use crate::plugins::shared_runtime::SharedTokio;
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};

pub fn draw_home_assistant_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut cache: ResMut<HaDiscoveryUiCache>,
    mut routing: ResMut<PresenceRouting>,
    bridge: Option<Res<HaDiscoverBridge>>,
    tokio_rt: Option<Res<SharedTokio>>,
    traffic: Option<Res<TrafficLogSink>>,
    chat: Option<Res<ChatState>>,
    vision_gaze: Option<Res<HaVisionGazeRuntime>>,
    eye_vrm: Res<VrmEyeLookatDebug>,
) {
    if !settings.ui.show_home_assistant {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let mut open = settings.ui.show_home_assistant;
    egui::Window::new("Home Assistant")
        .default_width(560.0)
        .default_height(520.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let mut persist_ha = false;
            let scroll_h = ui.available_height().max(120.0);
            egui::ScrollArea::vertical()
                .id_salt("ha_window_scroll")
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let ha = &mut settings.home_assistant;
                    let configured = home_assistant::configured(ha);

                    egui::CollapsingHeader::new("Connection")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let dot = if configured {
                                    egui::Color32::from_rgb(80, 200, 120)
                                } else {
                                    egui::Color32::from_rgb(200, 80, 80)
                                };
                                ui.colored_label(dot, "●");
                                ui.label(if configured {
                                    "Configured"
                                } else {
                                    "Not configured (set URL + token)"
                                });
                            });
                            ui.add(
                                egui::TextEdit::singleline(&mut ha.ha_url)
                                    .hint_text("http://192.168.4.7:8123"),
                            );
                            ui.small("Home Assistant URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut ha.ha_token)
                                    .hint_text("long-lived token")
                                    .password(true),
                            );
                            ui.small("Long-lived access token");
                            ui.add(
                                egui::TextEdit::singleline(&mut ha.bridge_url)
                                    .hint_text("http://host:8767 (optional)"),
                            );
                            ui.small("Voice bridge URL for /ha-proxy — leave empty to call HA directly");
                            ui.small("With URL + token set, a device scan runs once at startup (same as Discover).");

                            ui.horizontal(|ui| {
                                let can_go = configured
                                    && bridge.is_some()
                                    && tokio_rt.is_some()
                                    && !cache.is_refreshing;
                                if ui
                                    .add_enabled(can_go, egui::Button::new("Discover devices"))
                                    .clicked()
                                {
                                    if let (Some(b), Some(rt)) = (bridge.as_ref(), tokio_rt.as_ref()) {
                                        let tx = b.tx.clone();
                                        let snap = ha.clone();
                                        let log = traffic.as_ref().map(|t| (*t).clone());
                                        cache.is_refreshing = true;
                                        cache.last_error = None;
                                        rt.spawn(async move {
                                            let client = match reqwest::Client::builder()
                                                .timeout(std::time::Duration::from_secs(120))
                                                .build()
                                            {
                                                Ok(c) => c,
                                                Err(e) => {
                                                    let _ = tx.send(Err(format!("reqwest client: {e}")));
                                                    return;
                                                }
                                            };
                                            let res = home_assistant::discover(&client, &snap).await;
                                            if let Some(ref log) = log {
                                                match &res {
                                                    Ok(_) => log.push(
                                                        TrafficChannel::HomeAssistantHttp,
                                                        TrafficDirection::Outbound,
                                                        "discover: /api/states + registries — OK",
                                                        None,
                                                    ),
                                                    Err(e) => log.push(
                                                        TrafficChannel::HomeAssistantHttp,
                                                        TrafficDirection::Outbound,
                                                        format!("discover failed: {e}"),
                                                        None,
                                                    ),
                                                }
                                            }
                                            let _ = tx.send(res.map_err(|e| e.to_string()));
                                        });
                                    }
                                }
                                if cache.is_refreshing {
                                    ui.spinner();
                                    ui.label("Scanning…");
                                } else if let Some(ts) = cache.last_refresh_at_ms {
                                    ui.small(format!(
                                        "Last scan: {} — {} cams, {} mics, {} speakers, {} sensors",
                                        fmt_time(ts),
                                        cache.last.as_ref().map(|s| s.cameras.len()).unwrap_or(0),
                                        cache.last.as_ref().map(|s| s.mics.len()).unwrap_or(0),
                                        cache.last.as_ref().map(|s| s.speakers.len()).unwrap_or(0),
                                        cache
                                            .last
                                            .as_ref()
                                            .map(|s| s.detection_sensors.len())
                                            .unwrap_or(0),
                                    ));
                                }
                            });

                            if let Some(ref err) = cache.last_error {
                                ui.colored_label(egui::Color32::from_rgb(220, 140, 80), err);
                            }
                        });

                    if let Some(snap) = cache.last.as_ref() {
                        egui::CollapsingHeader::new("Device registry")
                            .default_open(true)
                            .show(ui, |ui| {
                                device_grid_cameras(
                                    ui,
                                    "ha_dev_cam",
                                    "Cameras",
                                    &snap.cameras,
                                    &mut ha.enabled_camera_ids,
                                    &mut persist_ha,
                                );
                                device_grid_media(
                                    ui,
                                    "ha_dev_mic",
                                    "Microphones / satellites",
                                    &snap.mics,
                                    &mut ha.enabled_mic_ids,
                                    &mut persist_ha,
                                );
                                device_grid_media(
                                    ui,
                                    "ha_dev_spk",
                                    "Speakers",
                                    &snap.speakers,
                                    &mut ha.enabled_speaker_ids,
                                    &mut persist_ha,
                                );
                                device_grid_detection(
                                    ui,
                                    "ha_dev_det",
                                    &snap.detection_sensors,
                                    &mut ha.detection_sensor_ids,
                                    &mut persist_ha,
                                );
                                if !ha.detection_sensor_ids.is_empty() {
                                    ui.small(format!(
                                        "Vision → gaze uses the first checked sensor: {}",
                                        ha.detection_sensor_ids[0]
                                    ));
                                }
                            });
                    } else {
                        ui.label("No discovery snapshot yet. Configure HA and click Discover.");
                    }

                    egui::CollapsingHeader::new("Vision → VRM gaze")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label(
                                "Polls the **first** detection sensor in “Use” order (same attributes as ha-voice-bridge: `detections[]` with person/face bbox).",
                            );
                            if ui
                                .checkbox(&mut ha.vision_gaze_enabled, "Drive look-at from HA detections")
                                .changed()
                            {
                                persist_ha = true;
                            }
                            if ui
                                .checkbox(
                                    &mut ha.vision_gaze_flip_horizontal,
                                    "Mirror horizontal gaze (swap left/right in frame)",
                                )
                                .changed()
                            {
                                persist_ha = true;
                            }
                            if ui
                                .add(egui::Slider::new(
                                    &mut ha.vision_gaze_smooth_tau_sec,
                                    0.05..=0.8,
                                )
                                .text("Look-at smoothing τ (s) — higher = smoother, slower"))
                                .changed()
                            {
                                persist_ha = true;
                            }
                            ui.small(
                                "Each poll updates a **goal**; the avatar eases toward it so eyes do not snap every HTTP response.",
                            );
                            ui.add(
                                egui::Slider::new(&mut ha.vision_gaze_poll_ms, 50u64..=2000u64)
                                    .text("Poll interval (ms)"),
                            );
                            if ui
                                .add(egui::Slider::new(&mut ha.vision_gaze_depth, 0.2..=8.0).text("Gaze depth (m)"))
                                .changed()
                            {
                                persist_ha = true;
                            }
                            if ui
                                .add(
                                    egui::Slider::new(
                                        &mut ha.vision_gaze_horizontal_sensitivity,
                                        0.02..=1.0,
                                    )
                                    .text("Horizontal sensitivity (VRM eye yaw range)"),
                                )
                                .on_hover_text(
                                    "VRM look-at range-maps head→target yaw. If the target is too far to the side, \
                                     yaw **saturates** and eyes only show full left or full right. Lower this to get \
                                     smooth left–center–right. Try 0.1–0.3 if you see that.",
                                )
                                .changed()
                            {
                                ha.vision_gaze_horizontal_sensitivity = ha.vision_gaze_horizontal_sensitivity.clamp(0.01, 1.0);
                                persist_ha = true;
                            }
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::DragValue::new(&mut ha.vision_gaze_image_width)
                                        .range(160.0..=4096.0)
                                        .suffix(" px"),
                                );
                                ui.label("×");
                                ui.add(
                                    egui::DragValue::new(&mut ha.vision_gaze_image_height)
                                        .range(120.0..=4096.0)
                                        .suffix(" px"),
                                );
                                ui.label("detection frame size (bbox coords)");
                            });
                            ui.small(
                                "Frame size only affects **pixel** bboxes; normalized 0–1 detections ignore it. Mouse look-at still works when this is off.",
                            );

                            ui.separator();
                            ui.label(
                                egui::RichText::new("Horizontal calibration (optional)").strong(),
                            );
                            ui.small(
                                "L / C / R are only the **detector’s nx** in frame, not your body position. For “straight in the eyes at center”, you must **Set center** while you stand in that pose and **this nx** is the one you get **then** (not an earlier value). If status shows **Δnx@C** far from 0, you are not at your saved center nx — re-set C or nudge **offsets** below. Flip mirror is a separate setting; the Set * buttons do not change it.",
                            );
                            if let Some(nx) = vision_gaze.as_ref().and_then(|v| v.last_nx) {
                                ui.label(format!("Last sample nx (for buttons): {nx:.4}"));
                            } else {
                                ui.small("No face/person sample yet — enable gaze and stand in frame.");
                            }
                            ui.label(format!(
                                "Saved: L={:?} · C={:?} · R={:?}",
                                ha.vision_gaze_cal_nx_left,
                                ha.vision_gaze_cal_nx_center,
                                ha.vision_gaze_cal_nx_right
                            ));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        vision_gaze
                                            .as_ref()
                                            .and_then(|v| v.last_nx)
                                            .is_some(),
                                        egui::Button::new("Set left"),
                                    )
                                    .clicked()
                                {
                                    if let Some(nx) =
                                        vision_gaze.as_ref().and_then(|v| v.last_nx)
                                    {
                                        ha.vision_gaze_cal_nx_left = Some(nx);
                                        persist_ha = true;
                                    }
                                }
                                if ui
                                    .add_enabled(
                                        vision_gaze
                                            .as_ref()
                                            .and_then(|v| v.last_nx)
                                            .is_some(),
                                        egui::Button::new("Set center"),
                                    )
                                    .clicked()
                                {
                                    if let Some(nx) =
                                        vision_gaze.as_ref().and_then(|v| v.last_nx)
                                    {
                                        ha.vision_gaze_cal_nx_center = Some(nx);
                                        persist_ha = true;
                                    }
                                }
                                if ui
                                    .add_enabled(
                                        vision_gaze
                                            .as_ref()
                                            .and_then(|v| v.last_nx)
                                            .is_some(),
                                        egui::Button::new("Set right"),
                                    )
                                    .clicked()
                                {
                                    if let Some(nx) =
                                        vision_gaze.as_ref().and_then(|v| v.last_nx)
                                    {
                                        ha.vision_gaze_cal_nx_right = Some(nx);
                                        persist_ha = true;
                                    }
                                }
                                if ui.button("Clear calibration").clicked() {
                                    ha.vision_gaze_cal_nx_left = None;
                                    ha.vision_gaze_cal_nx_center = None;
                                    ha.vision_gaze_cal_nx_right = None;
                                    persist_ha = true;
                                }
                            });
                            ui.label(egui::RichText::new("Local look-at offset (VRM space, m)").strong());
                            ui.small("Adds to the computed target. Use to fix residual “not quite straight” (model or camera height).");
                            ui.horizontal(|ui| {
                                ui.label("X");
                                if ui
                                    .add(egui::DragValue::new(&mut ha.vision_gaze_offset_x).speed(0.01).range(-0.6..=0.6))
                                    .changed()
                                {
                                    persist_ha = true;
                                }
                                ui.label("Y");
                                if ui
                                    .add(egui::DragValue::new(&mut ha.vision_gaze_offset_y).speed(0.01).range(-0.6..=0.6))
                                    .changed()
                                {
                                    persist_ha = true;
                                }
                                ui.label("Z");
                                if ui
                                    .add(egui::DragValue::new(&mut ha.vision_gaze_offset_z).speed(0.01).range(-0.6..=0.6))
                                    .changed()
                                {
                                    persist_ha = true;
                                }
                            });

                            if let Some(vg) = vision_gaze.as_ref() {
                                ui.separator();
                                if let Some(p) = vg.last_local {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "last local target (m): {:.2}, {:.2}, {:.2}",
                                            p.x, p.y, p.z
                                        ))
                                        .weak(),
                                    );
                                }
                                ui.monospace(&vg.last_diag);
                                let _ = vg.in_flight;
                            }

                            ui.separator();
                            egui::CollapsingHeader::new("VRM eye bones (runtime readback)")
                                .default_open(true)
                                .show(ui, |ui| {
                                    if !eye_vrm.ready {
                                        ui.label("No VRM sample yet (load the avatar, wait a frame).");
                                        return;
                                    }
                                    if eye_vrm.vrm_uses_expression_lookat {
                                        ui.label(
                                            egui::RichText::new(
                                                "VRM look-at is expression type; bevy does not run bone look-at. Readback may be idle.",
                                            )
                                            .weak(),
                                        );
                                    }
                                    ui.label(format!("LookAt: {}", eye_vrm.look_mode));
                                    if let Some(tw) = eye_vrm.target_world {
                                        ui.label(format!(
                                            "Target world (m): {:.3}, {:.3}, {:.3}",
                                            tw.x, tw.y, tw.z
                                        ));
                                    }
                                    let yaw_s = if eye_vrm.yaw_head_deg.is_nan() {
                                        "n/a (cursor)".to_string()
                                    } else {
                                        format!("{:.2}°", eye_vrm.yaw_head_deg)
                                    };
                                    let pitch_s = if eye_vrm.pitch_head_deg.is_nan() {
                                        "n/a (cursor)".to_string()
                                    } else {
                                        format!("{:.2}°", eye_vrm.pitch_head_deg)
                                    };
                                    ui.label(format!(
                                        "Head look-at space (before VRM RangeMap, same as bevy): yaw={yaw_s}  pitch={pitch_s}"
                                    ));
                                    ui.small("Angle from the head to the gaze target world position, not raw bbox nx alone.");
                                    ui.label(format!(
                                    "VRM RangeMap input caps (deg): h_outer_in={:.1}  h_inner_in={:.1}  v_down={:.1}  v_up={:.1} — if |yaw| is usually above the horizontal in_max, the eye saturates to full L/R.",
                                    eye_vrm.range_h_outer_in_deg, eye_vrm.range_h_inner_in_deg, eye_vrm.range_v_down_in_deg, eye_vrm.range_v_up_in_deg
                                ));
                                    let l = eye_vrm.left_eye_euler_yxz_deg;
                                    let r = eye_vrm.right_eye_euler_yxz_deg;
                                    ui.label("Mapped (deg) after range map (L vs R use inner/outer for yaw):");
                                    ui.label(format!(
                                    "  left:  yaw={:.3}  pitch={:.3}",
                                    eye_vrm.left_mapped_yaw_deg, eye_vrm.left_mapped_pitch_deg
                                ));
                                    ui.label(format!(
                                    "  right: yaw={:.3}  pitch={:.3}",
                                    eye_vrm.right_mapped_yaw_deg, eye_vrm.right_mapped_pitch_deg
                                ));
                                    ui.label("Actual eye-bone local rotation, Euler YXZ, deg (Y, X, Z) order from Quat::to_euler:");
                                    ui.label(format!(
                                    "  left:  ({:.2}, {:.2}, {:.2})",
                                    l.x, l.y, l.z
                                ));
                                    ui.label(format!(
                                    "  right: ({:.2}, {:.2}, {:.2})",
                                    r.x, r.y, r.z
                                ));
                                });
                        });

                    egui::CollapsingHeader::new("Presence router")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut ha.default_area).hint_text("e.g. Bedroom"),
                            );
                            ui.small("Default area when no active area is resolved");
                            ui.add(
                                egui::Slider::new(
                                    &mut ha.presence_timeout_ms,
                                    10_000u64..=300_000u64,
                                )
                                .text("Presence timeout (ms)"),
                            );
                            ui.separator();
                            ui.add(
                                egui::TextEdit::singleline(&mut routing.forced_area)
                                    .hint_text("override area (debug)"),
                            );
                            ui.small("When set, forces routing to this area (same as Airi forceArea)");
                            ui.add(
                                egui::TextEdit::singleline(&mut routing.vision_active_area)
                                    .hint_text("vision area stub"),
                            );
                            ui.small(
                                "Reserved for area routing from a vision pipeline; HA gaze (above) drives look-at only.",
                            );

                            ui.separator();
                            ui.strong("Current routing");
                            let snap_opt = cache.last.as_ref();
                            if let Some(snap) = snap_opt {
                                let active = routing::resolve_active_area(&routing, routing::now_ms());
                                let cam = routing::active_camera(snap, ha, &routing);
                                let mic = routing::active_mic(snap, ha, &routing);
                                let spk = routing::active_speaker(snap, ha, &routing);

                                egui::Grid::new("ha_route_grid")
                                    .num_columns(2)
                                    .spacing([10.0, 4.0])
                                    .show(ui, |ui| {
                                        kv(ui, "Active area", if active.is_empty() { "—" } else { &active });
                                        kv(
                                            ui,
                                            "Camera",
                                            &cam
                                                .map(|c| format!("{} ({})", c.label, c.entity_id))
                                                .unwrap_or_else(|| "None".into()),
                                        );
                                        kv(
                                            ui,
                                            "Microphone",
                                            &mic
                                                .map(|m| format!("{} ({})", m.label, m.entity_id))
                                                .unwrap_or_else(|| "None".into()),
                                        );
                                        kv(
                                            ui,
                                            "Speaker",
                                            &spk
                                                .map(|s| format!("{} ({})", s.label, s.entity_id))
                                                .unwrap_or_else(|| "None".into()),
                                        );
                                        let areas: String = if snap.all_areas.is_empty() {
                                            "None".into()
                                        } else {
                                            snap.all_areas.join(", ")
                                        };
                                        kv(ui, "Areas discovered", &areas);
                                    });
                            } else {
                                ui.label("Discover devices to see routing picks.");
                            }

                            if let Some(chat) = chat.as_ref() {
                                ui.separator();
                                ui.small("IronClaw gateway (HTTP/SSE) — not the same as HA voice idle/listening.");
                                let thinking = chat
                                    .thinking
                                    .as_ref()
                                    .map_or(false, |t| !t.trim().is_empty());
                                let convo = if thinking {
                                    "thinking (assistant)"
                                } else if !chat.streaming_buffer.is_empty() {
                                    "streaming reply"
                                } else {
                                    "idle / ready"
                                };
                                egui::Grid::new("ha_chat_grid")
                                    .num_columns(2)
                                    .spacing([10.0, 4.0])
                                    .show(ui, |ui| {
                                        kv(ui, "Assistant / chat", convo);
                                        kv(
                                            ui,
                                            "Last transport status",
                                            chat.last_status.as_deref().unwrap_or("—"),
                                        );
                                        let err = chat.last_error.as_deref().unwrap_or("—");
                                        ui.label("Last transport error");
                                        if err == "—" {
                                            ui.label(egui::RichText::new(err).weak());
                                        } else {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(230, 120, 100),
                                                err,
                                            );
                                        }
                                        ui.end_row();
                                    });
                                ui.small(
                                    "Transport errors clear automatically after a successful SSE reconnect (e.g. sse: open).",
                                );
                            }
                        });

                    egui::CollapsingHeader::new("Voice bridge")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(
                                "The ha-voice-bridge MCP server connects Home Assistant's voice pipeline to this hub (port 6121).",
                            );
                            ui.label(
                                "Configure the bridge via its env (e.g. SATELLITE_ID, TTS_ENGINE) in mcp.json — same as Airi.",
                            );
                        });
                });
            if persist_ha {
                if let Err(e) = settings.save_user() {
                    tracing::warn!(target: "home_assistant", "save user.toml: {e}");
                }
            }
        });
    settings.ui.show_home_assistant = open;
}

fn fmt_time(ms: u128) -> String {
    let now = home_assistant::discovery_timestamp_ms();
    let ago_s = now.saturating_sub(ms) / 1000;
    format!("{ago_s}s ago (epoch_ms={ms})")
}

fn kv(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.label(k);
    ui.label(egui::RichText::new(v).strong());
    ui.end_row();
}

/// HA-reported state → color (idle / on = good, unavailable = bad).
fn ha_state_color(state: Option<&str>) -> egui::Color32 {
    let s = state.unwrap_or("").trim().to_ascii_lowercase();
    match s.as_str() {
        "unavailable" | "unknown" => egui::Color32::from_rgb(220, 90, 90),
        "" => egui::Color32::from_rgb(150, 150, 160),
        "off" | "closed" => egui::Color32::from_rgb(130, 135, 150),
        "idle" | "on" | "playing" | "streaming" | "standby" | "active" | "home" | "paused"
        | "open" => egui::Color32::from_rgb(85, 190, 115),
        _ => egui::Color32::from_rgb(185, 195, 210),
    }
}

fn device_grid_cameras(
    ui: &mut egui::Ui,
    grid_id: &'static str,
    title: &str,
    items: &[HaCameraEntity],
    enabled: &mut Vec<String>,
    persist_ha: &mut bool,
) {
    if items.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(title).strong());
    egui::Grid::new((grid_id, title))
        .num_columns(5)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            ui.small("Use");
            ui.small("Name");
            ui.small("Entity ID");
            ui.small("Area");
            ui.small("State");
            ui.end_row();
            for c in items {
                let id = c.entity_id.as_str();
                let on = enabled.iter().any(|x| x == id);
                let mut checked = on;
                if ui.checkbox(&mut checked, "").changed() {
                    home_assistant::toggle_id(enabled, id);
                    *persist_ha = true;
                }
                ui.label(&c.label);
                ui.monospace(id);
                ui.label(if c.area.is_empty() {
                    "—"
                } else {
                    c.area.as_str()
                });
                let st = c.state.as_deref().unwrap_or("—");
                ui.colored_label(ha_state_color(c.state.as_deref()), st);
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}

fn device_grid_media(
    ui: &mut egui::Ui,
    grid_id: &'static str,
    title: &str,
    items: &[HaMediaEntity],
    enabled: &mut Vec<String>,
    persist_ha: &mut bool,
) {
    if items.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(title).strong());
    egui::Grid::new((grid_id, title))
        .num_columns(5)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            ui.small("Use");
            ui.small("Name");
            ui.small("Entity ID");
            ui.small("Area");
            ui.small("State");
            ui.end_row();
            for m in items {
                let on = enabled.iter().any(|x| x == &m.entity_id);
                let mut checked = on;
                if ui.checkbox(&mut checked, "").changed() {
                    home_assistant::toggle_id(enabled, &m.entity_id);
                    *persist_ha = true;
                }
                ui.label(&m.label);
                ui.monospace(&m.entity_id);
                ui.label(if m.area.is_empty() {
                    "—"
                } else {
                    m.area.as_str()
                });
                let st = m.state.as_deref().unwrap_or("—");
                ui.colored_label(ha_state_color(m.state.as_deref()), st);
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}

fn device_grid_detection(
    ui: &mut egui::Ui,
    grid_id: &'static str,
    items: &[HaDetectionSensorEntity],
    enabled: &mut Vec<String>,
    persist_ha: &mut bool,
) {
    if items.is_empty() {
        return;
    }
    ui.label(egui::RichText::new("Detection sensors").strong());
    egui::Grid::new((grid_id, "det"))
        .num_columns(5)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            ui.small("Use");
            ui.small("Name");
            ui.small("Entity ID");
            ui.small("Area");
            ui.small("State");
            ui.end_row();
            for s in items {
                let on = enabled.iter().any(|x| x == &s.entity_id);
                let mut checked = on;
                if ui.checkbox(&mut checked, "").changed() {
                    home_assistant::toggle_id(enabled, &s.entity_id);
                    *persist_ha = true;
                }
                ui.label(&s.label);
                ui.monospace(&s.entity_id);
                ui.label(if s.area.is_empty() {
                    "—"
                } else {
                    s.area.as_str()
                });
                let st = s.state.as_deref().unwrap_or("—");
                ui.colored_label(ha_state_color(s.state.as_deref()), st);
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}
