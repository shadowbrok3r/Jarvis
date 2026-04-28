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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crossbeam_channel::RecvTimeoutError;

use jarvis_avatar::a2f::{A2fClient, A2fConfig};
use jarvis_avatar::model_catalog::{list_vrm_models, resolve_vrm_load_argument};
use jarvis_avatar::paths::expand_home;
use jarvis_avatar::pose_library::{slugify, BoneRotation, PoseFile, PoseLibrary};

use crate::kimodo::{GenerateRequest, KimodoClient};
use crate::plugins::channel_server::HubBroadcast;
use crate::plugins::pose_capture::{
    CaptureCommandSender, CaptureRequest, CaptureView, CaptureFramingPreset,
};
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};
use crate::plugins::pose_driver::{
    BoneSnapshot, BoneSnapshotHandle, PoseCommand, PoseCommandSender, VRM_BONE_NAMES,
    VRM_EXPRESSION_NAMES,
};

use pose_authoring::{bone_map_from_euler_deg, make_fist_bones, sanitize_bone_map, MakeFistArgs, PoseBonesArgs};

// ---------- server state ------------------------------------------------------

/// Everything the MCP tool handlers need to touch. Cloned cheaply per request.
#[derive(Clone)]
pub struct JarvisMcpServer {
    pub pose_tx: PoseCommandSender,
    pub capture_tx: CaptureCommandSender,
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
        capture_tx: CaptureCommandSender,
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
            capture_tx,
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
        capture_tx: CaptureCommandSender,
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
            capture_tx,
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
pub struct ExpressionKeyframeArg {
    /// Time in seconds from clip start (must be non-decreasing after sort).
    pub time_s: f32,
    /// Expression preset → weight 0..=1 at this keyframe.
    pub weights: HashMap<String, f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnimateExpressionsArgs {
    /// At least one keyframe. Sampling is piecewise-linear between keyframes; after the last keyframe time, weights hold until `duration_seconds`.
    pub keyframes: Vec<ExpressionKeyframeArg>,
    /// Total clip length in seconds. If omitted, uses the largest `time_s` in keyframes (minimum 0.05s).
    #[serde(default)]
    pub duration_seconds: Option<f32>,
    /// When true, time wraps with `duration_seconds` as the period.
    #[serde(default)]
    pub looping: Option<bool>,
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
    /// Optional timeout override for this request (seconds). If omitted,
    /// `[mcp].kimodo_timeout_sec` is used.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListModelsArgs {
    /// Optional case-insensitive substring filter on the `.vrm` basename (e.g. `helen`).
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadVrmArgs {
    /// `models/name.vrm` (under `assets/`) or basename only (`name.vrm` in `assets/models/`).
    pub path: String,
}

fn default_capture_dim() -> u32 {
    1024
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CapturePoseViewsArgs {
    /// Output directory for PNGs (leading `~/` is expanded like jarvis-avatar config).
    pub output_dir: String,
    /// Prefix in filenames: `<capture_id>_<view>_<WxH>.png`.
    pub capture_id: String,
    #[serde(default = "default_capture_dim")]
    pub width: u32,
    #[serde(default = "default_capture_dim")]
    pub height: u32,
    /// View slugs: `front`, `left`, `right`, `front_left`, `front_right`.
    pub views: Vec<String>,
    /// Optional: `full_body` or `face_closeup` (camera distance / head focus).
    #[serde(default)]
    pub framing_preset: Option<String>,
    /// Bevy capture pipeline timeout in seconds (default 180, min 5, max 600).
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

fn parse_capture_view_slug(raw: &str) -> Result<CaptureView, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("empty view name".to_string());
    }
    let low = s.to_ascii_lowercase();
    match low.as_str() {
        "front" => Ok(CaptureView::Front),
        "left" => Ok(CaptureView::Left),
        "right" => Ok(CaptureView::Right),
        "front_left" | "frontleft" | "front-left" => Ok(CaptureView::FrontLeft),
        "front_right" | "frontright" | "front-right" => Ok(CaptureView::FrontRight),
        _ => Err(format!(
            "unknown view {s:?} — use front, left, right, front_left, front_right"
        )),
    }
}

// ---------- helpers -----------------------------------------------------------

fn ok_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(body.into())])
}

fn err_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(body.into())])
}

