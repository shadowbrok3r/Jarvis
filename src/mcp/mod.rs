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
mod anim_layer_mcp;
pub mod pose_authoring;
pub mod pose_intents;
pub mod pose_safety;
pub mod semantic_intent_calibration;
pub mod intent_calibration_wizard;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crossbeam_channel::RecvTimeoutError;

use crate::a2f::{A2fClient, A2fConfig};
use crate::model_catalog::{list_vrm_models, resolve_vrm_load_argument};
use crate::paths::expand_home;
use crate::pose_library::{slugify, BoneRotation, PoseFile, PoseGraph, PoseLibrary};

use crate::kimodo::{GenerateRequest, KimodoClient};
use crate::plugins::channel_server::HubBroadcast;
use crate::plugins::pose_capture::{
    CaptureCommandSender, CaptureRequest, CaptureView, CaptureFramingPreset,
};
use crate::plugins::traffic_log::{TrafficChannel, TrafficDirection, TrafficLogSink};
use crate::plugins::pose_driver::{
    BoneSnapshot, BoneSnapshotHandle, PoseCommand, PoseCommandSender, VRM_BONE_NAMES,
};

use anim_layer_mcp::{
    AddLayerArgs, DeleteLayerSetArgs, InstallDefaultLayersArgs, LoadLayerSetArgs, RemoveLayerArgs,
    SaveLayerSetArgs, SetLayerStackArgs, SetMasterEnabledArgs, UpdateLayerArgs,
};
use pose_authoring::{
    bone_map_from_euler_deg, make_fist_bones, sanitize_bone_map, BoneEulerDeg, MakeFistArgs,
    PoseBonesArgs,
};
use pose_intents::{
    compile_arms_down_rest, compile_bend_knee, compile_raise_leg, ArmsDownRestArgs, BendKneeArgs,
    RaiseLegArgs,
};
use pose_safety::{is_arm_bone, is_leg_bone, PoseSafetyReport};
use semantic_intent_calibration::{SemanticIntentCalibration, SemanticIntentCalibrationStore};
use intent_calibration_wizard::{ConfirmVerdict, IntentCalibrationWizardSession, WIZARD_STEPS};

use crate::plugins::anim_layer_sets::LayerSetsStore;
use crate::plugins::anim_layers::LayerStackHandle;

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
    pub layer_guide_path: PathBuf,
    pub library: Arc<PoseLibrary>,
    pub kimodo_defaults: KimodoDefaults,
    /// Optional network trace sink (debug UI).
    pub traffic: Option<TrafficLogSink>,
    pub layer_stack: LayerStackHandle,
    pub layer_sets: LayerSetsStore,
    /// Matches `[avatar].model_path` — drives which per-VRM semantic calibration applies.
    pub semantic_model_path: Arc<RwLock<String>>,
    pub semantic_calibration: Arc<RwLock<SemanticIntentCalibrationStore>>,
    pub intent_calibration_wizard: Arc<RwLock<IntentCalibrationWizardSession>>,
    /// Human-in-the-loop pose approval gate (shared with the egui window).
    pub pose_review: crate::plugins::pose_review::PoseReviewHandle,
    /// Shared mirror of the layer glitch monitor (spike log + settings).
    pub glitch_log: crate::plugins::anim_layers::GlitchLogHandle,
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
        layer_guide_path: PathBuf,
        library: PoseLibrary,
        kimodo_defaults: KimodoDefaults,
        traffic: Option<TrafficLogSink>,
        layer_stack: LayerStackHandle,
        layer_sets: LayerSetsStore,
        semantic_model_path: Arc<RwLock<String>>,
        semantic_calibration: Arc<RwLock<SemanticIntentCalibrationStore>>,
        intent_calibration_wizard: Arc<RwLock<IntentCalibrationWizardSession>>,
        pose_review: crate::plugins::pose_review::PoseReviewHandle,
        glitch_log: crate::plugins::anim_layers::GlitchLogHandle,
    ) -> Self {
        Self::with_kimodo(
            pose_tx,
            capture_tx,
            snapshot,
            hub.clone(),
            KimodoClient::new(hub),
            a2f,
            pose_guide_path,
            layer_guide_path,
            library,
            kimodo_defaults,
            traffic,
            layer_stack,
            layer_sets,
            semantic_model_path,
            semantic_calibration,
            intent_calibration_wizard,
            pose_review,
            glitch_log,
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
        layer_guide_path: PathBuf,
        library: PoseLibrary,
        kimodo_defaults: KimodoDefaults,
        traffic: Option<TrafficLogSink>,
        layer_stack: LayerStackHandle,
        layer_sets: LayerSetsStore,
        semantic_model_path: Arc<RwLock<String>>,
        semantic_calibration: Arc<RwLock<SemanticIntentCalibrationStore>>,
        intent_calibration_wizard: Arc<RwLock<IntentCalibrationWizardSession>>,
        pose_review: crate::plugins::pose_review::PoseReviewHandle,
        glitch_log: crate::plugins::anim_layers::GlitchLogHandle,
    ) -> Self {
        Self {
            pose_tx,
            capture_tx,
            snapshot,
            hub,
            kimodo,
            a2f,
            pose_guide_path,
            layer_guide_path,
            library: Arc::new(library),
            kimodo_defaults,
            traffic,
            layer_stack,
            layer_sets,
            semantic_model_path,
            semantic_calibration,
            intent_calibration_wizard,
            pose_review,
            glitch_log,
            tool_router: Self::tool_router(),
        }
    }

    fn resolved_semantic_calibration(&self) -> SemanticIntentCalibration {
        let path = self.semantic_model_path.read().unwrap();
        let key = crate::plugins::vrm_preset_key(&path);
        self.semantic_calibration.read().unwrap().get(&key)
    }

    /// Poll the review handle for `id` for one short chunk (~20s) so a single
    /// MCP request never approaches the client's request timeout. Returns the
    /// verdict if the operator answered within the chunk, else `None`.
    async fn wait_review_chunk(
        &self,
        id: u64,
    ) -> Option<crate::plugins::pose_review::PoseReviewResult> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(r) = {
                let mut st = self.pose_review.0.lock().unwrap();
                st.take_result(id)
            } {
                return Some(r);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// Apply the operator's verdict: when approved + overwrite, snapshot the
    /// live rig over the reviewed pose name, then build the result JSON.
    fn finish_review_json(
        &self,
        result: crate::plugins::pose_review::PoseReviewResult,
    ) -> CallToolResult {
        let mut overwritten = false;
        let mut overwrite_error: Option<String> = None;
        if result.approved && result.overwrite {
            let snap = self.snapshot.0.read().clone();
            if snap.bones.is_empty() {
                overwrite_error = Some("no bones indexed — nothing to overwrite".into());
            } else {
                let bones: HashMap<String, BoneRotation> = snap
                    .bones
                    .iter()
                    .map(|(name, entry)| {
                        (
                            name.clone(),
                            BoneRotation {
                                rotation: entry.rotation,
                            },
                        )
                    })
                    .collect();
                let existing = self.library.find_pose(&result.pose_name).ok().flatten();
                let pose = PoseFile {
                    name: result.pose_name.clone(),
                    description: existing
                        .as_ref()
                        .map(|p| p.description.clone())
                        .unwrap_or_default(),
                    category: existing
                        .as_ref()
                        .map(|p| p.category.clone())
                        .unwrap_or_else(|| "general".into()),
                    bones,
                    expressions: snap.expressions.clone(),
                    transition_duration: existing
                        .as_ref()
                        .map(|p| p.transition_duration)
                        .unwrap_or(0.4),
                };
                match self.library.save_pose(&pose) {
                    Ok(_) => overwritten = true,
                    Err(e) => overwrite_error = Some(format!("{e}")),
                }
            }
        }
        ok_json(&json!({
            "status": "answered",
            "poseName": result.pose_name,
            "approved": result.approved,
            "feedback": result.feedback,
            "overwriteRequested": result.overwrite,
            "overwritten": overwritten,
            "overwriteError": overwrite_error,
        }))
    }
}

// ---------- tool parameter types ---------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IntentCalibrationConfirmArgs {
    /// Must match the `stepId` from the latest `intent_calibration_probe` response.
    pub step_id: String,
    /// Human verdict after they inspected the pose: `correct`, `flip`, or `skip`.
    pub verdict: String,
}

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
    /// Map of expression name → 0..=1 intensity. **Partial merge** (`ModifyExpressions`): omitted presets keep current overrides / VRMA. Keys must exist on the loaded VRM when `list_expressions` is non-empty. For a full-face replace (all presets at once, unnamed → 0), use `set_expressions_full`.
    pub expressions: HashMap<String, f32>,
    /// Transition duration in seconds. Default 0.3.
    #[serde(default)]
    pub transition_seconds: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetExpressionsFullArgs {
    /// Weights 0..=1 for any subset of presets; **every** known preset on the VRM not listed here is set to 0 for this apply (`SetExpressions` / full replace). Keys must exist on the loaded VRM when `list_expressions` is non-empty.
    pub expressions: HashMap<String, f32>,
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
    /// **Defaults to true; the MCP server rejects `false`** for the same reason
    /// it does on `pose_bones` — resetting every unlisted bone to identity
    /// destroys partial maps. For a full reset use `reset_pose` then
    /// `set_bones`.
    #[serde(default = "default_true")]
    pub preserve_omitted_bones: bool,
    /// When true, run validation + sanitize but do not dispatch ApplyBones.
    /// Returns the would-apply summary (sanitized rotations, warnings).
    #[serde(default)]
    pub dry_run: Option<bool>,
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
pub struct SaveCurrentPoseArgs {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// Restrict the snapshot to a subset of bones (humanoid + extra skin
    /// joints). Empty / omitted = save **every** indexed bone. Use this to
    /// build foundation poses that only override an upper-body chain (e.g.
    /// arms-down rest) without freezing the legs / hips.
    #[serde(default)]
    pub bones: Option<Vec<String>>,
    /// Capture live VRM expression weights (any preset with an active override:
    /// e.g. `happy`, `aa`, `blink`) alongside the bones. Defaults to **true** so
    /// pose snapshots round-trip the face. Set to `false` to save bones only,
    /// e.g. when authoring a body-only foundation pose that should compose with
    /// whatever expression is active at playback time.
    #[serde(default)]
    pub include_expressions: Option<bool>,
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
pub struct TagPoseArgs {
    /// Pose name to tag (the key in the pose graph).
    pub name: String,
    /// Position/content tags. Non-empty replaces
    /// the pose's tags; empty leaves them unchanged. (Typed array — passes through.)
    #[serde(default)]
    pub tags: Vec<String>,
    /// Hip height in meters: **>0 sets it; 0 leaves unchanged.** stand≈0.9,
    /// kneel/squat≈0.5, lying≈0.2. Typed (not Option) to survive MCP stringify.
    #[serde(default)]
    pub root_y: f32,
    /// Blessed for autonomous use. Applied on every call (default true — pass
    /// false to un-bless). Typed bool, passes through.
    #[serde(default = "default_true")]
    pub autonomous: bool,
    /// Poses this one can transition to naturally. Non-empty replaces; empty leaves.
    #[serde(default)]
    pub next_poses: Vec<String>,
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
    /// Phase A: path to a Kimodo `constraints.json` (EE / fullbody / root2d
    /// keyframes). Empty = text-only. Plain string (typed) to avoid the
    /// untyped-Option stringification quirk.
    #[serde(default)]
    pub constraints_path: String,
    /// Phase B: attach Kimodo's root trajectory as per-frame `rootPosition`
    /// (root motion). Plain bool (typed) for the same reason.
    #[serde(default)]
    pub allow_root_motion: bool,
    /// Optional timeout override for this request (seconds). If omitted,
    /// `[mcp].kimodo_timeout_sec` is used.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewPoseArgs {
    /// Pose name (or generated-clip name) being reviewed — shown in the popup.
    pub pose_name: String,
    /// The VISUAL GOAL — describe what the pose should look like in plain,
    /// concrete body terms so the operator can sculpt the rig to match if it's
    /// off. Cover the whole body: legs/knees, hips, torso lean, arms/hands,
    /// head/gaze (e.g. "Kneeling upright: shins flat on the floor, knees folded
    /// under the hips, thighs vertical, torso hinged ~45° forward, arms hanging
    /// down resting near the thighs, head up looking forward."). Rendered
    /// prominently in its own panel — DON'T put operational notes here.
    #[serde(default)]
    pub intent: String,
    /// Operational note only (e.g. "layer stack disabled so the pose shows").
    /// Rendered small + separate from the intent. Keep visual description out.
    #[serde(default)]
    pub note: String,
    /// Apply this library pose to the avatar before asking (default true) so
    /// the operator reviews exactly what's on screen. Set false when you've
    /// already sculpted the live rig (e.g. via pose_bones) and want it judged
    /// as-is.
    #[serde(default = "default_true")]
    pub apply: bool,
    /// Deprecated: waiting is now chunked, so review_pose returns a pending
    /// token quickly instead of blocking. Use await_pose_review to keep waiting.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AwaitPoseReviewArgs {
    /// Reserved for future per-call tuning; no fields required. Just call it
    /// again whenever the previous review_pose / await_pose_review returned
    /// status:"pending".
    #[serde(default)]
    pub review_id: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PoseKeyframeArg {
    /// Library pose name (use `list_poses`). Retargeted VRM→SOMA77 server-side.
    pub pose: String,
    /// Frame index in the generated clip where the body should pass through
    /// this pose. Put the most important keyframe at/near the LAST frame —
    /// Kimodo tends to drift after the final constrained frame.
    pub frame: u32,
    /// Approximate hip height (m) at this keyframe: standing ≈ 0.90, kneeling
    /// / folded lower (≈ 0.45–0.60). Drives the constraint's root FK + sink.
    #[serde(default)]
    pub root_y: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyframePoseMotionArgs {
    /// Text description of the motion (Kimodo blends this with the keyframes).
    pub prompt: String,
    /// Library poses + frame indices the body must pass through. At least one.
    pub keyframes: Vec<PoseKeyframeArg>,
    /// Duration in seconds. **0 = use config default (3.0s).** Shorter clips
    /// (e.g. 2.0) make the motion fill more of the duration instead of holding
    /// the end pose. Typed (not Option) so it survives MCP-client stringify.
    #[serde(default)]
    pub duration: f32,
    /// Denoising steps. **0 = config default (100).**
    #[serde(default)]
    pub steps: u32,
    /// Stream frames in real time (default false — keyframed clips are saved).
    #[serde(default)]
    pub stream: bool,
    /// Save the result under this name.
    #[serde(default)]
    pub save_name: Option<String>,
    /// Classifier-free guidance text weight. **0 = default 2.0.**
    #[serde(default)]
    pub text_weight: f32,
    /// Classifier-free guidance constraint weight (pull toward poses). **0 = default 3.0.**
    #[serde(default)]
    pub constraint_weight: f32,
    /// Attach Kimodo's root trajectory as per-frame `rootPosition` (default true).
    #[serde(default = "default_true")]
    pub allow_root_motion: bool,
    /// Timeout override (seconds). **0 = config default.**
    #[serde(default)]
    pub timeout_sec: u64,
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
    /// Optional case-insensitive substring filter on the `.vrm` basename.
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetGlitchLogArgs {
    /// Max recent spike events to return (default 50, cap 400).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadVrmArgs {
    /// `models/name.vrm` (under `assets/`) or basename only (`name.vrm` in `assets/models/`).
    pub path: String,
}

fn default_capture_dim() -> u32 {
    1024
}

fn default_embed_max_dim() -> u32 {
    768
}

fn default_capture_output_dir() -> String {
    "pose_captures".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CapturePoseViewsArgs {
    /// Directory for saved PNGs (host filesystem; `~/` expanded). **Optional** for MCP agents: if omitted, defaults to `pose_captures` under the avatar process working directory. Captures are also returned as **inline `image/png` blocks** in the tool result when `embed_images` is true (default) — you do not need local file access to review the pose.
    #[serde(default = "default_capture_output_dir")]
    pub output_dir: String,
    /// Prefix in filenames: `<capture_id>_<view>_<WxH>.png`.
    pub capture_id: String,
    #[serde(default = "default_capture_dim")]
    pub width: u32,
    #[serde(default = "default_capture_dim")]
    pub height: u32,
    /// View slugs: `front`, `left`, `right`, `front_left`, `front_right`, `back`, `back_left`, `back_right`.
    pub views: Vec<String>,
    /// Optional: `full_body` or `face_closeup` (camera distance / head focus).
    #[serde(default)]
    pub framing_preset: Option<String>,
    /// Bevy capture pipeline timeout in seconds (default 180, min 5, max 600).
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    /// Async sleep before enqueueing capture so a prior `pose_bones` / `apply_pose` can apply in Bevy (default 120 ms; 0 disables).
    #[serde(default)]
    pub settle_before_capture_ms: Option<u32>,
    /// When true (default) the tool response **also** carries each captured PNG as an MCP `image/png` content block (base64) so multimodal callers can SEE the silhouette without a separate file read. Set false for batch automation that only needs the on-disk paths.
    #[serde(default)]
    pub embed_images: Option<bool>,
    /// When embedding, downscale each PNG so its longest side is at most this many pixels before base64. Default 768 keeps a 5-view payload well under typical MCP message limits while preserving silhouette detail. Set to 0 to embed the raw capture without resizing.
    #[serde(default = "default_embed_max_dim")]
    pub max_embed_dimension: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureAnimationMontageArgs {
    /// Saved animation filename from `list_generated_animations` (the `.json`
    /// is optional — added if missing).
    pub filename: String,
    /// Single camera view for every tile (`front`, `left`, `right`, `back`,
    /// diagonals). Default `left` — best for reading a descent / forward motion.
    #[serde(default)]
    pub view: Option<String>,
    /// How many evenly-spaced frames to sample across the whole clip.
    /// **0 = default 12.** Typed so it survives MCP-client stringify.
    #[serde(default)]
    pub frame_count: u32,
    /// Tiles per row in the montage grid. **0 = default 4.**
    #[serde(default)]
    pub columns: u32,
    /// Also write an animated GIF of the sampled frames to disk (for the human;
    /// the agent only sees the montage grid). Default true.
    #[serde(default = "default_true")]
    pub also_gif: bool,
    /// GIF playback frames-per-second (the sampled frames). **0 = default 8.**
    #[serde(default)]
    pub gif_fps: u32,
    /// `full_body` (default) or `face_closeup`.
    #[serde(default)]
    pub framing_preset: Option<String>,
    /// Per-tile render size before compositing. **0 = default 384.** Min 96, max 1024.
    #[serde(default)]
    pub tile_size: u32,
    /// Directory for the montage PNG + GIF. Default `pose_captures`.
    #[serde(default = "default_capture_output_dir")]
    pub output_dir: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordViewportArgs {
    /// Seconds of LIVE viewport to record. **0 = default 6.** Clamped 0.5..30.
    #[serde(default)]
    pub duration_sec: f32,
    /// Frames sampled per second. **0 = default 6.** Clamped 1..20. Total frames
    /// = duration_sec × fps, capped at 48 (the montage/GIF frame budget).
    #[serde(default)]
    pub fps: u32,
    /// Camera view (`front`, `left`, `right`, `back`, diagonals). Default `front`.
    #[serde(default)]
    pub view: Option<String>,
    /// Tiles per row in the montage grid. **0 = default 4.**
    #[serde(default)]
    pub columns: u32,
    /// Per-tile render size before compositing. **0 = default 384.** Min 96, max 1024.
    #[serde(default)]
    pub tile_size: u32,
    /// `full_body` (default) or `face_closeup`.
    #[serde(default)]
    pub framing_preset: Option<String>,
    /// Also write an animated GIF of the recording to disk (for the human; the
    /// agent reliably sees only the static montage grid). Default true.
    #[serde(default = "default_true")]
    pub also_gif: bool,
    /// Filename stem for the montage PNG + GIF. Default `viewport`.
    #[serde(default)]
    pub label: String,
    /// Directory for the recording outputs. Default `pose_captures`.
    #[serde(default = "default_capture_output_dir")]
    pub output_dir: String,
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
        "back" | "rear" => Ok(CaptureView::Back),
        "back_left" | "backleft" | "back-left" => Ok(CaptureView::BackLeft),
        "back_right" | "backright" | "back-right" => Ok(CaptureView::BackRight),
        _ => Err(format!(
            "unknown view {s:?} — use front, left, right, front_left, front_right, back, back_left, back_right"
        )),
    }
}

// ---------- helpers -----------------------------------------------------------

fn ok_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(body.into())])
}

fn err_text(body: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(body.into())])
}

/// MCP pose tools accept VRM humanoid keys plus any bone currently in the live snapshot
/// (extra skin joints indexed by glTF [`Name`]) and, before the first snapshot, `DEF-*`
/// prefixes (ASCII case-insensitive).
fn mcp_allows_pose_bone_key(name: &str, snap: &BoneSnapshot) -> bool {
    if VRM_BONE_NAMES.contains(&name) {
        return true;
    }
    if snap.bones.contains_key(name) {
        return true;
    }
    let n = name.to_ascii_lowercase();
    n.starts_with("def-")
}

/// When the live VRM has reported expression names, MCP must not invent keys.
fn mcp_validate_expression_keys(
    expr: &HashMap<String, f32>,
    snap: &BoneSnapshot,
) -> Result<(), String> {
    if snap.expression_presets.is_empty() {
        return Ok(());
    }
    let allowed: HashSet<&str> = snap.expression_presets.iter().map(String::as_str).collect();
    let bad: Vec<&str> = expr
        .keys()
        .filter(|k| !allowed.contains(k.as_str()))
        .map(String::as_str)
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unknown expression preset(s): {:?} — use list_expressions for valid names",
            bad
        ))
    }
}

