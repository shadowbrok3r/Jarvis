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

use jarvis_avatar::config::Settings;
use jarvis_avatar::paths::expand_home;
use jarvis_avatar::pose_library::{PoseLibrary, slugify};

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
    ) -> Result<usize, String> {
        let set = {
            let guard = self.inner.read();
            guard
                .sets
                .get(name)
                .cloned()
                .ok_or_else(|| format!("set '{name}' not found"))?
        };
        let rehydrated = set.hydrate_into_stack(stack, library);
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

    /// Clear the target stack and rebuild it from this set. Returns the
    /// number of layers successfully rehydrated.
    pub fn hydrate_into_stack(&self, stack: &mut LayerStack, library: &PoseLibrary) -> usize {
        stack.layers.clear();
        stack.master_enabled = self.master_enabled;
        let mut count = 0;
        for bp in &self.layers {
            match bp.to_layer(library) {
                Ok(layer) => {
                    stack.add_layer(layer);
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
    #[serde(default = "one")]
    pub speed: f32,
    #[serde(default = "yes")]
    pub looping: bool,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
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
            speed: layer.speed,
            looping: layer.looping,
        }
    }

    pub fn to_layer(&self, library: &PoseLibrary) -> Result<Layer, String> {
        let driver = self.driver.to_driver(library)?;
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
            },
            time: 0.0,
            speed: self.speed,
            playing: self.enabled,
            duration,
            looping: self.looping,
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
    },
    Blink {
        mean_interval: f32,
        double_blink_chance: f32,
    },
    WeightShift {
        rate_hz: f32,
        hip_roll_deg: f32,
        spine_counter_deg: f32,
    },
    FingerFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
    },
    ToeFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
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
            DriverKind::Breathing {
                rate_hz,
                pitch_deg,
                roll_deg,
            } => Self::Breathing {
                rate_hz: *rate_hz,
                pitch_deg: *pitch_deg,
                roll_deg: *roll_deg,
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
            } => Self::WeightShift {
                rate_hz: *rate_hz,
                hip_roll_deg: *hip_roll_deg,
                spine_counter_deg: *spine_counter_deg,
            },
            DriverKind::FingerFidget {
                amplitude_deg,
                frequency_hz,
                seed,
            } => Self::FingerFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
            },
            DriverKind::ToeFidget {
                amplitude_deg,
                frequency_hz,
                seed,
            } => Self::ToeFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
            },
        }
    }

    fn to_driver(&self, library: &PoseLibrary) -> Result<DriverKind, String> {
        Ok(match self {
            Self::Clip { filename } => {
                // Layers always serialise by clip filename (e.g. "wave.json"),
                // but older files could have stored the clip's display name.
                // Try both before giving up.
                let animation = library
                    .load_animation(filename)
                    .or_else(|_| find_animation_by_name(library, filename))
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
            Self::Breathing {
                rate_hz,
                pitch_deg,
                roll_deg,
            } => DriverKind::Breathing {
                rate_hz: *rate_hz,
                pitch_deg: *pitch_deg,
                roll_deg: *roll_deg,
            },
            Self::Blink {
                mean_interval,
                double_blink_chance,
            } => {
                let DriverKind::Blink {
                    next_in,
                    phase,
                    phase_t,
                    ..
                } = DriverKind::blink_default()
                else {
                    unreachable!()
                };
                DriverKind::Blink {
                    next_in,
                    phase,
                    phase_t,
                    mean_interval: *mean_interval,
                    double_blink_chance: *double_blink_chance,
                }
            }
            Self::WeightShift {
                rate_hz,
                hip_roll_deg,
                spine_counter_deg,
            } => DriverKind::WeightShift {
                rate_hz: *rate_hz,
                hip_roll_deg: *hip_roll_deg,
                spine_counter_deg: *spine_counter_deg,
            },
            Self::FingerFidget {
                amplitude_deg,
                frequency_hz,
                seed,
            } => DriverKind::FingerFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
            },
            Self::ToeFidget {
                amplitude_deg,
                frequency_hz,
                seed,
            } => DriverKind::ToeFidget {
                amplitude_deg: *amplitude_deg,
                frequency_hz: *frequency_hz,
                seed: *seed,
            },
        })
    }
}

fn find_animation_by_name(
    library: &PoseLibrary,
    needle: &str,
) -> Result<jarvis_avatar::pose_library::AnimationFile, jarvis_avatar::pose_library::LibraryError> {
    let metas = library.list_animations()?;
    let hit = metas
        .iter()
        .find(|m| m.name == needle || m.filename == needle)
        .ok_or_else(|| jarvis_avatar::pose_library::LibraryError::NotFound(needle.to_string()))?;
    library.load_animation(&hit.filename)
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
    commands.insert_resource(store);
}

fn resolve_path(raw: &str) -> PathBuf {
    expand_home(raw)
}
