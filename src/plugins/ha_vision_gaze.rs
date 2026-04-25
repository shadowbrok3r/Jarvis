//! Poll Home Assistant object-detection sensors (e.g. `sensor.arduino_object_detections`)
//! and drive [`LookAtRequestMessage`] the same way ha-voice-bridge did over WebSocket.
//!
//! Horizontal gaze uses `t` in [−1,1] and `x = 0.5 * t * depth * H_SCALE` (same as (0.5−nx)*depth*H
//! with `t = 1 - 2*nx` when uncalibrated). Optional **3-point nx calibration** maps your actual
//! left/centre/right frame positions to t∈{−1,0,1}. The look-at **goal** is **smoothed** per frame
//! so new polls do not snap the eyes. **VRM** eye `LookAt` uses per-axis range maps (yaw in °);
//! if the implied yaw exceeds the model’s `input_max`, eyes **saturate** and look all-left or
//! all-right with little gradation. Use `vision_gaze_horizontal_sensitivity` to scale only X
//! so intermediate gaze stays in range (vertical is usually fine).

use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use jarvis_avatar::config::HomeAssistantSettings;
use reqwest::Client;
use serde_json::Value;

use jarvis_avatar::config::Settings;
use jarvis_avatar::home_assistant;

use super::channel_server::LookAtRequestMessage;
use super::shared_runtime::SharedTokio;

/// `x = 0.5 * t * d * H_SCALE` matches legacy `(0.5 - nx) * d * H` with `t = 1 - 2*nx` on (0,1).
const H_SCALE: f32 = 1.5;
const V_SCALE: f32 = 1.0;
const NX_EPS: f32 = 1.0e-4;

#[derive(Debug)]
enum VisionGazeJobResult {
    Track { pos: Vec3, diag: String, nx: f32, ny: f32 },
    NoPerson { diag: String },
    Error(String),
}

#[derive(Resource)]
pub struct HaVisionGazeChannel {
    tx: Sender<VisionGazeJobResult>,
    rx: Receiver<VisionGazeJobResult>,
}

impl HaVisionGazeChannel {
    fn drain_completed(&self) {
        while self.rx.try_recv().is_ok() {}
    }
}

/// State for the HA vision gaze driver (goals, smoothing, UI samples).
#[derive(Resource)]
pub struct HaVisionGazeRuntime {
    pub in_flight: bool,
    pub last_fire_ms: u128,
    pub no_detection_streak: u32,
    pub last_diag: String,
    /// Last **smoothed** target written to look-at.
    pub last_local: Option<Vec3>,
    /// Last person/bface sample `nx, ny` in 0..1 (for calibration buttons).
    pub last_nx: Option<f32>,
    pub last_ny: Option<f32>,
    /// Raw look-at from the last poll (goal for smoothing).
    pub gaze_goal: Option<Vec3>,
    gaze_smoothed: Option<Vec3>,
    pub prev_enabled: bool,
}

