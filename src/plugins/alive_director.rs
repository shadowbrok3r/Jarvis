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

use jarvis_avatar::config::Settings;

use crate::plugins::anim_layers::{DriverKind, LayerStackHandle};
use crate::plugins::channel_server::LookAtRequestMessage;
use crate::plugins::look_at::LookAtRuntime;
use crate::plugins::native_anim_player::ActiveNativeAnimation;
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};
use crate::plugins::pose_library_assets::PoseLibraryAssets;
use crate::plugins::tts::TtsClip;

/// Recent picks excluded from the next selection (LRU fallback for small libs).
const RECENT_WINDOW: usize = 4;
/// Deliberate camera check-in at least every N picks.
const CHECKIN_EVERY: u32 = 4;
/// How much the expressive beat amplifies finger fidget amplitude / adds curl.
const FINGER_AMP_MUL: f32 = 1.9;
const FINGER_CURL_ADD: f32 = 16.0;
/// Toes get pushed harder — the owner wants pronounced toe articulation.
const TOE_AMP_MUL: f32 = 2.2;
const TOE_CURL_ADD: f32 = 22.0;

pub struct AliveDirectorPlugin;

impl Plugin for AliveDirectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DirectorState>()
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
struct DirectorState {
    next_pick_in: Option<Duration>,
    elapsed: Duration,
    recent: VecDeque<String>,
    /// Deferred gaze actions (look-away -> return).
    beats: Vec<Beat>,
    picks_since_checkin: u32,
    base_ensured: bool,
    /// Expressive finger/toe boost: saved baseline + countdown to restore.
    fidget_baseline: Option<FidgetBaseline>,
    fidget_restore_in: Option<Duration>,
    /// Director-applied expression presets + countdown to clear them.
    active_presets: Vec<String>,
    preset_clear_in: Option<Duration>,
    /// Cached VRM expression preset names (discovered once the rig is ready).
    preset_names: Vec<String>,
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
    mut active_anim: ResMut<ActiveNativeAnimation>,
    mut gaze: MessageWriter<LookAtRequestMessage>,
    mut state: ResMut<DirectorState>,
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

    // Advance the expressive-boost / preset-clear countdowns every tick so they
    // unwind regardless of dwell timing.
    advance_fidget_restore(&mut state, layers.as_deref(), dt);
    advance_preset_clear(&mut state, sender.as_deref(), dt);

    // SPEAKING GUARD: while she's actually talking to the owner, let chat-driven
    // gestures own her — clean up any director boosts/presets and stand down.
    if !speaking.is_empty() {
        force_restore(&mut state, layers.as_deref(), sender.as_deref());
        state.next_pick_in = None;
        state.elapsed = Duration::ZERO;
        return;
    }

    let attentive = look_runtime.gaze_active();

    if state.next_pick_in.is_none() {
        state.next_pick_in = Some(sample_dwell(pc, attentive));
        state.elapsed = Duration::ZERO;
    }
    state.elapsed += dt;
    let Some(target) = state.next_pick_in else {
        return;
    };
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
    let p_expressive = if attentive { 0.5 } else { 0.35 };
    let p_gaze = if attentive { 0.10 } else { 0.25 };
    let is_expressive = r < p_expressive;
    let is_gaze = !is_expressive && r < p_expressive + p_gaze;

