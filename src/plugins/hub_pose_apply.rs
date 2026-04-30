//! Route `vrm:apply-pose` and `vrm:apply-expression` envelopes that land on
//! the channel hub directly into [`PoseCommand`]s.
//!
//! Before this bridge, those envelopes were merely broadcast to peers (and
//! most visibly consumed by Kimodo). With Kimodo's streaming lane and the
//! native animation player co-existing, we need Bevy itself to react — so
//! this plugin listens to [`WsIncomingMessage`] and funnels any matching
//! envelope through the same [`PoseCommandSender`] the MCP tools use.

use std::collections::HashMap;

use bevy::prelude::*;
use serde_json::Value;

use crate::plugins::channel_server::WsIncomingMessage;
use crate::plugins::pose_driver::{PoseCommand, PoseCommandSender};

pub struct HubPoseApplyPlugin;

impl Plugin for HubPoseApplyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, bridge_hub_envelopes_into_pose_commands);
    }
}

fn bridge_hub_envelopes_into_pose_commands(
    mut inbox: MessageReader<WsIncomingMessage>,
    sender: Option<Res<PoseCommandSender>>,
) {
    let Some(sender) = sender else {
        return;
    };
    for WsIncomingMessage { envelope } in inbox.read() {
        match envelope.message_type.as_str() {
            "vrm:apply-pose" => {
                let preserve = envelope
                    .data
                    .get("preserveOmittedBones")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let blend_weight = envelope
                    .data
                    .get("blendWeight")
                    .and_then(Value::as_f64)
                    .map(|v| v as f32);
                let transition_seconds = envelope
                    .data
                    .get("transitionSeconds")
                    .or_else(|| envelope.data.get("transitionDuration"))
                    .and_then(Value::as_f64)
                    .map(|v| v as f32);
                if let Some(bones) = parse_bone_map(&envelope.data) {
                    if !bones.is_empty() {
                        sender.send(PoseCommand::ApplyBones {
                            bones,
                            preserve_omitted_bones: preserve,
                            blend_weight,
                            transition_seconds,
                        });
                    }
                }
                if let Some(weights) = parse_expression_map(&envelope.data) {
                    if !weights.is_empty() {
                        sender.send(PoseCommand::ApplyExpression {
                            weights,
                            cancel_expression_animation: true,
                        });
                    }
                }
            }
            "vrm:apply-expression" => {
                if let Some(weights) = parse_expression_map(&envelope.data) {
                    if !weights.is_empty() {
                        sender.send(PoseCommand::ApplyExpression {
                            weights,
                            cancel_expression_animation: true,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Bones arrive either as `{"bones": {"leftHand": {"rotation": [x,y,z,w]}}}`
/// or as the flat `{"leftHand": [x,y,z,w]}` Kimodo emits while streaming.
fn parse_bone_map(data: &Value) -> Option<HashMap<String, [f32; 4]>> {
    let mut out = HashMap::new();
    if let Some(obj) = data.get("bones").and_then(Value::as_object) {
        for (name, entry) in obj {
            if let Some(arr) = entry.get("rotation").and_then(Value::as_array) {
                if let Some(q) = quat_from_array(arr) {
                    out.insert(name.clone(), q);
                }
            } else if let Some(arr) = entry.as_array() {
                if let Some(q) = quat_from_array(arr) {
                    out.insert(name.clone(), q);
                }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_expression_map(data: &Value) -> Option<HashMap<String, f32>> {
    let obj = data.get("expressions").and_then(Value::as_object)?;
    let mut out = HashMap::new();
    for (name, v) in obj {
        if let Some(w) = v.as_f64() {
            out.insert(name.clone(), w as f32);
        }
    }
    Some(out)
}

fn quat_from_array(arr: &[Value]) -> Option<[f32; 4]> {
    if arr.len() != 4 {
        return None;
    }
    let mut q = [0.0f32; 4];
    for (i, v) in arr.iter().enumerate() {
        q[i] = v.as_f64()? as f32;
    }
    Some(q)
}