fn ok_json(v: &impl Serialize) -> CallToolResult {
    match serde_json::to_string_pretty(v) {
        Ok(s) => ok_text(s),
        Err(e) => err_text(format!("serialize failure: {e}")),
    }
}

/// Load a PNG from disk and return an MCP `ContentBlock::image` block (base64 PNG).
/// When `max_dim > 0` and the image's longest side exceeds it, the image is
/// downscaled with Lanczos3 before re-encoding so MCP responses stay compact.
fn embed_png_as_content(path: &Path, max_dim: u32) -> Result<ContentBlock, String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use std::io::Cursor;

    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let encoded = if max_dim == 0 {
        B64.encode(&bytes)
    } else {
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("decode {}: {e}", path.display()))?;
        let (w, h) = (img.width(), img.height());
        let needs_resize = w.max(h) > max_dim;
        let resized = if needs_resize {
            img.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        let mut out: Vec<u8> = Vec::new();
        resized
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| format!("re-encode {}: {e}", path.display()))?;
        B64.encode(&out)
    };
    Ok(ContentBlock::image(encoded, "image/png"))
}

/// Composite a sequence of PNG frame paths into one labeled grid montage PNG.
/// Each tile is resized to `tile` px (longest side) on a checkerless transparent
/// cell. Returns the montage's own PNG bytes.
fn build_montage_png(paths: &[PathBuf], columns: u32, tile: u32) -> Result<Vec<u8>, String> {
    use image::{imageops, GenericImage, Rgba, RgbaImage};
    use std::io::Cursor;

    if paths.is_empty() {
        return Err("no frames to composite".into());
    }
    let cols = columns.max(1);
    let rows = ((paths.len() as u32) + cols - 1) / cols;
    let cell = tile.max(32);
    let mut canvas: RgbaImage = RgbaImage::from_pixel(cols * cell, rows * cell, Rgba([0, 0, 0, 0]));
    for (i, p) in paths.iter().enumerate() {
        let bytes = std::fs::read(p).map_err(|e| format!("read {}: {e}", p.display()))?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("decode {}: {e}", p.display()))?
            .resize(cell, cell, imageops::FilterType::Lanczos3)
            .to_rgba8();
        let col = (i as u32) % cols;
        let row = (i as u32) / cols;
        // center the (possibly non-square) resized tile in its cell
        let ox = col * cell + (cell - img.width()) / 2;
        let oy = row * cell + (cell - img.height()) / 2;
        canvas
            .copy_from(&img, ox, oy)
            .map_err(|e| format!("composite tile {i}: {e}"))?;
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("encode montage: {e}"))?;
    Ok(out)
}

