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
    },
    ToeFidget {
        #[serde(default)]
        amplitude_deg: Option<f32>,
        #[serde(default)]
        frequency_hz: Option<f32>,
        #[serde(default)]
        seed: Option<u64>,
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
    pub speed: Option<f32>,
    #[serde(default)]
    pub looping: Option<bool>,
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
    pub speed: Option<f32>,
    #[serde(default)]
    pub playing: Option<bool>,
    #[serde(default)]
    pub looping: Option<bool>,
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
        } => DriverKind::FingerFidget {
            amplitude_deg: amplitude_deg.unwrap_or(1.2),
            frequency_hz: frequency_hz.unwrap_or(0.15),
            seed: seed.unwrap_or_else(rand::random),
        },
        LayerDriverSpec::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => DriverKind::ToeFidget {
            amplitude_deg: amplitude_deg.unwrap_or(0.8),
            frequency_hz: frequency_hz.unwrap_or(0.12),
            seed: seed.unwrap_or_else(rand::random),
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
    }
    if let Some(inc) = &args.mask_include {
        layer.mask.include = inc.clone();
    }
    if let Some(exc) = &args.mask_exclude {
        layer.mask.exclude = exc.clone();
    }
    if let Some(sp) = args.speed {
        layer.speed = sp.max(0.01);
    }
    if let Some(lp) = args.looping {
        layer.looping = lp;
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
        || p.seed.is_some();
    if !any {
        return Ok(());
    }
    match d {
        DriverKind::Clip { .. } | DriverKind::PoseHold { .. } => Err(
            "driver_params cannot change clip/pose_hold — remove_layer then add_layer".into(),
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
            Ok(())
        }
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
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
    if let Some(sp) = args.speed {
        layer.speed = sp.max(0.01);
    }
    if let Some(pl) = args.playing {
        layer.playing = pl;
    }
    if let Some(lp) = args.looping {
        layer.looping = lp;
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
        } => json!({
            "kind": "finger_fidget",
            "amplitudeDeg": amplitude_deg,
            "frequencyHz": frequency_hz,
            "seed": seed,
        }),
        DriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => json!({
            "kind": "toe_fidget",
            "amplitudeDeg": amplitude_deg,
            "frequencyHz": frequency_hz,
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
