//! Named **layer-set** persistence for the Animation Layers window.
//!
//! A layer set is a named snapshot of the `LayerStack` — weights, masks,
//! blend modes, driver params, and (for `Clip` / **pose-hold** layers) a
//! *reference* to the animation or pose file on disk. Runtime state (time,
//! phase, RNG seed, `playing`) is intentionally dropped on save so re-loading
//! a set gives a clean start.
//!
//! Sets are persisted to `config/anim_layer_sets.json`. The debug UI
//! exposes name → save / load / delete in the Animation Layers window.
//!
//! Clip layers are rehydrated on load by calling
//! `PoseLibraryAssets::library.load_animation(filename)`. If a clip's
//! file is missing (user renamed / deleted) we log a warning and skip
//! that single layer rather than refusing the whole set.
//!
//! Pose-hold layers resolve via [`PoseLibrary::load_pose_loose`].

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::Settings;
use crate::paths::expand_home;
use crate::pose_library::{AnimationMeta, PoseLibrary, slugify};

use super::anim_layers::{BlendMode, BoneMask, DriverKind, Layer, LayerStack};

pub struct AnimLayerSetsPlugin;

impl Plugin for AnimLayerSetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_layer_sets);
    }
}

/// Shared, cloneable store for all saved layer sets.
#[derive(Resource, Clone)]
pub struct LayerSetsStore {
    pub inner: Arc<RwLock<LayerSetsData>>,
    pub path: Arc<PathBuf>,
}

#[derive(Default)]
pub struct LayerSetsData {
    pub sets: HashMap<String, LayerSet>,
    pub last_error: Option<String>,
    pub last_status: Option<String>,
}

