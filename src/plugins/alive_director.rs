//! Alive "presence director" — supersedes the flat-random [`idle_tick`] with a
//! presence-aware, non-repetitive idle behavior loop layered over the always-on
//! procedural "alive base" (the `LayerStack`). See memory `alive-idle-director`.
//!

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use bevy::prelude::*;
use bevy_vrm1::prelude::{ExpressionEntityMap, Vrm};
use rand::RngExt;
use rand::seq::IndexedRandom;

use crate::config::Settings;

use crate::plugins::anim_layers::{DriverKind, LayerStackHandle};
use crate::plugins::channel_server::LookAtRequestMessage;
use crate::plugins::look_at::LookAtRuntime;
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};
use crate::plugins::pose_library_assets::PoseLibraryAssets;
use crate::plugins::tts::TtsClip;

/// Recent picks excluded from the next selection (LRU fallback for small libs).
const RECENT_WINDOW: usize = 4;
/// Deliberate camera check-in at least every N picks.
const CHECKIN_EVERY: u32 = 4;
/// Curl bias added at full expressive boost (degrees).
const FINGER_CURL_ADD: f32 = 16.0;
const TOE_CURL_ADD: f32 = 22.0;

pub struct AliveDirectorPlugin;

impl Plugin for AliveDirectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DirectorState>()
            .init_resource::<DirectorStatus>()
            .add_systems(Update, (advance_beats, run_director).chain());
    }
}

/// Saved finger/toe fidget params so an expressive boost can be reverted.
#[derive(Clone, Copy)]
struct FidgetBaseline {
    finger_amp: f32,
    finger_curl: f32,
    toe_amp: f32,
    toe_curl: f32,
}

#[derive(Resource, Default)]
pub struct DirectorState {
    pub next_pick_in: Option<Duration>,
    elapsed: Duration,
    recent: VecDeque<String>,
    /// Deferred gaze actions (look-away -> return).
    beats: Vec<Beat>,
    picks_since_checkin: u32,
    base_ensured: bool,
    /// Expressive finger/toe boost: saved baseline + countdown to restore.
    fidget_baseline: Option<FidgetBaseline>,
    fidget_restore_in: Option<Duration>,
    /// Boost envelope: current level (0..1) ramping toward the target.
    fidget_level: f32,
    fidget_target: f32,
    /// Director-applied expression presets + countdown to clear them.
    active_presets: Vec<String>,
    preset_clear_in: Option<Duration>,
    /// Cached VRM expression preset names (discovered once the rig is ready).
    preset_names: Vec<String>,
}

/// Read-only director telemetry for the debug UI.
#[derive(Resource, Default)]
pub struct DirectorStatus {
    pub next_pick_in: f32,
    pub last_episode: String,
    pub attentive: bool,
    pub speaking: bool,
    pub boost_level: f32,
}

impl DirectorState {
    fn remember(&mut self, id: String) {
        if self.recent.iter().any(|r| r == &id) {
            return;
        }
        self.recent.push_back(id);
        while self.recent.len() > RECENT_WINDOW {
            self.recent.pop_front();
        }
    }
}

/// A gaze action scheduled to fire `after` more time elapses.
struct Beat {
    after: Duration,
    target: Option<Vec3>,
}

/// Viewer position in the avatar's local space (eye height, in front).
fn viewer_target() -> Vec3 {
    Vec3::new(0.0, 1.4, 1.2)
}

/// Off-axis "thinking" gaze spots to glance toward before returning.
fn look_away_target() -> Vec3 {
    const SPOTS: [[f32; 3]; 4] = [
        [0.7, 1.55, 0.5],
        [-0.7, 1.55, 0.5],
        [0.4, 1.7, 0.3],
        [-0.4, 1.35, 0.6],
    ];
    let mut rng = rand::rng();
    let s = SPOTS.choose(&mut rng).copied().unwrap_or([0.6, 1.55, 0.5]);
    Vec3::new(s[0], s[1], s[2])
}

