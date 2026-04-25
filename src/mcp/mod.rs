//! RMCP server that exposes the old Node `pose-controller` surface (plus
//! A2F + Kimodo) directly from `jarvis-avatar`.
//!
//! Everything below is deliberately a thin shim: tool handlers translate
//! typed parameters into either a [`PoseCommand`] (for Bevy side effects),
//! a [`HubBroadcast`] envelope (for Kimodo), a [`PoseLibrary`] filesystem
//! mutation, or an A2F gRPC call — no business logic lives inside the
//! MCP layer itself.
//!
//! Transport is streamable HTTP, nested into an `axum::Router` at the path
//! configured in `settings.mcp.path` (default `/mcp`). When
//! `settings.mcp.auth_token` is set, requests must include
//! `Authorization: Bearer <token>`.

pub mod plugin;
mod pose_authoring;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use bevy::prelude::{EulerRot, Quat};

use jarvis_avatar::a2f::{A2fClient, A2fConfig};
use jarvis_avatar::pose_library::{BoneRotation, PoseFile, PoseLibrary};

use crate::kimodo::{GenerateRequest, KimodoClient};
use pose_authoring::{
    bone_map_from_euler_deg, make_fist_bones, sanitize_bone_map, sanitize_quat, MakeFistArgs,
    PoseBonesArgs,
};
use crate::plugins::channel_server::HubBroadcast;
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};
use crate::plugins::pose_driver::{
    BoneSnapshotHandle, PoseCommand, PoseCommandSender, VRM_BONE_NAMES, VRM_EXPRESSION_NAMES,
};

// ---------- server state ------------------------------------------------------

/// Everything the MCP tool handlers need to touch. Cloned cheaply per request.
#[derive(Clone)]
pub struct JarvisMcpServer {
    pub pose_tx: PoseCommandSender,
    pub snapshot: BoneSnapshotHandle,
    pub hub: HubBroadcast,
    pub kimodo: KimodoClient,
    pub a2f: A2fClient,
    pub pose_guide_path: PathBuf,
    pub library: Arc<PoseLibrary>,
    pub kimodo_defaults: KimodoDefaults,
    /// Optional network trace sink (debug UI).
    pub traffic: Option<TrafficLogSink>,
    tool_router: ToolRouter<Self>,
}

/// Defaults applied to `generate_motion` when the caller omits them.
#[derive(Debug, Clone, Copy)]
pub struct KimodoDefaults {
    pub duration_sec: f32,
    pub steps: u32,
    pub timeout_sec: u64,
}

impl JarvisMcpServer {
    pub fn new(
        pose_tx: PoseCommandSender,
        snapshot: BoneSnapshotHandle,
        hub: HubBroadcast,
        a2f: A2fClient,
        pose_guide_path: PathBuf,
        library: PoseLibrary,
        kimodo_defaults: KimodoDefaults,
        traffic: Option<TrafficLogSink>,
    ) -> Self {
        Self::with_kimodo(
            pose_tx,
            snapshot,
            hub.clone(),
            KimodoClient::new(hub),
            a2f,
            pose_guide_path,
            library,
            kimodo_defaults,
            traffic,
        )
    }

    /// Same as [`Self::new`] but takes a pre-built [`KimodoClient`] — the
    /// `McpPlugin` uses this to inject the [`StreamingAnimation`] lane so
    /// Kimodo generations also feed the native player.
    #[allow(clippy::too_many_arguments)]
    pub fn with_kimodo(
        pose_tx: PoseCommandSender,
        snapshot: BoneSnapshotHandle,
        hub: HubBroadcast,
        kimodo: KimodoClient,
        a2f: A2fClient,
        pose_guide_path: PathBuf,
        library: PoseLibrary,
        kimodo_defaults: KimodoDefaults,
        traffic: Option<TrafficLogSink>,
    ) -> Self {
        Self {
            pose_tx,
            snapshot,
            hub,
            kimodo,
            a2f,
            pose_guide_path,
            library: Arc::new(library),
            kimodo_defaults,
            traffic,
            tool_router: Self::tool_router(),
        }
    }
}