impl LayerSetsStore {
    pub fn sorted_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.read().sets.keys().cloned().collect();
        v.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        v
    }

    pub fn save_current(&self, name: &str, stack: &LayerStack) {
        let set = LayerSet::from_stack(name, stack);
        self.inner.write().sets.insert(name.to_string(), set);
    }

    pub fn delete(&self, name: &str) {
        self.inner.write().sets.remove(name);
    }

    pub fn load_into(
        &self,
        name: &str,
        stack: &mut LayerStack,
        library: &PoseLibrary,
        crossfade: bool,
    ) -> Result<usize, String> {
        let set = {
            let guard = self.inner.read();
            guard
                .sets
                .get(name)
                .cloned()
                .ok_or_else(|| format!("set '{name}' not found"))?
        };
        let rehydrated = set.hydrate_into_stack(stack, library, crossfade);
        Ok(rehydrated)
    }

    /// Persist the current in-memory map to disk. Writes a status / error
    /// message that the UI can surface.
    pub fn persist(&self) {
        let path: PathBuf = (*self.path).clone();
        let (file, error) = {
            let guard = self.inner.read();
            let file = LayerSetsFile {
                version: 1,
                sets: guard.sets.values().cloned().collect(),
            };
            (file, guard.last_error.clone())
        };
        let _ = error; // keep-warn
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                self.inner.write().last_error = Some(format!("create {}: {e}", parent.display()));
                return;
            }
        }
        let write_result = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("serialize: {e}"))
            .and_then(|s| {
                fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))
            });
        match write_result {
            Ok(()) => {
                let mut g = self.inner.write();
                g.last_error = None;
                g.last_status = Some(format!("saved → {}", path.display()));
            }
            Err(e) => {
                self.inner.write().last_error = Some(e);
            }
        }
    }

    /// Seed the in-memory map with the built-in combo presets, but only for
    /// names the user hasn't already saved. We do **not** persist here — the
    /// built-ins are regenerated deterministically every startup, so a user who
    /// deletes one just gets it back next run, while a user who *edits* a
    /// same-named set wins (their file copy loaded first by [`Self::reload`]).
    pub fn install_builtin_presets(&self) {
        let mut g = self.inner.write();
        for set in builtin_presets() {
            g.sets.entry(set.name.clone()).or_insert(set);
        }
    }

    pub fn reload(&self) {
        let path: PathBuf = (*self.path).clone();
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<LayerSetsFile>(&raw) {
                Ok(file) => {
                    let map: HashMap<String, LayerSet> =
                        file.sets.into_iter().map(|s| (s.name.clone(), s)).collect();
                    let mut g = self.inner.write();
                    g.sets = map;
                    g.last_error = None;
                    g.last_status = Some(format!("loaded {}", path.display()));
                }
                Err(e) => {
                    self.inner.write().last_error = Some(format!("parse {}: {e}", path.display()));
                }
            },
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                let mut g = self.inner.write();
                g.sets.clear();
                g.last_status = Some(format!("no file yet at {}", path.display()));
            }
            Err(e) => {
                self.inner.write().last_error = Some(format!("read {}: {e}", path.display()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// On-disk schema
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct LayerSetsFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    sets: Vec<LayerSet>,
}

fn default_version() -> u32 {
    1
}

/// One named collection of layer blueprints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerSet {
    pub name: String,
    #[serde(default)]
    pub master_enabled: bool,
    #[serde(default)]
    pub layers: Vec<LayerBlueprint>,
}

impl LayerSet {
    pub fn from_stack(name: &str, stack: &LayerStack) -> Self {
        Self {
            name: name.to_string(),
            master_enabled: stack.master_enabled,
            layers: stack
                .layers
                .iter()
                .map(LayerBlueprint::from_layer)
                .collect(),
        }
    }

    /// Rebuild the target stack from this set. With `crossfade`, existing
    /// layers retire (fade out and sweep) while the new set fades in — a
    /// runtime set swap morphs instead of cutting. Without it (boot), the
    /// stack is cleared and layers install at full gain. Returns the number
    /// of layers successfully rehydrated.
    pub fn hydrate_into_stack(
        &self,
        stack: &mut LayerStack,
        library: &PoseLibrary,
        crossfade: bool,
    ) -> usize {
        if crossfade {
            let ids: Vec<u64> = stack.layers.iter().map(|l| l.id).collect();
            for id in ids {
                stack.retire_layer(id);
            }
        } else {
            stack.layers.clear();
        }
        stack.master_enabled = self.master_enabled;
        // Scan the animations dir ONCE up front. Clip layers that don't resolve
        // by filename fall back to a name lookup; doing that per-layer used to
        // re-list + re-parse the whole animations dir for every layer, so a
        // big per-bone set (50+ clip layers, e.g. a full-body VRMA import whose
        // in-memory slices were never on disk) froze the app for many seconds.
        let anims = library.list_animations().unwrap_or_default();
        let mut count = 0;
        for bp in &self.layers {
            match bp.to_layer(library, &anims) {
                Ok(layer) => {
                    if crossfade {
                        stack.add_layer_faded(layer);
                    } else {
                        stack.add_layer(layer);
                    }
                    count += 1;
                }
                Err(e) => warn!("skipping layer '{}': {e}", bp.label),
            }
        }
        count
    }
}

/// Serializable shape of a single layer. Mirrors [`Layer`] but references
/// clips by filename instead of embedding the whole animation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerBlueprint {
    pub slug: String,
    pub label: String,
    pub driver: DriverBlueprint,
    #[serde(default = "one")]
    pub weight: f32,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub blend_mode: BlendModeBp,
    #[serde(default)]
    pub mask_include: Vec<String>,
    #[serde(default)]
    pub mask_exclude: Vec<String>,
    #[serde(default)]
    pub mask_include_subtrees: Vec<String>,
    #[serde(default)]
    pub mask_exclude_subtrees: Vec<String>,
    #[serde(default = "one")]
    pub speed: f32,
    #[serde(default = "yes")]
    pub looping: bool,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default = "default_loop_fade")]
    pub loop_fade: f32,
    #[serde(default)]
    pub ping_pong: bool,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}
fn default_loop_fade() -> f32 {
    0.25
}
fn default_thumb_bias() -> f32 {
    8.0
}
fn default_shoulder_deg() -> f32 {
    0.35
}
fn default_rate_jitter() -> f32 {
    0.15
}
fn default_dwell_min() -> f32 {
    6.0
}
fn default_dwell_max() -> f32 {
    14.0
}
fn default_leg_shift_seed() -> u64 {
    0xD1B5_4A32_D192_ED03
}

