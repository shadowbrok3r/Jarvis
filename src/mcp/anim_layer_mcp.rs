//! MCP helpers for the animation [`LayerStack`](crate::plugins::anim_layers::LayerStack).

use std::path::Path;

use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use jarvis_avatar::pose_library::PoseLibrary;

use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::{BlendMode, DriverKind, Layer, LayerStack};

// ---------------------------------------------------------------------------
// Args (serde + JsonSchema for MCP)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerDriverSpec {
    Clip {
        filename: String,
    },
    PoseHold {
        pose_ref: String,
    },
    Breathing {
        #[serde(default)]
        rate_hz: Option<f32>,
        #[serde(default)]
        pitch_deg: Option<f32>,
        #[serde(default)]
        roll_deg: Option<f32>,
    },
    Blink {
        #[serde(default)]
        mean_interval: Option<f32>,
        #[serde(default)]
        double_blink_chance: Option<f32>,
    },
    WeightShift {
        #[serde(default)]
        rate_hz: Option<f32>,
        #[serde(default)]
        hip_roll_deg: Option<f32>,
        #[serde(default)]
        spine_counter_deg: Option<f32>,
    },
    FingerFidget {
        #[serde(default)]
        amplitude_deg: Option<f32>,
        #[serde(default)]
        frequency_hz: Option<f32>,
        #[serde(default)]
        seed: Option<u64>,
        /// Resting inward curl (deg) the fingers oscillate around. Use a
        /// negative value if the rig hyperextends. Default ~9°.
        #[serde(default)]
        curl_bias_deg: Option<f32>,
        /// Resting thumb opposition (deg) — how far the thumb tucks toward the
        /// palm. Drives the thumb's own yaw axis so it reads relaxed instead of
        /// a thumbs-up. Default ~8°.
        #[serde(default)]
        curl_bias_thumb_deg: Option<f32>,
    },
    ToeFidget {
        #[serde(default)]
        amplitude_deg: Option<f32>,
        #[serde(default)]
        frequency_hz: Option<f32>,
        #[serde(default)]
        seed: Option<u64>,
        #[serde(default)]
        curl_bias_deg: Option<f32>,
    },
    /// Ambient head/neck glances (neck + head only — never the eyes, which the
    /// gaze system owns). Damps automatically while a face is being tracked.
    LookAround {
        /// Mean seconds between glance targets. Default ~3.5.
        #[serde(default)]
        mean_interval: Option<f32>,
        /// Max horizontal glance (deg). Default ~12.
        #[serde(default)]
        yaw_deg: Option<f32>,
        /// Max vertical glance (deg). Default ~6.
        #[serde(default)]
        pitch_deg: Option<f32>,
    },
    /// Slow whole-body balance sway (spine chain only). Adds the forward/back
    /// lean dimension that weight_shift lacks. Use `additive` blend.
    Sway {
        /// Sway cycle rate (Hz). Default ~0.05 (very slow).
        #[serde(default)]
        rate_hz: Option<f32>,
        /// Peak lean amount (deg). Default ~1.2.
        #[serde(default)]
        amount_deg: Option<f32>,
    },
    /// Relaxed pendular arm sway (upper/lower arms). Use `additive` blend.
    ArmSway {
        /// Swing rate (Hz). Default ~0.08.
        #[serde(default)]
        rate_hz: Option<f32>,
        /// Peak swing amount (deg). Default ~1.5.
        #[serde(default)]
        amount_deg: Option<f32>,
    },
    /// Coordinated lower-body contrapposto: a single slow wandering weight
    /// signal drives hips/spine/chest lean plus both legs (free-leg knee bend,
    /// thigh ab/adduction) and ankle postural micro-sway, phase-locked into one
    /// organic weight-transfer cycle. Self-contained — drives the whole lower
    /// body, so don't stack `weight_shift`/`sway` under it. Use `additive` blend.
    LegShift {
        /// Weight-transfer cycle rate (Hz). Default ~0.05 (very slow).
        #[serde(default)]
        rate_hz: Option<f32>,
        /// Hip lateral shift amount (deg). Default ~3.5.
        #[serde(default)]
        shift_deg: Option<f32>,
        /// Free-leg knee bend depth (deg). Default ~8.0.
        #[serde(default)]
        knee_bend_deg: Option<f32>,
        /// Hip yaw/roll sway amount (deg). Default ~2.5.
        #[serde(default)]
        hip_sway_deg: Option<f32>,
        /// Ankle postural micro-sway amount (deg). Default ~1.8.
        #[serde(default)]
        ankle_deg: Option<f32>,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddLayerArgs {
    pub slug: String,
    #[serde(default)]
    pub label: Option<String>,
    pub driver: LayerDriverSpec,
    #[serde(default)]
    pub weight: Option<f32>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `override` (absolute rotations) or `additive` / `rest_relative` (rest-relative deltas).
    #[serde(default)]
    pub blend_mode: Option<String>,
    #[serde(default)]
    pub mask_include: Option<Vec<String>>,
    #[serde(default)]
    pub mask_exclude: Option<Vec<String>>,
    #[serde(default)]
    pub mask_include_subtrees: Option<Vec<String>>,
    #[serde(default)]
    pub mask_exclude_subtrees: Option<Vec<String>>,
    #[serde(default)]
    pub speed: Option<f32>,
    #[serde(default)]
    pub looping: Option<bool>,
    /// Seconds of crossfade across the clip loop seam (kills the loop "twitch").
    #[serde(default)]
    pub loop_fade: Option<f32>,
    /// Bounce at the clip ends instead of wrapping (seamless back-and-forth).
    #[serde(default)]
    pub ping_pong: Option<bool>,
}

/// Batch payload for [`set_layer_stack`]: clears the stack and re-adds every
/// layer atomically in one MCP call. Use this whenever you would otherwise
/// chain `clear_layers` + N×`add_layer` (e.g. when authoring a new layer-set
/// preset). `master_enabled = None` leaves the current value alone.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetLayerStackArgs {
    pub layers: Vec<AddLayerArgs>,
    #[serde(default)]
    pub master_enabled: Option<bool>,
    /// Optional convenience: persist the resulting stack as a named layer-set
    /// in the same call (equivalent to a follow-up `save_layer_set`).
    #[serde(default)]
    pub save_as: Option<String>,
    #[serde(default)]
    pub persist: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct DriverParamsPatch {
    #[serde(default)]
    pub rate_hz: Option<f32>,
    #[serde(default)]
    pub pitch_deg: Option<f32>,
    #[serde(default)]
    pub roll_deg: Option<f32>,
    #[serde(default)]
    pub mean_interval: Option<f32>,
    #[serde(default)]
    pub double_blink_chance: Option<f32>,
    #[serde(default)]
    pub hip_roll_deg: Option<f32>,
    #[serde(default)]
    pub spine_counter_deg: Option<f32>,
    #[serde(default)]
    pub amplitude_deg: Option<f32>,
    #[serde(default)]
    pub frequency_hz: Option<f32>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub curl_bias_deg: Option<f32>,
    #[serde(default)]
    pub curl_bias_thumb_deg: Option<f32>,
    #[serde(default)]
    pub yaw_deg: Option<f32>,
    #[serde(default)]
    pub amount_deg: Option<f32>,
    #[serde(default)]
    pub shift_deg: Option<f32>,
    #[serde(default)]
    pub knee_bend_deg: Option<f32>,
    #[serde(default)]
    pub hip_sway_deg: Option<f32>,
    #[serde(default)]
    pub ankle_deg: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateLayerArgs {
    /// Numeric layer id from `list_layers`, or a unique `slug` / label (case-insensitive).
    pub id_or_slug: String,
    #[serde(default)]
    pub weight: Option<f32>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub blend_mode: Option<String>,
    #[serde(default)]
    pub mask_include: Option<Vec<String>>,
    #[serde(default)]
    pub mask_exclude: Option<Vec<String>>,
    #[serde(default)]
    pub mask_include_subtrees: Option<Vec<String>>,
    #[serde(default)]
    pub mask_exclude_subtrees: Option<Vec<String>>,
    #[serde(default)]
    pub speed: Option<f32>,
    #[serde(default)]
    pub playing: Option<bool>,
    #[serde(default)]
    pub looping: Option<bool>,
    #[serde(default)]
    pub reverse: Option<bool>,
    #[serde(default)]
    pub loop_fade: Option<f32>,
    #[serde(default)]
    pub ping_pong: Option<bool>,
    #[serde(default)]
    pub driver_params: Option<DriverParamsPatch>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveLayerArgs {
    pub id_or_slug: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMasterEnabledArgs {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstallDefaultLayersArgs {
    /// When set, assigns `LayerStack.master_enabled`. Defaults to `true`.
    #[serde(default)]
    pub master_enabled: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveLayerSetArgs {
    pub name: String,
    /// When true (default), writes `config/anim_layer_sets.json`.
    #[serde(default = "default_true")]
    pub persist: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadLayerSetArgs {
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteLayerSetArgs {
    pub name: String,
    #[serde(default = "default_true")]
    pub persist: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Parsing / driver construction
// ---------------------------------------------------------------------------

pub fn parse_blend_mode(raw: Option<&str>) -> Result<Option<BlendMode>, String> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let low = s.to_ascii_lowercase().replace('-', "_");
    match low.as_str() {
        "override" => Ok(Some(BlendMode::Override)),
        "additive" | "rest_relative" => Ok(Some(BlendMode::RestRelative)),
        _ => Err(format!(
            "invalid blend_mode {s:?} — use override or additive (rest_relative)"
        )),
    }
}

fn driver_from_spec(library: &PoseLibrary, spec: &LayerDriverSpec) -> Result<DriverKind, String> {
    Ok(match spec {
        LayerDriverSpec::Clip { filename } => {
            let animation = library
                .load_animation(filename)
                .or_else(|_| load_animation_loose(library, filename))
                .map_err(|e| format!("clip {filename:?}: {e}"))?;
            DriverKind::Clip {
                animation: Box::new(animation),
            }
        }
        LayerDriverSpec::PoseHold { pose_ref } => {
            let pose = library
                .load_pose_loose(pose_ref)
                .map_err(|e| format!("pose_hold {pose_ref:?}: {e}"))?;
            DriverKind::PoseHold {
                pose: Box::new(pose),
            }
        }
        LayerDriverSpec::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => {
            let mut d = DriverKind::breathing_default();
            if let DriverKind::Breathing {
                rate_hz: r,
                pitch_deg: p,
                roll_deg: rr,
            } = &mut d
            {
                if let Some(x) = rate_hz {
                    *r = *x;
                }
                if let Some(x) = pitch_deg {
                    *p = *x;
                }
                if let Some(x) = roll_deg {
                    *rr = *x;
                }
            }
            d
        }
        LayerDriverSpec::Blink {
            mean_interval,
            double_blink_chance,
        } => {
            let mut d = DriverKind::blink_default();
            if let DriverKind::Blink {
                mean_interval: mi,
                double_blink_chance: dc,
                ..
            } = &mut d
            {
                if let Some(x) = mean_interval {
                    *mi = x.max(0.05);
                }
                if let Some(x) = double_blink_chance {
                    *dc = x.clamp(0.0, 1.0);
                }
            }
            d
        }
        LayerDriverSpec::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => {
            let mut d = DriverKind::weight_shift_default();
            if let DriverKind::WeightShift {
                rate_hz: r,
                hip_roll_deg: h,
                spine_counter_deg: s,
            } = &mut d
            {
                if let Some(x) = rate_hz {
                    *r = *x;
                }
                if let Some(x) = hip_roll_deg {
                    *h = *x;
                }
                if let Some(x) = spine_counter_deg {
                    *s = *x;
                }
            }
            d
        }
        LayerDriverSpec::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
            curl_bias_thumb_deg,
        } => DriverKind::FingerFidget {
            amplitude_deg: amplitude_deg.unwrap_or(1.5),
            frequency_hz: frequency_hz.unwrap_or(0.35),
            seed: seed.unwrap_or_else(rand::random),
            curl_bias_deg: curl_bias_deg.unwrap_or(9.0),
            curl_bias_thumb_deg: curl_bias_thumb_deg.unwrap_or(8.0),
        },
        LayerDriverSpec::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
        } => DriverKind::ToeFidget {
            amplitude_deg: amplitude_deg.unwrap_or(1.2),
            frequency_hz: frequency_hz.unwrap_or(0.25),
            seed: seed.unwrap_or_else(rand::random),
            curl_bias_deg: curl_bias_deg.unwrap_or(4.0),
        },
        LayerDriverSpec::LookAround {
            mean_interval,
            yaw_deg,
            pitch_deg,
        } => {
            let mut d = DriverKind::look_around_default();
            if let DriverKind::LookAround {
                mean_interval: mi,
                yaw_deg: y,
                pitch_deg: p,
                ..
            } = &mut d
            {
                if let Some(x) = mean_interval {
                    *mi = *x;
                }
                if let Some(x) = yaw_deg {
                    *y = *x;
                }
                if let Some(x) = pitch_deg {
                    *p = *x;
                }
            }
            d
        }
        LayerDriverSpec::Sway {
            rate_hz,
            amount_deg,
        } => DriverKind::Sway {
            rate_hz: rate_hz.unwrap_or(0.05),
            amount_deg: amount_deg.unwrap_or(1.2),
        },
        LayerDriverSpec::ArmSway {
            rate_hz,
            amount_deg,
        } => DriverKind::ArmSway {
            rate_hz: rate_hz.unwrap_or(0.08),
            amount_deg: amount_deg.unwrap_or(1.5),
        },
        LayerDriverSpec::LegShift {
            rate_hz,
            shift_deg,
            knee_bend_deg,
            hip_sway_deg,
            ankle_deg,
        } => DriverKind::LegShift {
            rate_hz: rate_hz.unwrap_or(0.05),
            shift_deg: shift_deg.unwrap_or(3.5),
            knee_bend_deg: knee_bend_deg.unwrap_or(8.0),
            hip_sway_deg: hip_sway_deg.unwrap_or(2.5),
            ankle_deg: ankle_deg.unwrap_or(1.8),
            seed: 0xD1B5_4A32_D192_ED03,
        },
    })
}

fn load_animation_loose(
    library: &PoseLibrary,
    needle: &str,
) -> Result<jarvis_avatar::pose_library::AnimationFile, jarvis_avatar::pose_library::LibraryError>
{
    let metas = library.list_animations()?;
    let hit = metas
        .iter()
        .find(|m| m.name == needle || m.filename == needle)
        .ok_or_else(|| jarvis_avatar::pose_library::LibraryError::NotFound(needle.to_string()))?;
    library.load_animation(&hit.filename)
}

pub fn build_layer(library: &PoseLibrary, args: &AddLayerArgs) -> Result<Layer, String> {
    let driver = driver_from_spec(library, &args.driver)?;
    let label = args
        .label
        .clone()
        .unwrap_or_else(|| args.slug.clone());
    let blend_mode = parse_blend_mode(args.blend_mode.as_deref())?.unwrap_or(BlendMode::Override);
    let mut layer = Layer::new(args.slug.trim(), label, driver).blend(blend_mode);
    if let Some(w) = args.weight {
        layer.weight = w.clamp(0.0, 2.0);
    }
    if let Some(e) = args.enabled {
        layer.enabled = e;
        layer.playing = e;
        // Start the weight envelope matching the requested state so a layer
        // added disabled (promotion pattern) doesn't flash before fading out.
        layer.gain = if e { 1.0 } else { 0.0 };
    }
    if let Some(inc) = &args.mask_include {
        layer.mask.include = inc.clone();
    }
    if let Some(exc) = &args.mask_exclude {
        layer.mask.exclude = exc.clone();
    }
    if let Some(inc) = &args.mask_include_subtrees {
        layer.mask.include_subtrees = inc.clone();
    }
    if let Some(exc) = &args.mask_exclude_subtrees {
        layer.mask.exclude_subtrees = exc.clone();
    }
    if let Some(sp) = args.speed {
        layer.speed = sp.max(0.01);
    }
    if let Some(lp) = args.looping {
        layer.looping = lp;
    }
    if let Some(lf) = args.loop_fade {
        layer.loop_fade = lf.max(0.0);
    }
    if let Some(pp) = args.ping_pong {
        layer.ping_pong = pp;
    }
    Ok(layer)
}

pub fn resolve_layer_id(stack: &LayerStack, id_or_slug: &str) -> Result<u64, String> {
    let s = id_or_slug.trim();
    if s.is_empty() {
        return Err("id_or_slug is empty".into());
    }
    if let Ok(id) = s.parse::<u64>() {
        return stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.id)
            .ok_or_else(|| format!("no layer with id {id}"));
    }
    let low = s.to_ascii_lowercase();
    let hits: Vec<u64> = stack
        .layers
        .iter()
        .filter(|l| {
            l.slug.to_ascii_lowercase() == low || l.label.to_ascii_lowercase() == low
        })
        .map(|l| l.id)
        .collect();
    match hits.len() {
        0 => Err(format!("no layer matching slug/label {s:?}")),
        1 => Ok(hits[0]),
        _ => Err(format!(
            "ambiguous slug/label {s:?} — use numeric id from list_layers"
        )),
    }
}

pub fn apply_driver_patch(d: &mut DriverKind, p: &DriverParamsPatch) -> Result<(), String> {
    let any = p.rate_hz.is_some()
        || p.pitch_deg.is_some()
        || p.roll_deg.is_some()
        || p.mean_interval.is_some()
        || p.double_blink_chance.is_some()
        || p.hip_roll_deg.is_some()
        || p.spine_counter_deg.is_some()
        || p.amplitude_deg.is_some()
        || p.frequency_hz.is_some()
        || p.seed.is_some()
        || p.curl_bias_deg.is_some()
        || p.curl_bias_thumb_deg.is_some()
        || p.yaw_deg.is_some()
        || p.pitch_deg.is_some()
        || p.amount_deg.is_some()
        || p.shift_deg.is_some()
        || p.knee_bend_deg.is_some()
        || p.hip_sway_deg.is_some()
        || p.ankle_deg.is_some();
    if !any {
        return Ok(());
    }
    match d {
        DriverKind::Clip { .. }
        | DriverKind::PoseHold { .. }
        | DriverKind::ExpressionHold { .. } => Err(
            "driver_params cannot change clip/pose_hold/expression_hold — remove_layer then add_layer"
                .into(),
        ),
        DriverKind::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => {
            if let Some(x) = p.rate_hz {
                *rate_hz = x;
            }
            if let Some(x) = p.pitch_deg {
                *pitch_deg = x;
            }
            if let Some(x) = p.roll_deg {
                *roll_deg = x;
            }
            Ok(())
        }
        DriverKind::Blink {
            mean_interval,
            double_blink_chance,
            ..
        } => {
            if let Some(x) = p.mean_interval {
                *mean_interval = x.max(0.05);
            }
            if let Some(x) = p.double_blink_chance {
                *double_blink_chance = x.clamp(0.0, 1.0);
            }
            Ok(())
        }
        DriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => {
            if let Some(x) = p.rate_hz {
                *rate_hz = x;
            }
            if let Some(x) = p.hip_roll_deg {
                *hip_roll_deg = x;
            }
            if let Some(x) = p.spine_counter_deg {
                *spine_counter_deg = x;
            }
            Ok(())
        }
        DriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
            curl_bias_thumb_deg,
        } => {
            if let Some(x) = p.amplitude_deg {
                *amplitude_deg = x;
            }
            if let Some(x) = p.frequency_hz {
                *frequency_hz = x.max(0.001);
            }
            if let Some(x) = p.seed {
                *seed = x;
            }
            if let Some(x) = p.curl_bias_deg {
                *curl_bias_deg = x;
            }
            if let Some(x) = p.curl_bias_thumb_deg {
                *curl_bias_thumb_deg = x;
            }
            Ok(())
        }
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
        } => {
            if let Some(x) = p.amplitude_deg {
                *amplitude_deg = x;
            }
            if let Some(x) = p.frequency_hz {
                *frequency_hz = x.max(0.001);
            }
            if let Some(x) = p.seed {
                *seed = x;
            }
            if let Some(x) = p.curl_bias_deg {
                *curl_bias_deg = x;
            }
            Ok(())
        }
        DriverKind::LookAround {
            mean_interval,
            yaw_deg,
            pitch_deg,
            ..
        } => {
            if let Some(x) = p.mean_interval {
                *mean_interval = x.max(0.1);
            }
            if let Some(x) = p.yaw_deg {
                *yaw_deg = x;
            }
            if let Some(x) = p.pitch_deg {
                *pitch_deg = x;
            }
            Ok(())
        }
        DriverKind::Sway {
            rate_hz,
            amount_deg,
        }
        | DriverKind::ArmSway {
            rate_hz,
            amount_deg,
        } => {
            if let Some(x) = p.rate_hz {
                *rate_hz = x;
            }
            if let Some(x) = p.amount_deg {
                *amount_deg = x;
            }
            Ok(())
        }
        DriverKind::LegShift {
            rate_hz,
            shift_deg,
            knee_bend_deg,
            hip_sway_deg,
            ankle_deg,
            seed,
        } => {
            if let Some(x) = p.rate_hz {
                *rate_hz = x;
            }
            if let Some(x) = p.shift_deg {
                *shift_deg = x;
            }
            if let Some(x) = p.knee_bend_deg {
                *knee_bend_deg = x;
            }
            if let Some(x) = p.hip_sway_deg {
                *hip_sway_deg = x;
            }
            if let Some(x) = p.ankle_deg {
                *ankle_deg = x;
            }
            if let Some(x) = p.seed {
                *seed = x;
            }
            Ok(())
        }
    }
}

pub fn apply_layer_row_patch(layer: &mut Layer, args: &UpdateLayerArgs) -> Result<(), String> {
    if let Some(w) = args.weight {
        layer.weight = w.clamp(0.0, 2.0);
    }
    if let Some(e) = args.enabled {
        layer.enabled = e;
    }
    if let Some(bm) = parse_blend_mode(args.blend_mode.as_deref())? {
        layer.blend_mode = bm;
    }
    if let Some(inc) = &args.mask_include {
        layer.mask.include = inc.clone();
    }
    if let Some(exc) = &args.mask_exclude {
        layer.mask.exclude = exc.clone();
    }
    if let Some(inc) = &args.mask_include_subtrees {
        layer.mask.include_subtrees = inc.clone();
    }
    if let Some(exc) = &args.mask_exclude_subtrees {
        layer.mask.exclude_subtrees = exc.clone();
    }
    if let Some(sp) = args.speed {
        layer.speed = sp.max(0.01);
    }
    if let Some(pl) = args.playing {
        layer.playing = pl;
    }
    if let Some(lp) = args.looping {
        layer.looping = lp;
    }
    if let Some(rev) = args.reverse {
        layer.reverse = rev;
    }
    if let Some(lf) = args.loop_fade {
        layer.loop_fade = lf.max(0.0);
    }
    if let Some(pp) = args.ping_pong {
        layer.ping_pong = pp;
    }
    if let Some(ref dp) = args.driver_params {
        apply_driver_patch(&mut layer.driver, dp)?;
    }
    Ok(())
}

fn driver_to_json(d: &DriverKind) -> Value {
    match d {
        DriverKind::Clip { animation } => {
            json!({"kind": "clip", "name": animation.name, "frameCount": animation.frames.len()})
        }
        DriverKind::PoseHold { pose } => {
            json!({"kind": "pose_hold", "poseName": pose.name, "boneCount": pose.bones.len()})
        }
        DriverKind::ExpressionHold { expressions } => {
            json!({"kind": "expression_hold", "expressions": expressions})
        }
        DriverKind::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => json!({
            "kind": "breathing",
            "rateHz": rate_hz,
            "pitchDeg": pitch_deg,
            "rollDeg": roll_deg,
        }),
        DriverKind::Blink {
            mean_interval,
            double_blink_chance,
            ..
        } => json!({
            "kind": "blink",
            "meanInterval": mean_interval,
            "doubleBlinkChance": double_blink_chance,
        }),
        DriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => json!({
            "kind": "weight_shift",
            "rateHz": rate_hz,
            "hipRollDeg": hip_roll_deg,
            "spineCounterDeg": spine_counter_deg,
        }),
        DriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
            curl_bias_thumb_deg,
        } => json!({
            "kind": "finger_fidget",
            "amplitudeDeg": amplitude_deg,
            "frequencyHz": frequency_hz,
            "seed": seed,
            "curlBiasDeg": curl_bias_deg,
            "curlBiasThumbDeg": curl_bias_thumb_deg,
        }),
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
            curl_bias_deg,
        } => json!({
            "kind": "toe_fidget",
            "amplitudeDeg": amplitude_deg,
            "frequencyHz": frequency_hz,
            "seed": seed,
            "curlBiasDeg": curl_bias_deg,
        }),
        DriverKind::LookAround {
            mean_interval,
            yaw_deg,
            pitch_deg,
            ..
        } => json!({
            "kind": "look_around",
            "meanInterval": mean_interval,
            "yawDeg": yaw_deg,
            "pitchDeg": pitch_deg,
        }),
        DriverKind::Sway {
            rate_hz,
            amount_deg,
        } => json!({
            "kind": "sway",
            "rateHz": rate_hz,
            "amountDeg": amount_deg,
        }),
        DriverKind::ArmSway {
            rate_hz,
            amount_deg,
        } => json!({
            "kind": "arm_sway",
            "rateHz": rate_hz,
            "amountDeg": amount_deg,
        }),
        DriverKind::LegShift {
            rate_hz,
            shift_deg,
            knee_bend_deg,
            hip_sway_deg,
            ankle_deg,
            seed,
        } => json!({
            "kind": "leg_shift",
            "rateHz": rate_hz,
            "shiftDeg": shift_deg,
            "kneeBendDeg": knee_bend_deg,
            "hipSwayDeg": hip_sway_deg,
            "ankleDeg": ankle_deg,
            "seed": seed,
        }),
    }
}

pub fn stack_snapshot_json(stack: &LayerStack) -> Value {
    let layers: Vec<Value> = stack
        .layers
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "slug": l.slug,
                "label": l.label,
                "driver": driver_to_json(&l.driver),
                "weight": l.weight,
                "enabled": l.enabled,
                "blendMode": l.blend_mode.label(),
                "mask": {
                    "include": l.mask.include,
                    "exclude": l.mask.exclude,
                },
                "speed": l.speed,
                "playing": l.playing,
                "looping": l.looping,
                "reverse": l.reverse,
                "loopFade": l.loop_fade,
                "pingPong": l.ping_pong,
                "time": l.time,
                "duration": l.duration,
            })
        })
        .collect();
    json!({
        "masterEnabled": stack.master_enabled,
        "layerCount": stack.layers.len(),
        "clock": stack.clock,
        "layers": layers,
    })
}

pub fn install_default_layers_stack(stack: &mut LayerStack, master_enabled: Option<bool>) {
    stack.layers.clear();
    stack.install_default_procedural_layers();
    stack.master_enabled = master_enabled.unwrap_or(true);
}

pub fn read_layer_guide(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))
}

pub fn list_layer_set_names(store: &LayerSetsStore) -> Vec<String> {
    store.sorted_names()
}

pub fn save_layer_set_current(
    store: &LayerSetsStore,
    stack: &LayerStack,
    name: &str,
    persist: bool,
) {
    store.save_current(name, stack);
    if persist {
        store.persist();
    }
}

pub fn load_layer_set_named(
    store: &LayerSetsStore,
    stack: &mut LayerStack,
    library: &PoseLibrary,
    name: &str,
) -> Result<usize, String> {
    store.load_into(name, stack, library)
}

pub fn delete_layer_set_named(store: &LayerSetsStore, name: &str, persist: bool) {
    store.delete(name);
    if persist {
        store.persist();
    }
}