    if is_expressive {
        // Bring the alive factor + drive the helper presets, and present
        // toward the viewer.
        boost_fidget(&mut state, layers.as_deref());
        apply_expressive_presets(&mut state, sender.as_deref(), pc);
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

    // Body pose/anim on expressive + plain episodes (a pure glance shouldn't
    // also re-pose her).
    if !is_gaze {
        if let (Some(library), Some(sender)) = (library.as_ref(), sender.as_ref()) {
            let prefer_anim = !active_anim.is_playing();
            if let Some(id) = pick_body(
                library,
                sender,
                &mut active_anim,
                pc.idle_category.trim(),
                &state.recent,
                prefer_anim,
            ) {
                state.remember(id);
            }
        }
    }

    // Periodic deliberate check-in so she keeps meeting your eyes.
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

/// Amplify the finger- and toe-fidget driver layers for an expressive beat,
/// saving the baseline so [`advance_fidget_restore`] can revert it.
fn boost_fidget(state: &mut DirectorState, layers: Option<&LayerStackHandle>) {
    let Some(layers) = layers else { return };
    let need_capture = state.fidget_baseline.is_none();
    let captured = layers.with_write(|s| {
        let mut finger_amp = 0.0;
        let mut finger_curl = 0.0;
        let mut toe_amp = 0.0;
        let mut toe_curl = 0.0;
        for layer in s.layers.iter_mut() {
            match &mut layer.driver {
                DriverKind::FingerFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    if need_capture {
                        finger_amp = *amplitude_deg;
                        finger_curl = *curl_bias_deg;
                    }
                    let base_a = if need_capture { *amplitude_deg } else { finger_amp };
                    let base_c = if need_capture { *curl_bias_deg } else { finger_curl };
                    *amplitude_deg = base_a * FINGER_AMP_MUL;
                    *curl_bias_deg = base_c + FINGER_CURL_ADD;
                }
                DriverKind::ToeFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    if need_capture {
                        toe_amp = *amplitude_deg;
                        toe_curl = *curl_bias_deg;
                    }
                    let base_a = if need_capture { *amplitude_deg } else { toe_amp };
                    let base_c = if need_capture { *curl_bias_deg } else { toe_curl };
                    *amplitude_deg = base_a * TOE_AMP_MUL;
                    *curl_bias_deg = base_c + TOE_CURL_ADD;
                }
                _ => {}
            }
        }
        FidgetBaseline {
            finger_amp,
            finger_curl,
            toe_amp,
            toe_curl,
        }
    });
    if need_capture {
        state.fidget_baseline = Some(captured);
    }
    // Hold the boost a few seconds, then unwind.
    let mut rng = rand::rng();
    state.fidget_restore_in = Some(Duration::from_secs_f32(rng.random_range(3.0f32..6.0f32)));
}

fn advance_fidget_restore(state: &mut DirectorState, layers: Option<&LayerStackHandle>, dt: Duration) {
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
        restore_fidget(state, layers);
    }
}

fn restore_fidget(state: &mut DirectorState, layers: Option<&LayerStackHandle>) {
    let Some(b) = state.fidget_baseline.take() else {
        return;
    };
    let Some(layers) = layers else { return };
    layers.with_write(|s| {
        for layer in s.layers.iter_mut() {
            match &mut layer.driver {
                DriverKind::FingerFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    *amplitude_deg = b.finger_amp;
                    *curl_bias_deg = b.finger_curl;
                }
                DriverKind::ToeFidget {
                    amplitude_deg,
                    curl_bias_deg,
                    ..
                } => {
                    *amplitude_deg = b.toe_amp;
                    *curl_bias_deg = b.toe_curl;
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
    pc: &jarvis_avatar::config::PoseControllerSettings,
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

/// Immediately revert any active boost/presets (used by the speaking guard).
fn force_restore(
    state: &mut DirectorState,
    layers: Option<&LayerStackHandle>,
    sender: Option<&PoseCommandSender>,
) {
    state.fidget_restore_in = None;
    restore_fidget(state, layers);
    state.preset_clear_in = None;
    clear_presets(state, sender);
}

/// Choose a non-repeating pose/animation from the library and apply it.
/// Returns the chosen id (anim filename or pose name) to record in `recent`.
fn pick_body(
    library: &PoseLibraryAssets,
    sender: &PoseCommandSender,
    active_anim: &mut ActiveNativeAnimation,
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
                    active_anim.start(anim, meta.looping, meta.hold_duration);
                    return Some(meta.filename.clone());
                }
                Err(e) => warn!("alive director: load_animation({}) failed: {e}", meta.filename),
            }
        }
        return None;
    }

    let pose = fresh_poses.choose(&mut rng)?;
    let bones = pose
        .bones
        .iter()
        .map(|(k, v)| (k.clone(), v.rotation))
        .collect();
    sender.send(PoseCommand::ApplyBones {
        bones,
        preserve_omitted_bones: true,
        blend_weight: None,
        transition_seconds: Some(pose.transition_duration.max(0.2f32)),
    });
    if !pose.expressions.is_empty() {
        sender.send(PoseCommand::ApplyExpression {
            weights: pose.expressions.clone(),
            cancel_expression_animation: false,
        });
    }
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
fn sample_dwell(pc: &jarvis_avatar::config::PoseControllerSettings, attentive: bool) -> Duration {
    let min = pc.idle_interval_min_sec.max(1.0);
    let max = pc.idle_interval_max_sec.max(min + 0.5);
    let mut rng = rand::rng();
    let mut secs = rng.random_range(min..max);
    if attentive {
        secs *= 0.7f32;
    }
    Duration::from_secs_f32(secs)
}