impl LayerBlueprint {
    pub fn from_layer(layer: &Layer) -> Self {
        Self {
            slug: layer.slug.clone(),
            label: layer.label.clone(),
            driver: DriverBlueprint::from_driver(&layer.driver),
            weight: layer.weight,
            enabled: layer.enabled,
            blend_mode: match layer.blend_mode {
                BlendMode::Override => BlendModeBp::Override,
                BlendMode::RestRelative => BlendModeBp::Additive,
            },
            mask_include: layer.mask.include.clone(),
            mask_exclude: layer.mask.exclude.clone(),
            mask_include_subtrees: layer.mask.include_subtrees.clone(),
            mask_exclude_subtrees: layer.mask.exclude_subtrees.clone(),
            speed: layer.speed,
            looping: layer.looping,
            reverse: layer.reverse,
            loop_fade: layer.loop_fade,
            ping_pong: layer.ping_pong,
        }
    }

    pub fn to_layer(&self, library: &PoseLibrary, anims: &[AnimationMeta]) -> Result<Layer, String> {
        let driver = self.driver.to_driver(library, anims)?;
        let duration = driver.duration_hint();
        let blend_mode = match self.blend_mode {
            BlendModeBp::Override => BlendMode::Override,
            BlendModeBp::Additive => BlendMode::RestRelative,
        };
        Ok(Layer {
            id: 0,
            slug: self.slug.clone(),
            label: self.label.clone(),
            driver,
            weight: self.weight,
            enabled: self.enabled,
            blend_mode,
            mask: BoneMask {
                include: self.mask_include.clone(),
                exclude: self.mask_exclude.clone(),
                include_subtrees: self.mask_include_subtrees.clone(),
                exclude_subtrees: self.mask_exclude_subtrees.clone(),
            },
            time: 0.0,
            speed: self.speed,
            playing: self.enabled,
            duration,
            looping: self.looping,
            reverse: self.reverse,
            loop_fade: self.loop_fade,
            ping_pong: self.ping_pong,
            gain: if self.enabled { 1.0 } else { 0.0 },
            removing: false,
            fade_in_secs: 0.35,
            fade_out_secs: 0.45,
            auto_retire_after: None,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlendModeBp {
    #[default]
    Override,
    Additive,
}

/// Serializable, runtime-state-free shape of [`DriverKind`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverBlueprint {
    Clip {
        filename: String,
    },
    PoseHold {
        /// Slug filename (`{slugify(pose.name)}.json`) or display name.
        pose_ref: String,
    },
    Breathing {
        rate_hz: f32,
        pitch_deg: f32,
        roll_deg: f32,
        #[serde(default = "default_shoulder_deg")]
        shoulder_deg: f32,
        #[serde(default = "default_rate_jitter")]
        rate_jitter: f32,
    },
    Blink {
        mean_interval: f32,
        double_blink_chance: f32,
    },
    WeightShift {
        rate_hz: f32,
        hip_roll_deg: f32,
        spine_counter_deg: f32,
        #[serde(default = "default_dwell_min")]
        dwell_min: f32,
        #[serde(default = "default_dwell_max")]
        dwell_max: f32,
    },
    FingerFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
        #[serde(default)]
        curl_bias_deg: f32,
        #[serde(default = "default_thumb_bias")]
        curl_bias_thumb_deg: f32,
    },
    ToeFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
        #[serde(default)]
        curl_bias_deg: f32,
    },
    ExpressionHold {
        expressions: HashMap<String, f32>,
    },
    LookAround {
        mean_interval: f32,
        yaw_deg: f32,
        pitch_deg: f32,
    },
    Sway {
        rate_hz: f32,
        amount_deg: f32,
    },
    ArmSway {
        rate_hz: f32,
        amount_deg: f32,
    },
    LegShift {
        rate_hz: f32,
        shift_deg: f32,
        knee_bend_deg: f32,
        hip_sway_deg: f32,
        ankle_deg: f32,
        #[serde(default = "default_leg_shift_seed")]
        seed: u64,
    },
}

impl DriverBlueprint {
    fn from_driver(d: &DriverKind) -> Self {
        match d {
            DriverKind::Clip { animation } => Self::Clip {
                // `AnimationFile` doesn't track its source filename, so we
                // save the display name and rely on `find_animation_by_name`
                // to resolve it on load.
                filename: animation.name.clone(),
            },
            DriverKind::PoseHold { pose } => Self::PoseHold {
                pose_ref: format!("{}.json", slugify(&pose.name)),
            },
            DriverKind::ExpressionHold { expressions } => Self::ExpressionHold {
                expressions: expressions.clone(),
            },
            DriverKind::Breathing {
                rate_hz,
                pitch_deg,
                roll_deg,
                shoulder_deg,
                rate_jitter,
                ..
            } => Self::Breathing {
                rate_hz: *rate_hz,
                pitch_deg: *pitch_deg,
                roll_deg: *roll_deg,
                shoulder_deg: *shoulder_deg,
                rate_jitter: *rate_jitter,
            },
            DriverKind::Blink {
                mean_interval,
                double_blink_chance,
                ..
            } => Self::Blink {
                mean_interval: *mean_interval,
                double_blink_chance: *double_blink_chance,
            },
            DriverKind::WeightShift {
                rate_hz,
                hip_roll_deg,
                spine_counter_deg,
                dwell_min,
                dwell_max,
                ..
            } => Self::WeightShift {
                rate_hz: *rate_hz,
                hip_roll_deg: *hip_roll_deg,
                spine_counter_deg: *spine_counter_deg,
                dwell_min: *dwell_min,
                dwell_max: *dwell_max,
            },
            DriverKind::FingerFidget {
                amplitude_deg,
                frequency_hz,
                seed,
                curl_bias_deg,
                curl_bias_thumb_deg,
            } => Self::FingerFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
                curl_bias_deg: *curl_bias_deg,
                curl_bias_thumb_deg: *curl_bias_thumb_deg,
            },
            DriverKind::ToeFidget {
                amplitude_deg,
                frequency_hz,
                seed,
                curl_bias_deg,
            } => Self::ToeFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
                curl_bias_deg: *curl_bias_deg,
            },
            DriverKind::LookAround {
                mean_interval,
                yaw_deg,
                pitch_deg,
                ..
            } => Self::LookAround {
                mean_interval: *mean_interval,
                yaw_deg: *yaw_deg,
                pitch_deg: *pitch_deg,
            },
            DriverKind::Sway {
                rate_hz,
                amount_deg,
            } => Self::Sway {
                rate_hz: *rate_hz,
                amount_deg: *amount_deg,
            },
            DriverKind::ArmSway {
                rate_hz,
                amount_deg,
            } => Self::ArmSway {
                rate_hz: *rate_hz,
                amount_deg: *amount_deg,
            },
            DriverKind::LegShift {
                rate_hz,
                shift_deg,
                knee_bend_deg,
                hip_sway_deg,
                ankle_deg,
                seed,
            } => Self::LegShift {
                rate_hz: *rate_hz,
                shift_deg: *shift_deg,
                knee_bend_deg: *knee_bend_deg,
                hip_sway_deg: *hip_sway_deg,
                ankle_deg: *ankle_deg,
                seed: *seed,
            },
        }
    }

    fn to_driver(&self, library: &PoseLibrary, anims: &[AnimationMeta]) -> Result<DriverKind, String> {
        Ok(match self {
            Self::Clip { filename } => {
                // Layers always serialise by clip filename (e.g. "wave.json"),
                // but older files could have stored the clip's display name.
                // Try both before giving up. The name fallback uses the
                // pre-scanned `anims` list (no per-layer dir re-scan).
                let animation = library
                    .load_animation(filename)
                    .or_else(|_| find_animation_by_name(library, anims, filename))
                    .map_err(|e| format!("load_animation({filename}): {e}"))?;
                DriverKind::Clip {
                    animation: Box::new(animation),
                }
            }
            Self::PoseHold { pose_ref } => {
                let pose = library
                    .load_pose_loose(pose_ref)
                    .map_err(|e| format!("load_pose_loose({pose_ref}): {e}"))?;
                DriverKind::PoseHold {
                    pose: Box::new(pose),
                }
            }
            Self::ExpressionHold { expressions } => DriverKind::ExpressionHold {
                expressions: expressions.clone(),
            },
            Self::Breathing {
                rate_hz,
                pitch_deg,
                roll_deg,
                shoulder_deg,
                rate_jitter,
            } => {
                let mut d = DriverKind::breathing_default();
                if let DriverKind::Breathing {
                    rate_hz: r,
                    pitch_deg: pd,
                    roll_deg: rd,
                    shoulder_deg: sh,
                    rate_jitter: rj,
                    ..
                } = &mut d
                {
                    *r = *rate_hz;
                    *pd = *pitch_deg;
                    *rd = *roll_deg;
                    *sh = *shoulder_deg;
                    *rj = *rate_jitter;
                }
                d
            }
            Self::Blink {
                mean_interval,
                double_blink_chance,
            } => {
                let DriverKind::Blink {
                    next_in,
                    phase,
                    phase_t,
                    peak,
                    hold_secs,
                    ..
                } = DriverKind::blink_default()
                else {
                    unreachable!()
                };
                DriverKind::Blink {
                    next_in,
                    phase,
                    phase_t,
                    peak,
                    hold_secs,
                    mean_interval: *mean_interval,
                    double_blink_chance: *double_blink_chance,
                }
            }
            Self::WeightShift {
                rate_hz,
                hip_roll_deg,
                spine_counter_deg,
                dwell_min,
                dwell_max,
            } => {
                let mut d = DriverKind::weight_shift_default();
                if let DriverKind::WeightShift {
                    rate_hz: r,
                    hip_roll_deg: h,
                    spine_counter_deg: sc,
                    dwell_min: dmin,
                    dwell_max: dmax,
                    ..
                } = &mut d
                {
                    *r = *rate_hz;
                    *h = *hip_roll_deg;
                    *sc = *spine_counter_deg;
                    *dmin = *dwell_min;
                    *dmax = *dwell_max;
                }
                d
            }
            Self::FingerFidget {
                amplitude_deg,
                frequency_hz,
                seed,
                curl_bias_deg,
                curl_bias_thumb_deg,
            } => DriverKind::FingerFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
                curl_bias_deg: *curl_bias_deg,
                curl_bias_thumb_deg: *curl_bias_thumb_deg,
            },
            Self::ToeFidget {
                amplitude_deg,
                frequency_hz,
                seed,
                curl_bias_deg,
            } => DriverKind::ToeFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
                curl_bias_deg: *curl_bias_deg,
            },
            Self::LookAround {
                mean_interval,
                yaw_deg,
                pitch_deg,
            } => {
                let DriverKind::LookAround {
                    next_in,
                    cur_yaw,
                    cur_pitch,
                    target_yaw,
                    target_pitch,
                    damp,
                    ext_yaw,
                    ext_pitch,
                    ext_target_yaw,
                    ext_target_pitch,
                    ..
                } = DriverKind::look_around_default()
                else {
                    unreachable!()
                };
                DriverKind::LookAround {
                    mean_interval: *mean_interval,
                    yaw_deg: *yaw_deg,
                    pitch_deg: *pitch_deg,
                    next_in,
                    cur_yaw,
                    cur_pitch,
                    target_yaw,
                    target_pitch,
                    damp,
                    ext_yaw,
                    ext_pitch,
                    ext_target_yaw,
                    ext_target_pitch,
                }
            }
            Self::Sway {
                rate_hz,
                amount_deg,
            } => DriverKind::Sway {
                rate_hz: *rate_hz,
                amount_deg: *amount_deg,
            },
            Self::ArmSway {
                rate_hz,
                amount_deg,
            } => DriverKind::ArmSway {
                rate_hz: *rate_hz,
                amount_deg: *amount_deg,
            },
            Self::LegShift {
                rate_hz,
                shift_deg,
                knee_bend_deg,
                hip_sway_deg,
                ankle_deg,
                seed,
            } => DriverKind::LegShift {
                rate_hz: *rate_hz,
                shift_deg: *shift_deg,
                knee_bend_deg: *knee_bend_deg,
                hip_sway_deg: *hip_sway_deg,
                ankle_deg: *ankle_deg,
                seed: *seed,
            },
        })
    }
}