/// Fire any gaze beats whose countdown has elapsed (manual delayed-command queue).
fn advance_beats(
    time: Res<Time>,
    mut state: ResMut<DirectorState>,
    mut gaze: MessageWriter<LookAtRequestMessage>,
) {
    if state.beats.is_empty() {
        return;
    }
    let dt = Duration::from_secs_f32(time.delta_secs());
    let mut due: Vec<Option<Vec3>> = Vec::new();
    state.beats.retain_mut(|b| {
        if b.after <= dt {
            due.push(b.target);
            false
        } else {
            b.after -= dt;
            true
        }
    });
    for target in due {
        gaze.write(LookAtRequestMessage { local_target: target });
    }
}

fn run_director(
    time: Res<Time>,
    settings: Res<Settings>,
    library: Option<Res<PoseLibraryAssets>>,
    sender: Option<Res<PoseCommandSender>>,
    layers: Option<Res<LayerStackHandle>>,
    look_runtime: Res<LookAtRuntime>,
    speaking: Query<(), With<TtsClip>>,
    presets_q: Query<&ExpressionEntityMap, With<Vrm>>,
    mut gaze: MessageWriter<LookAtRequestMessage>,
    mut state: ResMut<DirectorState>,
    mut status: ResMut<DirectorStatus>,
) {
    let pc = &settings.pose_controller;
    if !pc.director_enabled {
        state.next_pick_in = None;
        state.elapsed = Duration::ZERO;
        state.beats.clear();
        return;
    }

    // Keep the always-on alive base running (once, so a deliberate UI master
    // disable isn't stomped every frame).
    if !state.base_ensured {
        if let Some(layers) = layers.as_ref() {
            layers.with_write(|s| {
                if !s.master_enabled {
                    s.master_enabled = true;
                }
            });
            state.base_ensured = true;
        }
    }

    let dt = Duration::from_secs_f32(time.delta_secs());

    // Advance the expressive-boost ramp / preset-clear countdowns every tick
    // so they unwind regardless of dwell timing.
    advance_fidget_ramp(&mut state, layers.as_deref(), dt, pc);
    advance_preset_clear(&mut state, sender.as_deref(), dt);

    // Skip director episodes while TTS/chat is speaking.
    if !speaking.is_empty() {
        force_restore(&mut state, layers.as_deref(), sender.as_deref());
        state.next_pick_in = None;
        state.elapsed = Duration::ZERO;
        status.speaking = true;
        return;
    }
    status.speaking = false;

    let attentive = look_runtime.gaze_active();
    status.attentive = attentive;
    status.boost_level = state.fidget_level;

    if state.next_pick_in.is_none() {
        state.next_pick_in = Some(sample_dwell(pc, attentive));
        state.elapsed = Duration::ZERO;
    }
    state.elapsed += dt;
    let Some(target) = state.next_pick_in else {
        return;
    };
    status.next_pick_in = target.saturating_sub(state.elapsed).as_secs_f32();
    if state.elapsed < target {
        return;
    }

    // Refresh the VRM preset-name cache once the rig is initialized.
    if state.preset_names.is_empty() {
        let mut names: Vec<String> = presets_q
            .iter()
            .flat_map(|map| map.0.keys().map(|k| k.0.clone()))
            .collect();
        names.sort();
        names.dedup();
        state.preset_names = names;
    }

    // ---- choose an episode kind (presence-weighted) ----
    let mut rng = rand::rng();
    let r: f32 = rng.random_range(0.0f32..1.0f32);
    let base_expr = pc.director_expressive_prob.clamp(0.0, 0.9);
    let base_gaze = pc.director_gaze_prob.clamp(0.0, 0.9);
    let p_expressive = if attentive { (base_expr + 0.15).min(0.9) } else { base_expr };
    let p_gaze = if attentive { base_gaze * 0.4 } else { base_gaze };
    let is_expressive = r < p_expressive;
    let is_gaze = !is_expressive && r < p_expressive + p_gaze;
    status.last_episode = if is_expressive {
        "expressive".into()
    } else if is_gaze {
        "gaze".into()
    } else {
        "plain".into()
    };

    if is_expressive {
        // Bring the alive factor + drive the helper presets, and present
        // toward the viewer.
        boost_fidget(&mut state, layers.as_deref());
        apply_expressive_presets(&mut state, sender.as_deref(), pc);
        // Occasional micro-expression: a faint face preset that fades in and
        // auto-retires — subtle mood flicker, never a snap.
        if pc.director_micro_expressions && rng.random_bool(0.15) {
            if let Some(layers) = layers.as_deref() {
                install_micro_expression(layers, &state.preset_names, &mut rng);
            }
        }
        gaze.write(LookAtRequestMessage {
            local_target: Some(viewer_target()),
        });
        state.picks_since_checkin = 0;
    } else if is_gaze {
        // Glance away now; return to the viewer after a beat ("thinking").
        gaze.write(LookAtRequestMessage {
            local_target: Some(look_away_target()),
        });
        let dwell = rng.random_range(2.0f32..3.5f32);
        state.beats.push(Beat {
            after: Duration::from_secs_f32(dwell),
            target: Some(viewer_target()),
        });
    }

    // Body pose/anim on expressive + plain episodes; skip on a glance-only beat.
    if !is_gaze {
        if let (Some(library), Some(layers)) = (library.as_ref(), layers.as_ref()) {
            let prefer_anim = layers.with_read(|s| {
                !s.layers
                    .iter()
                    .any(|l| l.slug == "episode-clip" && !l.removing)
            });
            if let Some(id) = pick_body(
                library,
                layers,
                pc.idle_category.trim(),
                &state.recent,
                prefer_anim,
            ) {
                state.remember(id);
            }
        }
    }

    // Periodic look-at reset toward the viewer.
    state.picks_since_checkin += 1;
    if state.picks_since_checkin >= CHECKIN_EVERY {
        gaze.write(LookAtRequestMessage {
            local_target: Some(viewer_target()),
        });
        state.picks_since_checkin = 0;
    }

    state.next_pick_in = Some(sample_dwell(pc, attentive));
    state.elapsed = Duration::ZERO;
}