/// MCP pose tools accept VRM humanoid keys plus any bone currently in the live snapshot
/// (extra skin joints indexed by glTF [`Name`]) and, before the first snapshot, `DEF-toe*`
/// / `DEF-ero*` prefixes (ASCII case-insensitive).
fn mcp_allows_pose_bone_key(name: &str, snap: &BoneSnapshot) -> bool {
    if VRM_BONE_NAMES.contains(&name) {
        return true;
    }
    if snap.bones.contains_key(name) {
        return true;
    }
    let n = name.to_ascii_lowercase();
    n.starts_with("def-toe") || n.starts_with("def-ero")
}

fn ok_json(v: &impl Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(v) {
        Ok(s) => ok_text(s),
        Err(e) => err_text(format!("serialize failure: {e}")),
    }
}

// ---------- tool handlers ----------------------------------------------------

#[tool_router(router = tool_router)]
impl JarvisMcpServer {
    #[tool(description = "List `.vrm` files under assets/models (sorted basenames + asset paths like models/foo.vrm for load_vrm). Optional filter: case-insensitive substring on basename. Read-only; cwd must be the crate root so assets/models resolves.")]
    async fn list_models(
        &self,
        Parameters(args): Parameters<ListModelsArgs>,
    ) -> CallToolResult {
        match list_vrm_models(args.filter.as_deref()) {
            Ok(entries) => ok_json(&json!({
                "modelsDir": jarvis_avatar::model_catalog::models_dir().display().to_string(),
                "count": entries.len(),
                "models": entries,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Hot-swap the displayed VRM at runtime (no app restart). Path: models/name.vrm or basename name.vrm under assets/models. Clears bone snapshot / transitions / expression animation state; updates Settings.model_path; respawns idle VRMA from [avatar].idle_vrma_path when set. Spring/collider presets reload if auto_load_spring_preset is true when the new rig initializes.")]
    async fn load_vrm(&self, Parameters(args): Parameters<LoadVrmArgs>) -> CallToolResult {
        match resolve_vrm_load_argument(&args.path) {
            Ok(asset_path) => {
                self.pose_tx.send(PoseCommand::LoadVrm {
                    asset_path: asset_path.clone(),
                });
                ok_json(&json!({
                    "queued": true,
                    "assetPath": asset_path,
                    "note": "pose_bones / expressions may no-op until the new rig is indexed; Kimodo playback may still target the prior skeleton until you reset.",
                }))
            }
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "List every saved VRM pose (name, description, category, bone count). Use before apply_pose when you need a known baseline for motion or capture.")]
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
                cancel_expression_animation: true,
            });
        }
        ok_text(format!(
            "applied pose \"{}\" ({} bones, transition {:.2}s)",
            pose.name,
            pose.bones.len(),
            args.transition_seconds.unwrap_or(pose.transition_duration)
        ))
    }

    #[tool(description = "Set VRM expression blendshape weights (0..=1). Partial map: omitted keys stay as-is. Layer facial polish on top of Kimodo or pose_bones; combine small weights (e.g. happy+relaxed) for natural faces — see get_pose_guide table.")]
    async fn set_expression(
        &self,
        Parameters(args): Parameters<SetExpressionArgs>,
    ) -> CallToolResult {
        let names: Vec<String> = args.expressions.keys().cloned().collect();
        self.pose_tx.send(PoseCommand::ApplyExpression {
            weights: args.expressions,
            cancel_expression_animation: true,
        });
        ok_text(format!("set expressions: {}", names.join(", ")))
    }

    #[tool(description = "Play a short in-engine VRM expression curve (piecewise-linear keyframes). Stops idle VRMA like other manual pose commands. Omitted expression keys in a keyframe default to 0 when lerping into keys that list them. Cancels on reset_pose / set_expression / apply_pose with expressions. Layered in-app expression drivers (blink, etc.) still run first each frame; animated channels override last. After one-shot playback, last sampled weights remain until changed. Verify with capture_pose_views + framing_preset face_closeup.")]
    async fn animate_expressions(
        &self,
        Parameters(args): Parameters<AnimateExpressionsArgs>,
    ) -> CallToolResult {
        const MAX_KEYFRAMES: usize = 256;
        if args.keyframes.is_empty() {
            return err_text("keyframes must contain at least one entry".to_string());
        }
        if args.keyframes.len() > MAX_KEYFRAMES {
            return err_text(format!(
                "too many keyframes (max {MAX_KEYFRAMES})"
            ));
        }
        let mut frames: Vec<(f32, HashMap<String, f32>)> = args
            .keyframes
            .into_iter()
            .map(|k| {
                let w: HashMap<String, f32> = k
                    .weights
                    .into_iter()
                    .map(|(n, v)| (n, v.clamp(0.0, 1.0)))
                    .collect();
                (k.time_s, w)
            })
            .collect();
        frames.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let max_t = frames
            .iter()
            .map(|(t, _)| *t)
            .fold(0.0f32, f32::max);
        let mut duration = args.duration_seconds.unwrap_or(max_t).max(0.05).min(120.0);
        if duration + 1e-4 < max_t {
            duration = max_t.max(0.05);
        }
        let looping = args.looping.unwrap_or(false);
        let kf_count = frames.len();
        self.pose_tx.send(PoseCommand::AnimateExpressions {
            keyframes: frames,
            duration_seconds: duration,
            looping,
        });
        ok_json(&json!({
            "started": true,
            "durationSeconds": duration,
            "looping": looping,
            "keyframeCount": kf_count,
            "note": "Sampling runs in Bevy PostUpdate; capture_pose_views after wall-clock sleep >= duration for one-shot verification.",
        }))
    }

    #[tool(description = "Directly set bone rotations as quaternions [x, y, z, w]. Keep components in [-0.3, 0.3] for natural motion.")]
    async fn set_bones(&self, Parameters(args): Parameters<SetBonesArgs>) -> CallToolResult {
        let snap = self.snapshot.0.read();
        for bone in args.bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "invalid bone \"{bone}\" — use get_bone_reference (humanoid + extraBones)"
                ));
            }
        }
        drop(snap);
        let count = args.bones.len();
        let bones: HashMap<String, [f32; 4]> = args
            .bones
            .into_iter()
            .map(|(k, v)| (k, v.rotation))
            .collect();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: args.preserve_omitted_bones,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(format!("set {count} bone(s)"))
    }

    #[tool(description = "Set many bones at once using intrinsic local Euler degrees per bone (pitch/yaw/roll). Safer than raw quaternions: angles are clamped per bone; response lists warnings. Helen/Rigify named toes DEF-toe_{big,index,middle,ring,little}.{L,R} share the same ±180° display-yaw rebasing as the Bones tab (see get_pose_guide). Prefer this for body posing; see get_pose_guide for knee/elbow sign conventions.")]
    async fn pose_bones(&self, Parameters(args): Parameters<PoseBonesArgs>) -> CallToolResult {
        let snap = self.snapshot.0.read();
        for bone in args.bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "invalid bone \"{bone}\" — use get_bone_reference (humanoid + extraBones)"
                ));
            }
        }
        drop(snap);
        let (quats, mut warnings) = bone_map_from_euler_deg(&args.bones);
        let (sanitized, mut w2) = sanitize_bone_map(quats);
        warnings.append(&mut w2);
        let count = sanitized.len();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones: sanitized,
            preserve_omitted_bones: args.preserve_omitted_bones,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_json(&json!({
            "appliedBones": count,
            "warnings": warnings,
        }))
    }

    #[tool(description = "Blend both hands toward a canned fist (amount 0..1). Fingers stay within safe curl templates — use for believable grips instead of hand-tuning many quaternions.")]
    async fn make_fist(&self, Parameters(args): Parameters<MakeFistArgs>) -> CallToolResult {
        let do_left = args.left.unwrap_or(true);
        let do_right = args.right.unwrap_or(true);
        if !do_left && !do_right {
            return err_text("specify at least one of left=true or right=true".to_string());
        }
        let bones = make_fist_bones(args.amount, do_left, do_right);
        let (sanitized, warnings) = sanitize_bone_map(bones);
        let count = sanitized.len();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones: sanitized,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_json(&json!({
            "appliedBones": count,
            "warnings": warnings,
        }))
    }

    #[tool(description = "Reset the avatar to the default pose and clear every expression.")]
    async fn reset_pose(&self) -> CallToolResult {
        self.pose_tx.send(PoseCommand::ResetPose);
        ok_text("reset pose and expressions")
    }

    #[tool(description = "Save a new pose to the library. CRITICAL: read get_pose_guide first, keep quaternion x/y/z in [-0.3, 0.3].")]
    async fn create_pose(&self, Parameters(args): Parameters<CreatePoseArgs>) -> CallToolResult {
        let snap = self.snapshot.0.read();
        for bone in args.bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "invalid bone \"{bone}\" in create_pose — use get_bone_reference (humanoid + extraBones)"
                ));
            }
        }
        drop(snap);
        let pose = PoseFile {
            name: args.name.clone(),
            description: args.description.unwrap_or_default(),
            category: args.category.unwrap_or_else(|| "general".into()),
            bones: args.bones,
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
                    cancel_expression_animation: true,
                });
            }
        }
        ok_text(format!(
            "saved pose \"{}\" ({} bones)",
            pose.name,
            pose.bones.len()
        ))
    }

    #[tool(description = "Get the full list of VRM humanoid bone names, expression presets, and (when a VRM is loaded) extra skin joint names from the live rig.")]
    async fn get_bone_reference(&self) -> CallToolResult {
        let snap = self.snapshot.0.read();
        let mut extra: Vec<String> = snap
            .bones
            .keys()
            .filter(|k| !VRM_BONE_NAMES.contains(&k.as_str()))
            .cloned()
            .collect();
        extra.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        ok_json(&json!({
            "bones": VRM_BONE_NAMES,
            "extraBones": extra,
            "expressions": VRM_EXPRESSION_NAMES,
            "note": "Rotations are quaternions [x, y, z, w] in normalized pose space (identity = bind). Expression values are 0..=1. Keep x/y/z in [-0.3, 0.3]. `extraBones` lists Rigify-style joints (e.g. DEF-toe*) present on the loaded avatar; `pose_bones` also accepts DEF-toe* / DEF-ero* by prefix before the first snapshot.",
        }))
    }

    #[tool(description = "Get the comprehensive VRM pose authoring guide — bone hierarchy, quaternion cheatsheet, per-bone natural ranges. READ BEFORE creating poses.")]
    async fn get_pose_guide(&self) -> CallToolResult {
        match std::fs::read_to_string(&self.pose_guide_path) {
            Ok(s) => ok_text(s),
            Err(e) => err_text(format!(
                "pose guide not found at {}: {e}",
                self.pose_guide_path.display()
            )),
        }
    }

    #[tool(description = "Read the current normalized pose quaternion of every indexed bone (humanoid + extra skin joints) on the loaded VRM.")]
    async fn get_current_bone_state(&self) -> CallToolResult {
        let snap = self.snapshot.0.read().clone();
        ok_json(&snap.bones)
    }

    #[tool(description = "Tiny per-axis tweak: adds delta_x/delta_y/delta_z to the bone's current pose quaternion components, then renormalizes (NOT Euler degrees). Use very small steps (often ±0.02–0.05 on one axis) for micro-corrections after pose_bones or Kimodo playback.")]
    async fn adjust_bone(&self, Parameters(args): Parameters<AdjustBoneArgs>) -> CallToolResult {
        let snap = self.snapshot.0.read().clone();
        if !mcp_allows_pose_bone_key(&args.bone_name, &snap) {
            return err_text(format!(
                "invalid bone \"{}\" — use get_bone_reference (humanoid + extraBones)",
                args.bone_name
            ));
        }
        let dx = args.delta_x.unwrap_or(0.0);
        let dy = args.delta_y.unwrap_or(0.0);
        let dz = args.delta_z.unwrap_or(0.0);
        if dx == 0.0 && dy == 0.0 && dz == 0.0 {
            return err_text("specify at least one of delta_x / delta_y / delta_z".to_string());
        }

        let [cx, cy, cz, cw] = snap
            .bones
            .get(&args.bone_name)
            .map(|e| e.rotation)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);

        let nx = cx + dx;
        let ny = cy + dy;
        let nz = cz + dz;
        let len = (nx * nx + ny * ny + nz * nz + cw * cw).sqrt().max(1e-6);
        let q = [nx / len, ny / len, nz / len, cw / len];

        let bones = HashMap::from([(args.bone_name.clone(), q)]);
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: None,
        });
        ok_text(format!(
            "adjusted {}: [{cx:.3},{cy:.3},{cz:.3},{cw:.3}] → [{:.3},{:.3},{:.3},{:.3}]",
            args.bone_name, q[0], q[1], q[2], q[3]
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

    #[tool(description = "Full-body motion clip from a text prompt via Kimodo (hub peer must be online). Use clear phase-separated prompts for floor work (sit, extend legs, return to stand). Optional save_name writes JSON under pose_library.animations_dir — check librarySaveVerified in the response; if librarySaveMissing appears, align Kimodo JARVIS_ANIMATIONS_DIR with config (see docs/MCP_POSE_ANIMATION_GUIDE.md).")]
    async fn generate_motion(
        &self,
        Parameters(args): Parameters<GenerateMotionArgs>,
    ) -> CallToolResult {
        let timeout_sec = args
            .timeout_sec
            .unwrap_or(self.kimodo_defaults.timeout_sec)
            .clamp(10, 3600);
        let req = GenerateRequest {
            prompt: args.prompt,
            duration: args.duration.unwrap_or(self.kimodo_defaults.duration_sec),
            steps: args.steps.unwrap_or(self.kimodo_defaults.steps),
            stream: args.stream.unwrap_or(true),
            save_name: args.save_name,
            timeout: std::time::Duration::from_secs(timeout_sec),
        };
        match self.kimodo.generate_motion(req).await {
            Ok(outcome) => {
                let mut v = match serde_json::to_value(&outcome) {
                    Ok(val) => val,
                    Err(e) => return err_text(format!("serialize failure: {e}")),
                };
                if let (Some(name), "done" | "ready") =
                    (outcome.save_name.as_ref(), outcome.final_status.as_str())
                {
                    if let Some(obj) = v.as_object_mut() {
                        let expected = self
                            .library
                            .animations_dir
                            .join(format!("{}.json", slugify(name)));
                        obj.insert(
                            "expectedLibraryPath".to_string(),
                            json!(expected.display().to_string()),
                        );
                        if expected.exists() {
                            obj.insert("librarySaveVerified".to_string(), json!(true));
                        } else {
                            obj.insert(
                                "librarySaveMissing".to_string(),
                                json!("expected JSON not in jarvis-avatar [pose_library].animations_dir; set JARVIS_ANIMATIONS_DIR in kimodo-motion-service to the same path"),
                            );
                        }
                    }
                }
                match serde_json::to_string_pretty(&v) {
                    Ok(s) => ok_text(s),
                    Err(e) => err_text(format!("serialize failure: {e}")),
                }
            }
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

    #[tool(description = "Update category / looping / holdDuration on a saved .json animation from list_generated_animations. Use after generate_motion to tag clips for layering or replay rules.")]
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

    #[tool(description = "Render transparent PNG snapshots of the avatar from one or more camera views (front / left / right / diagonals). Blocks until Bevy finishes all screenshots or timeout. Use framing_preset full_body or face_closeup to match validation docs.")]
    async fn capture_pose_views(
        &self,
        Parameters(args): Parameters<CapturePoseViewsArgs>,
    ) -> CallToolResult {
        let mut views = Vec::with_capacity(args.views.len());
        for s in &args.views {
            match parse_capture_view_slug(s) {
                Ok(v) => views.push(v),
                Err(e) => return err_text(e),
            }
        }
        if views.is_empty() {
            return err_text("views must include at least one view".to_string());
        }
        let framing = match args.framing_preset.as_deref().map(str::trim) {
            None | Some("") => None,
            Some("full_body") => Some(CaptureFramingPreset::FullBody),
            Some("face_closeup") => Some(CaptureFramingPreset::FaceCloseup),
            Some(x) => {
                return err_text(format!(
                    "invalid framing_preset {x:?} — use full_body or face_closeup"
                ));
            }
        };
        let timeout = Duration::from_secs(args.timeout_sec.unwrap_or(180).clamp(5, 600));
        let output_dir = expand_home(Path::new(args.output_dir.trim()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let req = CaptureRequest {
            output_dir,
            capture_id: args.capture_id,
            width: args.width.max(64).min(8192),
            height: args.height.max(64).min(8192),
            views,
            framing_preset: framing,
            camera_overrides: None,
            response_tx: tx,
        };
        if self.capture_tx.0.send(req).is_err() {
            return err_text(
                "capture command channel closed — is PoseCapturePlugin loaded?".to_string(),
            );
        }
        match tokio::task::spawn_blocking(move || rx.recv_timeout(timeout)).await {
            Ok(Ok(result)) => ok_json(&result),
            Ok(Err(RecvTimeoutError::Timeout)) => {
                err_text("capture timed out — try a longer timeout_sec or fewer views".to_string())
            }
            Ok(Err(RecvTimeoutError::Disconnected)) => {
                err_text("capture response channel closed before result".to_string())
            }
            Err(e) => err_text(format!("capture task join: {e}")),
        }
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
                "jarvis-avatar pose MCP (VRM): Always call get_pose_guide first — it is the full authoring manual (Euler vs quaternion, knee/elbow signs, capture loop). \
Workflow: baseline apply_pose or reset_pose → pose_bones (degrees, clamped) and/or make_fist for hands → generate_motion for clips (check librarySaveVerified) → set_expression or animate_expressions (time-varying face) → tiny adjust_bone quaternion deltas only → capture_pose_views to verify. \
Use list_models then load_vrm to swap the on-screen `.vrm` at runtime (expressions / bone snapshot reset; idle VRMA follows config). \
Kimodo writes animations to the same folder as [pose_library].animations_dir (see kimodo-motion-service env JARVIS_ANIMATIONS_DIR). \
Multi-clip animation layering (stacked playback) is configured in the in-app debug UI / anim_layer_sets.json — not separate MCP tools yet.",
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