fn find_animation_by_name(
    library: &PoseLibrary,
    anims: &[AnimationMeta],
    needle: &str,
) -> Result<crate::pose_library::AnimationFile, crate::pose_library::LibraryError> {
    let hit = anims
        .iter()
        .find(|m| m.name == needle || m.filename == needle)
        .ok_or_else(|| crate::pose_library::LibraryError::NotFound(needle.to_string()))?;
    library.load_animation(&hit.filename)
}

// ---------------------------------------------------------------------------
// Built-in combo presets
// ---------------------------------------------------------------------------

/// A layer blueprint with sensible defaults filled in — keeps the preset
/// definitions below readable.
fn bp(
    slug: &str,
    label: &str,
    driver: DriverBlueprint,
    weight: f32,
    blend: BlendModeBp,
) -> LayerBlueprint {
    LayerBlueprint {
        slug: slug.to_string(),
        label: label.to_string(),
        driver,
        weight,
        enabled: true,
        blend_mode: blend,
        mask_include: Vec::new(),
        mask_exclude: Vec::new(),
        mask_include_subtrees: Vec::new(),
        mask_exclude_subtrees: Vec::new(),
        speed: 1.0,
        looping: true,
        reverse: false,
        loop_fade: 0.25,
        ping_pong: false,
    }
}