/// Candidate presets a micro-expression may borrow, in preference order.
const MICRO_EXPR_CANDIDATES: [&str; 3] = ["relaxed", "happy", "surprised"];

/// Install a faint auto-retiring ExpressionHold layer (mood flicker).
fn install_micro_expression(
    layers: &LayerStackHandle,
    preset_names: &[String],
    rng: &mut impl rand::Rng,
) {
    let candidates: Vec<&String> = preset_names
        .iter()
        .filter(|n| {
            MICRO_EXPR_CANDIDATES
                .iter()
                .any(|c| n.eq_ignore_ascii_case(c))
        })
        .collect();
    let Some(name) = candidates.choose(rng) else {
        return;
    };
    let weight = rng.random_range(0.12f32..0.25);
    let retire = rng.random_range(1.0f32..2.5);
    let mut expressions = std::collections::HashMap::new();
    expressions.insert((*name).clone(), weight);
    layers.with_write(|s| {
        s.retire_slug("micro-expr");
        let mut layer = crate::plugins::anim_layers::Layer::new(
            "micro-expr",
            "Micro Expression",
            crate::plugins::anim_layers::DriverKind::ExpressionHold { expressions },
        )
        .blend(crate::plugins::anim_layers::BlendMode::RestRelative);
        layer.fade_in_secs = 0.4;
        layer.auto_retire_after = Some(retire);
        s.add_layer_faded(layer);
    });
}

/// Begin ramping the expressive fidget boost in (capturing the baseline once).
fn boost_fidget(state: &mut DirectorState, layers: Option<&LayerStackHandle>) {
    let Some(layers) = layers else { return };
    if state.fidget_baseline.is_none() {
        let captured = layers.with_read(|s| {
            let mut b = FidgetBaseline {
                finger_amp: 0.0,
                finger_curl: 0.0,
                toe_amp: 0.0,
                toe_curl: 0.0,
            };
            for layer in s.layers.iter() {
                match &layer.driver {
                    DriverKind::FingerFidget {
                        amplitude_deg,
                        curl_bias_deg,
                        ..
                    } => {
                        b.finger_amp = *amplitude_deg;
                        b.finger_curl = *curl_bias_deg;
                    }
                    DriverKind::ToeFidget {
                        amplitude_deg,
                        curl_bias_deg,
                        ..
                    } => {
                        b.toe_amp = *amplitude_deg;
                        b.toe_curl = *curl_bias_deg;
                    }
                    _ => {}
                }
            }
            b
        });
        state.fidget_baseline = Some(captured);
    }
    state.fidget_target = 1.0;
    // Hold the boost a few seconds, then unwind.
    let mut rng = rand::rng();
    state.fidget_restore_in = Some(Duration::from_secs_f32(rng.random_range(3.0f32..6.0f32)));
}