impl Default for HaVisionGazeRuntime {
    fn default() -> Self {
        Self {
            in_flight: false,
            last_fire_ms: 0,
            no_detection_streak: 0,
            last_diag: String::new(),
            last_local: None,
            last_nx: None,
            last_ny: None,
            gaze_goal: None,
            gaze_smoothed: None,
            prev_enabled: false,
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn json_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn bbox_is_normalized(x1: f32, y1: f32, x2: f32, y2: f32) -> bool {
    const SL: f32 = 1.001;
    x1 <= SL && y1 <= SL && x2 <= SL && y2 <= SL
}

/// Horizontal t∈[−1,1]: optional 3-point nx map, else `t = 1 - 2*nx` on 0..1.
fn horizontal_t(ha: &HomeAssistantSettings, nx: f32) -> (f32, String) {
    if let (Some(l), Some(c), Some(r)) = (
        ha.vision_gaze_cal_nx_left,
        ha.vision_gaze_cal_nx_center,
        ha.vision_gaze_cal_nx_right,
    ) {
        if l < c && c < r {
            // [l, c] → [−1, 0] and (c, r] → (0, 1]; `nx == c` must be in the *left* segment so t = 0.
            let t = if nx < l {
                -1.0
            } else if nx > r {
                1.0
            } else if nx <= c {
                (nx - c) / (c - l).max(NX_EPS)
            } else {
                (nx - c) / (r - c).max(NX_EPS)
            }
            .clamp(-1.0, 1.0);
            (t, format!(" t={t:.2} (cal)"))
        } else {
            let nxc = clamp01(nx);
            let t = 1.0 - 2.0 * nxc;
            (t, " cal order (ignored)".into())
        }
    } else {
        let nxc = clamp01(nx);
        let t = 1.0 - 2.0 * nxc;
        (t, String::new())
    }
}

/// `x = 0.5 * t * d * H_SCALE * h_sens`, then optional horizontal flip. Vertical unchanged from vrm-bridgism.
fn build_lookat(nx: f32, ny: f32, ha: &HomeAssistantSettings) -> (Vec3, String) {
    let d = ha.vision_gaze_depth.max(0.05);
    let (t, hpart) = horizontal_t(ha, nx);
    let h_sens = ha.vision_gaze_horizontal_sensitivity.clamp(0.01, 1.0);
    let mut x = 0.5 * t * d * H_SCALE * h_sens;
    if ha.vision_gaze_flip_horizontal {
        x = -x;
    }
    let y = (0.4 - ny) * d * V_SCALE + 1.5;
    // `LookAtTarget` idle in `look_at.rs` is at +Z in VRM-local space
    // (`Vec3::new(0.0, 1.4, 1.0)`). Using `-d` places targets behind the head,
    // driving yaw toward ~180° and saturating eyes at full left/right.
    let z = d;
    let flip = if ha.vision_gaze_flip_horizontal { " · flip_h" } else { "" };
    let h_sens_s = if (h_sens - 1.0).abs() > 1.0e-3 {
        format!(" · h_sens={h_sens:.2}")
    } else {
        String::new()
    };
    let d_center = if let (Some(l), Some(c), Some(r)) = (
        ha.vision_gaze_cal_nx_left,
        ha.vision_gaze_cal_nx_center,
        ha.vision_gaze_cal_nx_right,
    ) {
        if l < c && c < r {
            format!(" · Δnx@C={:+.3} (0 @ straight when nx≈C)", nx - c)
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let off = Vec3::new(
        ha.vision_gaze_offset_x,
        ha.vision_gaze_offset_y,
        ha.vision_gaze_offset_z,
    );
    let off_s = if off.length_squared() > 1e-8 {
        format!(
            " · off=({:.2},{:.2},{:.2})",
            off.x, off.y, off.z
        )
    } else {
        String::new()
    };
    let p = Vec3::new(x, y, z) + off;
    let diag = format!(" · nx={nx:.3} ny={ny:.3} · depth={d:.2}{hpart}{h_sens_s}{flip}{d_center}{off_s}");
    (p, diag)
}

fn label_str(d: &Value) -> String {
    d.get("label")
        .or_else(|| d.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn detection_summary(detections: &[Value]) -> String {
    let parts: Vec<String> = detections
        .iter()
        .map(|d| {
            let l = d
                .get("label")
                .or_else(|| d.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            format!("{l}")
        })
        .collect();
    if parts.is_empty() {
        "(empty)".into()
    } else {
        parts.join(", ")
    }
}

fn extract_person_centroid(
    detections: &[Value],
    img_w: f32,
    img_h: f32,
) -> Option<(f32, f32, String)> {
    let w = img_w.max(1.0);
    let h = img_h.max(1.0);

    for d in detections {
        let label = label_str(d);
        if label != "person" && label != "face" {
            continue;
        }

        if let Some(bbox) = d.get("bbox").and_then(|b| b.as_array()) {
            if bbox.len() >= 4 {
                let x1 = json_f64(bbox.get(0)?)? as f32;
                let y1 = json_f64(bbox.get(1)?)? as f32;
                let x2 = json_f64(bbox.get(2)?)? as f32;
                let y2 = json_f64(bbox.get(3)?)? as f32;
                let norm = bbox_is_normalized(x1, y1, x2, y2);
                let face_y = y1 + (y2 - y1) * 0.15;
                let cx = (x1 + x2) * 0.5;
                let (nx, ny, tag) = if norm {
                    (
                        clamp01(cx),
                        clamp01(face_y),
                        format!("bbox_norm[{x1:.3},{y1:.3},{x2:.3},{y2:.3}]"),
                    )
                } else {
                    (
                        clamp01(cx / w),
                        clamp01(face_y / h),
                        format!("bbox_px[{x1:.0},{y1:.0},{x2:.0},{y2:.0}]÷{w:.0}×{h:.0}"),
                    )
                };
                return Some((nx, ny, tag));
            }
        }

        if let (Some(x), Some(y)) = (d.get("x").and_then(json_f64), d.get("y").and_then(json_f64)) {
            let fw = d
                .get("width")
                .or_else(|| d.get("w"))
                .and_then(json_f64)
                .unwrap_or(0.0) as f32;
            let fh = d
                .get("height")
                .or_else(|| d.get("h"))
                .and_then(json_f64)
                .unwrap_or(0.0) as f32;
            let x = x as f32;
            let y = y as f32;
            let face_y = y + fh * 0.15;
            let cx = x + fw * 0.5;
            let norm = fw <= 1.001 && fh <= 1.001 && x <= 1.001 && y <= 1.001 && (x + fw) <= 1.05;
            let (nx, ny, tag) = if norm {
                (
                    clamp01(cx),
                    clamp01(face_y),
                    format!("xywh_norm(x={x:.3},y={y:.3},w={fw:.3},h={fh:.3})"),
                )
            } else {
                (
                    clamp01(cx / w),
                    clamp01(face_y / h),
                    format!("xywh_px(x={x:.0},y={y:.0},w={fw:.0},h={fh:.0})"),
                )
            };
            return Some((nx, ny, tag));
        }

        if let (Some(cx), Some(cy)) = (
            d.get("center_x").and_then(json_f64),
            d.get("center_y").and_then(json_f64),
        ) {
            let cx = cx as f32;
            let cy = cy as f32;
            let norm = cx <= 1.001 && cy <= 1.001;
            let (nx, ny, tag) = if norm {
                (clamp01(cx), clamp01(cy), "center_norm".into())
            } else {
                (
                    clamp01(cx / w),
                    clamp01(cy / h),
                    format!("center_px({cx:.0},{cy:.0})"),
                )
            };
            return Some((nx, ny, tag));
        }

        return Some((0.5, 0.3, "person/face (no bbox) → frame center".into()));
    }
    None
}

fn analyse_detections(state: &Value, ha: &HomeAssistantSettings) -> VisionGazeJobResult {
    let Some(attrs) = state.get("attributes") else {
        return VisionGazeJobResult::NoPerson {
            diag: "no state.attributes".into(),
        };
    };
    let Some(det) = attrs.get("detections") else {
        return VisionGazeJobResult::NoPerson {
            diag: "no attributes.detections".into(),
        };
    };
    let Some(detections) = det.as_array() else {
        return VisionGazeJobResult::NoPerson {
            diag: "attributes.detections is not an array".into(),
        };
    };

    let w = ha.vision_gaze_image_width.max(1.0);
    let h = ha.vision_gaze_image_height.max(1.0);
    let depth = ha.vision_gaze_depth;

    if detections.is_empty() {
        return VisionGazeJobResult::NoPerson {
            diag: format!("detections[] empty · depth={depth:.2}"),
        };
    }

    if let Some((nx, ny, geom)) = extract_person_centroid(detections, w, h) {
        let (pos, h_extra) = build_lookat(nx, ny, ha);
        let diag = format!(
            "{geom}{h_extra} → local ({:.2},{:.2},{:.2})",
            pos.x, pos.y, pos.z
        );
        VisionGazeJobResult::Track { pos, diag, nx, ny }
    } else {
        VisionGazeJobResult::NoPerson {
            diag: format!(
                "no person/face in {} · [{}] · try label names or depth={depth:.2}",
                detections.len(),
                detection_summary(detections)
            ),
        }
    }
}

pub struct HaVisionGazePlugin;

impl Plugin for HaVisionGazePlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded();
        app.insert_resource(HaVisionGazeChannel { tx, rx })
            .init_resource::<HaVisionGazeRuntime>()
            .add_systems(
                Update,
                (vision_gaze_pump_results, ha_vision_gaze_interpolate, vision_gaze_schedule_polls)
                    .chain(),
            );
    }
}

fn ha_vision_gaze_interpolate(
    time: Res<Time>,
    settings: Res<Settings>,
    mut rt: ResMut<HaVisionGazeRuntime>,
    mut look: MessageWriter<LookAtRequestMessage>,
) {
    if !settings.home_assistant.vision_gaze_enabled {
        return;
    }
    let ha = &settings.home_assistant;
    let Some(goal) = rt.gaze_goal else {
        return;
    };
    let tau = ha.vision_gaze_smooth_tau_sec.max(0.04);
    let dt = time.delta_secs();
    let a = 1.0 - (-dt / tau).exp();

    let sm = if let Some(s) = rt.gaze_smoothed {
        s.lerp(goal, a)
    } else {
        goal
    };
    rt.gaze_smoothed = Some(sm);
    rt.last_local = Some(sm);
    look.write(LookAtRequestMessage {
        local_target: Some(sm),
    });
}

fn vision_gaze_pump_results(
    channel: Option<Res<HaVisionGazeChannel>>,
    settings: Res<Settings>,
    mut rt: ResMut<HaVisionGazeRuntime>,
    mut look: MessageWriter<LookAtRequestMessage>,
) {
    let Some(ch) = channel else {
        return;
    };
    if !settings.home_assistant.vision_gaze_enabled {
        return;
    }

    loop {
        match ch.rx.try_recv() {
            Ok(msg) => {
                rt.in_flight = false;
                match msg {
                    VisionGazeJobResult::Track { pos, diag, nx, ny } => {
                        rt.no_detection_streak = 0;
                        rt.last_diag = diag;
                        rt.last_nx = Some(nx);
                        rt.last_ny = Some(ny);
                        rt.gaze_goal = Some(pos);
                    }
                    VisionGazeJobResult::NoPerson { diag } => {
                        rt.last_diag = diag;
                        rt.no_detection_streak += 1;
                        if rt.no_detection_streak > 6 {
                            rt.gaze_goal = None;
                            rt.gaze_smoothed = None;
                            rt.last_nx = None;
                            rt.last_ny = None;
                            rt.last_local = None;
                            look.write(LookAtRequestMessage { local_target: None });
                        }
                    }
                    VisionGazeJobResult::Error(e) => {
                        rt.last_diag = e.clone();
                    }
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
}

fn vision_gaze_schedule_polls(
    settings: Res<Settings>,
    tokio: Option<Res<SharedTokio>>,
    channel: Option<Res<HaVisionGazeChannel>>,
    mut rt: ResMut<HaVisionGazeRuntime>,
    mut look: MessageWriter<LookAtRequestMessage>,
) {
    let Some(tokio) = tokio else {
        return;
    };
    let Some(ch) = channel else {
        return;
    };

    let ha = &settings.home_assistant;

    if !ha.vision_gaze_enabled {
        if rt.prev_enabled {
            ch.drain_completed();
            look.write(LookAtRequestMessage { local_target: None });
            rt.no_detection_streak = 0;
            rt.last_diag = "off".into();
            rt.last_local = None;
            rt.gaze_goal = None;
            rt.gaze_smoothed = None;
            rt.last_nx = None;
            rt.last_ny = None;
        }
        rt.prev_enabled = false;
        rt.in_flight = false;
        return;
    }
    if !rt.prev_enabled {
        ch.drain_completed();
    }
    rt.prev_enabled = true;

    if !home_assistant::configured(ha) {
        rt.last_diag = "HA URL/token not set".into();
        rt.in_flight = false;
        return;
    }

    let Some(entity_id) = ha
        .detection_sensor_ids
        .first()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.clone())
    else {
        rt.last_diag = "enable a detection sensor (Device registry)".into();
        rt.in_flight = false;
        return;
    };

    let poll_ms = ha.vision_gaze_poll_ms.max(50);
    let now = now_ms();
    if rt.in_flight {
        return;
    }
    if now.saturating_sub(rt.last_fire_ms) < poll_ms as u128 {
        return;
    }

    rt.in_flight = true;
    rt.last_fire_ms = now;

    let tx = ch.tx.clone();
    let ha_clone = ha.clone();

    tokio.spawn(async move {
        let client = match Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(VisionGazeJobResult::Error(format!("reqwest client: {e}")));
                return;
            }
        };

        match home_assistant::fetch_state(&client, &ha_clone, &entity_id).await {
            Ok(state) => {
                let msg = analyse_detections(&state, &ha_clone);
                let _ = tx.send(msg);
            }
            Err(e) => {
                let _ = tx.send(VisionGazeJobResult::Error(e.to_string()));
            }
        }
    });
}
