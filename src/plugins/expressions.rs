//! ACT token → avatar action dispatcher.
//!
//! Reads every [`ChatCompleteMessage`] the gateway plugin publishes,
//! parses out ACT tokens (bracket + pipe syntax), and applies:
//!
//! * the matching VRM **expression preset(s)** via [`PoseCommand::SetExpression`]
//!   (same queue as MCP / Pose Controller so `world.flush()` runs before
//!   `bind_expressions`), using [`EmotionBinding::merged_expression_weights`], and
//! * the matching **animation clip** from the pose library (via
//!   `ActiveNativeAnimation`).
//!
//! Which action fires for each emotion is controlled by [`EmotionMap`] —
//! users can edit that table from the Emotion Mappings debug window. An
//! emotion with no binding falls back to the legacy [`Emotion`] enum so
//! old content (e.g. `happy`, `sad`) still animates the face even before
//! the user customises anything.
//!
//! After `hold_seconds` elapses the face decays back to `neutral`, same
//! as before.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::prelude::*;

use jarvis_avatar::act::{Emotion, emotion_from_act_json, emotion_labels};
use jarvis_avatar::emotions::EmotionBinding;
use jarvis_avatar::pose_library::AnimationFile;

use super::channel_server::ChatCompleteMessage;
use super::chat_pipeline_status::{ChatPipelineStage, ChatPipelineStatus};
use super::emotion_map::EmotionMapRes;
use super::native_anim_player::ActiveNativeAnimation;
use super::pose_driver::{PoseCommand, PoseCommandSender};
use super::pose_library_assets::PoseLibraryAssets;

pub struct ExpressionsPlugin;

impl Plugin for ExpressionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExpressionState>().add_systems(
            Update,
            (apply_chat_expressions, decay_expression_to_neutral),
        );
    }
}

#[derive(Resource)]
struct ExpressionState {
    active_until: Option<Instant>,
    default_hold: Duration,
}

impl Default for ExpressionState {
    fn default() -> Self {
        Self {
            active_until: None,
            default_hold: Duration::from_secs_f32(2.5),
        }
    }
}

fn apply_chat_expressions(
    mut chat: MessageReader<ChatCompleteMessage>,
    pose_tx: Option<Res<PoseCommandSender>>,
    mut state: ResMut<ExpressionState>,
    emotion_map: Option<Res<EmotionMapRes>>,
    pose_lib: Option<Res<PoseLibraryAssets>>,
    mut active_anim: Option<ResMut<ActiveNativeAnimation>>,
    mut pipeline: ResMut<ChatPipelineStatus>,
) {
    for msg in chat.read() {
        let labels = emotion_labels(&msg.content);
        let Some(label) = labels.into_iter().next() else {
            continue;
        };

        pipeline.set(
            ChatPipelineStage::ApplyingActToVrm,
            format!("emotion `{label}`"),
        );

        // Resolve emotion → binding (either the user's EmotionMap entry
        // or a synthesised one derived from the legacy `Emotion` enum).
        let binding: EmotionBinding = resolve_binding(&label, emotion_map.as_deref());

        // -------- Expression -------------------------------------------------
        if binding.drives_expressions() {
            if let Some(tx) = pose_tx.as_ref() {
                let merged = binding.merged_expression_weights();
                let weights: HashMap<String, f32> = merged
                    .into_iter()
                    .map(|(k, w)| (k, w.clamp(0.0, 1.0)))
                    .collect();
                tx.send(PoseCommand::SetExpression {
                    weights: weights.clone(),
                });
                let hold = if binding.hold_seconds > 0.0 {
                    Duration::from_secs_f32(binding.hold_seconds)
                } else {
                    state.default_hold
                };
                state.active_until = Some(Instant::now() + hold);
                let preview: Vec<String> = weights
                    .iter()
                    .map(|(k, w)| format!("{k}@{w:.2}"))
                    .collect();
                info!(
                    "emotion '{label}' → VRM expressions [{}] for {:.1}s",
                    preview.join(", "),
                    hold.as_secs_f32()
                );
            } else {
                warn!("emotion '{label}': PoseCommandSender missing — face not driven");
            }
        }

        // -------- Animation --------------------------------------------------
        let (Some(filename), Some(lib), Some(active)) = (
            binding.animation.as_deref(),
            pose_lib.as_deref(),
            active_anim.as_deref_mut(),
        ) else {
            continue;
        };
        match lib.library.load_animation(filename) {
            Ok(animation) => {
                let (looping, hold) = animation_playback_params(&animation, &binding);
                info!(
                    "emotion '{label}' → animation '{}' ({} frames, looping={looping})",
                    animation.name,
                    animation.frames.len()
                );
                active.start(animation, looping, hold);
            }
            Err(e) => {
                warn!("emotion '{label}' animation '{filename}' failed to load: {e}");
            }
        }
    }
}

fn animation_playback_params(anim: &AnimationFile, binding: &EmotionBinding) -> (bool, f32) {
    let looping = binding.looping.or(anim.looping).unwrap_or(false);
    let hold = anim.hold_duration.unwrap_or(0.5);
    (looping, hold)
}

fn resolve_binding(label: &str, map: Option<&EmotionMapRes>) -> EmotionBinding {
    if let Some(m) = map {
        if let Some(binding) = m.inner.resolve(label) {
            return binding.clone();
        }
    }
    // Fallback: the label might still match the legacy `Emotion` enum, in
    // which case we synthesise a binding that only drives the face so
    // existing deployments without an emotions.json keep working.
    let legacy_json = format!("{{\"emotion\":\"{label}\"}}");
    if let Some(em) = emotion_from_act_json(&legacy_json) {
        return EmotionBinding {
            expression: Some(em.vrm_expression_name().to_string()),
            expression_weight: 1.0,
            hold_seconds: 2.5,
            ..Default::default()
        };
    }
    // Unknown label — no-op. Caller skips both branches.
    EmotionBinding::default()
}

fn decay_expression_to_neutral(
    pose_tx: Option<Res<PoseCommandSender>>,
    mut state: ResMut<ExpressionState>,
) {
    let Some(until) = state.active_until else {
        return;
    };
    if Instant::now() < until {
        return;
    }
    if let Some(tx) = pose_tx.as_ref() {
        let mut weights = HashMap::new();
        weights.insert(Emotion::Neutral.vrm_expression_name().to_string(), 1.0);
        tx.send(PoseCommand::SetExpression { weights });
    }
    state.active_until = None;
}