/// Ramp the boost level toward its target and write interpolated fidget params
/// each tick — the boost breathes in and out instead of stepping.
fn advance_fidget_ramp(
    state: &mut DirectorState,
    layers: Option<&LayerStackHandle>,
    dt: Duration,
    pc: &crate::config::PoseControllerSettings,
) {
    let due = match state.fidget_restore_in {
        Some(t) if t <= dt => true,
        Some(t) => {
            state.fidget_restore_in = Some(t - dt);
            false
        }
        None => false,
    };
    if due {
        state.fidget_restore_in = None;
        state.fidget_target = 0.0;
    }

    let Some(b) = state.fidget_baseline else {
        return;
    };
    if (state.fidget_level - state.fidget_target).abs() < 1e-4 {
        if state.fidget_target <= 0.0 {
            // Fully released: drop the baseline so UI edits re-capture.
            state.fidget_baseline = None;
        }
        return;
    }
    let Some(layers) = layers else { return };
    let step = dt.as_secs_f32() / pc.director_boost_ramp_secs.max(0.05);
    state.fidget_level = if state.fidget_level < state.fidget_target {
        (state.fidget_level + step).min(state.fidget_target)
    } else {
        (state.fidget_level - step).max(state.fidget_target)
    };
    let lvl = crate::plugins::anim_layers::smoothstep(state.fidget_level);
    let f_mul = 1.0 + (pc.director_finger_boost_mul.max(1.0) - 1.0) * lvl;
    let t_mul = 1.0 + (pc.director_toe_boost_mul.max(1.0) - 1.0) * lvl;
    layers.with_write(|s| {
        for layer in s.layers.iter_mut() {
            match &mut layer.driver {
                DriverKind::FingerFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    *amplitude_deg = b.finger_amp * f_mul;
                    *curl_bias_deg = b.finger_curl + FINGER_CURL_ADD * lvl;
                }
                DriverKind::ToeFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    *amplitude_deg = b.toe_amp * t_mul;
                    *curl_bias_deg = b.toe_curl + TOE_CURL_ADD * lvl;
                }
                _ => {}
            }
        }
    });
}

/// Pick + apply a random subset of the VRM's matching expression presets
fn apply_expressive_presets(
    state: &mut DirectorState,
    sender: Option<&PoseCommandSender>,
    pc: &crate::config::PoseControllerSettings,
) {
    let Some(sender) = sender else { return };
    if state.preset_names.is_empty() || pc.director_expressive_preset_match.is_empty() {
        return;
    }
    let mut rng = rand::rng();
    let patterns: Vec<String> = pc
        .director_expressive_preset_match
        .iter()
        .map(|p| p.to_lowercase())
        .collect();
    let mut matched: Vec<String> = state
        .preset_names
        .iter()
        .filter(|name| {
            let lname = name.to_lowercase();
            patterns.iter().any(|p| !p.is_empty() && lname.contains(p))
        })
        .cloned()
        .collect();
    if matched.is_empty() {
        return;
    }

    // Zero the previous set first so overlapping beats never leak weights.
    clear_presets(state, Some(sender));

    // Pick up to 3 distinct presets.
    let n = rng.random_range(1usize..4).min(matched.len());
    let mut weights: HashMap<String, f32> = HashMap::new();
    for _ in 0..n {
        if matched.is_empty() {
            break;
        }
        let idx = rng.random_range(0usize..matched.len());
        let key = matched.swap_remove(idx);
        let w = rng.random_range(0.6f32..1.0f32);
        weights.insert(key, w);
    }
    if weights.is_empty() {
        return;
    }

    state.active_presets = weights.keys().cloned().collect();
    sender.send(PoseCommand::ApplyExpression {
        weights,
        cancel_expression_animation: false,
    });
    state.preset_clear_in = Some(Duration::from_secs_f32(rng.random_range(3.0f32..6.0f32)));
}