fn clip_layer(filename: &str, label: &str) -> LayerBlueprint {
    bp(
        filename,
        label,
        DriverBlueprint::Clip {
            filename: filename.to_string(),
        },
        1.0,
        BlendModeBp::Override,
    )
}

// Procedural-driver blueprint constructors — values mirror the `*_default()`
// fns in `anim_layers.rs` so a loaded preset matches the live defaults.
fn breathing_bp() -> DriverBlueprint {
    DriverBlueprint::Breathing {
        rate_hz: 0.25,
        pitch_deg: 0.6,
        roll_deg: 0.3,
        shoulder_deg: default_shoulder_deg(),
        rate_jitter: default_rate_jitter(),
    }
}
fn blink_bp() -> DriverBlueprint {
    DriverBlueprint::Blink {
        mean_interval: 4.0,
        double_blink_chance: 0.18,
    }
}
fn weight_shift_bp() -> DriverBlueprint {
    DriverBlueprint::WeightShift {
        rate_hz: 0.07,
        hip_roll_deg: 1.5,
        spine_counter_deg: 0.8,
        dwell_min: default_dwell_min(),
        dwell_max: default_dwell_max(),
    }
}
fn finger_fidget_bp() -> DriverBlueprint {
    DriverBlueprint::FingerFidget {
        amplitude_deg: 1.5,
        frequency_hz: 0.35,
        seed: 0x9E37_79B9_7F4A_7C15,
        curl_bias_deg: 9.0,
        curl_bias_thumb_deg: 8.0,
    }
}
fn toe_fidget_bp() -> DriverBlueprint {
    DriverBlueprint::ToeFidget {
        amplitude_deg: 1.2,
        frequency_hz: 0.25,
        seed: 0xBF58_476D_1CE4_E5B9,
        curl_bias_deg: 4.0,
    }
}
fn look_around_bp() -> DriverBlueprint {
    DriverBlueprint::LookAround {
        mean_interval: 3.5,
        yaw_deg: 12.0,
        pitch_deg: 6.0,
    }
}
fn sway_bp(amount_deg: f32) -> DriverBlueprint {
    DriverBlueprint::Sway {
        rate_hz: 0.05,
        amount_deg,
    }
}
fn arm_sway_bp(amount_deg: f32) -> DriverBlueprint {
    DriverBlueprint::ArmSway {
        rate_hz: 0.08,
        amount_deg,
    }
}
fn leg_shift_bp() -> DriverBlueprint {
    DriverBlueprint::LegShift {
        rate_hz: 0.05,
        shift_deg: 3.5,
        knee_bend_deg: 8.0,
        hip_sway_deg: 2.5,
        ankle_deg: 1.8,
        seed: default_leg_shift_seed(),
    }
}

