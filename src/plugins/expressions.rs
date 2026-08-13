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
use std::sync::Arc;

use bevy::prelude::*;
use parking_lot::RwLock;

use crate::act::{emotion_from_act_json, emotion_labels};
use crate::emotions::{a2f_emotion_hints, EmotionBinding};
use crate::pose_library::AnimationFile;

use super::anim_layers::LayerStackHandle;
use super::channel_server::ChatCompleteMessage;
use super::chat_pipeline_status::{ChatPipelineStage, ChatPipelineStatus};
use super::emotion_map::EmotionMapRes;
use super::pose_driver::{PoseCommand, PoseCommandSender};
use super::pose_library_assets::PoseLibraryAssets;

pub struct ExpressionsPlugin;

impl Plugin for ExpressionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CurrentEmotionHint>()
            .add_systems(Update, apply_chat_expressions);
    }
}

/// Latest ACT emotion mapped to A2F emotion keys — snapshotted by the TTS
/// dispatcher so speech carries the current mood instead of a fixed bias.
#[derive(Resource, Clone, Default)]
pub struct CurrentEmotionHint(pub Arc<RwLock<HashMap<String, f32>>>);

fn apply_chat_expressions(
    mut chat: MessageReader<ChatCompleteMessage>,
    pose_tx: Option<Res<PoseCommandSender>>,
    emotion_hint: Res<CurrentEmotionHint>,
    emotion_map: Option<Res<EmotionMapRes>>,
    pose_lib: Option<Res<PoseLibraryAssets>>,
    layer_stack: Option<Res<LayerStackHandle>>,
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
                // A2F hint tracks the current mood for subsequent speech.
                *emotion_hint.0.write() = a2f_emotion_hints(&weights);
                let hold = if binding.hold_seconds > 0.0 {
                    binding.hold_seconds
                } else {
                    2.5
                };
                tx.send(PoseCommand::EmotionEnvelope {
                    weights: weights.clone(),
                    hold_seconds: Some(hold),
                });
                let preview: Vec<String> = weights
                    .iter()
                    .map(|(k, w)| format!("{k}@{w:.2}"))
                    .collect();
                info!(
                    "emotion '{label}' → VRM expressions [{}] for {hold:.1}s (enveloped)",
                    preview.join(", "),
                );
            } else {
                warn!("emotion '{label}': PoseCommandSender missing — face not driven");
            }
        }

        // -------- Animation --------------------------------------------------
        let (Some(filename), Some(lib), Some(layers)) = (
            binding.animation.as_deref(),
            pose_lib.as_deref(),
            layer_stack.as_deref(),
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
                layers.with_write(|s| s.play_episode_clip(animation, looping, hold));
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