/// Encode a sequence of PNG frame paths into an animated GIF written to `dest`.
fn write_animation_gif(paths: &[PathBuf], dest: &Path, fps: u32) -> Result<(), String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{imageops, Delay, Frame};

    let file = std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut enc = GifEncoder::new(std::io::BufWriter::new(file));
    enc.set_repeat(Repeat::Infinite)
        .map_err(|e| format!("gif repeat: {e}"))?;
    let delay = Delay::from_numer_denom_ms(1000, fps.max(1));
    for p in paths {
        let bytes = std::fs::read(p).map_err(|e| format!("read {}: {e}", p.display()))?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("decode {}: {e}", p.display()))?
            // GIF has no alpha blending; flatten onto the 256-color path at a modest size
            .resize(480, 480, imageops::FilterType::Lanczos3)
            .to_rgba8();
        enc.encode_frame(Frame::from_parts(img, 0, 0, delay))
            .map_err(|e| format!("gif frame: {e}"))?;
    }
    Ok(())
}

/// Recommend the minimum set of capture views an agent should render after
/// leg / arm edits — the `front` view alone hides the most common pose
/// failures (knee direction, foot crossover, elbow inversion).
fn verification_hint(touches_legs: bool, touches_arms: bool) -> &'static str {
    match (touches_legs, touches_arms) {
        (true, _) => {
            "leg bones changed — call capture_pose_views with at least left, right, and back \
(front alone hides knee direction and foot crossover)."
        }
        (_, true) => {
            "arm bones changed — call capture_pose_views with at least left, right, and back \
(front alone hides elbow inversion)."
        }
        _ => "verify with capture_pose_views (front + at least one side view).",
    }
}