fn advance_preset_clear(state: &mut DirectorState, sender: Option<&PoseCommandSender>, dt: Duration) {
    let due = match state.preset_clear_in {
        Some(t) if t <= dt => true,
        Some(t) => {
            state.preset_clear_in = Some(t - dt);
            false
        }
        None => false,
    };
    if due {
        state.preset_clear_in = None;
        clear_presets(state, sender);
    }
}

fn clear_presets(state: &mut DirectorState, sender: Option<&PoseCommandSender>) {
    let keys = std::mem::take(&mut state.active_presets);
    if keys.is_empty() {
        return;
    }
    if let Some(sender) = sender {
        let weights: HashMap<String, f32> = keys.into_iter().map(|k| (k, 0.0)).collect();
        sender.send(PoseCommand::ApplyExpression {
            weights,
            cancel_expression_animation: false,
        });
    }
}

/// Revert any active boost/presets (used by the speaking guard). The boost
/// ramps back down; presets clear immediately.
fn force_restore(
    state: &mut DirectorState,
    _layers: Option<&LayerStackHandle>,
    sender: Option<&PoseCommandSender>,
) {
    state.fidget_restore_in = None;
    state.fidget_target = 0.0;
    state.preset_clear_in = None;
    clear_presets(state, sender);
}

/// Choose a non-repeating pose/animation from the library and install it as a
/// crossfading episode layer above the idle base. Returns the chosen id
/// (anim filename or pose name) to record in `recent`.
fn pick_body(
    library: &PoseLibraryAssets,
    layers: &LayerStackHandle,
    category: &str,
    recent: &VecDeque<String>,
    prefer_anim: bool,
) -> Option<String> {
    let cat_ok = |c: &str| category.is_empty() || c.eq_ignore_ascii_case(category);
    let anims = library.animations();
    let poses = library.poses();
    let filtered_anims: Vec<_> = anims.iter().filter(|a| cat_ok(&a.category)).collect();
    let filtered_poses: Vec<_> = poses.iter().filter(|p| cat_ok(&p.category)).collect();
    let fresh_anims = exclude_recent(&filtered_anims, recent, |a| &a.filename);
    let fresh_poses = exclude_recent(&filtered_poses, recent, |p| &p.name);

    let mut rng = rand::rng();
    let use_anim = prefer_anim && !fresh_anims.is_empty() && (fresh_poses.is_empty() || rng.random_bool(0.5));

    if use_anim {
        if let Some(meta) = fresh_anims.choose(&mut rng) {
            match library.library.load_animation(&meta.filename) {
                Ok(anim) => {
                    layers.with_write(|s| {
                        s.play_episode_clip(anim, meta.looping, meta.hold_duration)
                    });
                    return Some(meta.filename.clone());
                }
                Err(e) => warn!("alive director: load_animation({}) failed: {e}", meta.filename),
            }
        }
        return None;
    }

    let pose = fresh_poses.choose(&mut rng).copied()?;
    let fade = pose.transition_duration.max(0.35);
    // Stances retire on their own so the idle base always comes back.
    let hold = rng.random_range(8.0f32..20.0);
    layers.with_write(|s| s.play_pose_episode(pose.clone(), fade, Some(hold)));
    Some(pose.name.clone())
}

/// Filter out recently-played ids; if that empties the set, keep the full set.
fn exclude_recent<'a, T, F>(items: &[&'a T], recent: &VecDeque<String>, id_of: F) -> Vec<&'a T>
where
    F: Fn(&T) -> &String,
{
    let fresh: Vec<&'a T> = items
        .iter()
        .copied()
        .filter(|it| !recent.iter().any(|r| r == id_of(*it)))
        .collect();
    if fresh.is_empty() {
        items.to_vec()
    } else {
        fresh
    }
}

/// Dwell before the next episode. Attentive (engaged) = shorter; ambient = longer.
fn sample_dwell(pc: &crate::config::PoseControllerSettings, attentive: bool) -> Duration {
    let min = pc.idle_interval_min_sec.max(1.0);
    let max = pc.idle_interval_max_sec.max(min + 0.5);
    let mut rng = rand::rng();
    let mut secs = rng.random_range(min..max);
    if attentive {
        secs *= 0.7f32;
    }
    Duration::from_secs_f32(secs)
}