/// The full ambient "alive" procedural stack, reused as the base of several
/// presets. `arm_sway` is split out so presets that lock the arms (e.g.
/// arms-crossed) can omit it.
fn alive_stack(arm_sway: bool) -> Vec<LayerBlueprint> {
    let a = BlendModeBp::Additive;
    let mut v = vec![
        bp("breathing", "Breathing", breathing_bp(), 1.0, a),
        bp("auto-blink", "Auto-Blink", blink_bp(), 1.0, BlendModeBp::Override),
        bp("leg-shift", "Leg Shift", leg_shift_bp(), 0.85, a),
        bp("look-around", "Look Around", look_around_bp(), 1.0, a),
        bp("finger-fidget", "Finger Fidget", finger_fidget_bp(), 0.9, a),
        bp("toe-fidget", "Toe Fidget", toe_fidget_bp(), 0.7, a),
    ];
    if arm_sway {
        v.push(bp("arm-sway", "Arm Sway", arm_sway_bp(1.5), 0.6, a));
    }
    v
}

/// Named combo presets shipped with the app. Each stacks a base clip (or none)
/// under the ambient procedural drivers so the avatar reads as alive instead of
/// looping a single canned clip.
fn builtin_presets() -> Vec<LayerSet> {
    let a = BlendModeBp::Additive;
    vec![
        // Pure ambient standing idle — no base clip, just the living stack.
        LayerSet {
            name: "Alive — Standing".to_string(),
            master_enabled: true,
            layers: alive_stack(true),
        },
        // Arms crossed: clip pins the arms (Override), so we skip arm-sway and
        // fingers and keep the torso/head alive underneath.
        LayerSet {
            name: "Alive — Arms Crossed".to_string(),
            master_enabled: true,
            layers: {
                let mut l = vec![clip_layer("idle_arms_crossed.json", "Arms Crossed")];
                l.extend([
                    bp("breathing", "Breathing", breathing_bp(), 1.0, a),
                    bp("auto-blink", "Auto-Blink", blink_bp(), 1.0, BlendModeBp::Override),
                    bp("weight-shift", "Weight Shift", weight_shift_bp(), 0.6, a),
                    bp("sway", "Body Sway", sway_bp(0.9), 0.7, a),
                    bp("look-around", "Look Around", look_around_bp(), 1.0, a),
                ]);
                l
            },
        },
        // Hands clasped in front — arms are positioned but can still drift a
        // little, so keep a gentle arm-sway at low weight.
        LayerSet {
            name: "Alive — Hands Clasped".to_string(),
            master_enabled: true,
            layers: {
                let mut l = vec![clip_layer("idle_hands_clasp_front.json", "Hands Clasped")];
                l.extend([
                    bp("breathing", "Breathing", breathing_bp(), 1.0, a),
                    bp("auto-blink", "Auto-Blink", blink_bp(), 1.0, BlendModeBp::Override),
                    bp("weight-shift", "Weight Shift", weight_shift_bp(), 0.7, a),
                    bp("sway", "Body Sway", sway_bp(1.0), 0.8, a),
                    bp("arm-sway", "Arm Sway", arm_sway_bp(0.8), 0.4, a),
                    bp("look-around", "Look Around", look_around_bp(), 1.0, a),
                ]);
                l
            },
        },
        // Restless: no base clip, heavier sway + fidget weights for a more
        // fidgety, energetic idle.
        LayerSet {
            name: "Restless".to_string(),
            master_enabled: true,
            layers: vec![
                bp("breathing", "Breathing", breathing_bp(), 1.0, a),
                bp("auto-blink", "Auto-Blink", blink_bp(), 1.0, BlendModeBp::Override),
                bp("weight-shift", "Weight Shift", weight_shift_bp(), 1.0, a),
                bp("sway", "Body Sway", sway_bp(1.8), 1.0, a),
                bp("arm-sway", "Arm Sway", arm_sway_bp(2.4), 0.9, a),
                bp("look-around", "Look Around", look_around_bp(), 1.0, a),
                bp("finger-fidget", "Finger Fidget", finger_fidget_bp(), 1.0, a),
                bp("toe-fidget", "Toe Fidget", toe_fidget_bp(), 0.9, a),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Startup: build store + eager load
// ---------------------------------------------------------------------------

fn load_layer_sets(mut commands: Commands, settings: Res<Settings>) {
    let path = resolve_path(&settings.anim_layer_sets.path);
    let store = LayerSetsStore {
        inner: Arc::new(RwLock::new(LayerSetsData::default())),
        path: Arc::new(path),
    };
    store.reload();
    store.install_builtin_presets();
    commands.insert_resource(store);
}

fn resolve_path(raw: &str) -> PathBuf {
    expand_home(raw)
}