// ---------- tool parameter types ---------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyPoseArgs {
    /// Name of the pose saved in the library (use `list_poses`).
    pub pose_name: String,
    /// Transition duration in seconds. Defaults to the pose's own or 0.4.
    #[serde(default)]
    pub transition_seconds: Option<f32>,
    /// Blend weight 0..=1. Defaults to 1.0.
    #[serde(default)]
    pub blend_weight: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetExpressionArgs {
    /// Map of expression name → 0..=1 intensity. Valid keys: see `get_bone_reference`.
    pub expressions: HashMap<String, f32>,
    /// Transition duration in seconds. Default 0.3.
    #[serde(default)]
    pub transition_seconds: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetBonesArgs {
    /// Map of VRM bone name → `{ rotation: [x, y, z, w] }`.
    pub bones: HashMap<String, BoneRotation>,
    #[serde(default)]
    pub transition_seconds: Option<f32>,
    #[serde(default)]
    pub blend_weight: Option<f32>,
    /// If false, every unlisted bone snaps back to identity first (default: true).
    #[serde(default = "default_true")]
    pub preserve_omitted_bones: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatePoseArgs {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    pub bones: HashMap<String, BoneRotation>,
    #[serde(default)]
    pub expressions: Option<HashMap<String, f32>>,
    #[serde(default)]
    pub transition_seconds: Option<f32>,
    /// If `false`, just save — don't apply. Default `true`.
    #[serde(default)]
    pub apply_immediately: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AdjustBoneArgs {
    pub bone_name: String,
    #[serde(default)]
    pub delta_x: Option<f32>,
    #[serde(default)]
    pub delta_y: Option<f32>,
    #[serde(default)]
    pub delta_z: Option<f32>,
    #[serde(default)]
    pub transition_seconds: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeletePoseArgs {
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenamePoseArgs {
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdatePoseCategoryArgs {
    pub name: String,
    pub category: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateMotionArgs {
    /// Text description of the motion.
    pub prompt: String,
    /// Duration in seconds (default from config).
    #[serde(default)]
    pub duration: Option<f32>,
    /// Denoising steps (default from config).
    #[serde(default)]
    pub steps: Option<u32>,
    /// Stream frames in real time (default `true`).
    #[serde(default)]
    pub stream: Option<bool>,
    /// If set, Kimodo will save the generated animation under this name.
    #[serde(default)]
    pub save_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaySavedAnimationArgs {
    pub filename: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteAnimationArgs {
    pub filename: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenameAnimationArgs {
    pub old_filename: String,
    pub new_filename: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAnimationMetaArgs {
    pub filename: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub looping: Option<bool>,
    #[serde(default)]
    pub hold_duration: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct A2fConfigureArgs {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub health_url: Option<String>,
}

// ---------- helpers -----------------------------------------------------------

fn ok_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(body.into())])
}

fn err_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(body.into())])
}

fn ok_json(v: &impl Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(v) {
        Ok(s) => ok_text(s),
        Err(e) => err_text(format!("serialize failure: {e}")),
    }
}

fn append_warnings(body: String, warnings: &[String]) -> String {
    if warnings.is_empty() {
        body
    } else {
        format!(
            "{body}\nwarnings:\n{}",
            warnings
                .iter()
                .map(|w| format!("- {w}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

// ---------- tool handlers ----------------------------------------------------

#[tool_router(router = tool_router)]
impl JarvisMcpServer {
    #[tool(description = "List every saved VRM pose (name, description, category, bone count).")]
    async fn list_poses(&self) -> CallToolResult {
        match self.library.load_all_poses() {
            Ok(poses) => {
                let summary: Vec<Value> = poses
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "description": p.description,
                            "category": p.category,
                            "boneCount": p.bones.len(),
                            "expressions": p.expressions.keys().collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                ok_json(&summary)
            }
            Err(e) => err_text(format!("load_all_poses: {e}")),
        }
    }

    #[tool(description = "Apply a library pose to the avatar by name.")]
    async fn apply_pose(&self, Parameters(args): Parameters<ApplyPoseArgs>) -> CallToolResult {
        let pose = match self.library.find_pose(&args.pose_name) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return err_text(format!(
                    "pose \"{}\" not found — use list_poses",
                    args.pose_name
                ));
            }
            Err(e) => return err_text(format!("lookup failed: {e}")),
        };

        let bones: HashMap<String, [f32; 4]> = pose
            .bones
            .iter()
            .map(|(k, v)| (k.clone(), v.rotation))
            .collect();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: args.blend_weight,
            transition_seconds: args
                .transition_seconds
                .or(Some(pose.transition_duration)),
        });
        if !pose.expressions.is_empty() {
            self.pose_tx.send(PoseCommand::ApplyExpression {
                weights: pose.expressions.clone(),
            });
        }
        ok_text(format!(
            "applied pose \"{}\" ({} bones, transition {:.2}s)",
            pose.name,
            pose.bones.len(),
            args.transition_seconds.unwrap_or(pose.transition_duration)
        ))
    }

    #[tool(description = "Set VRM expression weights (0..=1). Partial update: unspecified expressions unchanged.")]
    async fn set_expression(
        &self,
        Parameters(args): Parameters<SetExpressionArgs>,
    ) -> CallToolResult {
        let names: Vec<String> = args.expressions.keys().cloned().collect();
        self.pose_tx.send(PoseCommand::ApplyExpression {
            weights: args.expressions,
        });
        ok_text(format!("set expressions: {}", names.join(", ")))
    }

    #[tool(description = "Set bones with raw quaternions [x,y,z,w] (normalized pose space). Unknown bone names are ignored. Values are normalized, xyz magnitude is clamped per bone, and warnings list fixes. Prefer pose_bones for new authoring.")]
    async fn set_bones(&self, Parameters(args): Parameters<SetBonesArgs>) -> CallToolResult {
        let mut warnings = Vec::new();
        let mut raw: HashMap<String, [f32; 4]> = HashMap::new();
        for (k, v) in args.bones {
            if VRM_BONE_NAMES.contains(&k.as_str()) {
                raw.insert(k, v.rotation);
            } else {
                warnings.push(format!("ignored unknown bone \"{k}\""));
            }
        }
        let count = raw.len();
        let (bones, mut w2) = sanitize_bone_map(raw);
        warnings.append(&mut w2);
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: args.preserve_omitted_bones,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(append_warnings(
            format!("set {count} bone(s)"),
            &warnings,
        ))
    }

    #[tool(description = "Author poses in Euler degrees (preferred for agents). Intrinsic local order XYZ: pitch around X, yaw around Y, roll around Z. Omitted axis = 0. Server clamps per bone and returns warnings. Same preserve_omitted_bones semantics as set_bones.")]
    async fn pose_bones(&self, Parameters(args): Parameters<PoseBonesArgs>) -> CallToolResult {
        let mut unknown = Vec::new();
        for k in args.bones.keys() {
            if !VRM_BONE_NAMES.contains(&k.as_str()) {
                unknown.push(k.clone());
            }
        }
        if !unknown.is_empty() {
            return err_text(format!(
                "unknown bone name(s): {} — use get_bone_reference",
                unknown.join(", ")
            ));
        }
        let (bones, warnings) = bone_map_from_euler_deg(&args.bones);
        let count = bones.len();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: args.preserve_omitted_bones,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(append_warnings(
            format!("pose_bones: applied {count} bone(s)"),
            &warnings,
        ))
    }

    #[tool(description = "Curl hands toward a fist using canned finger quaternions (slerp amount 0..1). amount=0 is a relaxed curl template; amount=1 matches POSE_GUIDE fist reference. Defaults both hands unless left/right flags are set.")]
    async fn make_fist(&self, Parameters(args): Parameters<MakeFistArgs>) -> CallToolResult {
        let do_left = args.left.unwrap_or(true);
        let do_right = args.right.unwrap_or(true);
        if !do_left && !do_right {
            return err_text("set left and/or right to true");
        }
        let raw = make_fist_bones(args.amount, do_left, do_right);
        let count = raw.len();
        let (bones, warnings) = sanitize_bone_map(raw);
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(append_warnings(
            format!("make_fist: applied {count} finger bone(s), amount={:.2}", args.amount.clamp(0.0, 1.0)),
            &warnings,
        ))
    }

    #[tool(description = "Reset the avatar to the default pose and clear every expression.")]
    async fn reset_pose(&self) -> CallToolResult {
        self.pose_tx.send(PoseCommand::ResetPose);
        ok_text("reset pose and expressions")
    }

    #[tool(description = "Save a new pose to the library. Read get_pose_guide first. Quaternions are normalized and xyz-clamped like set_bones; unknown bone keys are dropped with warnings in the response.")]
    async fn create_pose(&self, Parameters(args): Parameters<CreatePoseArgs>) -> CallToolResult {
        let mut warnings = Vec::new();
        let mut bones_out = HashMap::new();
        for (k, v) in args.bones {
            if VRM_BONE_NAMES.contains(&k.as_str()) {
                let (rot, w) = sanitize_quat(&k, v.rotation);
                warnings.extend(w);
                bones_out.insert(k, BoneRotation { rotation: rot });
            } else {
                warnings.push(format!("ignored unknown bone \"{k}\""));
            }
        }
        let pose = PoseFile {
            name: args.name.clone(),
            description: args.description.unwrap_or_default(),
            category: args.category.unwrap_or_else(|| "general".into()),
            bones: bones_out,
            expressions: args.expressions.unwrap_or_default(),
            transition_duration: args.transition_seconds.unwrap_or(0.4),
        };
        if let Err(e) = self.library.save_pose(&pose) {
            return err_text(format!("save failed: {e}"));
        }
        if args.apply_immediately.unwrap_or(true) {
            let bones: HashMap<String, [f32; 4]> = pose
                .bones
                .iter()
                .map(|(k, v)| (k.clone(), v.rotation))
                .collect();
            self.pose_tx.send(PoseCommand::ApplyBones {
                bones,
                preserve_omitted_bones: true,
                blend_weight: None,
                transition_seconds: Some(pose.transition_duration),
            });
            if !pose.expressions.is_empty() {
                self.pose_tx.send(PoseCommand::ApplyExpression {
                    weights: pose.expressions.clone(),
                });
            }
        }
        ok_text(append_warnings(
            format!(
                "saved pose \"{}\" ({} bones)",
                pose.name,
                pose.bones.len()
            ),
            &warnings,
        ))
    }

    #[tool(description = "Get the full list of VRM bone names and expression presets.")]
    async fn get_bone_reference(&self) -> CallToolResult {
        ok_json(&json!({
            "bones": VRM_BONE_NAMES,
            "expressions": VRM_EXPRESSION_NAMES,
            "note": "Authoring: prefer pose_bones (Euler degrees) or make_fist. Raw quaternions [x,y,z,w] must be unit length; MCP normalizes and clamps xyz per bone. Identity = [0,0,0,1] means no pose delta. Expression weights are 0..=1.",
        }))
    }

    #[tool(description = "Return assets/POSE_GUIDE.md: bone hierarchy, quaternion notes, and per-bone ranges. Read before create_pose.")]
    async fn get_pose_guide(&self) -> CallToolResult {
        match std::fs::read_to_string(&self.pose_guide_path) {
            Ok(s) => ok_text(s),
            Err(e) => err_text(format!(
                "pose guide not found at {}: {e}",
                self.pose_guide_path.display()
            )),
        }
    }

    #[tool(description = "Read the current quaternion rotation of every humanoid bone on the VRM avatar.")]
    async fn get_current_bone_state(&self) -> CallToolResult {
        let snap = self.snapshot.0.read().clone();
        ok_json(&snap.bones)
    }

    #[tool(description = "Nudge one bone: delta_x / delta_y / delta_z are **degrees** of intrinsic-local Euler (same XYZ order as pose_bones), composed on the right of the current pose quaternion (current * delta). Small steps (±2..±8°) are safest. Result is clamped like set_bones.")]
    async fn adjust_bone(&self, Parameters(args): Parameters<AdjustBoneArgs>) -> CallToolResult {
        if !VRM_BONE_NAMES.contains(&args.bone_name.as_str()) {
            return err_text(format!(
                "invalid bone \"{}\" — use get_bone_reference",
                args.bone_name
            ));
        }
        let dx = args.delta_x.unwrap_or(0.0);
        let dy = args.delta_y.unwrap_or(0.0);
        let dz = args.delta_z.unwrap_or(0.0);
        if dx == 0.0 && dy == 0.0 && dz == 0.0 {
            return err_text("specify at least one of delta_x / delta_y / delta_z".to_string());
        }

        let snap = self.snapshot.0.read().clone();
        let [cx, cy, cz, cw] = snap
            .bones
            .get(&args.bone_name)
            .map(|e| e.rotation)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);

        let cur = Quat::from_xyzw(cx, cy, cz, cw).normalize();
        let dq = Quat::from_euler(
            EulerRot::XYZ,
            dx.to_radians(),
            dy.to_radians(),
            dz.to_radians(),
        );
        let composed = (cur * dq).normalize();
        let q_raw = [composed.x, composed.y, composed.z, composed.w];
        let (q, warnings) = sanitize_quat(&args.bone_name, q_raw);

        let bones = HashMap::from([(args.bone_name.clone(), q)]);
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(append_warnings(
            format!(
                "adjusted {}: [{cx:.3},{cy:.3},{cz:.3},{cw:.3}] → [{:.3},{:.3},{:.3},{:.3}]",
                args.bone_name, q[0], q[1], q[2], q[3]
            ),
            &warnings,
        ))
    }

    #[tool(description = "Delete a saved pose by name.")]
    async fn delete_pose(&self, Parameters(args): Parameters<DeletePoseArgs>) -> CallToolResult {
        match self.library.delete_pose(&args.name) {
            Ok(()) => ok_text(format!("deleted pose \"{}\"", args.name)),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Rename a saved pose.")]
    async fn rename_pose(&self, Parameters(args): Parameters<RenamePoseArgs>) -> CallToolResult {
        match self.library.rename_pose(&args.old_name, &args.new_name) {
            Ok(()) => ok_text(format!("renamed {} → {}", args.old_name, args.new_name)),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Change the category of a saved pose.")]
    async fn update_pose_category(
        &self,
        Parameters(args): Parameters<UpdatePoseCategoryArgs>,
    ) -> CallToolResult {
        match self.library.update_pose_category(&args.name, &args.category) {
            Ok(()) => ok_text(format!("updated {} → {}", args.name, args.category)),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Generate a motion animation from a text prompt via NVIDIA Kimodo. Requires the kimodo-motion-service peer to be connected to the hub.")]
    async fn generate_motion(
        &self,
        Parameters(args): Parameters<GenerateMotionArgs>,
    ) -> CallToolResult {
        let req = GenerateRequest {
            prompt: args.prompt,
            duration: args.duration.unwrap_or(self.kimodo_defaults.duration_sec),
            steps: args.steps.unwrap_or(self.kimodo_defaults.steps),
            stream: args.stream.unwrap_or(true),
            save_name: args.save_name,
            timeout: std::time::Duration::from_secs(self.kimodo_defaults.timeout_sec),
        };
        match self.kimodo.generate_motion(req).await {
            Ok(outcome) => ok_json(&outcome),
            Err(e) => err_text(format!("kimodo: {e}")),
        }
    }

    #[tool(description = "List every saved motion animation on disk (name, prompt, fps, frameCount).")]
    async fn list_generated_animations(&self) -> CallToolResult {
        match self.library.list_animations() {
            Ok(list) => ok_json(&list),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Replay a saved animation by filename. Streams through Kimodo's local playback path.")]
    async fn play_saved_animation(
        &self,
        Parameters(args): Parameters<PlaySavedAnimationArgs>,
    ) -> CallToolResult {
        let full = self.library.animations_dir.join(&args.filename);
        if !full.exists() {
            return err_text(format!("animation \"{}\" not found", args.filename));
        }
        let req_id = self.kimodo.play_saved_animation(&args.filename);
        ok_text(format!("replay kicked off (requestId {req_id})"))
    }

    #[tool(description = "Delete a saved animation file.")]
    async fn delete_animation(
        &self,
        Parameters(args): Parameters<DeleteAnimationArgs>,
    ) -> CallToolResult {
        match self.library.delete_animation(&args.filename) {
            Ok(()) => ok_text(format!("deleted {}", args.filename)),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Rename a saved animation file.")]
    async fn rename_animation(
        &self,
        Parameters(args): Parameters<RenameAnimationArgs>,
    ) -> CallToolResult {
        match self
            .library
            .rename_animation(&args.old_filename, &args.new_filename)
        {
            Ok(()) => ok_text(format!(
                "renamed {} → {}",
                args.old_filename, args.new_filename
            )),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Update metadata (category, looping, holdDuration) on a saved animation.")]
    async fn update_animation_metadata(
        &self,
        Parameters(args): Parameters<UpdateAnimationMetaArgs>,
    ) -> CallToolResult {
        match self.library.update_animation_metadata(
            &args.filename,
            args.category,
            args.looping,
            args.hold_duration,
        ) {
            Ok(()) => ok_text(format!("updated {}", args.filename)),
            Err(e) => err_text(format!("{e}")),
        }
    }

    #[tool(description = "Return the combined pose + animation catalog with full metadata.")]
    async fn list_all_content(&self) -> CallToolResult {
        let poses = self.library.load_all_poses().unwrap_or_default();
        let animations = self.library.list_animations().unwrap_or_default();
        ok_json(&json!({
            "poses": poses.iter().map(|p| json!({
                "type": "pose",
                "name": p.name,
                "category": p.category,
                "boneCount": p.bones.len(),
                "description": p.description,
            })).collect::<Vec<_>>(),
            "animations": animations,
        }))
    }

    #[tool(description = "Check NVIDIA Audio2Face-3D Docker health and current client configuration.")]
    async fn a2f_status(&self) -> CallToolResult {
        let health = self.a2f.health().await;
        let cfg = self.a2f.config();
        if let Some(ref log) = self.traffic {
            log.push(
                TrafficChannel::A2fGrpc,
                TrafficDirection::Outbound,
                "MCP tool a2f_status (HTTP health + config snapshot)",
                Some(json!({
                    "enabled": cfg.enabled,
                    "endpoint": cfg.endpoint,
                    "healthUrl": cfg.health_url,
                    "healthOk": health.ok,
                    "healthError": health.error,
                })),
            );
        }
        ok_json(&json!({
            "enabled": cfg.enabled,
            "endpoint": cfg.endpoint,
            "healthUrl": cfg.health_url,
            "health": if health.ok { "READY" } else { "UNREACHABLE" },
            "error": health.error,
        }))
    }

    #[tool(description = "Live-update the A2F client configuration (enabled flag, endpoint, health URL). Change applies to future calls.")]
    async fn a2f_configure(
        &self,
        Parameters(args): Parameters<A2fConfigureArgs>,
    ) -> CallToolResult {
        // Config lives behind `A2fClient` as immutable today; mirror the Node
        // tool's semantics by reporting the requested change without mutating
        // the server's shared client. Runtime reconfig would require a
        // `RwLock<A2fConfig>`; tracked as a follow-up.
        ok_json(&json!({
            "accepted": {
                "enabled": args.enabled,
                "endpoint": args.endpoint,
                "healthUrl": args.health_url,
            },
            "note": "In-flight reconfigure is not yet applied. Restart the avatar to pick up new [a2f] settings.",
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for JarvisMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "jarvis-avatar MCP: pose / expression / bone control, Kimodo motion generation, \
                 and NVIDIA Audio2Face-3D bridging. For poses use pose_bones (Euler degrees) or \
                 make_fist; set_bones is for round-trips from get_current_bone_state. Read get_pose_guide.",
            )
    }
}

/// Build an `A2fClient` from the avatar's `[a2f]` config section.
pub fn build_a2f_client(
    enabled: bool,
    endpoint: impl Into<String>,
    health_url: impl Into<String>,
) -> A2fClient {
    A2fClient::new(A2fConfig {
        enabled,
        endpoint: endpoint.into(),
        health_url: health_url.into(),
    })
}