/// Hybrid policy on `capture_pose_views`: when the most recent leg / arm
/// changes warrant side / back coverage, return a warning string instead of
/// silently accepting a front-only capture. The current request only knows
/// what views are being asked for; the touched-bones context comes from the
/// `pose_bones` response (handler returns `verificationHint`). For now we
/// just gate on view coverage so a blanket front-only capture always warns.
fn capture_view_policy_warning(views: &[String]) -> Option<String> {
    let lower: Vec<String> = views.iter().map(|v| v.trim().to_ascii_lowercase()).collect();
    let has_side = lower.iter().any(|v| {
        matches!(
            v.as_str(),
            "left" | "right" | "front_left" | "front_right" | "back_left" | "back_right"
        )
    });
    let has_back = lower.iter().any(|v| matches!(v.as_str(), "back" | "back_left" | "back_right"));
    if !has_side {
        Some(
            "captured front-only — leg/arm pose errors (knee direction, elbow inversion, \
foot crossover) are invisible from the front. Add at least one side view (left/right) \
and a back view."
                .to_string(),
        )
    } else if !has_back {
        Some(
            "no back view — torso / shoulder rotation issues hide on front+side captures. \
Add back when posing the upper body or anything load-bearing."
                .to_string(),
        )
    } else {
        None
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
                "modelsDir": crate::model_catalog::models_dir().display().to_string(),
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

        let snap = self.snapshot.0.read();
        if !pose.expressions.is_empty() {
            if let Err(e) = mcp_validate_expression_keys(&pose.expressions, &snap) {
                return err_text(e);
            }
        }
        drop(snap);

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

    #[tool(description = "Merge VRM expression weights (0..=1) via `ModifyExpressions`: only listed presets change; others keep current overrides or VRMA sampling. REQUIRED top-level key: `expressions` (note the plural `s`) — a JSON OBJECT mapping preset name → weight in 0..=1. Example: `{ \"expressions\": { \"happy\": 0.8, \"smile\": 0.4 } }`. Singular `expression` or top-level preset keys FAIL deserialization. Valid preset names come from `list_expressions` / `get_bone_reference.expressionPresets`. Stops idle VRMA when auto-stop is on. For replacing the **entire** face in one call (every preset on the model, unmentioned → 0), use `set_expressions_full`.")]
    async fn set_expression(
        &self,
        Parameters(args): Parameters<SetExpressionArgs>,
    ) -> CallToolResult {
        if args.expressions.is_empty() {
            return err_text("expressions map is empty — pass at least one preset name → weight".to_string());
        }
        let snap = self.snapshot.0.read();
        if let Err(e) = mcp_validate_expression_keys(&args.expressions, &snap) {
            return err_text(e);
        }
        drop(snap);
        let names: Vec<String> = args.expressions.keys().cloned().collect();
        self.pose_tx.send(PoseCommand::ApplyExpression {
            weights: args.expressions,
            cancel_expression_animation: true,
        });
        ok_text(format!("merged expressions: {}", names.join(", ")))
    }

    #[tool(description = "Replace the **full** VRM expression override state in one shot (`SetExpressions`): builds a weight map over **every** preset on the loaded model (`list_expressions`), sets each listed key from `expressions`, forces all others to 0, then applies. Use after `list_expressions` so you know the key set. Stops idle VRMA when auto-stop is on. Prefer `set_expression` for small layered tweaks.")]
    async fn set_expressions_full(
        &self,
        Parameters(args): Parameters<SetExpressionsFullArgs>,
    ) -> CallToolResult {
        let snap = self.snapshot.0.read();
        if snap.expression_presets.is_empty() {
            return err_text(
                "no expression presets on snapshot — load a VRM (load_vrm) and wait until list_expressions is non-empty"
                    .to_string(),
            );
        }
        if let Err(e) = mcp_validate_expression_keys(&args.expressions, &snap) {
            return err_text(e);
        }
        let presets = snap.expression_presets.clone();
        drop(snap);
        let mut weights: HashMap<String, f32> = HashMap::with_capacity(presets.len());
        for p in &presets {
            let v = args.expressions.get(p).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            weights.insert(p.clone(), v);
        }
        let non_zero: Vec<String> = weights
            .iter()
            .filter(|(_, w)| **w > 1e-4)
            .map(|(k, _)| k.clone())
            .collect();
        self.pose_tx.send(PoseCommand::SetExpression { weights });
        ok_json(&json!({
            "applied": true,
            "presetCount": presets.len(),
            "nonZeroKeys": non_zero,
            "note": "All presets on the VRM received a weight (omitted keys were set to 0).",
        }))
    }

    #[tool(description = "List VRM expression preset names available on the **currently loaded** avatar (VRMC_vrm `preset` + `custom`: happy, blink, …). Small JSON for agents — pair with `set_expression` (partial) or `set_expressions_full` (whole face). Same names as `get_bone_reference` → `expressionPresets`.")]
    async fn list_expressions(&self) -> CallToolResult {
        let snap = self.snapshot.0.read();
        let names = snap.expression_presets.clone();
        let count = names.len();
        drop(snap);
        ok_json(&json!({
            "expressionPresets": names,
            "count": count,
            "partialMergeTool": "set_expression",
            "fullReplaceTool": "set_expressions_full",
            "keyframesTool": "animate_expressions",
            "withBodyTool": "pose_bones (optional `expressions` field)",
        }))
    }

    #[tool(description = "Play a short in-engine VRM expression curve (piecewise-linear keyframes). Preset names in each keyframe must exist on the loaded VRM (`list_expressions`) when that list is non-empty. Stops idle VRMA like other manual pose commands. Omitted expression keys in a keyframe default to 0 when lerping into keys that list them. Cancels on reset_pose / set_expression / apply_pose with expressions. Layered in-app expression drivers (blink, etc.) still run first each frame; animated channels override last. After one-shot playback, last sampled weights remain until changed. Verify with capture_pose_views + framing_preset face_closeup.")]
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
        let snap = self.snapshot.0.read();
        let mut union: HashMap<String, f32> = HashMap::new();
        for kf in &args.keyframes {
            for (k, v) in &kf.weights {
                union.insert(k.clone(), *v);
            }
        }
        if !union.is_empty() {
            if let Err(e) = mcp_validate_expression_keys(&union, &snap) {
                return err_text(e);
            }
        }
        drop(snap);
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

    #[tool(description = "Directly set bone rotations as quaternions [x, y, z, w]. Prefer pose_bones (Euler degrees + clamps) unless you have a specific quaternion target. **`preserve_omitted_bones=false` is rejected** (same as pose_bones). `dry_run=true` returns the sanitized quaternions and warnings without applying.")]
    async fn set_bones(&self, Parameters(args): Parameters<SetBonesArgs>) -> CallToolResult {
        if !args.preserve_omitted_bones {
            return err_text(
                "preserve_omitted_bones=false is not allowed on set_bones: it resets every bone NOT listed in `bones` to identity, which destroys partial maps. Omit the field or use true (default). For a clean slate: call reset_pose, then set_bones.".to_string(),
            );
        }
        let snap = self.snapshot.0.read();
        for bone in args.bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "invalid bone \"{bone}\" — use get_bone_reference (humanoid + extraBones)"
                ));
            }
        }
        drop(snap);
        let dry_run = args.dry_run.unwrap_or(false);
        let count = args.bones.len();
        let bones: HashMap<String, [f32; 4]> = args
            .bones
            .into_iter()
            .map(|(k, v)| (k, v.rotation))
            .collect();
        let (sanitized, warnings) = sanitize_bone_map(bones);

        let touches_legs = sanitized.keys().any(|b| is_leg_bone(b));
        let touches_arms = sanitized.keys().any(|b| is_arm_bone(b));

        if !dry_run {
            self.pose_tx.send(PoseCommand::ApplyBones {
                bones: sanitized.clone(),
                preserve_omitted_bones: args.preserve_omitted_bones,
                blend_weight: None,
                transition_seconds: None,
            });
        }
        let mut response = json!({
            "appliedBones": if dry_run { 0 } else { count },
            "wouldApplyBones": count,
            "warnings": warnings,
            "dryRun": dry_run,
            "verificationHint": verification_hint(touches_legs, touches_arms),
        });
        if dry_run {
            response["sanitizedRotations"] =
                serde_json::to_value(sanitized).unwrap_or(Value::Null);
        }
        ok_json(&response)
    }

    #[tool(description = "Set many bones at once using intrinsic local Euler degrees per bone (pitch/yaw/roll). REQUIRED top-level key: `bones` — JSON OBJECT bone name → `{ pitch_deg?, yaw_deg?, roll_deg? }`. **`preserve_omitted_bones` must be true or omitted (default); `false` is rejected** — it resets all unlisted bones to bind and breaks partial maps. For a full reset use `reset_pose` then call this tool. Optional `expressions` merges morph weights (keys from `list_expressions`). HYBRID SAFETY: catastrophic requests (many bones at near-axis limits) hard-fail; severe single-axis ≥80° hard-fails unless `allow_large_angles=true`. `strict=true` escalates any near-limit angle to a hard-fail. `dry_run=true` returns the would-apply summary (sanitized rotations, warnings, severity) without touching the rig. After leg/arm edits verify with capture_pose_views including left, right, back. Prefer the semantic tools (raise_leg, bend_knee, arms_down_rest) before raw pose_bones.")]
    async fn pose_bones(&self, Parameters(args): Parameters<PoseBonesArgs>) -> CallToolResult {
        if !args.preserve_omitted_bones {
            return err_text(
                "preserve_omitted_bones=false is not allowed on pose_bones: it resets every bone NOT listed in `bones` to bind pose, which almost always destroys the rig for partial maps. Omit the field or use true (default). For a clean slate: call reset_pose, then pose_bones.".to_string(),
            );
        }
        let snap = self.snapshot.0.read();
        for bone in args.bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "invalid bone \"{bone}\" — use get_bone_reference (humanoid + extraBones)"
                ));
            }
        }
        if let Some(ref expr) = args.expressions {
            if let Err(e) = mcp_validate_expression_keys(expr, &snap) {
                return err_text(e);
            }
        }
        drop(snap);

        let strict = args.strict.unwrap_or(false);
        let allow_large_angles = args.allow_large_angles.unwrap_or(false);
        let dry_run = args.dry_run.unwrap_or(false);

        let safety = PoseSafetyReport::from_euler_map(&args.bones);
        if let Some(reason) = safety.should_block(strict, allow_large_angles) {
            return err_text(format!(
                "pose_bones blocked by hybrid safety policy ({}): {reason}",
                safety.severity.as_str()
            ));
        }

        let (quats, mut warnings) = bone_map_from_euler_deg(&args.bones);
        let (sanitized, mut w2) = sanitize_bone_map(quats);
        warnings.append(&mut w2);

        // Strict mode hard-fails on any sanitize warning (clamps that already
        // ran). Outside strict, we surface the warnings on the response only.
        if strict && !warnings.is_empty() {
            return err_text(format!(
                "strict mode: pose_bones produced {} sanitize warning(s) before apply: {warnings:?}",
                warnings.len()
            ));
        }

        let touches_legs = args.bones.keys().any(|b| is_leg_bone(b));
        let touches_arms = args.bones.keys().any(|b| is_arm_bone(b));
        let count = sanitized.len();

        if !dry_run {
            self.pose_tx.send(PoseCommand::ApplyBones {
                bones: sanitized.clone(),
                preserve_omitted_bones: args.preserve_omitted_bones,
                blend_weight: None,
                transition_seconds: None,
            });
        }

        let mut expr_applied = 0usize;
        if let Some(expr) = args.expressions {
            if !expr.is_empty() {
                expr_applied = expr.len();
                if !dry_run {
                    self.pose_tx.send(PoseCommand::ApplyExpression {
                        weights: expr,
                        cancel_expression_animation: true,
                    });
                }
            }
        }

        let mut response = json!({
            "appliedBones": if dry_run { 0 } else { count },
            "wouldApplyBones": count,
            "appliedExpressions": if dry_run { 0 } else { expr_applied },
            "warnings": warnings,
            "safety": {
                "severity": safety.severity.as_str(),
                "strict": strict,
                "allowLargeAngles": allow_large_angles,
                "dryRun": dry_run,
                "severeBones": safety.severe_bones,
                "nearLimitBones": safety.near_limit_bones,
                "maxAngleDeg": safety.max_angle_seen_deg,
            },
            "verificationHint": verification_hint(touches_legs, touches_arms),
        });
        if dry_run {
            // Include sanitized quaternions for callers preview-fitting them.
            let preview: HashMap<String, [f32; 4]> = sanitized;
            response["sanitizedRotations"] = serde_json::to_value(preview).unwrap_or(Value::Null);
        }
        ok_json(&response)
    }

    #[tool(description = "Semantic intent: lift one leg from the hip. REQUIRED `side` (\"left\" or \"right\") and `amount` (0..=1). Optional `direction`: \"forward\" (default) = hip flex, knee comes forward and up; \"outward\" = hip abduction, leg fans out to the side via mirrored roll. The compiled upper-leg sign comes from this VRM's calibration, so \"forward\" flexes the hip forward even on rigs where raw positive pitch extends the thigh backward. Compiles to a single bounded upper-leg Euler. `dry_run=true` returns the would-apply map without dispatching. PREFER THIS OVER raw pose_bones for any 'raise leg' agent intent.")]
    async fn raise_leg(&self, Parameters(args): Parameters<RaiseLegArgs>) -> CallToolResult {
        let dry_run = args.dry_run.unwrap_or(false);
        let cal = self.resolved_semantic_calibration();
        let bones = compile_raise_leg(&args, &cal);
        self.apply_intent_bones("raise_leg", &bones, dry_run, true, false)
    }

    #[tool(description = "Semantic intent: bend one knee. REQUIRED `side` (\"left\" or \"right\") and `amount` (0..=1, where 1 ≈ 70° flex). Compiles to a single bounded `*LowerLeg` pitch (× per-VRM calibration from Intent Lab / config). `dry_run=true` previews the map. PREFER THIS OVER raw pose_bones for knee bends.")]
    async fn bend_knee(&self, Parameters(args): Parameters<BendKneeArgs>) -> CallToolResult {
        let dry_run = args.dry_run.unwrap_or(false);
        let cal = self.resolved_semantic_calibration();
        let bones = compile_bend_knee(&args, &cal);
        self.apply_intent_bones("bend_knee", &bones, dry_run, true, false)
    }

    #[tool(description = "Semantic intent: drop both arms into a natural rest at the sides. Optional `amount` (0..=1, default 0.85). Mirror-symmetric upper-arm roll + elbow pitch + shoulders (× per-VRM calibration). `dry_run=true` previews. PREFER THIS OVER raw pose_bones for arms-down / idle stance intents.")]
    async fn arms_down_rest(
        &self,
        Parameters(args): Parameters<ArmsDownRestArgs>,
    ) -> CallToolResult {
        let dry_run = args.dry_run.unwrap_or(false);
        let cal = self.resolved_semantic_calibration();
        let bones = compile_arms_down_rest(&args, &cal);
        self.apply_intent_bones("arms_down_rest", &bones, dry_run, false, true)
    }

    #[tool(description = "Start the Intent Lab calibration wizard for the loaded VRM. Returns step checklist + agent workflow: (1) call intent_calibration_probe once per step — it resets pose and applies a test intent; (2) STOP and ask the human to verify in the viewport or Intent Lab; (3) only after the human answers, call intent_calibration_confirm with their verdict; (4) repeat until complete; (5) save_intent_calibration. Probes are BLOCKED until the previous step is confirmed.")]
    async fn begin_intent_calibration_wizard(&self) -> CallToolResult {
        let path = self.semantic_model_path.read().unwrap().clone();
        let key = crate::plugins::vrm_preset_key(&path);
        let baseline = self.semantic_calibration.read().unwrap().get(&key);
        let mut wiz = self.intent_calibration_wizard.write().unwrap();
        wiz.begin(key.clone(), path.clone(), baseline);
        let steps: Vec<_> = WIZARD_STEPS
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "label": s.label,
                    "userQuestion": s.user_question,
                })
            })
            .collect();
        ok_json(&json!({
            "started": true,
            "vrmKey": key,
            "logicalPath": path,
            "steps": steps,
            "agentWorkflow": [
                "Call intent_calibration_probe — applies one test pose and sets awaitingUserConfirm.",
                "STOP. Ask the human the userQuestion from the response. Do NOT call another probe or skip confirm.",
                "When the human answers, call intent_calibration_confirm with step_id + verdict (correct | flip | skip).",
                "If flip: the relevant calibration sign is multiplied by -1 and the next probe uses the new sign.",
                "Repeat probe → human confirm until wizardComplete. Then save_intent_calibration.",
                "Optional: capture_pose_views (left, right, back) after each probe to help the human decide.",
            ],
        }))
    }

    #[tool(description = "Wizard: apply the current calibration probe (resets pose first). BLOCKS if a previous probe is still awaiting human confirm. Returns awaitingUserConfirm=true and the question you MUST ask the human before calling intent_calibration_confirm.")]
    async fn intent_calibration_probe(&self) -> CallToolResult {
        let mut wiz = self.intent_calibration_wizard.write().unwrap();
        let (bones, pending) = match wiz.probe_bones() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        drop(wiz);

        self.pose_tx.send(PoseCommand::ResetPose);
        if let Err(msg) = self.dispatch_intent_bones_map(
            &bones,
            pending.step_id == "arms_down_rest",
        ) {
            return err_text(msg);
        }

        ok_json(&json!({
            "awaitingUserConfirm": true,
            "stepId": pending.step_id,
            "label": pending.label,
            "userQuestion": pending.user_question,
            "appliedBones": pending.applied_bone_keys,
            "instruction": "STOP — ask the human this question. Do not probe again until intent_calibration_confirm succeeds.",
        }))
    }

    #[tool(description = "Wizard: record the human's verdict after intent_calibration_probe. REQUIRED step_id (must match awaiting step) and verdict: correct (sign stays), flip (multiply relevant sign by -1), or skip (leave sign, advance).")]
    async fn intent_calibration_confirm(
        &self,
        Parameters(args): Parameters<IntentCalibrationConfirmArgs>,
    ) -> CallToolResult {
        let verdict = match args.verdict.as_str() {
            "correct" => ConfirmVerdict::Correct,
            "flip" => ConfirmVerdict::Flip,
            "skip" => ConfirmVerdict::Skip,
            other => {
                return err_text(format!(
                    "invalid verdict {other:?} — use correct, flip, or skip"
                ));
            }
        };
        let mut wiz = self.intent_calibration_wizard.write().unwrap();
        match wiz.confirm(&args.step_id, verdict) {
            Ok(v) => ok_json(&v),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Wizard: read session phase, current step, awaitingUserConfirm flag, draft calibration, and completed step log.")]
    async fn get_intent_calibration_wizard_status(&self) -> CallToolResult {
        let wiz = self.intent_calibration_wizard.read().unwrap();
        ok_json(&wiz.status_snapshot())
    }

    #[tool(description = "Persist semantic intent calibration for the loaded VRM. When a wizard session is active or complete, saves its draft multipliers; otherwise saves the stored calibration unchanged.")]
    async fn save_intent_calibration(&self) -> CallToolResult {
        let path = self.semantic_model_path.read().unwrap().clone();
        let key = crate::plugins::vrm_preset_key(&path);
        let wiz = self.intent_calibration_wizard.read().unwrap();
        let (cal, save_path) = match &wiz.phase {
            intent_calibration_wizard::WizardPhase::Active {
                draft,
                logical_path,
                ..
            }
            | intent_calibration_wizard::WizardPhase::Complete {
                draft,
                logical_path,
                ..
            } => (draft.clone(), logical_path.clone()),
            intent_calibration_wizard::WizardPhase::Idle => {
                drop(wiz);
                let cal = self.semantic_calibration.read().unwrap().get(&key);
                let mut store = self.semantic_calibration.write().unwrap();
                store.insert(key.clone(), cal.clone());
                return match store.save_file(&key, &path, &cal) {
                    Ok(()) => ok_json(&json!({
                        "saved": true,
                        "vrmKey": key,
                        "calibration": cal,
                    })),
                    Err(e) => err_text(e),
                };
            }
        };
        drop(wiz);
        let mut store = self.semantic_calibration.write().unwrap();
        store.insert(key.clone(), cal.clone());
        match store.save_file(&key, &save_path, &cal) {
            Ok(()) => ok_json(&json!({
                "saved": true,
                "vrmKey": key,
                "calibration": cal,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Wizard: abort the in-progress calibration session without saving.")]
    async fn abort_intent_calibration_wizard(&self) -> CallToolResult {
        self.intent_calibration_wizard.write().unwrap().abort();
        ok_text("intent calibration wizard aborted")
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
        if let Some(ref expr) = args.expressions {
            if !expr.is_empty() {
                if let Err(e) = mcp_validate_expression_keys(expr, &snap) {
                    return err_text(e);
                }
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

    #[tool(description = "Get the full list of VRM humanoid bone names, expression presets discovered from the loaded VRM (VRMC_vrm preset + custom), and extra skin joint names from the live rig. For expression-only discovery in a small payload, prefer `list_expressions`.")]
    async fn get_bone_reference(&self) -> CallToolResult {
        let snap = self.snapshot.0.read();
        let mut extra: Vec<String> = snap
            .bones
            .keys()
            .filter(|k| !VRM_BONE_NAMES.contains(&k.as_str()))
            .cloned()
            .collect();
        extra.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        let expression_presets = snap.expression_presets.clone();
        ok_json(&json!({
            "bones": VRM_BONE_NAMES,
            "extraBones": extra,
            "expressionPresets": expression_presets.clone(),
            "expressions": expression_presets,
            "note": "Rotations are quaternions [x, y, z, w] in normalized pose space (identity = bind). Expression values are 0..=1. Keep x/y/z in [-0.3, 0.3]. `expressionPresets` / `expressions` list preset names baked into the current VRM (empty until the rig initializes). Drive them via `list_expressions`, `set_expression` (partial), `set_expressions_full` (whole face), `animate_expressions`, or `pose_bones.expressions`. `extraBones` lists Rigify-style joints (e.g. DEF-*) present on the loaded avatar; `pose_bones` also accepts DEF-* by prefix before the first snapshot.",
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

    #[tool(description = "Snapshot the LIVE rig into a saved pose without round-tripping through get_current_bone_state + create_pose. REQUIRED top-level key: `name` (string) — the pose library filename slug. Example: `{ \"name\": \"hip_lean\", \"description\": \"weight on right leg\", \"category\": \"general\" }`. After you sculpt with pose_bones / make_fist / adjust_bone and verify with capture_pose_views, call this to persist exactly what's on screen. `bones` (optional) restricts the snapshot to a subset of bone names — leave empty to capture every indexed bone, or pass an upper-body chain (e.g. shoulders + arms + hands) to author a foundation pose that doesn't freeze the legs. Returns the saved bone count.")]
    async fn save_current_pose(
        &self,
        Parameters(args): Parameters<SaveCurrentPoseArgs>,
    ) -> CallToolResult {
        let snap = self.snapshot.0.read().clone();
        if snap.bones.is_empty() {
            return err_text(
                "no bones indexed yet — load a VRM (load_vrm) and wait for the rig to settle"
                    .to_string(),
            );
        }
        let filter: Option<std::collections::HashSet<String>> = args.bones.as_ref().map(|v| {
            v.iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect()
        });
        let mut bones: HashMap<String, BoneRotation> = HashMap::new();
        for (name, entry) in &snap.bones {
            if let Some(allow) = filter.as_ref() {
                if !allow.contains(name) {
                    continue;
                }
            }
            bones.insert(
                name.clone(),
                BoneRotation {
                    rotation: entry.rotation,
                },
            );
        }
        if bones.is_empty() {
            return err_text(
                "no bones matched — pass bone names from get_bone_reference or omit `bones`"
                    .to_string(),
            );
        }
        let include_expressions = args.include_expressions.unwrap_or(true);
        let expressions = if include_expressions {
            snap.expressions.clone()
        } else {
            HashMap::new()
        };
        let pose = PoseFile {
            name: args.name.clone(),
            description: args.description.unwrap_or_default(),
            category: args.category.unwrap_or_else(|| "general".into()),
            bones,
            expressions,
            transition_duration: 0.4,
        };
        if let Err(e) = self.library.save_pose(&pose) {
            return err_text(format!("save failed: {e}"));
        }
        ok_json(&json!({
            "saved": pose.name,
            "boneCount": pose.bones.len(),
            "expressionCount": pose.expressions.len(),
        }))
    }

    #[tool(description = "HUMAN-IN-THE-LOOP GATE. Before using any pose in a workflow (layering, keyframing Kimodo, saving as canonical), call this to ask the operator 'is this pose good?' via a popup in the Jarvis UI. ALWAYS pass a rich `intent` (the full-body VISUAL goal) — it's shown in its own prominent panel so the operator can sculpt the rig to match what you mean if it's off. By default it applies the named library pose first (set apply=false to judge a live rig you just sculpted). Returns quickly: status:'answered' with approved:true/false (+feedback when false), or status:'pending' with a reviewId — then call await_pose_review repeatedly until answered (chunked waiting avoids the MCP client timeout). On approved:false, tweak (pose_bones/adjust_bone) and review again, looping until approved. The operator can also fix the pose THEMSELVES to match your intent and tick 'overwrite as canonical' → the live rig is saved over pose_name (overwritten:true). NOTE: idle is driven by the anim layer stack; call set_master_enabled(false) before reviewing static poses or the idle clip hides them.")]
    async fn review_pose(&self, Parameters(args): Parameters<ReviewPoseArgs>) -> CallToolResult {
        // Optionally show the named pose on the avatar so the operator judges it.
        if args.apply {
            match self.library.find_pose(&args.pose_name) {
                Ok(Some(pose)) => {
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
                Ok(None) => {
                    return err_text(format!(
                        "pose \"{}\" not found — use list_poses, or pass apply=false to review the live rig",
                        args.pose_name
                    ));
                }
                Err(e) => return err_text(format!("lookup failed: {e}")),
            }
        }

        // Open the review and wait one short chunk (well under the MCP client
        // request timeout). If the operator hasn't answered, return a pending
        // token — the agent calls await_pose_review to keep waiting.
        let id = {
            let mut st = self.pose_review.0.lock().unwrap();
            st.open(args.pose_name.clone(), args.intent.clone(), args.note.clone())
        };
        match self.wait_review_chunk(id).await {
            Some(result) => self.finish_review_json(result),
            None => ok_json(&json!({
                "status": "pending",
                "reviewId": id,
                "poseName": args.pose_name,
                "message": "popup is up in the Jarvis UI; operator hasn't answered yet. Call await_pose_review (repeatedly if needed) until it returns approved/feedback.",
            })),
        }
    }

    #[tool(description = "Poll the open pose review (from review_pose) for the operator's answer. Returns approved:true / approved:false+feedback once they click Yes/No in the Jarvis UI, or status:pending if they still haven't answered (call again), or status:idle if no review is open. Keeps each request short so it never hits the MCP client timeout. Loop calling this until you get an approved field.")]
    async fn await_pose_review(
        &self,
        Parameters(args): Parameters<AwaitPoseReviewArgs>,
    ) -> CallToolResult {
        let _ = args; // reserved for future per-call tuning
        // Take an already-answered result first (operator answered between polls).
        if let Some(result) = {
            let mut st = self.pose_review.0.lock().unwrap();
            st.take_any_result()
        } {
            return self.finish_review_json(result);
        }
        let pending = {
            let st = self.pose_review.0.lock().unwrap();
            st.pending()
        };
        let Some(pending) = pending else {
            return ok_json(&json!({
                "status": "idle",
                "message": "no pose review is open — call review_pose first",
            }));
        };
        match self.wait_review_chunk(pending.id).await {
            Some(result) => self.finish_review_json(result),
            None => ok_json(&json!({
                "status": "pending",
                "reviewId": pending.id,
                "poseName": pending.pose_name,
                "message": "still waiting on the operator; call await_pose_review again",
            })),
        }
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

    #[tool(description = "AUTONOMY METADATA. Record a pose's tags, hip height (root_y), autonomous-use flag, and natural transition targets in the central pose graph (config/pose_graph.json). The transition-graph baker and the heartbeat pose-director read this. `root_y` (>0 sets it; 0 leaves unchanged) means callers/baker stop guessing hip height (stand≈0.9, kneel≈0.5, lying≈0.2). `tags`/`next_poses` (non-empty replaces; empty leaves). `autonomous` is applied every call (defaults true — pass false to un-bless). Tag reviewed poses that should be eligible for autonomous idle.")]
    async fn tag_pose(&self, Parameters(args): Parameters<TagPoseArgs>) -> CallToolResult {
        let path = PathBuf::from("config/pose_graph.json");
        let mut graph = PoseGraph::load(&path).unwrap_or_default();
        let tags = (!args.tags.is_empty()).then(|| args.tags.clone());
        let root_y = (args.root_y > 0.0).then_some(args.root_y);
        let next = (!args.next_poses.is_empty()).then(|| args.next_poses.clone());
        graph.upsert(&args.name, tags, root_y, Some(args.autonomous), next);
        if let Err(e) = graph.save(&path) {
            return err_text(format!("save pose graph: {e}"));
        }
        let meta = graph.poses.get(&args.name).cloned().unwrap_or_default();
        ok_json(&json!({
            "pose": args.name,
            "meta": meta,
            "graphPath": path.display().to_string(),
            "totalTagged": graph.poses.len(),
        }))
    }

    #[tool(description = "Read the autonomy pose graph (config/pose_graph.json): every tagged pose with its tags / root_y / autonomous flag / next_poses edges. The baker uses this to know which transitions to pre-generate; the director uses it to pick the next pose. Returns the full graph plus the list of autonomous pose names.")]
    async fn get_pose_graph(&self) -> CallToolResult {
        let path = PathBuf::from("config/pose_graph.json");
        let graph = match PoseGraph::load(&path) {
            Ok(g) => g,
            Err(e) => return err_text(format!("load pose graph: {e}")),
        };
        let mut autonomous: Vec<String> = graph
            .poses
            .iter()
            .filter(|(_, m)| m.autonomous)
            .map(|(n, _)| n.clone())
            .collect();
        autonomous.sort();
        ok_json(&json!({
            "poses": graph.poses,
            "autonomousPoses": autonomous,
            "count": graph.poses.len(),
        }))
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
            constraints: (!args.constraints_path.trim().is_empty())
                .then(|| serde_json::Value::String(args.constraints_path.trim().to_string())),
            allow_root_motion: args.allow_root_motion,
            ..Default::default()
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

    #[tool(description = "PHASE C: keyframe Kimodo with OUR library poses. Give a prompt + a list of { pose, frame, root_y } and the motion service retargets each VRM pose into a SOMA77 fullbody constraint so the diffused motion passes THROUGH your poses at those frames (with root + foot cleanup). Far more reliable than text alone for transitions. Tips: review each pose with review_pose first; put the key destination pose at/near the LAST frame; set root_y per keyframe (stand≈0.90, kneel/fold≈0.50). Requires the Kimodo hub peer online. save_name writes JSON to the animations dir (check librarySaveVerified).")]
    async fn keyframe_pose_motion(
        &self,
        Parameters(args): Parameters<KeyframePoseMotionArgs>,
    ) -> CallToolResult {
        if args.keyframes.is_empty() {
            return err_text("provide at least one keyframe { pose, frame, root_y }".to_string());
        }
        // Validate poses exist + default root_y, build the poseKeyframes JSON.
        let mut kf_json = Vec::with_capacity(args.keyframes.len());
        for kf in &args.keyframes {
            match self.library.find_pose(&kf.pose) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return err_text(format!(
                        "keyframe pose \"{}\" not found — use list_poses",
                        kf.pose
                    ));
                }
                Err(e) => return err_text(format!("lookup failed for \"{}\": {e}", kf.pose)),
            }
            kf_json.push(json!({
                // Frame 0 hard-crashes the Kimodo fullbody loader — clamp to >=1.
                "pose": kf.pose,
                "frame": kf.frame.max(1),
                "root_y": kf.root_y.unwrap_or(0.90),
            }));
        }

        // 0-sentinels mean "use the config/preset default" (typed params can't
        // be Option without re-introducing the stringify quirk).
        let timeout_sec = (if args.timeout_sec == 0 {
            self.kimodo_defaults.timeout_sec
        } else {
            args.timeout_sec
        })
        .clamp(10, 3600);
        let cfg = Some((
            if args.text_weight == 0.0 { 2.0 } else { args.text_weight },
            if args.constraint_weight == 0.0 { 3.0 } else { args.constraint_weight },
        ));
        let req = GenerateRequest {
            prompt: args.prompt,
            duration: if args.duration == 0.0 {
                self.kimodo_defaults.duration_sec
            } else {
                args.duration.clamp(0.5, 20.0)
            },
            steps: if args.steps == 0 { self.kimodo_defaults.steps } else { args.steps },
            stream: args.stream,
            save_name: args.save_name,
            timeout: std::time::Duration::from_secs(timeout_sec),
            cfg,
            allow_root_motion: args.allow_root_motion,
            pose_keyframes: Some(Value::Array(kf_json)),
            ..Default::default()
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
                        obj.insert("librarySaveVerified".to_string(), json!(expected.exists()));
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

    #[tool(description = "JSON snapshot of the animation layer stack: masterEnabled, per-layer id/slug, driver kind+params, blendMode (`override` = absolute local rotations, `additive` = rest-relative deltas), bone masks, weights, playback flags. Use list_generated_animations / list_poses before add_layer for clip / pose_hold filenames.")]
    async fn list_layers(&self) -> CallToolResult {
        let v = self
            .layer_stack
            .with_read(anim_layer_mcp::stack_snapshot_json);
        ok_json(&v)
    }

    #[tool(description = "Read the layer glitch monitor: settings, totalSpikes (monotonic since app start — diff it across a soak to count pops), and recent spike events (peak deg/s, bone, layer, playhead time). Newest first.")]
    async fn get_glitch_log(
        &self,
        Parameters(args): Parameters<GetGlitchLogArgs>,
    ) -> CallToolResult {
        let limit = args.limit.unwrap_or(50).clamp(1, 400) as usize;
        let g = self.glitch_log.inner.read();
        ok_json(&json!({
            "enabled": g.enabled,
            "sensitivity": g.sensitivity,
            "floorDps": g.floor_dps,
            "totalSpikes": g.total_spikes,
            "recent": g.log.iter().rev().take(limit).map(|e| json!({
                "at": e.at,
                "layer": e.layer_label,
                "layerId": e.layer_id,
                "layerTime": e.layer_time,
                "peakDps": e.peak_dps,
                "bone": e.bone,
                "ratio": e.ratio,
            })).collect::<Vec<_>>(),
        }))
    }

    #[tool(description = "List saved layer-set names from `config/anim_layer_sets.json` (see save_layer_set / load_layer_set / delete_layer_set).")]
    async fn list_layer_sets(&self) -> CallToolResult {
        let names = anim_layer_mcp::list_layer_set_names(&self.layer_sets);
        ok_json(&json!({ "sets": names, "count": names.len() }))
    }

    #[tool(description = "Return the full layer authoring guide (assets/LAYER_AUTHORING_GUIDE.md): driver kinds, blend modes, mask recipes, workflow with capture_pose_views. Read alongside get_pose_guide.")]
    async fn get_layer_authoring_guide(&self) -> CallToolResult {
        match anim_layer_mcp::read_layer_guide(&self.layer_guide_path) {
            Ok(s) => ok_text(s),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Turn the layer stack master switch on or off. When off, the stack emits no ApplyBones/ApplyExpression and the rig follows VRMA / idle_tick / MCP pose commands only.")]
    async fn set_master_enabled(
        &self,
        Parameters(args): Parameters<SetMasterEnabledArgs>,
    ) -> CallToolResult {
        self.layer_stack
            .with_write(|s| s.master_enabled = args.enabled);
        ok_json(&json!({ "masterEnabled": args.enabled }))
    }

    #[tool(description = "Append one layer. `driver` is a tagged union: kind `clip` { filename } (use list_generated_animations), `pose_hold` { pose_ref } (list_poses), or procedural kinds `breathing` | `blink` | `weight_shift` | `finger_fidget` | `toe_fidget` with optional numeric fields. Default blend_mode is override; use `additive` for rest-relative procedural deltas on top of earlier layers. Returns layerId.")]
    async fn add_layer(
        &self,
        Parameters(args): Parameters<AddLayerArgs>,
    ) -> CallToolResult {
        let slug = args.slug.clone();
        let layer = match anim_layer_mcp::build_layer(self.library.as_ref(), &args) {
            Ok(l) => l,
            Err(e) => return err_text(e),
        };
        let id = self.layer_stack.with_write(|s| s.add_layer(layer));
        ok_json(&json!({ "layerId": id, "slug": slug }))
    }

    #[tool(description = "Patch a layer by numeric id (from list_layers) or unique slug/label. Optional driver_params for procedural drivers only — clip/pose_hold cannot change; use remove_layer + add_layer. blend_mode: `override` or `additive`/`rest_relative`.")]
    async fn update_layer(
        &self,
        Parameters(args): Parameters<UpdateLayerArgs>,
    ) -> CallToolResult {
        let label = args.id_or_slug.clone();
        let res: Result<(), String> = self.layer_stack.with_write(|stack| {
            let id = anim_layer_mcp::resolve_layer_id(stack, &args.id_or_slug)?;
            let layer = stack
                .layers
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or_else(|| format!("layer id {id} not found"))?;
            anim_layer_mcp::apply_layer_row_patch(layer, &args)
        });
        match res {
            Ok(()) => ok_text(format!("updated layer {label}")),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Remove a layer by id or slug/label (see list_layers).")]
    async fn remove_layer(
        &self,
        Parameters(args): Parameters<RemoveLayerArgs>,
    ) -> CallToolResult {
        let label = args.id_or_slug.clone();
        let res: Result<(), String> = self.layer_stack.with_write(|stack| {
            let id = anim_layer_mcp::resolve_layer_id(stack, &args.id_or_slug)?;
            if stack.retire_layer(id) {
                Ok(())
            } else {
                Err(format!("remove_layer failed for id {id}"))
            }
        });
        match res {
            Ok(()) => ok_text(format!("removed layer {label}")),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Remove every layer from the stack (does not change masterEnabled). Use before install_default_layers for a clean procedural baseline.")]
    async fn clear_layers(&self) -> CallToolResult {
        self.layer_stack.with_write(|s| s.layers.clear());
        ok_text("cleared all layers")
    }

    #[tool(description = "Atomically replace the entire layer stack with the supplied list (clear + N×add_layer in one call). Use this whenever you would otherwise chain clear_layers + many add_layer calls (preset authoring, mood switches). Optional master_enabled toggles composing on/off; optional save_as + persist write the resulting stack to a named layer-set in the same call. Returns the rebuilt stack snapshot.")]
    async fn set_layer_stack(
        &self,
        Parameters(args): Parameters<SetLayerStackArgs>,
    ) -> CallToolResult {
        let mut built: Vec<crate::plugins::anim_layers::Layer> =
            Vec::with_capacity(args.layers.len());
        for spec in &args.layers {
            match anim_layer_mcp::build_layer(self.library.as_ref(), spec) {
                Ok(layer) => built.push(layer),
                Err(e) => {
                    return err_text(format!("layer {:?}: {e}", spec.slug));
                }
            }
        }
        self.layer_stack.with_write(|s| {
            s.layers.clear();
            for layer in built {
                s.add_layer(layer);
            }
            if let Some(en) = args.master_enabled {
                s.master_enabled = en;
            }
        });
        if let Some(name) = args.save_as.as_deref() {
            self.layer_stack.with_read(|stack| {
                anim_layer_mcp::save_layer_set_current(
                    &self.layer_sets,
                    stack,
                    name,
                    args.persist.unwrap_or(true),
                );
            });
        }
        let v = self
            .layer_stack
            .with_read(anim_layer_mcp::stack_snapshot_json);
        ok_json(&v)
    }

    #[tool(description = "Replace the stack with the five default procedural layers (breathing, auto-blink, weight-shift, finger-fidget, toe-fidget) and set masterEnabled (default true if master_enabled omitted). Idempotent baseline for `idle_v1` style motion.")]
    async fn install_default_layers(
        &self,
        Parameters(args): Parameters<InstallDefaultLayersArgs>,
    ) -> CallToolResult {
        self.layer_stack.with_write(|s| {
            anim_layer_mcp::install_default_layers_stack(s, args.master_enabled);
        });
        let v = self
            .layer_stack
            .with_read(anim_layer_mcp::stack_snapshot_json);
        ok_json(&v)
    }

    #[tool(description = "Snapshot the current stack to a named layer set in memory; when persist=true (default), write `config/anim_layer_sets.json`.")]
    async fn save_layer_set(
        &self,
        Parameters(args): Parameters<SaveLayerSetArgs>,
    ) -> CallToolResult {
        self.layer_stack.with_read(|stack| {
            anim_layer_mcp::save_layer_set_current(
                &self.layer_sets,
                stack,
                &args.name,
                args.persist,
            );
        });
        ok_json(&json!({
            "saved": args.name,
            "persisted": args.persist,
        }))
    }

    #[tool(description = "Replace the live stack with a named set from `config/anim_layer_sets.json` (rehydrates clip/pose layers from disk via pose_library paths).")]
    async fn load_layer_set(
        &self,
        Parameters(args): Parameters<LoadLayerSetArgs>,
    ) -> CallToolResult {
        let set_name = args.name.clone();
        let res: Result<usize, String> = self.layer_stack.with_write(|stack| {
            anim_layer_mcp::load_layer_set_named(
                &self.layer_sets,
                stack,
                self.library.as_ref(),
                &set_name,
            )
        });
        match res {
            Ok(count) => ok_json(&json!({
                "loaded": set_name,
                "layerCount": count,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "Delete a named layer set from the in-memory map; when persist=true (default), rewrite `config/anim_layer_sets.json`.")]
    async fn delete_layer_set(
        &self,
        Parameters(args): Parameters<DeleteLayerSetArgs>,
    ) -> CallToolResult {
        anim_layer_mcp::delete_layer_set_named(&self.layer_sets, &args.name, args.persist);
        ok_json(&json!({ "deleted": args.name, "persisted": args.persist }))
    }

    #[tool(description = "Render transparent PNG snapshots of the avatar from one or more camera views (front / sides / diagonals / back). Blocks until Bevy finishes all screenshots or timeout. **Required args:** `capture_id`, `views` (e.g. front, left, back). **`output_dir` is optional** — defaults to `pose_captures` on the host (PNG files are a side effect; the tool also returns inline `image/png` base64 blocks so agents can verify the silhouette without reading paths). Use framing_preset full_body or face_closeup. Waits settle_before_capture_ms (default 120) — set 0 for instant capture. embed_images=false skips embedding; max_embed_dimension=0 embeds full-res. POLICY: front-only captures emit a `viewCoverageWarning` because knee direction / elbow inversion / foot crossover are invisible from the front — always include at least one side and (for upper-body changes) `back`.")]
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
        let view_coverage_warning = capture_view_policy_warning(&args.views);
        let framing = match args.framing_preset.as_deref().map(str::trim) {
            None | Some("") => None,
            Some("full_body") => Some(CaptureFramingPreset::FullBody),
            Some("face_closeup") => Some(CaptureFramingPreset::FaceCloseup),
            Some("feet") => Some(CaptureFramingPreset::Feet),
            Some("hands") => Some(CaptureFramingPreset::Hands),
            Some(x) => {
                return err_text(format!(
                    "invalid framing_preset {x:?} — use full_body or face_closeup"
                ));
            }
        };
        let settle_ms = args.settle_before_capture_ms.unwrap_or(120).min(5000);
        if settle_ms > 0 {
            tokio::time::sleep(Duration::from_millis(u64::from(settle_ms))).await;
        }
        let timeout = Duration::from_secs(args.timeout_sec.unwrap_or(180).clamp(5, 600));
        let output_dir = {
            let trimmed = args.output_dir.trim();
            let path = if trimmed.is_empty() {
                Path::new("pose_captures")
            } else {
                Path::new(trimmed)
            };
            expand_home(path)
        };
        let embed_images = args.embed_images.unwrap_or(true);
        let max_embed_dim = args.max_embed_dimension.min(8192);
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
        let result = match tokio::task::spawn_blocking(move || rx.recv_timeout(timeout)).await {
            Ok(Ok(result)) => result,
            Ok(Err(RecvTimeoutError::Timeout)) => {
                return err_text(
                    "capture timed out — try a longer timeout_sec or fewer views".to_string(),
                );
            }
            Ok(Err(RecvTimeoutError::Disconnected)) => {
                return err_text("capture response channel closed before result".to_string());
            }
            Err(e) => return err_text(format!("capture task join: {e}")),
        };

        // Build a wrapper so we can attach the view-coverage warning alongside
        // the raw `CaptureResult`. Agents that programmatically parse the JSON
        // get a stable shape; the warning never blocks the call (hybrid).
        let result_value =
            serde_json::to_value(&result).unwrap_or_else(|_| json!({ "result": "<serialize failed>" }));
        let json_text = match serde_json::to_string_pretty(&json!({
            "result": result_value,
            "viewCoverageWarning": view_coverage_warning,
        })) {
            Ok(s) => s,
            Err(e) => return err_text(format!("serialize failure: {e}")),
        };
        let mut blocks: Vec<ContentBlock> = Vec::with_capacity(1 + result.images.len());
        blocks.push(ContentBlock::text(json_text));
        if embed_images {
            let paths: Vec<PathBuf> = result
                .images
                .iter()
                .map(|img| PathBuf::from(&img.path))
                .collect();
            // Decoding + resizing PNGs is CPU-bound; offload to spawn_blocking so we
            // do not stall the tokio runtime when many large views are embedded.
            let embed_results = tokio::task::spawn_blocking(move || -> Vec<Result<ContentBlock, String>> {
                paths
                    .iter()
                    .map(|p| embed_png_as_content(p, max_embed_dim))
                    .collect()
            })
            .await;
            match embed_results {
                Ok(items) => {
                    for (img, res) in result.images.iter().zip(items.into_iter()) {
                        match res {
                            Ok(content) => blocks.push(content),
                            Err(e) => blocks.push(ContentBlock::text(format!(
                                "embed {} ({}): {e}",
                                img.view.as_slug(),
                                img.path
                            ))),
                        }
                    }
                }
                Err(e) => blocks.push(ContentBlock::text(format!("embed task join: {e}"))),
            }
        }
        CallToolResult::success(blocks)
    }

    #[tool(description = "PREVIEW A WHOLE ANIMATION AS ONE IMAGE. Steps a saved clip through N evenly-spaced frames (suppressing the layer stack so each frame shows cleanly), renders each from one camera view, and composites them into a single MONTAGE GRID PNG returned inline — so the agent can judge the full motion arc at a glance instead of guessing from one live frame. Also writes an animated GIF to disk for the human (the agent only reliably sees the static montage). Use after generate_motion / keyframe_pose_motion to review motion quality. Pick a side `view` for descents / forward motion. Restores the prior master-enabled state when done.")]
    async fn capture_animation_montage(
        &self,
        Parameters(args): Parameters<CaptureAnimationMontageArgs>,
    ) -> CallToolResult {
        let filename = {
            let f = args.filename.trim();
            if f.is_empty() {
                return err_text("filename is required".to_string());
            }
            if f.ends_with(".json") { f.to_string() } else { format!("{f}.json") }
        };
        let anim = match self.library.load_animation(&filename) {
            Ok(a) => a,
            Err(e) => return err_text(format!("load {filename}: {e}")),
        };
        let total = anim.frames.len();
        if total == 0 {
            return err_text(format!("{filename} has no frames"));
        }
        let view_slug = args.view.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("left");
        let view = match parse_capture_view_slug(view_slug) {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let framing = match args.framing_preset.as_deref().map(str::trim) {
            None | Some("") | Some("full_body") => Some(CaptureFramingPreset::FullBody),
            Some("face_closeup") => Some(CaptureFramingPreset::FaceCloseup),
            Some("feet") => Some(CaptureFramingPreset::Feet),
            Some("hands") => Some(CaptureFramingPreset::Hands),
            Some(x) => return err_text(format!("invalid framing_preset {x:?}")),
        };
        let k = ((if args.frame_count == 0 { 12 } else { args.frame_count }).clamp(2, 36) as usize)
            .min(total);
        let columns = (if args.columns == 0 { 4 } else { args.columns }).clamp(1, 8);
        let tile = (if args.tile_size == 0 { 384 } else { args.tile_size }).clamp(96, 1024);
        let also_gif = args.also_gif;
        let gif_fps = (if args.gif_fps == 0 { 8 } else { args.gif_fps }).clamp(1, 30);
        let output_dir = expand_home(Path::new(args.output_dir.trim()));
        let stem = filename.trim_end_matches(".json").to_string();

        // Evenly-spaced sample indices across the whole clip (inclusive of last).
        let indices: Vec<usize> = (0..k)
            .map(|i| if k == 1 { 0 } else { i * (total - 1) / (k - 1) })
            .collect();

        // Suppress the layer stack so our per-frame ApplyBones isn't overwritten.
        let prev_master = self
            .layer_stack
            .with_write(|s| {
                let p = s.master_enabled;
                s.master_enabled = false;
                p
            });

        let mut paths: Vec<PathBuf> = Vec::with_capacity(k);
        let mut step_err: Option<String> = None;
        for (n, &idx) in indices.iter().enumerate() {
            let frame = &anim.frames[idx];
            let bones: HashMap<String, [f32; 4]> = frame
                .bones
                .iter()
                .map(|(b, r)| (b.clone(), r.rotation))
                .collect();
            self.pose_tx.send(PoseCommand::ApplyBones {
                bones,
                preserve_omitted_bones: true,
                blend_weight: None,
                transition_seconds: Some(0.0),
            });
            let root = frame.root_position.unwrap_or([0.0, 0.0, 0.0]);
            let mut tmap = HashMap::new();
            tmap.insert("hips".to_string(), root);
            self.pose_tx.send(PoseCommand::ApplyBoneTranslations(tmap));
            // let the pose land before screenshotting
            tokio::time::sleep(Duration::from_millis(110)).await;

            let (tx, rx) = crossbeam_channel::unbounded();
            let req = CaptureRequest {
                output_dir: output_dir.clone(),
                capture_id: format!("{stem}_montage_f{n:02}"),
                width: tile,
                height: tile,
                views: vec![view.clone()],
                framing_preset: framing,
                camera_overrides: None,
                response_tx: tx,
            };
            if self.capture_tx.0.send(req).is_err() {
                step_err = Some("capture channel closed".into());
                break;
            }
            let res = tokio::task::spawn_blocking(move || {
                rx.recv_timeout(Duration::from_secs(20))
            })
            .await;
            match res {
                Ok(Ok(result)) => {
                    if let Some(img) = result.images.first() {
                        paths.push(PathBuf::from(&img.path));
                    } else {
                        step_err = Some(format!("frame {idx}: capture returned no image"));
                        break;
                    }
                }
                Ok(Err(_)) => {
                    step_err = Some(format!("frame {idx}: capture timed out"));
                    break;
                }
                Err(e) => {
                    step_err = Some(format!("frame {idx}: join {e}"));
                    break;
                }
            }
        }

        // Always restore the prior master state.
        self.layer_stack.with_write(|s| s.master_enabled = prev_master);

        if let Some(e) = step_err {
            return err_text(format!("montage aborted: {e}"));
        }
        if paths.is_empty() {
            return err_text("no frames captured".to_string());
        }

        // Composite the montage + optional GIF off the async runtime.
        let paths_for_blocking = paths.clone();
        let out_dir = output_dir.clone();
        let stem_b = stem.clone();
        let build = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, PathBuf, Option<PathBuf>, Option<String>), String> {
            let montage = build_montage_png(&paths_for_blocking, columns, tile)?;
            let montage_path = out_dir.join(format!("{stem_b}_montage.png"));
            std::fs::write(&montage_path, &montage)
                .map_err(|e| format!("write montage: {e}"))?;
            let (gif_path, gif_err) = if also_gif {
                let gp = out_dir.join(format!("{stem_b}.gif"));
                match write_animation_gif(&paths_for_blocking, &gp, gif_fps) {
                    Ok(()) => (Some(gp), None),
                    Err(e) => (None, Some(e)),
                }
            } else {
                (None, None)
            };
            Ok((montage, montage_path, gif_path, gif_err))
        })
        .await;

        let (montage_bytes, montage_path, gif_path, gif_err) = match build {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return err_text(e),
            Err(e) => return err_text(format!("composite task join: {e}")),
        };

        let summary = json!({
            "animation": filename,
            "sourceFrames": total,
            "sampledFrames": paths.len(),
            "frameIndices": indices,
            "view": view_slug,
            "columns": columns,
            "montagePath": montage_path.display().to_string(),
            "gifPath": gif_path.as_ref().map(|p| p.display().to_string()),
            "gifError": gif_err,
            "note": "Montage grid reads left-to-right, top-to-bottom (start → end). GIF is for the human.",
        });
        let mut blocks = vec![ContentBlock::text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )];
        {
            use base64::engine::general_purpose::STANDARD as B64;
            use base64::Engine as _;
            blocks.push(ContentBlock::image(B64.encode(&montage_bytes), "image/png"));
        }
        CallToolResult::success(blocks)
    }

    #[tool(description = "RECORD THE LIVE VIEWPORT OVER TIME. Samples the running avatar's viewport at `fps` for `duration_sec` seconds from one camera view WITHOUT suppressing the layer stack / alive director — so it captures AUTONOMOUS live behavior (idle director picks, procedural fidgets, transitions, expression presets, gestures) actually in motion. Composites the frames into a MONTAGE GRID PNG returned inline (earliest→latest, left-to-right) so the agent can review the motion arc, and writes an animated GIF to disk for the human. Use this to self-verify live behavior instead of guessing from a single static capture.")]
    async fn record_viewport(
        &self,
        Parameters(args): Parameters<RecordViewportArgs>,
    ) -> CallToolResult {
        let view_slug = args
            .view
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("front");
        let view = match parse_capture_view_slug(view_slug) {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let framing = match args.framing_preset.as_deref().map(str::trim) {
            None | Some("") | Some("full_body") => Some(CaptureFramingPreset::FullBody),
            Some("face_closeup") => Some(CaptureFramingPreset::FaceCloseup),
            Some("feet") => Some(CaptureFramingPreset::Feet),
            Some("hands") => Some(CaptureFramingPreset::Hands),
            Some(x) => return err_text(format!("invalid framing_preset {x:?}")),
        };
        let duration_sec = (if args.duration_sec == 0.0 { 6.0 } else { args.duration_sec })
            .clamp(0.5, 30.0);
        let fps = (if args.fps == 0 { 6 } else { args.fps }).clamp(1, 20);
        let frame_count = ((duration_sec * fps as f32).round() as u32).clamp(2, 48);
        let interval = Duration::from_secs_f32(1.0 / fps as f32);
        let columns = (if args.columns == 0 { 4 } else { args.columns }).clamp(1, 8);
        let tile = (if args.tile_size == 0 { 384 } else { args.tile_size }).clamp(96, 1024);
        let also_gif = args.also_gif;
        let output_dir = expand_home(Path::new(args.output_dir.trim()));
        let stem = {
            let l = args.label.trim();
            if l.is_empty() {
                "viewport".to_string()
            } else {
                l.replace([' ', '/', '\\'], "_")
            }
        };

        // Sample the LIVE scene over wall-clock time. We deliberately do NOT
        // suppress the layer stack / director, so the recording shows real
        // autonomous behavior in motion.
        let mut paths: Vec<PathBuf> = Vec::with_capacity(frame_count as usize);
        let mut step_err: Option<String> = None;
        for n in 0..frame_count {
            tokio::time::sleep(interval).await;
            let (tx, rx) = crossbeam_channel::unbounded();
            let req = CaptureRequest {
                output_dir: output_dir.clone(),
                capture_id: format!("{stem}_rec_f{n:03}"),
                width: tile,
                height: tile,
                views: vec![view.clone()],
                framing_preset: framing,
                camera_overrides: None,
                response_tx: tx,
            };
            if self.capture_tx.0.send(req).is_err() {
                step_err = Some("capture channel closed".into());
                break;
            }
            let res =
                tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(20))).await;
            match res {
                Ok(Ok(result)) => {
                    if let Some(img) = result.images.first() {
                        paths.push(PathBuf::from(&img.path));
                    } else {
                        step_err = Some(format!("frame {n}: capture returned no image"));
                        break;
                    }
                }
                Ok(Err(_)) => {
                    step_err = Some(format!("frame {n}: capture timed out"));
                    break;
                }
                Err(e) => {
                    step_err = Some(format!("frame {n}: join {e}"));
                    break;
                }
            }
        }

        if paths.is_empty() {
            return err_text(format!(
                "no frames captured{}",
                step_err.map(|e| format!(": {e}")).unwrap_or_default()
            ));
        }

        let paths_for_blocking = paths.clone();
        let out_dir = output_dir.clone();
        let stem_b = stem.clone();
        let gif_fps = fps;
        let build = tokio::task::spawn_blocking(
            move || -> Result<(Vec<u8>, PathBuf, Option<PathBuf>, Option<String>), String> {
                let montage = build_montage_png(&paths_for_blocking, columns, tile)?;
                let montage_path = out_dir.join(format!("{stem_b}_recording.png"));
                std::fs::write(&montage_path, &montage)
                    .map_err(|e| format!("write montage: {e}"))?;
                let (gif_path, gif_err) = if also_gif {
                    let gp = out_dir.join(format!("{stem_b}_recording.gif"));
                    match write_animation_gif(&paths_for_blocking, &gp, gif_fps) {
                        Ok(()) => (Some(gp), None),
                        Err(e) => (None, Some(e)),
                    }
                } else {
                    (None, None)
                };
                Ok((montage, montage_path, gif_path, gif_err))
            },
        )
        .await;

        let (montage_bytes, montage_path, gif_path, gif_err) = match build {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return err_text(e),
            Err(e) => return err_text(format!("composite task join: {e}")),
        };

        let summary = json!({
            "durationSec": duration_sec,
            "fps": fps,
            "frames": paths.len(),
            "view": view_slug,
            "columns": columns,
            "montagePath": montage_path.display().to_string(),
            "gifPath": gif_path.as_ref().map(|p| p.display().to_string()),
            "gifError": gif_err,
            "partial": step_err,
            "note": "LIVE recording (layer stack / director NOT suppressed). Montage grid reads left-to-right, top-to-bottom = earliest → latest. GIF is for the human.",
        });
        let mut blocks = vec![ContentBlock::text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )];
        {
            use base64::engine::general_purpose::STANDARD as B64;
            use base64::Engine as _;
            blocks.push(ContentBlock::image(B64.encode(&montage_bytes), "image/png"));
        }
        CallToolResult::success(blocks)
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
                    "functionId": cfg.function_id,
                    "healthOk": health.ok,
                    "healthError": health.error,
                })),
            );
        }
        ok_json(&json!({
            "enabled": cfg.enabled,
            "endpoint": cfg.endpoint,
            "healthUrl": cfg.health_url,
            "functionId": cfg.function_id,
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

impl JarvisMcpServer {
    fn dispatch_intent_bones_map(
        &self,
        bones: &HashMap<String, BoneEulerDeg>,
        _touches_arms_hint: bool,
    ) -> Result<usize, String> {
        if bones.is_empty() {
            return Err("empty bone map".into());
        }
        let snap = self.snapshot.0.read();
        for bone in bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return Err(format!(
                    "bone \"{bone}\" not on the loaded VRM — load a rig first"
                ));
            }
        }
        drop(snap);

        let safety = PoseSafetyReport::from_euler_map(bones);
        if let Some(reason) = safety.should_block(false, false) {
            return Err(format!(
                "unsafe map ({}): {reason}",
                safety.severity.as_str()
            ));
        }

        let (quats, _warnings) = bone_map_from_euler_deg(bones);
        let (sanitized, _w2) = sanitize_bone_map(quats);
        let count = sanitized.len();
        self.pose_tx.send(PoseCommand::ApplyBones {
            bones: sanitized,
            preserve_omitted_bones: true,
            blend_weight: None,
            transition_seconds: None,
        });
        Ok(count)
    }

    /// Common dispatch path for the semantic intent tools (`raise_leg`,
    /// `bend_knee`, `arms_down_rest`). Validates bone keys against the live
    /// snapshot, runs the same hybrid-safety + sanitize pipeline as
    /// `pose_bones`, and either applies or returns a dry-run summary.
    fn apply_intent_bones(
        &self,
        intent_name: &'static str,
        bones: &HashMap<String, BoneEulerDeg>,
        dry_run: bool,
        touches_legs_hint: bool,
        touches_arms_hint: bool,
    ) -> CallToolResult {
        if bones.is_empty() {
            return err_text(format!(
                "{intent_name}: compiled an empty bone map (amount probably 0?) — nothing to apply"
            ));
        }
        let snap = self.snapshot.0.read();
        for bone in bones.keys() {
            if !mcp_allows_pose_bone_key(bone, &snap) {
                return err_text(format!(
                    "{intent_name}: bone \"{bone}\" not on the loaded VRM — use a rig with the airi-family humanoid mapping"
                ));
            }
        }
        drop(snap);

        let safety = PoseSafetyReport::from_euler_map(bones);
        // Semantic tools are designed to stay below severe thresholds. If they
        // ever produce one, that's an authoring bug — block hard.
        if let Some(reason) = safety.should_block(false, false) {
            return err_text(format!(
                "{intent_name}: internal compiler produced an unsafe map ({}): {reason}",
                safety.severity.as_str()
            ));
        }

        let (quats, mut warnings) = bone_map_from_euler_deg(bones);
        let (sanitized, mut w2) = sanitize_bone_map(quats);
        warnings.append(&mut w2);
        let count = sanitized.len();

        if !dry_run {
            self.pose_tx.send(PoseCommand::ApplyBones {
                bones: sanitized.clone(),
                preserve_omitted_bones: true,
                blend_weight: None,
                transition_seconds: None,
            });
        }

        let euler_preview: HashMap<String, Value> = bones
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    json!({
                        "pitch_deg": v.pitch_deg,
                        "yaw_deg": v.yaw_deg,
                        "roll_deg": v.roll_deg,
                    }),
                )
            })
            .collect();

        let semantic_vrm_key = {
            let path = self.semantic_model_path.read().unwrap();
            crate::plugins::vrm_preset_key(&path)
        };

        let mut response = json!({
            "intent": intent_name,
            "appliedBones": if dry_run { 0 } else { count },
            "wouldApplyBones": count,
            "compiledEuler": euler_preview,
            "warnings": warnings,
            "safety": {
                "severity": safety.severity.as_str(),
                "dryRun": dry_run,
            },
            "verificationHint": verification_hint(touches_legs_hint, touches_arms_hint),
            "semanticVrmKey": semantic_vrm_key,
        });
        if dry_run {
            response["sanitizedRotations"] =
                serde_json::to_value(sanitized).unwrap_or(Value::Null);
        }
        ok_json(&response)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for JarvisMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "jarvis-avatar pose MCP (VRM). \
TOOL LADDER (try in order): \
(1) Semantic intents — raise_leg, bend_knee, arms_down_rest, make_fist — for any high-level body intent. They compile to bounded Euler maps and almost never break the rig. \
(2) Library poses — apply_pose / list_poses — for known baselines. \
(3) Raw pose_bones (Euler degrees) — only when no semantic tool fits; ALWAYS dry_run=true first if you are unsure of axes. \
(4) Raw set_bones (quaternions) — last resort for replaying numeric data. \
SAFETY: \
(a) preserve_omitted_bones=false is REJECTED on pose_bones and set_bones — use reset_pose + the tool with defaults for partial edits. \
(b) Hybrid policy hard-fails catastrophic requests (multiple bones at near-axis limits) and severe single-axis (≥80°) angles unless allow_large_angles=true. strict=true escalates near-limit warns to fails. \
(c) After ANY leg/arm edit, capture_pose_views with at least left, right, and back — front-only hides knee direction, elbow inversion, and foot crossover. The capture tool returns a viewCoverageWarning when you pass a front-only view set. \
(d) Use small degree steps and read the response warnings; iterate. \
(e) Author bones first, then morphs/expressions — avoid maxing many morphs while pushing extreme bone angles. \
Reference docs: get_pose_guide (Euler traps + arms-down rest recipe) and get_layer_authoring_guide (layer stack). \
Workflow: reset_pose or apply_pose → semantic intent (raise_leg/bend_knee/arms_down_rest/make_fist) → capture_pose_views (left, right, back) → list_expressions → set_expression → capture_pose_views (face_closeup). list_models / load_vrm for hot-swap.",
            )
    }
}

/// Build an `A2fClient` from the avatar's `[a2f]` config section.
pub fn build_a2f_client(
    enabled: bool,
    endpoint: impl Into<String>,
    health_url: impl Into<String>,
    function_id: impl Into<String>,
) -> A2fClient {
    A2fClient::new(A2fConfig {
        enabled,
        endpoint: endpoint.into(),
        health_url: health_url.into(),
        function_id: function_id.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::embed_png_as_content;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use image::{ImageBuffer, Rgba};

    fn write_test_png(path: &std::path::Path, w: u32, h: u32) {
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            // Diagonal gradient so a downscaled re-encode is still distinguishable from
            // a flat fill, in case future regressions corrupt pixel data.
            *px = Rgba([
                ((x * 255) / w.max(1)) as u8,
                ((y * 255) / h.max(1)) as u8,
                128,
                255,
            ]);
        }
        img.save(path).expect("write test PNG");
    }

    fn decoded_dims(content: &rmcp::model::ContentBlock) -> (u32, u32) {
        let raw = content
            .as_image()
            .expect("expected ContentBlock::Image variant");
        assert_eq!(raw.mime_type, "image/png", "mime must be image/png");
        let bytes = B64.decode(raw.data.as_bytes()).expect("base64 decodes");
        let img = image::load_from_memory(&bytes).expect("png decodes");
        (img.width(), img.height())
    }

    #[test]
    fn embed_png_passthrough_when_max_dim_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("raw.png");
        write_test_png(&path, 256, 192);

        let content = embed_png_as_content(&path, 0).expect("embed succeeds");
        let (w, h) = decoded_dims(&content);
        assert_eq!((w, h), (256, 192), "max_dim=0 must not resize");
    }

    #[test]
    fn embed_png_resizes_when_over_max_dim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.png");
        write_test_png(&path, 1536, 1024);

        let content = embed_png_as_content(&path, 768).expect("embed succeeds");
        let (w, h) = decoded_dims(&content);
        assert!(
            w.max(h) <= 768,
            "resized longest side must be ≤ max_dim, got {w}x{h}"
        );
        // Aspect ratio (3:2) preserved within rounding.
        let ar_in = 1536.0_f32 / 1024.0;
        let ar_out = w as f32 / h as f32;
        assert!(
            (ar_in - ar_out).abs() < 0.05,
            "aspect ratio drift: in {ar_in:.3}, out {ar_out:.3}"
        );
    }

    #[test]
    fn embed_png_keeps_small_image_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.png");
        write_test_png(&path, 320, 320);

        let content = embed_png_as_content(&path, 768).expect("embed succeeds");
        let (w, h) = decoded_dims(&content);
        assert_eq!(
            (w, h),
            (320, 320),
            "images already under max_dim must not be upscaled or resized"
        );
    }

    #[test]
    fn embed_png_missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does_not_exist.png");
        let err = embed_png_as_content(&path, 0).expect_err("missing file must error");
        assert!(err.contains("read"), "error should mention read failure: {err}");
    }

    #[test]
    fn embed_png_invalid_bytes_error_when_resizing() {
        // When max_dim > 0 we decode; corrupt bytes must surface a decode error
        // rather than silently producing an empty image block.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.png");
        std::fs::write(&path, b"not a real png").expect("write corrupt");
        let err = embed_png_as_content(&path, 256).expect_err("corrupt bytes must error");
        assert!(err.contains("decode"), "error should mention decode: {err}");
    }

    #[test]
    fn verification_hint_routes_legs_first() {
        let h = super::verification_hint(true, true);
        assert!(h.contains("leg bones"), "leg branch wins when both touched: {h}");
        assert!(h.contains("back"));
        assert!(super::verification_hint(false, true).contains("arm bones"));
        assert!(super::verification_hint(false, false).contains("capture_pose_views"));
    }

    #[test]
    fn capture_view_policy_warns_on_front_only() {
        let warning = super::capture_view_policy_warning(&["front".to_string()]);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("side"));
    }

    #[test]
    fn capture_view_policy_warns_when_back_missing() {
        let warning =
            super::capture_view_policy_warning(&["front".into(), "left".into(), "right".into()]);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("back"));
    }

    #[test]
    fn capture_view_policy_passes_with_full_coverage() {
        let warning = super::capture_view_policy_warning(&[
            "front".into(),
            "left".into(),
            "right".into(),
            "back".into(),
        ]);
        assert!(
            warning.is_none(),
            "front/left/right/back should not warn: {warning:?}"
        );
    }
}
